use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use desklink_crypto::{
    DeviceIdentity, NoiseResponder, SecureLane, SecureRole, SecureSession, SessionId,
};
use desklink_protocol::{
    Codec, ControlMessage, DeviceCapabilities, DeviceRole, FrameFlags, NoiseHandshake,
    NoiseHandshakeStep, PROTOCOL_VERSION, Platform, VideoConfig, decode_control,
    decode_cursor_update, decode_input, decode_noise_handshake, encode_control,
    encode_noise_handshake, encode_video_config, encode_video_packet,
};
use desklink_transport::{QuicClient, RelayJoin, TransportError, TransportEvent};
use desklink_video::{EncodedFrame, packetize_frame};
use ed25519_dalek::VerifyingKey;
use tokio::sync::{mpsc, watch};
use zeroize::Zeroize;

use crate::host::{
    HOST_COMMAND_CAPACITY, HostCommand, HostError, HostEvent, HostMetrics, HostState,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(15);
// Nonterminal events may be dropped when this reserve would be consumed. That keeps the
// bounded event path nonblocking while guaranteeing room for ReleaseAll, Error, and Closed.
const TERMINAL_EVENT_RESERVE: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum WorkerPhase {
    Connecting = 0,
    WaitingForApproval = 1,
    NegotiatingCapabilities = 2,
    Connected = 3,
    Stopping = 4,
    Closed = 5,
}

impl WorkerPhase {
    fn load(phase: &AtomicU8) -> Self {
        match phase.load(Ordering::Acquire) {
            0 => Self::Connecting,
            1 => Self::WaitingForApproval,
            2 => Self::NegotiatingCapabilities,
            3 => Self::Connected,
            4 => Self::Stopping,
            _ => Self::Closed,
        }
    }
}

pub(crate) struct HostWorker {
    commands: mpsc::Sender<HostCommand>,
    cancellation: watch::Sender<bool>,
    phase: Arc<AtomicU8>,
    command_gate: Mutex<()>,
    thread: Option<thread::JoinHandle<()>>,
}

impl HostWorker {
    pub(crate) fn start(
        client: QuicClient,
        identity: DeviceIdentity,
        session_id: SessionId,
        relay_authentication: [u8; 32],
        events: mpsc::Sender<HostEvent>,
    ) -> Result<Self, HostError> {
        let (commands, receiver) = mpsc::channel(HOST_COMMAND_CAPACITY);
        let (cancellation, cancellation_receiver) = watch::channel(false);
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        let phase = Arc::new(AtomicU8::new(WorkerPhase::Connecting as u8));
        let worker_phase = phase.clone();
        let thread = thread::Builder::new()
            .name("desklink-host".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(run_worker(
                        client,
                        identity,
                        session_id,
                        relay_authentication,
                        receiver,
                        cancellation_receiver,
                        events,
                        worker_phase.clone(),
                        ready_sender,
                    )),
                    Err(_) => {
                        let _ = ready_sender.send(Err(HostError::WorkerStopped));
                        emit_terminal(&events, Some(HostError::WorkerStopped));
                    }
                }
                worker_phase.store(WorkerPhase::Closed as u8, Ordering::Release);
            })
            .map_err(|_| HostError::WorkerStopped)?;
        match ready_receiver
            .recv()
            .map_err(|_| HostError::WorkerStopped)?
        {
            Ok(()) => Ok(Self {
                commands,
                cancellation,
                phase,
                command_gate: Mutex::new(()),
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }

    pub(crate) fn state(&self) -> HostState {
        WorkerPhase::load(&self.phase).into()
    }

    pub(crate) fn send(&self, command: HostCommand) -> Result<(), HostError> {
        let _command_gate = self
            .command_gate
            .lock()
            .map_err(|_| HostError::WorkerStopped)?;
        let phase = WorkerPhase::load(&self.phase);
        if command.requires_connection()
            && !matches!(
                phase,
                WorkerPhase::NegotiatingCapabilities | WorkerPhase::Connected
            )
        {
            return Err(HostError::InvalidState);
        }
        if matches!(command, HostCommand::Approve { .. })
            && phase != WorkerPhase::WaitingForApproval
        {
            return Err(HostError::InvalidState);
        }
        if matches!(command, HostCommand::Reject)
            && !matches!(
                phase,
                WorkerPhase::Connecting
                    | WorkerPhase::WaitingForApproval
                    | WorkerPhase::NegotiatingCapabilities
                    | WorkerPhase::Connected
            )
        {
            return Err(HostError::InvalidState);
        }
        if matches!(command, HostCommand::Stop)
            && matches!(phase, WorkerPhase::Stopping | WorkerPhase::Closed)
        {
            return Err(HostError::InvalidState);
        }

        let next_phase = match &command {
            HostCommand::Approve { .. } => Some(WorkerPhase::NegotiatingCapabilities),
            HostCommand::Reject | HostCommand::Stop => Some(WorkerPhase::Stopping),
            _ => None,
        };
        let cancels_worker = matches!(&command, HostCommand::Reject | HostCommand::Stop);
        self.commands
            .try_send(command)
            .map_err(|_| HostError::CommandQueueFull)?;

        // Command acceptance and the externally visible phase transition share this gate. A
        // full command queue therefore leaves the prior phase intact, so a rejected Approve
        // cannot unlock media and a rejected Reject/Stop cannot invent a terminal state.
        if let Some(next_phase) = next_phase {
            self.phase.store(next_phase as u8, Ordering::Release);
        }
        if cancels_worker {
            // Cancellation is signalled only after the terminal command entered the bounded
            // command path. It interrupts Noise and capability waits immediately.
            let _ = self.cancellation.send(true);
        }
        Ok(())
    }

    pub(crate) fn shutdown(&mut self) {
        let _ = self.send(HostCommand::Stop);
        // destroy is an ownership teardown operation, so it must still interrupt the worker if
        // the bounded command queue is already full. The phase remains unchanged in that case.
        let _ = self.cancellation.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HostWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn emit_nonterminal(events: &mpsc::Sender<HostEvent>, event: HostEvent) {
    // Never await application event capacity on the worker. Input, keyframe, metrics, and
    // approval notifications are best-effort under backpressure; terminal notifications below
    // retain their reserved slots and are never displaced by this path.
    if events.capacity() > TERMINAL_EVENT_RESERVE {
        let _ = events.try_send(event);
    }
}

fn emit_terminal(events: &mpsc::Sender<HostEvent>, error: Option<HostError>) {
    // Every worker producer uses emit_nonterminal, so the three slots reserved above make this
    // nonblocking sequence deliver ReleaseAll before its optional Error and final Closed state.
    let _ = events.try_send(HostEvent::ReleaseAll);
    if let Some(error) = error {
        let _ = events.try_send(HostEvent::Error(error));
    }
    let _ = events.try_send(HostEvent::State(HostState::Closed));
}

async fn run_worker(
    client: QuicClient,
    identity: DeviceIdentity,
    session_id: SessionId,
    mut relay_authentication: [u8; 32],
    mut commands: mpsc::Receiver<HostCommand>,
    mut cancellation: watch::Receiver<bool>,
    events: mpsc::Sender<HostEvent>,
    phase: Arc<AtomicU8>,
    ready: std::sync::mpsc::SyncSender<Result<(), HostError>>,
) {
    let join = client
        .join(RelayJoin::host(session_id, relay_authentication))
        .await
        .map_err(transport_error);
    relay_authentication.zeroize();
    if let Err(error) = join {
        let _ = ready.send(Err(error.clone()));
        emit_terminal(&events, Some(error));
        phase.store(WorkerPhase::Closed as u8, Ordering::Release);
        return;
    }
    let _ = ready.send(Ok(()));
    let result = run_session(
        &client,
        identity,
        &mut commands,
        &mut cancellation,
        &events,
        &phase,
    )
    .await;

    phase.store(WorkerPhase::Closed as u8, Ordering::Release);
    emit_terminal(&events, result.err());
}

async fn run_session(
    client: &QuicClient,
    identity: DeviceIdentity,
    commands: &mut mpsc::Receiver<HostCommand>,
    cancellation: &mut watch::Receiver<bool>,
    events: &mpsc::Sender<HostEvent>,
    phase: &Arc<AtomicU8>,
) -> Result<(), HostError> {
    let Some(mut secure) = perform_noise_handshake(client, identity, cancellation).await? else {
        return Ok(());
    };
    let peer = secure.peer_identity();
    phase.store(WorkerPhase::WaitingForApproval as u8, Ordering::Release);
    emit_nonterminal(
        events,
        HostEvent::ApprovalRequested {
            device_id: peer.device_id(),
            verify_key: *peer.verify_key().as_bytes(),
            fingerprint: fingerprint(peer.verify_key()),
        },
    );

    if !wait_for_approval(
        peer.device_id(),
        peer.verify_key(),
        commands,
        cancellation,
        events,
    )
    .await?
    {
        return Ok(());
    }
    phase.store(
        WorkerPhase::NegotiatingCapabilities as u8,
        Ordering::Release,
    );
    negotiate_controller(client, &mut secure, cancellation).await?;
    if *cancellation.borrow() {
        return Ok(());
    }
    phase.store(WorkerPhase::Connected as u8, Ordering::Release);
    run_connected(client, &mut secure, commands, cancellation, events).await
}

/*
 * The worker functions below are deliberately kept after the event helpers so every event
 * emission can use the same bounded, nonblocking policy.
 */

async fn perform_noise_handshake(
    client: &QuicClient,
    identity: DeviceIdentity,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<Option<SecureSession>, HostError> {
    let handshake = async {
        let first = decode_noise_handshake(&client.next_control().await.map_err(transport_error)?)
            .map_err(protocol_error)?;
        if first.step != NoiseHandshakeStep::InitiatorHello {
            return Err(HostError::Protocol(
                "received an unexpected Noise handshake step".into(),
            ));
        }
        let (mut responder, response) =
            NoiseResponder::accept_pairing(&first.payload, identity).map_err(crypto_error)?;
        client
            .send_control(
                encode_noise_handshake(&NoiseHandshake {
                    protocol_version: PROTOCOL_VERSION,
                    step: NoiseHandshakeStep::ResponderHello,
                    payload: response,
                })
                .map_err(protocol_error)?,
            )
            .await
            .map_err(transport_error)?;
        let finish = decode_noise_handshake(&client.next_control().await.map_err(transport_error)?)
            .map_err(protocol_error)?;
        if finish.step != NoiseHandshakeStep::InitiatorFinish {
            return Err(HostError::Protocol(
                "received an unexpected Noise handshake step".into(),
            ));
        }
        responder.receive(&finish.payload).map_err(crypto_error)?;
        responder
            .finish()
            .map_err(crypto_error)
            .map(|cipher| cipher.into_secure_session(SecureRole::Responder))
    };
    tokio::select! {
        result = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake) => {
            let secure = result
                .map_err(|_| HostError::Transport("authenticated Noise handshake timed out".into()))??;
            Ok(Some(secure))
        }
        changed = cancellation.changed() => {
            let _ = changed;
            Ok(None)
        }
    }
}

async fn wait_for_approval(
    device_id: [u8; 16],
    verify_key: VerifyingKey,
    commands: &mut mpsc::Receiver<HostCommand>,
    cancellation: &mut watch::Receiver<bool>,
    events: &mpsc::Sender<HostEvent>,
) -> Result<bool, HostError> {
    loop {
        tokio::select! {
            biased;
            changed = cancellation.changed() => {
                let _ = changed;
                return Ok(false);
            }
            command = commands.recv() => match command {
                Some(HostCommand::Approve { controller_device_id, controller_verify_key }) => {
                    if controller_device_id != device_id || controller_verify_key != *verify_key.as_bytes() {
                        return Err(HostError::ControllerIdentityMismatch);
                    }
                    return Ok(true);
                }
                Some(HostCommand::Reject | HostCommand::Stop) | None => return Ok(false),
                Some(HostCommand::ReleaseAll) => {
                    emit_nonterminal(events, HostEvent::ReleaseAll);
                }
                Some(_) => return Err(HostError::InvalidState),
            },
        }
    }
}

async fn negotiate_controller(
    client: &QuicClient,
    secure: &mut SecureSession,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<(), HostError> {
    send_control(
        client,
        secure,
        ControlMessage::Hello {
            platform: Platform::MacOS,
            role: DeviceRole::Host,
        },
    )
    .await?;
    send_control(
        client,
        secure,
        ControlMessage::Capabilities(DeviceCapabilities {
            platform: Platform::MacOS,
            role: DeviceRole::Host,
            codecs: vec![Codec::H264],
            width: 1920,
            height: 1080,
        }),
    )
    .await?;

    let negotiation = async {
        let mut received_hello = false;
        loop {
            let ciphertext = client.next_control().await.map_err(transport_error)?;
            let plaintext = secure
                .open(SecureLane::Control, &ciphertext)
                .map_err(crypto_error)?;
            match decode_control(&plaintext).map_err(protocol_error)? {
                ControlMessage::Hello {
                    role: DeviceRole::Controller,
                    ..
                } => received_hello = true,
                ControlMessage::Capabilities(capabilities)
                    if received_hello
                        && capabilities.role == DeviceRole::Controller
                        && capabilities.codecs.contains(&Codec::H264) =>
                {
                    return Ok(());
                }
                ControlMessage::Capabilities(_) => {
                    return Err(HostError::InvalidControllerCapabilities);
                }
                ControlMessage::Hello { .. } | ControlMessage::RequestKeyframe { .. } => {}
            }
        }
    };
    tokio::select! {
        result = tokio::time::timeout(NEGOTIATION_TIMEOUT, negotiation) => {
            result
                .map_err(|_| HostError::Transport("capability negotiation timed out".into()))?
        }
        changed = cancellation.changed() => {
            let _ = changed;
            Ok(())
        }
    }
}

async fn run_connected(
    client: &QuicClient,
    secure: &mut SecureSession,
    commands: &mut mpsc::Receiver<HostCommand>,
    cancellation: &mut watch::Receiver<bool>,
    events: &mpsc::Sender<HostEvent>,
) -> Result<(), HostError> {
    let mut video = VideoState::default();
    let mut metrics = HostMetrics::default();
    loop {
        if *cancellation.borrow() {
            return Ok(());
        }
        tokio::select! {
            biased;
            changed = cancellation.changed() => {
                let _ = changed;
                return Ok(());
            }
            command = commands.recv() => match command {
                Some(command) => {
                    if !handle_command(client, secure, command, &mut video, &mut metrics, events).await? {
                        return Ok(());
                    }
                }
                None => return Ok(()),
            },
            event = client.next_event() => {
                match event.map_err(transport_error)? {
                    TransportEvent::Control(ciphertext) => {
                        let plaintext = secure.open(SecureLane::Control, &ciphertext).map_err(crypto_error)?;
                        if matches!(decode_control(&plaintext).map_err(protocol_error)?, ControlMessage::RequestKeyframe { .. }) {
                            metrics.keyframe_requests = metrics.keyframe_requests.saturating_add(1);
                            emit_nonterminal(events, HostEvent::KeyframeRequested);
                        }
                    }
                    TransportEvent::Input(ciphertext) => {
                        let plaintext = secure.open(SecureLane::Input, &ciphertext).map_err(crypto_error)?;
                        let input = decode_input(&plaintext, now_micros()).map_err(protocol_error)?;
                        metrics.received_input_events = metrics.received_input_events.saturating_add(1);
                        emit_nonterminal(events, HostEvent::Input(input.event));
                    }
                    TransportEvent::Closed { reason } => return Err(HostError::Transport(format!("transport closed: {reason}"))),
                    TransportEvent::VideoConfig(_) | TransportEvent::VideoDatagram(_) | TransportEvent::CursorDatagram(_) => {
                        return Err(HostError::Protocol(
                            "controller sent data on a host-only transport lane".into(),
                        ));
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct VideoState {
    config: Option<(u64, u32, u16, u16)>,
    next_access_unit_is_keyframe: bool,
}

async fn handle_command(
    client: &QuicClient,
    secure: &mut SecureSession,
    command: HostCommand,
    video: &mut VideoState,
    metrics: &mut HostMetrics,
    events: &mpsc::Sender<HostEvent>,
) -> Result<bool, HostError> {
    match command {
        HostCommand::Reject | HostCommand::Stop => Ok(false),
        HostCommand::Approve { .. } => Err(HostError::InvalidState),
        HostCommand::SendVideoConfig {
            stream_id,
            version,
            width,
            height,
            bytes,
        } => {
            let config = VideoConfig {
                protocol_version: PROTOCOL_VERSION,
                stream_id,
                config_version: version,
                codec: Codec::H264,
                width,
                height,
                sequence_header: bytes,
            };
            let plaintext = encode_video_config(&config).map_err(protocol_error)?;
            let ciphertext = secure
                .seal(SecureLane::VideoConfig, &plaintext)
                .map_err(crypto_error)?;
            client
                .send_video_config(ciphertext)
                .await
                .map_err(transport_error)?;
            video.config = Some((stream_id, version, width, height));
            video.next_access_unit_is_keyframe = true;
            metrics.sent_video_configs = metrics.sent_video_configs.saturating_add(1);
            Ok(true)
        }
        HostCommand::SendVideoAccessUnit {
            stream_id,
            frame_id,
            config_version,
            bytes,
        } => {
            let Some((configured_stream, configured_version, width, height)) = video.config else {
                return Err(HostError::InvalidState);
            };
            if (stream_id, config_version) != (configured_stream, configured_version) {
                return Err(HostError::InvalidState);
            }
            let flags = if video.next_access_unit_is_keyframe {
                video.next_access_unit_is_keyframe = false;
                FrameFlags(FrameFlags::KEYFRAME.0 | FrameFlags::CONFIG.0)
            } else {
                FrameFlags(FrameFlags::VIDEO_ALIVE.0)
            };
            let frame = EncodedFrame {
                stream_id,
                frame_id,
                config_version,
                capture_timestamp_us: now_micros(),
                width,
                height,
                flags,
                data: bytes,
            };
            for packet in packetize_frame(&frame).map_err(protocol_error)? {
                let plaintext = encode_video_packet(&packet).map_err(protocol_error)?;
                let ciphertext = secure
                    .seal(SecureLane::VideoDatagram, &plaintext)
                    .map_err(crypto_error)?;
                client
                    .send_video_datagram(ciphertext)
                    .await
                    .map_err(transport_error)?;
                metrics.sent_video_packets = metrics.sent_video_packets.saturating_add(1);
            }
            Ok(true)
        }
        HostCommand::SendCursor { stream_id, bytes } => {
            let cursor = decode_cursor_update(&bytes).map_err(protocol_error)?;
            if cursor.stream_id != stream_id {
                return Err(HostError::InvalidState);
            }
            let ciphertext = secure
                .seal(SecureLane::CursorDatagram, &bytes)
                .map_err(crypto_error)?;
            client
                .send_cursor_datagram(ciphertext)
                .await
                .map_err(transport_error)?;
            Ok(true)
        }
        HostCommand::RequestKeyframe => {
            metrics.keyframe_requests = metrics.keyframe_requests.saturating_add(1);
            emit_nonterminal(events, HostEvent::KeyframeRequested);
            Ok(true)
        }
        HostCommand::ReleaseAll => {
            emit_nonterminal(events, HostEvent::ReleaseAll);
            Ok(true)
        }
    }
}

async fn send_control(
    client: &QuicClient,
    secure: &mut SecureSession,
    message: ControlMessage,
) -> Result<(), HostError> {
    let plaintext = encode_control(&message).map_err(protocol_error)?;
    let ciphertext = secure
        .seal(SecureLane::Control, &plaintext)
        .map_err(crypto_error)?;
    client
        .send_control(ciphertext)
        .await
        .map_err(transport_error)
}

fn fingerprint(verify_key: VerifyingKey) -> String {
    let key = verify_key.as_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}…{:02x}{:02x}{:02x}{:02x}",
        key[0], key[1], key[2], key[3], key[28], key[29], key[30], key[31]
    )
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_micros() as u64)
}

fn transport_error(error: TransportError) -> HostError {
    HostError::Transport(error.to_string())
}

fn protocol_error(error: desklink_protocol::ProtocolError) -> HostError {
    HostError::Protocol(error.to_string())
}

fn crypto_error(error: desklink_crypto::CryptoError) -> HostError {
    HostError::Crypto(error.to_string())
}
