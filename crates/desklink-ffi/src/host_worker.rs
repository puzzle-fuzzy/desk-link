use std::{
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use desklink_crypto::{
    DeviceIdentity, NoiseResponder, SecureLane, SecureRole, SecureSession, SessionId,
};
use desklink_protocol::{
    Codec, ControlMessage, DeviceCapabilities, DeviceRole, FrameFlags, H264Profile, InputContext,
    InputEnvelope, MAX_INPUT_AGE_US, MAX_INPUT_FUTURE_SKEW_US, NoiseHandshake, NoiseHandshakeStep,
    PROTOCOL_VERSION, Platform, VideoConfig, decode_control, decode_cursor_update,
    decode_noise_handshake, decode_session_input, decode_transfer, encode_control,
    encode_noise_handshake, encode_transfer, encode_video_config,
};
use desklink_session::{ReconnectDecision, ReconnectPolicy, ReconnectSchedule};
use desklink_transport::{QuicClient, QuicClientConfig, RelayJoin, TransportError, TransportEvent};
use desklink_video::{EncodedFrame, encode_video_frame};
use ed25519_dalek::VerifyingKey;
use rand_core::{OsRng, RngCore};
use tokio::sync::{mpsc, watch};
use zeroize::Zeroize;

use crate::host::{
    HOST_COMMAND_CAPACITY, HostCommand, HostError, HostEvent, HostMetrics, HostState,
};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(15);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const RELAY_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
// Nonterminal events may be dropped when this reserve would be consumed. That keeps the
// bounded event path nonblocking while guaranteeing room for ReleaseAll, Error, and Closed.
const TERMINAL_EVENT_RESERVE: usize = 3;

static INPUT_EPOCH_COUNTER: OnceLock<Result<AtomicU64, ()>> = OnceLock::new();

fn allocate_input_epoch() -> Result<u64, HostError> {
    allocate_input_epoch_with(&INPUT_EPOCH_COUNTER, |bytes| {
        OsRng.try_fill_bytes(bytes).map_err(|_| ())
    })
}

fn allocate_input_epoch_with(
    counter: &OnceLock<Result<AtomicU64, ()>>,
    fill_random: impl FnOnce(&mut [u8]) -> Result<(), ()>,
) -> Result<u64, HostError> {
    let initialized = counter.get_or_init(|| initialize_input_epoch_counter(fill_random));
    let counter = initialized
        .as_ref()
        .map_err(|_| HostError::Protocol("host input epoch initialization failed".into()))?;
    advance_input_epoch(counter)
}

fn initialize_input_epoch_counter(
    fill_random: impl FnOnce(&mut [u8]) -> Result<(), ()>,
) -> Result<AtomicU64, ()> {
    let mut bytes = [0_u8; 8];
    fill_random(&mut bytes)?;
    let seed = u64::from_le_bytes(bytes);
    if seed == 0 || seed == u64::MAX {
        return Err(());
    }
    Ok(AtomicU64::new(seed))
}

fn advance_input_epoch(counter: &AtomicU64) -> Result<u64, HostError> {
    counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
            epoch.checked_add(1)
        })
        .map(|previous| previous + 1)
        .map_err(|_| HostError::Protocol("host input epoch exhausted".into()))
}

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
                        match connect_relay(initial_config).await {
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
    // Never await application event capacity on the worker. Keyframe, metrics, and
    // approval notifications are best-effort under backpressure; terminal notifications below
    // retain their reserved slots and are never displaced by this path.
    if events.capacity() > TERMINAL_EVENT_RESERVE {
        let _ = events.try_send(event);
    }
}

fn emit_required(events: &mpsc::Sender<HostEvent>, event: HostEvent) -> Result<(), HostError> {
    // Stateful input and release barriers may never be silently dropped. If the
    // application is stalled, fail closed while the three terminal slots still
    // guarantee ReleaseAll/Error/Closed without waiting on event capacity.
    if events.capacity() <= TERMINAL_EVENT_RESERVE {
        return Err(HostError::CommandQueueFull);
    }
    events.try_send(event).map_err(|_| HostError::WorkerStopped)
}

fn emit_terminal(events: &mpsc::Sender<HostEvent>, error: Option<HostError>) {
    // Every nonterminal producer preserves the three slots above, making this
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
    let join = join_relay(
        &client,
        RelayJoin::host_with_participant(session_id, relay_authentication, host_participant_id),
    )
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
                if let Err(error) = emit_required(&events, HostEvent::ReleaseAll) {
                    terminal_error = Some(error);
                    break;
                }
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
    let client = connect_relay(config.clone())
        .await
        .map_err(transport_error)?;
    join_relay(
        &client,
        RelayJoin::host_with_participant(session_id, relay_authentication, host_participant_id),
    )
    .await
    .map_err(transport_error)?;
    Ok(client)
}

