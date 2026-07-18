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
    decode_cursor_update, decode_input, decode_noise_handshake, decode_transfer, encode_control,
    encode_noise_handshake, encode_transfer, encode_video_config, encode_video_packet,
};
use desklink_session::{ReconnectDecision, ReconnectPolicy, ReconnectSchedule};
use desklink_transport::{QuicClient, QuicClientConfig, RelayJoin, TransportError, TransportEvent};
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

    const fn is_terminal(self) -> bool {
        matches!(self, Self::Stopping | Self::Closed)
    }
}

fn try_advance_worker_phase(phase: &AtomicU8, phase_gate: &Mutex<()>, next: WorkerPhase) -> bool {
    let Ok(_phase_gate) = phase_gate.lock() else {
        return false;
    };
    if WorkerPhase::load(phase).is_terminal() {
        return false;
    }
    phase.store(next as u8, Ordering::Release);
    true
}

#[cfg(test)]
fn record_terminal_admission(phase: &AtomicU8, phase_gate: &Mutex<()>) {
    let Ok(_phase_gate) = phase_gate.lock() else {
        return;
    };
    record_terminal_admission_locked(phase);
}

fn record_terminal_admission_locked(phase: &AtomicU8) {
    phase.store(WorkerPhase::Stopping as u8, Ordering::Release);
}

fn record_closed(phase: &AtomicU8, phase_gate: &Mutex<()>) {
    let Ok(_phase_gate) = phase_gate.lock() else {
        return;
    };
    phase.store(WorkerPhase::Closed as u8, Ordering::Release);
}