async fn connect_relay(config: QuicClientConfig) -> Result<QuicClient, TransportError> {
    tokio::time::timeout(RELAY_CONNECT_TIMEOUT, QuicClient::connect(config))
        .await
        .map_err(|_| TransportError::Connection("relay connection timed out".to_owned()))?
}

async fn join_relay(client: &QuicClient, join: RelayJoin) -> Result<(), TransportError> {
    tokio::time::timeout(RELAY_REQUEST_TIMEOUT, client.join(join))
        .await
        .map_err(|_| TransportError::Connection("relay join timed out".to_owned()))?
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
    wait_for_approval_with_timeout(
        APPROVAL_TIMEOUT,
        device_id,
        verify_key,
        commands,
        cancellation,
        events,
    )
    .await
}

async fn wait_for_approval_with_timeout(
    timeout: Duration,
    device_id: [u8; 16],
    verify_key: VerifyingKey,
    commands: &mut mpsc::Receiver<HostCommand>,
    cancellation: &mut watch::Receiver<bool>,
    events: &mpsc::Sender<HostEvent>,
) -> Result<bool, HostError> {
    tokio::time::timeout(
        timeout,
        wait_for_approval_command(device_id, verify_key, commands, cancellation, events),
    )
    .await
    .map_err(|_| HostError::ApprovalTimeout)?
}

async fn wait_for_approval_command(
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
                    emit_required(events, HostEvent::ReleaseAll)?;
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
            h264_profiles: vec![H264Profile::Main],
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
                | ControlMessage::InputState { .. }
                | ControlMessage::SetVideoQuality { .. }
                | ControlMessage::SetVideoProfile { .. }
                | ControlMessage::VideoQualityState { .. }
                | ControlMessage::VideoNetworkFeedback { .. }
                | ControlMessage::VideoPathCandidateOffer { .. }
                | ControlMessage::VideoPathCandidateAnswer { .. } => {}
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
    let mut input_policy = HostInputPolicy::default();
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
                        let input = decode_session_input(&plaintext).map_err(protocol_error)?;
                        if !input_policy.admit(&input, video.input_context(), now_micros()) {
                            continue;
                        }
                        emit_required(events, HostEvent::Input(input.event))?;
                        metrics.received_input_events = metrics.received_input_events.saturating_add(1);
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
            | TransportEvent::VideoReliable(_)
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InputClockAnchor {
    remote_us: u64,
    local_us: u64,
}

/// One policy per authenticated Noise session. Target changes reset only the
/// clock calibration; the session's sequence high-water mark never moves back.
/// At u64::MAX, only a new Noise session can reopen sequence admission.
#[derive(Default)]
struct HostInputPolicy {
    context: Option<InputContext>,
    last_sequence: u64,
    clock_anchor: Option<InputClockAnchor>,
}

impl HostInputPolicy {
    fn admit(
        &mut self,
        input: &InputEnvelope,
        published: Option<InputContext>,
        now_us: u64,
    ) -> bool {
        let context = input.context();
        // Retired/unpublished targets cannot poison either calibration or the
        // sequence gate, even with an extreme timestamp or sequence number.
        if !context.is_valid() || published != Some(context) || input.sequence <= self.last_sequence
        {
            return false;
        }
        if self.context != Some(context) {
            self.context = Some(context);
            self.clock_anchor = None;
        }
        if !self.timestamp_is_fresh(input.timestamp_us, now_us) {
            // Mirror the native Windows host's recovery behavior: a correction
            // to either wall clock re-anchors the newest authenticated input,
            // instead of turning ordinary clock skew into a terminal error.
            self.clock_anchor = Some(InputClockAnchor {
                remote_us: input.timestamp_us,
                local_us: now_us,
            });
        }
        self.last_sequence = input.sequence;
        true
    }

    fn timestamp_is_fresh(&mut self, remote_us: u64, local_us: u64) -> bool {
        let Some(anchor) = self.clock_anchor else {
            self.clock_anchor = Some(InputClockAnchor {
                remote_us,
                local_us,
            });
            return true;
        };
        let mapped_local_us = if remote_us >= anchor.remote_us {
            anchor.local_us.saturating_add(remote_us - anchor.remote_us)
        } else {
            anchor.local_us.saturating_sub(anchor.remote_us - remote_us)
        };
        mapped_local_us >= local_us.saturating_sub(MAX_INPUT_AGE_US)
            && mapped_local_us <= local_us.saturating_add(MAX_INPUT_FUTURE_SKEW_US)
    }
}

#[derive(Default)]
struct VideoState {
    config: Option<PublishedVideoConfig>,
    next_access_unit_is_keyframe: bool,
}

#[derive(Clone, Copy)]
struct PublishedVideoConfig {
    context: InputContext,
    version: u32,
    width: u16,
    height: u16,
}

impl VideoState {
    fn input_context(&self) -> Option<InputContext> {
        self.config.map(|config| config.context)
    }

    fn validate_geometry_version(
        &self,
        stream_id: u64,
        version: u32,
        width: u16,
        height: u16,
    ) -> Result<(), HostError> {
        if let Some(current) = self.config
            && current.context.stream_id == stream_id
            && (current.width, current.height) != (width, height)
            && version <= current.version
        {
            return Err(HostError::Protocol(
                "input geometry requires a new video configuration version".into(),
            ));
        }
        Ok(())
    }

    fn context_for_geometry(
        &self,
        stream_id: u64,
        width: u16,
        height: u16,
    ) -> Result<InputContext, HostError> {
        if let Some(current) = self.config
            && (current.context.stream_id, current.width, current.height)
                == (stream_id, width, height)
        {
            return Ok(current.context);
        }
        // The current FFI host only exposes encoded dimensions and stream ID.
        // A display ID/capture-topology generation must be added before these
        // checks can claim to bind inputs to arbitrary source-monitor changes.
        Ok(InputContext {
            stream_id,
            input_epoch: allocate_input_epoch()?,
        })
    }
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
            video.validate_geometry_version(stream_id, version, width, height)?;
            let context = video.context_for_geometry(stream_id, width, height)?;
            let config = VideoConfig {
                protocol_version: PROTOCOL_VERSION,
                stream_id,
                input_epoch: context.input_epoch,
                config_version: version,
                codec: Codec::H264,
                width,
                height,
                sequence_header: bytes,
            };
            let plaintext = encode_video_config(&config).map_err(protocol_error)?;
            if video
                .input_context()
                .is_some_and(|previous| previous != context)
            {
                emit_required(events, HostEvent::ReleaseAll)?;
                video.config = None;
            }
            let ciphertext = secure
                .seal(SecureLane::VideoConfig, &plaintext)
                .map_err(crypto_error)?;
            client
                .send_video_config(ciphertext)
                .await
                .map_err(transport_error)?;
            video.config = Some(PublishedVideoConfig {
                context,
                version,
                width,
                height,
            });
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
            let Some(config) = video.config else {
                return Err(HostError::InvalidState);
            };
            if (stream_id, config_version) != (config.context.stream_id, config.version) {
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
                width: config.width,
                height: config.height,
                flags,
                data: bytes,
            };
            for plaintext in encode_video_frame(&frame).map_err(protocol_error)? {
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
            emit_required(events, HostEvent::ReleaseAll)?;
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
    use std::sync::{
        Mutex, OnceLock,
        atomic::{AtomicU8, AtomicU64, Ordering},
    };

    use super::{
        HostCommand, HostError, HostEvent, HostInputPolicy, HostState, InputClockAnchor,
        PublishedVideoConfig, VideoState, WorkerPhase, advance_input_epoch,
        allocate_input_epoch_with, command_is_admissible, contains_idr, emit_nonterminal,
        emit_required, emit_terminal, is_retryable_host_error, record_terminal_admission,
        try_advance_worker_phase, wait_for_approval_with_timeout,
    };
    use desklink_protocol::{InputContext, InputEnvelope, InputEvent};
    use ed25519_dalek::SigningKey;
    use tokio::sync::{mpsc, watch};

    fn clock_input(context: InputContext, sequence: u64, timestamp_us: u64) -> InputEnvelope {
        InputEnvelope {
            sequence,
            timestamp_us,
            stream_id: context.stream_id,
            input_epoch: context.input_epoch,
            event: InputEvent::MouseMove { x: 1, y: 2 },
        }
    }

    #[test]
    fn input_clock_calibrates_both_ten_minute_offsets_and_preserves_a_same_target_anchor() {
        let target = InputContext {
            stream_id: 7,
            input_epoch: 15,
        };
        let local = 10_000_000_000_u64;
        for remote in [local - 600_000_000, local + 600_000_000] {
            let mut policy = HostInputPolicy::default();
            assert!(policy.admit(&clock_input(target, 1, remote), Some(target), local));
            let anchor = policy.clock_anchor;
            assert!(policy.admit(
                &clock_input(target, 2, remote + 100),
                Some(target),
                local + 100
            ));
            assert_eq!(
                policy.clock_anchor, anchor,
                "same-target quality changes must not recalibrate clocks"
            );
        }
    }

    #[test]
    fn input_clock_reanchors_remote_and_local_wall_clock_jumps_without_replaying_sequence() {
        let target = InputContext {
            stream_id: 7,
            input_epoch: 15,
        };
        let base = 10_000_000_000_u64;
        let mut policy = HostInputPolicy::default();
        let samples = [
            (base, base),
            (base + 600_000_000, base + 100),
            (base - 600_000_000, base + 200),
            (base - 600_000_000 + 100, base + 600_000_000),
            (base - 600_000_000 + 200, base - 600_000_000),
        ];
        for (index, (remote_us, local_us)) in samples.into_iter().enumerate() {
            assert!(policy.admit(
                &clock_input(target, index as u64 + 1, remote_us),
                Some(target),
                local_us
            ));
            assert_eq!(
                policy.clock_anchor,
                Some(InputClockAnchor {
                    remote_us,
                    local_us
                })
            );
        }
        let anchor = policy.clock_anchor;
        assert!(!policy.admit(&clock_input(target, 1, u64::MAX), Some(target), 0));
        assert!(!policy.admit(&clock_input(target, 5, 0), Some(target), u64::MAX));
        assert_eq!(policy.last_sequence, 5);
        assert_eq!(policy.clock_anchor, anchor);
    }

    #[test]
    fn old_or_unpublished_target_cannot_poison_clock_or_sequence_even_at_u64_max() {
        let old = InputContext {
            stream_id: 7,
            input_epoch: 15,
        };
        let current = InputContext {
            stream_id: 7,
            input_epoch: 16,
        };
        let base = 10_000_000_000_u64;
        let mut policy = HostInputPolicy::default();
        assert!(!policy.admit(&clock_input(old, u64::MAX, u64::MAX), None, 0));
        assert_eq!(policy.clock_anchor, None);
        assert_eq!(policy.last_sequence, 0);
        assert!(policy.admit(&clock_input(old, 1, base), Some(old), base));
        let anchor = policy.clock_anchor;
        assert!(!policy.admit(&clock_input(old, u64::MAX, u64::MAX), Some(current), 0));
        assert_eq!(policy.last_sequence, 1);
        assert_eq!(policy.clock_anchor, anchor);
        assert!(policy.admit(
            &clock_input(current, 2, base + 200),
            Some(current),
            base + 100
        ));
        assert_eq!(
            policy.clock_anchor,
            Some(InputClockAnchor {
                remote_us: base + 200,
                local_us: base + 100
            })
        );
        assert_eq!(policy.context, Some(current));
    }

    #[test]
    fn input_clock_handles_u64_limits_and_never_wraps_sequence_within_one_noise_session() {
        let target = InputContext {
            stream_id: u64::MAX,
            input_epoch: u64::MAX,
        };
        let mut policy = HostInputPolicy::default();
        assert!(policy.admit(
            &clock_input(target, 1, u64::MAX - 1),
            Some(target),
            10_000_000
        ));
        assert!(policy.admit(&clock_input(target, 2, u64::MAX), Some(target), u64::MAX));
        assert!(policy.admit(&clock_input(target, 3, 0), Some(target), 0));
        assert!(policy.admit(&clock_input(target, u64::MAX, 1), Some(target), 1));
        let anchor = policy.clock_anchor;
        for sequence in [0, 1, 3, u64::MAX] {
            assert!(!policy.admit(&clock_input(target, sequence, 2), Some(target), 2));
        }
        assert_eq!(policy.clock_anchor, anchor);
        let mut next_noise = HostInputPolicy::default();
        assert!(next_noise.admit(&clock_input(target, 1, 2), Some(target), 2));
    }

    #[test]
    fn epoch_seed_rejects_zero_maximum_and_entropy_errors_without_fallback_or_retry() {
        let expected = HostError::Protocol("host input epoch initialization failed".into());
        for seed in [0_u64, u64::MAX] {
            let counter = OnceLock::new();
            let result = allocate_input_epoch_with(&counter, |bytes| {
                bytes.copy_from_slice(&seed.to_le_bytes());
                Ok(())
            });
            assert_eq!(result, Err(expected.clone()));
            assert_eq!(
                allocate_input_epoch_with(&counter, |_| panic!(
                    "failed initialization must stay failed"
                )),
                Err(expected.clone())
            );
        }
        let counter = OnceLock::new();
        let result = allocate_input_epoch_with(&counter, |bytes| {
            bytes.copy_from_slice(&42_u64.to_le_bytes());
            Err(())
        });
        assert_eq!(result, Err(expected.clone()));
        assert_eq!(
            allocate_input_epoch_with(&counter, |_| panic!("entropy failure must not be retried")),
            Err(expected)
        );
    }

    #[test]
    fn valid_epoch_seed_is_read_once_and_then_checked_monotonically() {
        let counter = OnceLock::new();
        assert_eq!(
            allocate_input_epoch_with(&counter, |bytes| {
                bytes.copy_from_slice(&42_u64.to_le_bytes());
                Ok(())
            }),
            Ok(43)
        );
        assert_eq!(
            allocate_input_epoch_with(&counter, |_| panic!("seed must not be regenerated")),
            Ok(44)
        );
    }

    #[test]
    fn epoch_allocator_is_shared_across_video_state_and_refuses_to_wrap() {
        let first = VideoState::default()
            .context_for_geometry(7, 640, 360)
            .unwrap();
        let second = VideoState::default()
            .context_for_geometry(7, 640, 360)
            .unwrap();
        assert!(first.is_valid());
        assert!(second.input_epoch > first.input_epoch);

        let last = AtomicU64::new(u64::MAX - 1);
        assert_eq!(advance_input_epoch(&last), Ok(u64::MAX));
        assert!(matches!(
            advance_input_epoch(&last),
            Err(HostError::Protocol(_))
        ));
        assert_eq!(last.load(Ordering::Acquire), u64::MAX);
    }

    #[test]
    fn encoded_geometry_reconfiguration_preserves_only_the_matching_input_context() {
        let mut state = VideoState::default();
        assert_eq!(state.input_context(), None);
        let context = state.context_for_geometry(7, 640, 360).unwrap();
        // Planning a configuration does not grant it before publication.
        assert_eq!(state.input_context(), None);
        state.config = Some(PublishedVideoConfig {
            context,
            version: 1,
            width: 640,
            height: 360,
        });
        assert_eq!(state.context_for_geometry(7, 640, 360).unwrap(), context);
        state.config.as_mut().unwrap().version = 2;
        assert!(state.validate_geometry_version(7, 2, 800, 600).is_err());
        assert!(state.validate_geometry_version(7, 1, 800, 600).is_err());
        assert!(state.validate_geometry_version(7, 3, 800, 600).is_ok());
        assert!(state.validate_geometry_version(7, 2, 640, 360).is_ok());
        // A new stream is a distinct (stream, config_version) frame namespace.
        assert!(state.validate_geometry_version(8, 1, 800, 600).is_ok());
        assert_eq!(state.context_for_geometry(7, 640, 360).unwrap(), context);
        assert_ne!(state.context_for_geometry(7, 800, 600).unwrap(), context);
        assert_ne!(state.context_for_geometry(8, 640, 360).unwrap(), context);
    }

    #[test]
    fn required_release_fails_closed_without_consuming_terminal_capacity() {
        let (events, mut receiver) = mpsc::channel(5);
        emit_nonterminal(&events, HostEvent::KeyframeRequested);
        emit_nonterminal(&events, HostEvent::KeyframeRequested);
        assert_eq!(events.capacity(), 3);
        assert_eq!(
            emit_required(&events, HostEvent::ReleaseAll),
            Err(HostError::CommandQueueFull)
        );
        emit_terminal(&events, Some(HostError::CommandQueueFull));
        let mut delivered = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            delivered.push(event);
        }
        assert_eq!(
            delivered,
            vec![
                HostEvent::KeyframeRequested,
                HostEvent::KeyframeRequested,
                HostEvent::ReleaseAll,
                HostEvent::Error(HostError::CommandQueueFull),
                HostEvent::State(HostState::Closed),
            ]
        );
    }

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

    #[tokio::test]
    async fn approval_timeout_is_terminal_and_does_not_enter_reconnect() {
        let (commands_sender, mut commands) = mpsc::channel(1);
        let _commands_sender = commands_sender;
        let (_cancellation_sender, mut cancellation) = watch::channel(false);
        let (events, _events_receiver) = mpsc::channel(4);
        let signing_key = SigningKey::from_bytes(&[7; 32]);

        let error = wait_for_approval_with_timeout(
            std::time::Duration::from_millis(1),
            [1; 16],
            signing_key.verifying_key(),
            &mut commands,
            &mut cancellation,
            &events,
        )
        .await
        .expect_err("approval should time out");
        assert_eq!(error, HostError::ApprovalTimeout);
        assert!(!is_retryable_host_error(&error));
    }
}