pub(crate) struct HostWorker {
    commands: mpsc::Sender<HostCommand>,
    cancellation: watch::Sender<bool>,
    phase: Arc<AtomicU8>,
    phase_gate: Arc<Mutex<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl HostWorker {
    pub(crate) fn start(
        client: QuicClient,
        reconnect_config: Option<QuicClientConfig>,
        identity: DeviceIdentity,
        session_id: SessionId,
        relay_authentication: [u8; 32],
        events: mpsc::Sender<HostEvent>,
    ) -> Result<Self, HostError> {
        let (commands, receiver) = mpsc::channel(HOST_COMMAND_CAPACITY);
        let (cancellation, cancellation_receiver) = watch::channel(false);
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        let phase = Arc::new(AtomicU8::new(WorkerPhase::Connecting as u8));
        let phase_gate = Arc::new(Mutex::new(()));
        let worker_phase = phase.clone();
        let worker_phase_gate = phase_gate.clone();
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
                        reconnect_config,
                        identity,
                        session_id,
                        relay_authentication,
                        receiver,
                        cancellation_receiver,
                        events,
                        worker_phase.clone(),
                        worker_phase_gate.clone(),
                        ready_sender,
                    )),
                    Err(_) => {
                        let _ = ready_sender.send(Err(HostError::WorkerStopped));
                        emit_terminal(&events, Some(HostError::WorkerStopped));
                        record_closed(&worker_phase, &worker_phase_gate);
                    }
                }
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
                phase_gate,
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }

    pub(crate) fn start_from_config(
        reconnect_config: QuicClientConfig,
        identity: DeviceIdentity,
        session_id: SessionId,
        relay_authentication: [u8; 32],
        events: mpsc::Sender<HostEvent>,
    ) -> Result<Self, HostError> {
        let (commands, receiver) = mpsc::channel(HOST_COMMAND_CAPACITY);
        let (cancellation, cancellation_receiver) = watch::channel(false);
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        let phase = Arc::new(AtomicU8::new(WorkerPhase::Connecting as u8));
        let phase_gate = Arc::new(Mutex::new(()));
        let worker_phase = phase.clone();
        let worker_phase_gate = phase_gate.clone();
        let initial_config = reconnect_config.clone();
        let thread = thread::Builder::new()
            .name("desklink-host".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(async move {
                        match QuicClient::connect(initial_config).await {
                            Ok(client) => {
                                run_worker(
                                    client,
                                    Some(reconnect_config),
                                    identity,
                                    session_id,
                                    relay_authentication,
                                    receiver,
                                    cancellation_receiver,
                                    events,
                                    worker_phase.clone(),
                                    worker_phase_gate.clone(),
                                    ready_sender,
                                )
                                .await;
                            }
                            Err(error) => {
                                let error = transport_error(error);
                                let _ = ready_sender.send(Err(error.clone()));
                                emit_terminal(&events, Some(error));
                                record_closed(&worker_phase, &worker_phase_gate);
                            }
                        }
                    }),
                    Err(_) => {
                        let _ = ready_sender.send(Err(HostError::WorkerStopped));
                        emit_terminal(&events, Some(HostError::WorkerStopped));
                        record_closed(&worker_phase, &worker_phase_gate);
                    }
                }
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
                phase_gate,
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
        let _phase_gate = self
            .phase_gate
            .lock()
            .map_err(|_| HostError::WorkerStopped)?;
        let phase = WorkerPhase::load(&self.phase);
        if !command_is_admissible(&command, phase) {
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
        if cancels_worker {
            record_terminal_admission_locked(&self.phase);
        } else if let Some(next_phase) = next_phase {
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
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    pub(crate) fn cancel(&self) {
        // Cancellation is deliberately independent of command-queue admission. Destruction
        // must still interrupt a worker whose bounded command queue is saturated.
        let _ = self.commands.try_send(HostCommand::Stop);
        let _ = self.cancellation.send(true);
    }
}

fn command_is_admissible(command: &HostCommand, phase: WorkerPhase) -> bool {
    if phase.is_terminal() {
        return false;
    }
    if command.requires_connection() {
        return matches!(
            phase,
            WorkerPhase::NegotiatingCapabilities | WorkerPhase::Connected
        );
    }
    if matches!(command, HostCommand::Approve { .. }) {
        return phase == WorkerPhase::WaitingForApproval;
    }
    !matches!(command, HostCommand::Reject)
        || matches!(
            phase,
            WorkerPhase::Connecting
                | WorkerPhase::WaitingForApproval
                | WorkerPhase::NegotiatingCapabilities
                | WorkerPhase::Connected
        )
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

#[allow(clippy::too_many_arguments)]
async fn run_worker(
    mut client: QuicClient,
    reconnect_config: Option<QuicClientConfig>,
    identity: DeviceIdentity,
    session_id: SessionId,
    relay_authentication: [u8; 32],
    mut commands: mpsc::Receiver<HostCommand>,
    mut cancellation: watch::Receiver<bool>,
    events: mpsc::Sender<HostEvent>,
    phase: Arc<AtomicU8>,
    phase_gate: Arc<Mutex<()>>,
    ready: std::sync::mpsc::SyncSender<Result<(), HostError>>,
) {
    let mut relay_authentication = relay_authentication;
    let host_participant_id = identity.device_id;
    let join = client
        .join(RelayJoin::host_with_participant(
            session_id,
            relay_authentication,
            host_participant_id,
        ))
        .await
        .map_err(transport_error);
    if let Err(error) = join {
        relay_authentication.zeroize();
        let _ = ready.send(Err(error.clone()));
        emit_terminal(&events, Some(error));
        record_closed(&phase, &phase_gate);
        return;
    }
    let _ = ready.send(Ok(()));
    let mut approved_controller = None;
    let mut reconnect_schedule = ReconnectSchedule::new(ReconnectPolicy::default(), None);
    let mut terminal_error = None;
    loop {
        match run_session(
            &client,
            &identity,
            &mut commands,
            &mut cancellation,
            &events,
            &phase,
            &phase_gate,
            &mut approved_controller,
        )
        .await
        {
            Ok(()) => break,
            Err(error)
                if reconnect_config.is_some()
                    && is_retryable_host_error(&error)
                    && !*cancellation.borrow() =>
            {
                let _ = try_advance_worker_phase(&phase, &phase_gate, WorkerPhase::Connecting);
                emit_nonterminal(&events, HostEvent::ReleaseAll);
                emit_nonterminal(&events, HostEvent::State(HostState::Connecting));
                match reconnect_host(
                    reconnect_config.as_ref().expect("checked above"),
                    session_id,
                    relay_authentication,
                    host_participant_id,
                    &mut reconnect_schedule,
                    &mut cancellation,
                )
                .await
                {
                    Ok(Some(reconnected)) => {
                        client = reconnected;
                        reconnect_schedule.reset();
                    }
                    Ok(None) => break,
                    Err(reconnect_error) => {
                        terminal_error = Some(reconnect_error);
                        break;
                    }
                }
            }
            Err(error) => {
                terminal_error = Some(error);
                break;
            }
        }
    }

    emit_nonterminal(&events, HostEvent::State(HostState::Stopping));
    record_closed(&phase, &phase_gate);
    emit_terminal(&events, terminal_error);
    relay_authentication.zeroize();
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    client: &QuicClient,
    identity: &DeviceIdentity,
    commands: &mut mpsc::Receiver<HostCommand>,
    cancellation: &mut watch::Receiver<bool>,
    events: &mpsc::Sender<HostEvent>,
    phase: &Arc<AtomicU8>,
    phase_gate: &Arc<Mutex<()>>,
    approved_controller: &mut Option<ApprovedController>,
) -> Result<(), HostError> {
    let handshake_identity = identity.with_secret_key_bytes(|secret| {
        DeviceIdentity::from_secret_key(identity.device_id, secret)
    });
    let Some(mut secure) =
        perform_noise_handshake(client, handshake_identity, cancellation).await?
    else {
        return Ok(());
    };
    let peer = secure.peer_identity();
    {
        let _phase_gate = phase_gate.lock().map_err(|_| HostError::WorkerStopped)?;
        if WorkerPhase::load(phase).is_terminal() {
            return Ok(());
        }
        phase.store(WorkerPhase::WaitingForApproval as u8, Ordering::Release);
        if !approved_controller.is_some_and(|approved| {
            approved.device_id == peer.device_id()
                && approved.verify_key == *peer.verify_key().as_bytes()
        }) {
            emit_nonterminal(
                events,
                HostEvent::ApprovalRequested {
                    device_id: peer.device_id(),
                    verify_key: *peer.verify_key().as_bytes(),
                    fingerprint: fingerprint(peer.verify_key()),
                },
            );
        }
    }

    let approved = approved_controller.is_some_and(|approved| {
        approved.device_id == peer.device_id()
            && approved.verify_key == *peer.verify_key().as_bytes()
    });
    if !approved {
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
        *approved_controller = Some(ApprovedController {
            device_id: peer.device_id(),
            verify_key: *peer.verify_key().as_bytes(),
        });
    }
    if !try_advance_worker_phase(phase, phase_gate, WorkerPhase::NegotiatingCapabilities) {
        return Ok(());
    }
    emit_nonterminal(events, HostEvent::State(HostState::NegotiatingCapabilities));
    negotiate_controller(client, &mut secure, cancellation).await?;
    if *cancellation.borrow() {
        return Ok(());
    }
    if !try_advance_worker_phase(phase, phase_gate, WorkerPhase::Connected) {
        return Ok(());
    }
    emit_nonterminal(events, HostEvent::State(HostState::Connected));
    run_connected(client, &mut secure, commands, cancellation, events).await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ApprovedController {
    device_id: [u8; 16],
    verify_key: [u8; 32],
}

fn is_retryable_host_error(error: &HostError) -> bool {
    matches!(error, HostError::Transport(_))
}

async fn reconnect_host(
    config: &QuicClientConfig,
    session_id: SessionId,
    relay_authentication: [u8; 32],
    host_participant_id: [u8; 16],
    schedule: &mut ReconnectSchedule,
    cancellation: &mut watch::Receiver<bool>,
) -> Result<Option<QuicClient>, HostError> {
    let mut last_error = None;
    loop {
        let decision = schedule.next(now_unix_s());
        let delay = match decision {
            ReconnectDecision::RetryAfter { delay, .. } => delay,
            ReconnectDecision::Exhausted => {
                return Err(last_error.unwrap_or_else(|| {
                    HostError::Transport("host reconnect retry budget exhausted".into())
                }));
            }
            ReconnectDecision::SessionExpired => {
                return Err(HostError::Transport(
                    "host pairing session expired before reconnect".into(),
                ));
            }
        };
        let sleep = tokio::time::sleep(delay);
        tokio::pin!(sleep);
        tokio::select! {
            biased;
            changed = cancellation.changed() => {
                let _ = changed;
                return Ok(None);
            }
            () = &mut sleep => {}
        }
        if *cancellation.borrow() {
            return Ok(None);
        }
        match connect_and_join(
            config,
            session_id,
            relay_authentication,
            host_participant_id,
        )
        .await
        {
            Ok(client) => return Ok(Some(client)),
            Err(error) => last_error = Some(error),
        }
    }
}

async fn connect_and_join(
    config: &QuicClientConfig,
    session_id: SessionId,
    relay_authentication: [u8; 32],
    host_participant_id: [u8; 16],
) -> Result<QuicClient, HostError> {
    let client = QuicClient::connect(config.clone())
        .await
        .map_err(transport_error)?;
    client
        .join(RelayJoin::host_with_participant(
            session_id,
            relay_authentication,
            host_participant_id,
        ))
        .await
        .map_err(transport_error)?;
    Ok(client)
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
                ControlMessage::Hello { .. }
                | ControlMessage::RequestKeyframe { .. }
                | ControlMessage::AccessDenied { .. }
                | ControlMessage::DisplayList { .. }
                | ControlMessage::SelectDisplay { .. }
                | ControlMessage::SetAudioEnabled { .. }
                | ControlMessage::AudioState { .. }
                | ControlMessage::SetVideoQuality { .. }
                | ControlMessage::VideoQualityState { .. }
                | ControlMessage::VideoNetworkFeedback { .. } => {}
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
                    TransportEvent::Transfer(ciphertext) => {
                        let plaintext = secure.open(SecureLane::Transfer, &ciphertext).map_err(crypto_error)?;
                        let message = decode_transfer(&plaintext).map_err(protocol_error)?;
                        emit_nonterminal(events, HostEvent::Transfer(message));
                    }
            TransportEvent::Closed { reason } => return Err(HostError::Transport(format!("transport closed: {reason}"))),
            TransportEvent::PeerDisconnected { .. } => {
                return Err(HostError::Transport("controller disconnected".to_owned()));
            }
            TransportEvent::VideoConfig(_)
            | TransportEvent::VideoDatagram(_)
            | TransportEvent::CursorDatagram(_)
            | TransportEvent::AudioDatagram(_) => {
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
            let config_flag = video.next_access_unit_is_keyframe;
            let keyframe_flag = config_flag || contains_idr(&bytes);
            let flags = if keyframe_flag {
                video.next_access_unit_is_keyframe = false;
                FrameFlags(
                    FrameFlags::KEYFRAME.0 | if config_flag { FrameFlags::CONFIG.0 } else { 0 },
                )
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
        HostCommand::SendTransfer(message) => {
            let plaintext = encode_transfer(&message).map_err(protocol_error)?;
            let ciphertext = secure
                .seal(SecureLane::Transfer, &plaintext)
                .map_err(crypto_error)?;
            client
                .send_transfer(ciphertext)
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

fn contains_idr(bytes: &[u8]) -> bool {
    let mut index = 0;
    while index + 3 < bytes.len() {
        let start_code_len = if bytes[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[index..].starts_with(&[0, 0, 1]) {
            3
        } else {
            index += 1;
            continue;
        };
        let nal_start = index + start_code_len;
        if nal_start < bytes.len() && bytes[nal_start] & 0x1f == 5 {
            return true;
        }
        index = nal_start;
    }
    false
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_micros() as u64)
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
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

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, atomic::AtomicU8};

    use super::{
        HostCommand, HostError, WorkerPhase, command_is_admissible, contains_idr,
        is_retryable_host_error, record_terminal_admission, try_advance_worker_phase,
    };

    #[test]
    fn terminal_admission_blocks_late_worker_completion_transitions() {
        let phase = AtomicU8::new(WorkerPhase::NegotiatingCapabilities as u8);
        let gate = Mutex::new(());

        record_terminal_admission(&phase, &gate);

        assert!(!try_advance_worker_phase(
            &phase,
            &gate,
            WorkerPhase::Connected
        ));
        assert!(!try_advance_worker_phase(
            &phase,
            &gate,
            WorkerPhase::WaitingForApproval
        ));
        assert_eq!(WorkerPhase::load(&phase), WorkerPhase::Stopping);
    }

    #[test]
    fn terminal_phase_rejects_every_public_command() {
        let commands = [
            HostCommand::Approve {
                controller_device_id: [0; 16],
                controller_verify_key: [0; 32],
            },
            HostCommand::Reject,
            HostCommand::SendVideoConfig {
                stream_id: 1,
                version: 1,
                width: 1,
                height: 1,
                bytes: vec![],
            },
            HostCommand::SendVideoAccessUnit {
                stream_id: 1,
                frame_id: 1,
                config_version: 1,
                bytes: vec![],
            },
            HostCommand::SendCursor {
                stream_id: 1,
                bytes: vec![],
            },
            HostCommand::RequestKeyframe,
            HostCommand::ReleaseAll,
            HostCommand::Stop,
        ];

        for phase in [WorkerPhase::Stopping, WorkerPhase::Closed] {
            for command in &commands {
                assert!(!command_is_admissible(command, phase), "{command:?}");
            }
        }
    }

    #[test]
    fn detects_idr_nals_in_three_and_four_byte_annex_b_start_codes() {
        assert!(contains_idr(&[0, 0, 1, 0x65, 0x88]));
        assert!(contains_idr(&[0, 0, 0, 1, 0x25, 0x88]));
        assert!(!contains_idr(&[0, 0, 1, 0x41, 0x88]));
    }

    #[test]
    fn only_transport_failures_enter_host_reconnect_path() {
        assert!(is_retryable_host_error(&HostError::Transport(
            "closed".into()
        )));
        assert!(!is_retryable_host_error(&HostError::Protocol(
            "malformed".into()
        )));
        assert!(!is_retryable_host_error(&HostError::Crypto(
            "tampered".into()
        )));
    }
}
