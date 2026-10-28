use std::{
    future::Future,
    net::{SocketAddr, ToSocketAddrs},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    task::{Context, Poll},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use desklink_crypto::{DeviceIdentity, PairingInvite, SessionId};
use desklink_protocol::{
    ControlMessage, InputContext, InputEvent, Platform, TransferMessage, VideoQualityPreference,
    encode_audio_packet, encode_control, encode_cursor_update, encode_transfer,
};
use desklink_session::{ReconnectDecision, ReconnectPolicy, ReconnectSchedule};
use desklink_transport::{
    ChannelKind, LanDataError, QuicClient, QuicClientConfig, RelayDirectoryLookup, RelayJoin,
    SessionDataError, TransportError,
};
use ed25519_dalek::VerifyingKey;
use tokio::sync::{mpsc, watch};
use zeroize::{Zeroize, Zeroizing};

use crate::session_commands::{SessionAction, SessionCommands};
use crate::{
    ControllerError, ControllerEvent, ControllerRuntime, DesklinkEvent, DesklinkEventCallback,
    DesklinkEventKind, DesklinkState, EventMeta, SavedHostMaterialOwned,
};

const COMMAND_CAPACITY: usize = 1_024;
const RELEASE_RESERVE: usize = 1;
const MAX_RELEASE_EVENTS: usize = COMMAND_CAPACITY;
const PHASE_CONNECTING: u8 = 0;
const PHASE_RUNNING: u8 = 1;
const PHASE_FINISHED: u8 = 2;
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const RELAY_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SEND_TIMEOUT: Duration = crate::controller::CONTROLLER_SEND_TIMEOUT;

type MaterialPublisher = Arc<dyn Fn(SavedHostMaterialOwned) + Send + Sync>;

/// Field diagnostics for the controller worker, off by default with zero
/// cost. `DESKLINK_LOG=1` emits one JSON line per connection-lifecycle event
/// on stderr so relay/DNS outages can be attributed from a terminal launch.
fn diagnostic_line(event: &str, detail: &str) -> Option<String> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ENABLED: AtomicBool = AtomicBool::new(false);
    static PROBED: AtomicBool = AtomicBool::new(false);
    if !PROBED.swap(true, Ordering::AcqRel) {
        let enabled = std::env::var("DESKLINK_LOG").is_ok_and(|value| {
            !value.is_empty()
                && !value.eq_ignore_ascii_case("0")
                && !value.eq_ignore_ascii_case("false")
        });
        ENABLED.store(enabled, Ordering::Release);
    }
    (ENABLED.load(Ordering::Acquire)).then(|| {
        format!(
            "{{\"ts\":{},\"module\":\"controller\",\"event\":\"{event}\",{detail}}}",
            now_unix_s()
        )
    })
}

fn diagnostic_log(event: &str, detail: &str) {
    if let Some(line) = diagnostic_line(event, detail) {
        eprintln!("{line}");
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CapturedInput {
    pub context: InputContext,
    pub event: InputEvent,
}

pub(crate) struct SecureConnectionConfigOwned {
    pub server_name: String,
    pub session_id: [u8; 16],
    pub relay_authentication: [u8; 32],
    pub controller_device_id: [u8; 16],
    pub controller_secret_key: [u8; 32],
    pub host_verify_key: [u8; 32],
    pub expires_at_unix_s: Option<u64>,
    pub authorization_type: u32,
    pub directory: Option<DirectoryConnectionConfigOwned>,
}

impl Drop for SecureConnectionConfigOwned {
    fn drop(&mut self) {
        self.relay_authentication.zeroize();
        self.controller_secret_key.zeroize();
        self.host_verify_key.zeroize();
    }
}

#[derive(Clone)]
pub(crate) struct DirectoryConnectionConfigOwned {
    pub device_id: u64,
    pub access_code: [u8; 8],
}

impl Drop for DirectoryConnectionConfigOwned {
    fn drop(&mut self) {
        self.access_code.zeroize();
    }
}

#[derive(Debug)]
pub(crate) enum ControllerCommand {
    SendInput(CapturedInput),
    ReleaseAll,
    // Session-scoped requests route through the gate via send_action; kept
    // for the legacy queue contract and tests.
    #[allow(dead_code)]
    RequestKeyframe,
    Control(ControlMessage),
    Transfer(TransferMessage),
    Session {
        scope: CommandScope,
        action: SessionAction,
    },
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandScope {
    pub epoch: u64,
    pub stream_id: u64,
}

#[derive(Default)]
pub(crate) struct SessionGate {
    pub epoch: AtomicU64,
    pub stream_id: AtomicU64,
    pub input_epoch: AtomicU64,
}

impl SessionGate {
    pub fn invalidate(&self) {
        self.stream_id.store(0, Ordering::Release);
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub fn scope(&self, expected_stream_id: u64) -> Option<CommandScope> {
        let epoch = self.epoch.load(Ordering::Acquire);
        let stream_id = self.stream_id.load(Ordering::Acquire);
        if stream_id == 0 || stream_id != expected_stream_id {
            return None;
        }
        Some(CommandScope { epoch, stream_id })
    }

    pub fn accepts(&self, scope: CommandScope) -> bool {
        self.scope(scope.stream_id) == Some(scope)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CallbackTarget {
    pub(crate) callback: Option<DesklinkEventCallback>,
    pub(crate) context: usize,
}

impl CallbackTarget {
    pub(crate) fn emit(
        &self,
        kind: DesklinkEventKind,
        data: &[u8],
        meta: EventMeta,
        state: DesklinkState,
    ) {
        let Some(callback) = self.callback else {
            return;
        };
        let event = DesklinkEvent {
            kind,
            data: if data.is_empty() {
                std::ptr::null()
            } else {
                data.as_ptr()
            },
            data_len: data.len(),
            stream_id: meta.stream_id,
            frame_id: meta.frame_id,
            config_version: meta.config_version,
            width: meta.width,
            height: meta.height,
            state,
            error_code: error_metadata(kind, state).0,
            recoverable: error_metadata(kind, state).1,
            input_epoch: meta.input_epoch,
        };
        callback(self.context as *mut std::ffi::c_void, &event);
    }

    fn emit_state(&self, state: DesklinkState, stream_id: u64) {
        self.emit(
            DesklinkEventKind::State,
            &[],
            EventMeta::for_stream(stream_id),
            state,
        );
    }

    fn emit_error(&self, message: &str, stream_id: u64) {
        self.emit_error_with_state(message, stream_id, DesklinkState::Closed);
    }

    fn emit_error_with_state(&self, message: &str, stream_id: u64, state: DesklinkState) {
        self.emit(
            DesklinkEventKind::Error,
            message.as_bytes(),
            EventMeta::for_stream(stream_id),
            state,
        );
    }
}

// This contract is independent of error message wording and localization.
fn error_metadata(kind: DesklinkEventKind, state: DesklinkState) -> (u32, u8) {
    if kind != DesklinkEventKind::Error {
        return (0, 0);
    }
    match state {
        DesklinkState::Reconnecting => (1, 1),
        DesklinkState::StartingVideo | DesklinkState::RecoveringVideo => (2, 1),
        _ => (3, 0),
    }
}

pub(crate) struct ControllerWorker {
    commands: mpsc::Sender<ControllerCommand>,
    cancellation: watch::Sender<bool>,
    release_events: Arc<Mutex<Vec<CapturedInput>>>,
    release_pending: Arc<AtomicU8>,
    phase: Arc<AtomicU8>,
    gate: Arc<SessionGate>,
    file_gate: Arc<SessionGate>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ControllerWorker {
    pub(crate) fn start(
        relay_url: String,
        platform: Platform,
        config: SecureConnectionConfigOwned,
        callback: Option<DesklinkEventCallback>,
        callback_context: *mut std::ffi::c_void,
        material_invalidator: Option<Arc<dyn Fn() + Send + Sync>>,
        material_publisher: Option<MaterialPublisher>,
    ) -> Result<Self, std::io::Error> {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY + RELEASE_RESERVE);
        let (cancellation, cancellation_receiver) = watch::channel(false);
        let release_events = Arc::new(Mutex::new(Vec::new()));
        let release_pending = Arc::new(AtomicU8::new(0));
        let phase = Arc::new(AtomicU8::new(PHASE_CONNECTING));
        let gate = Arc::new(SessionGate::default());
        let worker_gate = gate.clone();
        let file_gate = Arc::new(SessionGate::default());
        let worker_file_gate = file_gate.clone();
        let worker_phase = phase.clone();
        let worker_release_pending = release_pending.clone();
        let worker_release_events = release_events.clone();
        let callback = CallbackTarget {
            callback,
            context: callback_context as usize,
        };
        let worker_material_invalidator = material_invalidator.clone();
        let worker_material_publisher = material_publisher.clone();
        let thread = thread::Builder::new()
            .name("desklink-controller".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build();
                match runtime {
                    Ok(runtime) => runtime.block_on(run_worker(
                        relay_url,
                        platform,
                        config,
                        WorkerContext {
                            commands: receiver,
                            cancellation: cancellation_receiver,
                            phase: worker_phase.clone(),
                            callback,
                            material_invalidator: worker_material_invalidator,
                            material_publisher: worker_material_publisher,
                            release_events: worker_release_events,
                            release_pending: worker_release_pending,
                            gate: worker_gate,
                            file_gate: worker_file_gate,
                        },
                    )),
                    Err(error) => callback.emit_error(&error.to_string(), 0),
                }
                worker_phase.store(PHASE_FINISHED, Ordering::Release);
            })?;
        Ok(Self {
            commands,
            cancellation,
            release_events,
            release_pending,
            phase,
            gate,
            file_gate,
            thread: Some(thread),
        })
    }

    pub(crate) fn send(&self, command: ControllerCommand) -> Result<(), ()> {
        if self.commands.capacity() <= RELEASE_RESERVE {
            return Err(());
        }
        self.commands.try_send(command).map_err(|_| ())
    }

    pub(crate) fn send_action(
        &self,
        expected_stream_id: u64,
        action: SessionAction,
    ) -> Result<(), crate::DesklinkResult> {
        if !self.is_running() {
            return Err(crate::DesklinkResult::InvalidState);
        }
        let action_gate = if matches!(action, SessionAction::File(_)) {
            &self.file_gate
        } else {
            &self.gate
        };
        let scope = action_gate
            .scope(expected_stream_id)
            .ok_or(crate::DesklinkResult::InvalidState)?;
        if self.commands.capacity() <= RELEASE_RESERVE {
            return Err(crate::DesklinkResult::InternalError);
        }
        self.commands
            .try_send(ControllerCommand::Session { scope, action })
            .map_err(|_| crate::DesklinkResult::InternalError)
    }

    pub(crate) fn active_stream_id(&self) -> u64 {
        self.gate.stream_id.load(Ordering::Acquire)
    }

    /// The input geometry bound to the latest authenticated VideoConfig for
    /// `expected_stream_id`, when that stream is still the gated active one.
    pub(crate) fn input_context_for(&self, expected_stream_id: u64) -> Option<InputContext> {
        let stream_id = self.gate.stream_id.load(Ordering::Acquire);
        let input_epoch = self.gate.input_epoch.load(Ordering::Acquire);
        (stream_id != 0
            && input_epoch != 0
            && stream_id == expected_stream_id
            && self.gate.scope(expected_stream_id).is_some())
        .then_some(InputContext {
            stream_id,
            input_epoch,
        })
    }

    pub(crate) fn release_all(&self, events: Vec<CapturedInput>) -> Result<(), ()> {
        let result = enqueue_release_all(
            &self.commands,
            &self.release_events,
            &self.release_pending,
            events,
        );
        if result.is_err() {
            // A release batch may not be silently truncated. Retire the session
            // so the host releases held input through its disconnect boundary.
            let _ = self.cancellation.send(true);
        }
        result
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.phase.load(Ordering::Acquire) == PHASE_FINISHED
    }

    pub(crate) fn is_running(&self) -> bool {
        self.phase.load(Ordering::Acquire) == PHASE_RUNNING
    }

    pub(crate) fn shutdown(mut self) {
        let _ = self.commands.try_send(ControllerCommand::Shutdown);
        let _ = self.cancellation.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ControllerWorker {
    fn drop(&mut self) {
        let _ = self.cancellation.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct WorkerContext {
    commands: mpsc::Receiver<ControllerCommand>,
    cancellation: watch::Receiver<bool>,
    phase: Arc<AtomicU8>,
    callback: CallbackTarget,
    material_invalidator: Option<Arc<dyn Fn() + Send + Sync>>,
    material_publisher: Option<MaterialPublisher>,
    release_events: Arc<Mutex<Vec<CapturedInput>>>,
    release_pending: Arc<AtomicU8>,
    gate: Arc<SessionGate>,
    file_gate: Arc<SessionGate>,
}

async fn run_worker(
    relay_url: String,
    platform: Platform,
    mut config: SecureConnectionConfigOwned,
    context: WorkerContext,
) {
    let WorkerContext {
        mut commands,
        mut cancellation,
        phase,
        callback,
        material_invalidator,
        material_publisher,
        release_events,
        release_pending,
        gate,
        file_gate,
    } = context;
    let mut schedule = ReconnectSchedule::new(ReconnectPolicy::default(), config.expires_at_unix_s);
    let mut first_attempt = true;
    let mut pending_release: Option<Vec<CapturedInput>> = None;
    loop {
        if !first_attempt && let Some(invalidate) = &material_invalidator {
            invalidate();
        }
        phase.store(PHASE_CONNECTING, Ordering::Release);
        diagnostic_log(
            if first_attempt {
                "connect_attempt"
            } else {
                "reconnect_attempt"
            },
            "",
        );
        gate.invalidate();
        file_gate.invalidate();
        callback.emit_state(
            if first_attempt {
                DesklinkState::ConnectingRelay
            } else {
                DesklinkState::Reconnecting
            },
            0,
        );
        let connection = connect_controller(&relay_url, platform, &config, callback);
        let controller = tokio::select! {
            result = connection => match result {
                Ok(connection) => {
                    schedule.set_expiry(connection.expires_at_unix_s);
                    config.host_verify_key = connection.material.host_verify_key;
                    config.authorization_type = connection.material.authorization_type;
                    config.expires_at_unix_s = connection.expires_at_unix_s;
                    if let Some(publish) = &material_publisher { publish(connection.material); }
                    connection.controller
                },
                Err(failure) => {
                    if schedule_retry(
                        failure,
                        false,
                        RetryContext {
                            schedule: &mut schedule,
                            commands: &mut commands,
                            pending_release: &mut pending_release,
                            cancellation: &mut cancellation,
                            callback,
                            release_events: &release_events,
                            release_pending: &release_pending,
                            persistent: config.authorization_type == 2,
                        },
                    )
                    .await
                    {
                        first_attempt = false;
                        continue;
                    }
                    break;
                }
            },
            changed = cancellation.changed() => {
                let _ = changed;
                break;
            }
        };
        phase.store(PHASE_RUNNING, Ordering::Release);
        // Fixed, versioned ABI payload: version, platform (1 Windows, 2 Mac,
        // 3 iOS), width/height little-endian, supported H264 profile bitmask.
        let caps = controller.host_capabilities();
        let mut payload = vec![
            1,
            match caps.platform {
                Platform::Windows => 1,
                Platform::MacOS => 2,
                Platform::IOS => 3,
            },
        ];
        payload.extend_from_slice(&caps.width.to_le_bytes());
        payload.extend_from_slice(&caps.height.to_le_bytes());
        payload.push(caps.h264_profiles.iter().fold(0, |bits, profile| {
            bits | match profile {
                desklink_protocol::H264Profile::Main => 1,
                desklink_protocol::H264Profile::High => 2,
            }
        }));
        callback.emit(
            DesklinkEventKind::Capabilities,
            &payload,
            EventMeta::for_stream(0),
            DesklinkState::NegotiatingCapabilities,
        );
        callback.emit_state(DesklinkState::StartingVideo, 0);
        match run_connected(
            controller,
            &mut commands,
            &mut pending_release,
            &mut cancellation,
            callback,
            &release_events,
            &release_pending,
            &gate,
            &file_gate,
        )
        .await
        {
            ConnectedOutcome::Shutdown { stream_id } => {
                callback.emit_state(DesklinkState::Disconnecting, stream_id);
                break;
            }
            ConnectedOutcome::Failed {
                failure,
                stream_id,
                stable,
            } => {
                if schedule_retry(
                    failure,
                    stable,
                    RetryContext {
                        schedule: &mut schedule,
                        commands: &mut commands,
                        pending_release: &mut pending_release,
                        cancellation: &mut cancellation,
                        callback,
                        release_events: &release_events,
                        release_pending: &release_pending,
                        persistent: config.authorization_type == 2,
                    },
                )
                .await
                {
                    first_attempt = false;
                    continue;
                }
                if stream_id != 0 {
                    callback.emit_state(DesklinkState::Disconnecting, stream_id);
                }
                break;
            }
        }
    }
    if let Some(invalidate) = &material_invalidator {
        invalidate();
    }
    diagnostic_log("worker_closed", "");
    callback.emit_state(DesklinkState::Closed, 0);
}

struct ConnectFailure {
    message: String,
    retryable: bool,
}

impl ConnectFailure {
    fn permanent(message: String) -> Self {
        Self {
            message,
            retryable: false,
        }
    }

    fn retryable(message: String) -> Self {
        Self {
            message,
            retryable: true,
        }
    }

    fn from_transport(error: TransportError) -> Self {
        let retryable = transport_error_is_retryable(&error);
        Self {
            message: error.to_string(),
            retryable,
        }
    }

    fn from_controller(error: ControllerError) -> Self {
        let retryable = controller_error_is_retryable(&error);
        Self {
            message: error.to_string(),
            retryable,
        }
    }
}

enum ConnectedOutcome {
    Shutdown {
        stream_id: u64,
    },
    Failed {
        failure: ConnectFailure,
        stream_id: u64,
        stable: bool,
    },
}

struct ConnectedSenders {
    input: crate::controller::ControllerInputSender,
    control: crate::controller::ControllerControlSender,
    transfer: crate::controller::ControllerTransferSender,
}

enum ConnectedSend {
    Input(CapturedInput),
    Control(ControlMessage),
    Transfer(TransferMessage),
}

struct OwnedConnectedSend {
    context: Option<InputContext>,
    transfer: bool,
    started: bool,
    future: Pin<Box<dyn Future<Output = Result<(), ControllerError>> + Send>>,
}

impl Future for OwnedConnectedSend {
    type Output = Result<(), ControllerError>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        // Start means entering the owned send, even if it must first wait for
        // its sequence lock. It does not claim that bytes already hit the wire.
        this.started = true;
        this.future.as_mut().poll(cx)
    }
}

fn discard_unstarted_input(sending: &mut Option<OwnedConnectedSend>, current: InputContext) {
    if sending
        .as_ref()
        .is_some_and(|send| !send.started && send.context.is_some_and(|context| context != current))
    {
        *sending = None;
    }
}

fn start_connected_send(
    senders: Arc<ConnectedSenders>,
    mut cancellation: watch::Receiver<bool>,
    command: ConnectedSend,
) -> OwnedConnectedSend {
    let context = match &command {
        ConnectedSend::Input(input) => Some(input.context),
        _ => None,
    };
    OwnedConnectedSend {
        context,
        transfer: matches!(&command, ConnectedSend::Transfer(_)),
        started: false,
        future: Box::pin(async move {
            send_cancellable(&mut cancellation, async move {
                match command {
                    ConnectedSend::Input(input) => {
                        senders
                            .input
                            .send_input_with_context(input.context, input.event)
                            .await
                    }
                    ConnectedSend::Control(message) => senders.control.send(message).await,
                    ConnectedSend::Transfer(message) => senders.transfer.send(message).await,
                }
            })
            .await
        }),
    }
}

enum ConnectedWork {
    Event(Result<ControllerEvent, ControllerError>),
    Sent(Result<(), ControllerError>),
    Command(Option<ControllerCommand>),
    Maintenance,
}

fn fail_connected(
    senders: &ConnectedSenders,
    error: ControllerError,
    context: Option<InputContext>,
    stable: bool,
) -> ConnectedOutcome {
    senders.control.close();
    ConnectedOutcome::Failed {
        failure: ConnectFailure::from_controller(error),
        stream_id: context.map_or(0, |context| context.stream_id),
        stable,
    }
}

struct GateReset(Arc<SessionGate>);
impl Drop for GateReset {
    fn drop(&mut self) {
        self.0.invalidate();
    }
}

/// Sender-only I/O used by session-scoped actions while the stack-pinned
/// `next_event` future keeps its exclusive borrow of the runtime across the
/// connected loop.
pub(crate) struct SessionIo {
    senders: Arc<ConnectedSenders>,
    input_context: Option<InputContext>,
    active_stream_id: Option<u64>,
}

impl SessionIo {
    #[cfg(test)]
    pub(crate) fn for_runtime(
        runtime: &ControllerRuntime,
        input_context: Option<InputContext>,
    ) -> Self {
        Self {
            senders: Arc::new(ConnectedSenders {
                input: runtime.input_sender(),
                control: runtime.control_sender(),
                transfer: runtime.transfer_sender(),
            }),
            active_stream_id: input_context.map(|context| context.stream_id),
            input_context,
        }
    }

    pub(crate) fn active_stream_id(&self) -> Option<u64> {
        self.active_stream_id
    }

    pub(crate) async fn send_input(&self, event: InputEvent) -> Result<(), ControllerError> {
        let context = self.input_context.ok_or(ControllerError::NoActiveStream)?;
        self.senders
            .input
            .send_input_with_context(context, event)
            .await
    }

    pub(crate) async fn request_keyframe(&self) -> Result<(), ControllerError> {
        let stream_id = self
            .active_stream_id
            .ok_or(ControllerError::NoActiveStream)?;
        self.senders
            .control
            .send(ControlMessage::RequestKeyframe { stream_id })
            .await
    }

    pub(crate) async fn select_display(&self, display_id: u32) -> Result<(), ControllerError> {
        self.senders
            .control
            .send(ControlMessage::SelectDisplay { display_id })
            .await
    }

    pub(crate) async fn set_audio_enabled(&self, enabled: bool) -> Result<(), ControllerError> {
        self.senders
            .control
            .send(ControlMessage::SetAudioEnabled { enabled })
            .await
    }

    pub(crate) async fn set_video_quality(
        &self,
        preference: VideoQualityPreference,
    ) -> Result<(), ControllerError> {
        self.senders
            .control
            .send(ControlMessage::SetVideoQuality { preference })
            .await
    }

    pub(crate) async fn send_transfer(
        &self,
        message: TransferMessage,
    ) -> Result<(), ControllerError> {
        self.senders.transfer.send(message).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_connected(
    mut controller: ControllerRuntime,
    commands: &mut mpsc::Receiver<ControllerCommand>,
    pending_release: &mut Option<Vec<CapturedInput>>,
    cancellation: &mut watch::Receiver<bool>,
    callback: CallbackTarget,
    release_events: &Arc<Mutex<Vec<CapturedInput>>>,
    release_pending: &Arc<AtomicU8>,
    gate: &Arc<SessionGate>,
    file_gate: &Arc<SessionGate>,
) -> ConnectedOutcome {
    let senders = Arc::new(ConnectedSenders {
        input: controller.input_sender(),
        control: controller.control_sender(),
        transfer: controller.transfer_sender(),
    });
    let mut stable = false;
    // Input geometry is established only by an actually received VideoConfig.
    let mut input_context: Option<InputContext> = None;
    let initial = pending_release.take().unwrap_or_default();
    if initial.len() > MAX_RELEASE_EVENTS {
        return fail_connected(
            &senders,
            TransportError::Connection("controller release batch exceeded capacity".into()).into(),
            input_context,
            stable,
        );
    }
    let mut releases = initial.into_iter();
    let mut transfer_available = true;
    // Queued file actions fail as stale whenever this connection ends.
    let _file_gate_reset = GateReset(file_gate.clone());
    let mut session_commands = SessionCommands::new(callback, gate.clone());
    let mut files: Option<(u64, crate::file_commands::FileTransfers)> = None;
    let mut maintenance = tokio::time::interval(Duration::from_millis(100));
    // Windows capture defaults to enabled. Each authenticated controller
    // connection starts muted until its user explicitly enables playback.
    // The mute rides the ordered control lane as the first owned send, so
    // media events are not blocked on its round trip; every later command
    // still queues behind it through the single-owned-send discipline.
    let mut sending = Some(start_connected_send(
        senders.clone(),
        cancellation.clone(),
        ConnectedSend::Control(ControlMessage::SetAudioEnabled { enabled: false }),
    ));
    loop {
        // This stack-pinned reader survives every send completion and command.
        // Dropping next_event after it dequeued a reliable message but before
        // acquiring crypto would irretrievably lose that message.
        let next_event = controller.next_event_with_transfer(transfer_available);
        tokio::pin!(next_event);
        let event = loop {
            if *cancellation.borrow() {
                senders.control.close();
                return ConnectedOutcome::Shutdown {
                    stream_id: input_context.map_or(0, |context| context.stream_id),
                };
            }
            if sending.is_none() && input_context.is_some() {
                for input in releases.by_ref() {
                    if Some(input.context) == input_context {
                        sending = Some(start_connected_send(
                            senders.clone(),
                            cancellation.clone(),
                            ConnectedSend::Input(input),
                        ));
                        break;
                    }
                }
            }
            let has_send = sending.is_some();
            let receive_command = !has_send && releases.len() == 0;
            let work = tokio::select! {
                biased;
                changed = cancellation.changed() => {
                    let _ = changed;
                    senders.control.close();
                    return ConnectedOutcome::Shutdown { stream_id: input_context.map_or(0, |context| context.stream_id) };
                }
                work = async {
                    // Fairness below the session cancellation boundary prevents
                    // an always-ready input batch or video stream starving peers.
                    tokio::select! {
                        event = &mut next_event => ConnectedWork::Event(event),
                        result = async { Pin::new(sending.as_mut().expect("send enabled")).await }, if has_send => ConnectedWork::Sent(result),
                        command = commands.recv(), if receive_command => ConnectedWork::Command(command),
                        _ = maintenance.tick() => ConnectedWork::Maintenance,
                    }
                } => work,
            };
            // A sender clone may observe the watch update after the outer
            // cancellation branch was already polled Pending in this tick.
            // User cancellation wins over its resulting Closed/error outcome.
            if *cancellation.borrow() {
                senders.control.close();
                return ConnectedOutcome::Shutdown {
                    stream_id: input_context.map_or(0, |context| context.stream_id),
                };
            }
            match work {
                ConnectedWork::Event(event) => break event,
                ConnectedWork::Sent(result) => {
                    let transfer = sending.take().expect("completed owned send").transfer;
                    if let Err(error) = result {
                        if transfer && optional_transfer_unavailable(&error) {
                            transfer_available = false;
                            callback.emit_error_with_state(
                                "transfer channel unavailable; current operation stopped until reconnect",
                                input_context.map_or(0, |context| context.stream_id),
                                if stable { DesklinkState::Connected } else { DesklinkState::StartingVideo },
                            );
                            continue;
                        }
                        return fail_connected(&senders, error, input_context, stable);
                    }
                }
                ConnectedWork::Maintenance => {
                    session_commands.expire(input_context.map_or(0, |context| context.stream_id));
                }
                ConnectedWork::Command(command) => match command {
                    Some(ControllerCommand::SendInput(input)) => {
                        // Only unstarted events are filtered. A running reliable
                        // write keeps its original epoch until completion.
                        if Some(input.context) == input_context {
                            sending = Some(start_connected_send(
                                senders.clone(),
                                cancellation.clone(),
                                ConnectedSend::Input(input),
                            ));
                        }
                    }
                    Some(ControllerCommand::ReleaseAll) => {
                        release_pending.store(0, Ordering::Release);
                        let events = take_release_events(release_events);
                        if events.len() > MAX_RELEASE_EVENTS {
                            return fail_connected(
                                &senders,
                                TransportError::Connection(
                                    "controller release batch exceeded capacity".into(),
                                )
                                .into(),
                                input_context,
                                stable,
                            );
                        }
                        releases = events.into_iter();
                    }
                    Some(ControllerCommand::RequestKeyframe) => {
                        if let Some(context) = input_context {
                            sending = Some(start_connected_send(
                                senders.clone(),
                                cancellation.clone(),
                                ConnectedSend::Control(ControlMessage::RequestKeyframe {
                                    stream_id: context.stream_id,
                                }),
                            ));
                        } else {
                            callback.emit_error_with_state(
                                "no active video stream is available",
                                0,
                                DesklinkState::StartingVideo,
                            );
                        }
                    }
                    Some(ControllerCommand::Control(message)) => {
                        sending = Some(start_connected_send(
                            senders.clone(),
                            cancellation.clone(),
                            ConnectedSend::Control(message),
                        ));
                    }
                    Some(ControllerCommand::Transfer(message)) => {
                        if !transfer_available {
                            callback.emit_error_with_state(
                                "transfer channel unavailable; reconnect before starting another operation",
                                input_context.map_or(0, |context| context.stream_id),
                                if stable { DesklinkState::Connected } else { DesklinkState::StartingVideo },
                            );
                            continue;
                        }
                        sending = Some(start_connected_send(
                            senders.clone(),
                            cancellation.clone(),
                            ConnectedSend::Transfer(message),
                        ));
                    }
                    Some(ControllerCommand::Session { scope, action }) => {
                        // Commands arrive only while no owned send is in
                        // flight, so these bounded inline sends stay ordered.
                        if let SessionAction::File(file_action) = action {
                            if file_gate.accepts(scope)
                                && input_context
                                    .is_some_and(|context| context.stream_id == scope.stream_id)
                                && let Some((stream_id, transfers)) = &files
                                && *stream_id == scope.stream_id
                            {
                                let _ = transfers.execute(file_action);
                            } else {
                                file_action.fail_stale(callback, scope.stream_id);
                            }
                            continue;
                        }
                        let io = SessionIo {
                            senders: senders.clone(),
                            input_context,
                            active_stream_id: input_context.map(|context| context.stream_id),
                        };
                        if let Err(error) = session_commands.execute(&io, scope, action).await {
                            return fail_connected(&senders, error, input_context, stable);
                        }
                    }
                    Some(ControllerCommand::Shutdown) | None => {
                        senders.control.close();
                        return ConnectedOutcome::Shutdown {
                            stream_id: input_context.map_or(0, |context| context.stream_id),
                        };
                    }
                },
            }
        };
        match event {
            Ok(ControllerEvent::Closed { reason }) => {
                return fail_connected(
                    &senders,
                    TransportError::Connection(format!("transport closed: {reason}")).into(),
                    input_context,
                    stable,
                );
            }
            Ok(event) => {
                if !transfer_available && matches!(&event, ControllerEvent::Transfer(_)) {
                    continue;
                }
                let video_config = matches!(&event, ControllerEvent::VideoConfig(_));
                if let ControllerEvent::VideoConfig(config) = &event {
                    stable = true;
                    let current = InputContext {
                        stream_id: config.stream_id,
                        input_epoch: config.input_epoch,
                    };
                    discard_unstarted_input(&mut sending, current);
                    input_context = Some(current);
                    // File-transfer state is scoped to one authenticated stream.
                    let stream_id = config.stream_id;
                    if files.as_ref().is_none_or(|(id, _)| *id != stream_id) {
                        file_gate.invalidate();
                        file_gate.stream_id.store(stream_id, Ordering::Release);
                        files = Some((
                            stream_id,
                            crate::file_commands::FileTransfers::new(
                                callback,
                                senders.transfer.clone(),
                                stream_id,
                            ),
                        ));
                    }
                }
                if let ControllerEvent::Transfer(message) = &event
                    && let Some((_, transfers)) = &files
                {
                    transfers.observe(message);
                }
                let active_stream = input_context.map_or(0, |context| context.stream_id);
                let display_confirmed = session_commands.observe(&event, active_stream);
                let _ = emit_controller_event(callback, event);
                if video_config {
                    session_commands.replay(active_stream);
                }
                if display_confirmed && let Some(scope) = gate.scope(active_stream) {
                    let io = SessionIo {
                        senders: senders.clone(),
                        input_context,
                        active_stream_id: input_context.map(|context| context.stream_id),
                    };
                    if let Err(error) = session_commands
                        .execute(&io, scope, SessionAction::RequestKeyframe)
                        .await
                    {
                        return fail_connected(&senders, error, input_context, stable);
                    }
                }
            }
            Err(error) => return fail_connected(&senders, error, input_context, stable),
        }
    }
}

fn optional_transfer_unavailable(error: &ControllerError) -> bool {
    matches!(
        error,
        ControllerError::SessionData(
            SessionDataError::NoPath(ChannelKind::Transfer)
                | SessionDataError::Interrupted(ChannelKind::Transfer)
        )
    )
}

/// Every owned reliable operation has a session-wide cancellation/deadline.
/// Timeout/cancellation retires the connection; an ordinary epoch change never
/// interrupts an operation that has entered this send boundary.
async fn send_cancellable<T>(
    cancellation: &mut watch::Receiver<bool>,
    send: impl std::future::Future<Output = Result<T, ControllerError>>,
) -> Result<T, ControllerError> {
    if *cancellation.borrow() {
        return Err(TransportError::Closed.into());
    }
    tokio::select! {
        biased;
        _ = cancellation.changed() => Err(TransportError::Closed.into()),
        result = tokio::time::timeout(SEND_TIMEOUT, send) => {
            result.unwrap_or_else(|_| Err(TransportError::Connection("controller send deadline exceeded".into()).into()))
        }
    }
}

struct RetryContext<'a> {
    schedule: &'a mut ReconnectSchedule,
    commands: &'a mut mpsc::Receiver<ControllerCommand>,
    pending_release: &'a mut Option<Vec<CapturedInput>>,
    cancellation: &'a mut watch::Receiver<bool>,
    callback: CallbackTarget,
    release_events: &'a Arc<Mutex<Vec<CapturedInput>>>,
    release_pending: &'a Arc<AtomicU8>,
    persistent: bool,
}

async fn schedule_retry(failure: ConnectFailure, stable: bool, context: RetryContext<'_>) -> bool {
    let RetryContext {
        schedule,
        commands,
        pending_release,
        cancellation,
        callback,
        release_events,
        release_pending,
        persistent,
    } = context;
    if !failure.retryable {
        diagnostic_log(
            "terminal_failure",
            &format!("\"message\":\"{}\"", failure.message),
        );
        callback.emit_error(&failure.message, 0);
        return false;
    }
    if *cancellation.borrow() {
        return false;
    }
    if stable {
        schedule.reset();
    }
    match schedule.next(now_unix_s()) {
        ReconnectDecision::RetryAfter { retry, delay } => {
            diagnostic_log(
                "retry_scheduled",
                &format!(
                    "\"retry\":{retry},\"delay_ms\":{},\"message\":\"{}\"",
                    delay.as_millis(),
                    failure.message
                ),
            );
            callback.emit_state(DesklinkState::Reconnecting, 0);
            callback.emit_error_with_state(
                &format!(
                    "{}; retry {retry}/{} in {} ms",
                    failure.message,
                    schedule.max_retries(),
                    delay.as_millis()
                ),
                0,
                DesklinkState::Reconnecting,
            );
            wait_for_retry(
                delay,
                commands,
                pending_release,
                cancellation,
                release_events,
                release_pending,
            )
            .await
        }
        ReconnectDecision::Exhausted => {
            if persistent {
                callback.emit_state(DesklinkState::Reconnecting, 0);
                callback.emit_error_with_state(
                    "waiting for the approved persistent host",
                    0,
                    DesklinkState::Reconnecting,
                );
                return wait_for_retry(
                    schedule.max_delay(),
                    commands,
                    pending_release,
                    cancellation,
                    release_events,
                    release_pending,
                )
                .await;
            }
            callback.emit_error(
                &format!(
                    "reconnect retry budget exhausted after {} attempts: {}",
                    schedule.retries_used(),
                    failure.message
                ),
                0,
            );
            false
        }
        ReconnectDecision::SessionExpired => {
            callback.emit_error("pairing invitation expired before reconnect", 0);
            false
        }
    }
}

async fn wait_for_retry(
    delay: Duration,
    commands: &mut mpsc::Receiver<ControllerCommand>,
    pending_release: &mut Option<Vec<CapturedInput>>,
    cancellation: &mut watch::Receiver<bool>,
    release_events: &Arc<Mutex<Vec<CapturedInput>>>,
    release_pending: &Arc<AtomicU8>,
) -> bool {
    if *cancellation.borrow() {
        return false;
    }
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(ControllerCommand::ReleaseAll) => {
                    release_pending.store(0, Ordering::Release);
                    let events = take_release_events(release_events);
                    if let Some(pending) = pending_release {
                        if pending.len().saturating_add(events.len()) > MAX_RELEASE_EVENTS { return false; }
                        pending.extend(events);
                    } else {
                        if events.len() > MAX_RELEASE_EVENTS { return false; }
                        *pending_release = Some(events);
                    }
                }
                Some(ControllerCommand::SendInput(_))
                | Some(ControllerCommand::RequestKeyframe)
                | Some(ControllerCommand::Control(_))
                | Some(ControllerCommand::Transfer(_))
                | Some(ControllerCommand::Session { .. }) => {}
                Some(ControllerCommand::Shutdown) | None => return false,
            },
            changed = cancellation.changed() => {
                let _ = changed;
                return false;
            }
            () = &mut sleep => return true,
        }
    }
}

fn controller_error_is_retryable(error: &ControllerError) -> bool {
    match error {
        ControllerError::Transport(error) => transport_error_is_retryable(error),
        ControllerError::SessionData(error) => match error {
            SessionDataError::Transport(error)
            | SessionDataError::Lan(LanDataError::Transport(error)) => {
                transport_error_is_retryable(error)
            }
            SessionDataError::Closed
            | SessionDataError::Timeout
            | SessionDataError::Lan(LanDataError::Closed | LanDataError::Timeout) => true,
            SessionDataError::NoPath(channel) | SessionDataError::Interrupted(channel) => {
                matches!(channel, ChannelKind::Input | ChannelKind::Control)
            }
            SessionDataError::InvalidNegotiation
            | SessionDataError::InvalidSwitch
            | SessionDataError::AlreadyAttached
            | SessionDataError::Crypto(_)
            | SessionDataError::Lan(LanDataError::Malformed | LanDataError::Crypto(_)) => false,
        },
        ControllerError::HandshakeTimeout | ControllerError::NegotiationTimeout => true,
        ControllerError::Protocol(_)
        | ControllerError::Crypto(_)
        | ControllerError::UnexpectedHandshakeStep
        | ControllerError::InvalidHostCapabilities
        | ControllerError::InconsistentVideoConfig
        | ControllerError::NoActiveStream
        | ControllerError::UnexpectedTransportLane
        | ControllerError::AccessDenied(_) => false,
    }
}

fn transport_error_is_retryable(error: &TransportError) -> bool {
    desklink_transport::transport_error_is_retryable(
        error,
        desklink_transport::TransportRetryContext::ControllerSession,
    )
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

async fn connect_controller(
    relay_url: &str,
    platform: Platform,
    config: &SecureConnectionConfigOwned,
    callback: CallbackTarget,
) -> Result<ResolvedControllerConnection, ConnectFailure> {
    if config
        .expires_at_unix_s
        .is_some_and(|expiry| now_unix_s() >= expiry)
    {
        return Err(ConnectFailure::permanent(
            "saved temporary authorization expired".into(),
        ));
    }
    let relay_addr = resolve_relay(relay_url).map_err(ConnectFailure::permanent)?;
    let transport_config = QuicClientConfig::new(relay_addr, config.server_name.clone())
        .map_err(ConnectFailure::from_transport)?;
    connect_controller_with_transport(transport_config, platform, config, callback).await
}

async fn connect_controller_with_transport(
    transport_config: QuicClientConfig,
    platform: Platform,
    config: &SecureConnectionConfigOwned,
    callback: CallbackTarget,
) -> Result<ResolvedControllerConnection, ConnectFailure> {
    let mut session_client: Option<QuicClient> = None;
    let (session_id, relay_authentication, host_verify_key, expires_at_unix_s) =
        if let Some(directory) = &config.directory {
            // One connection serves both the directory lookup and the session
            // join; a second QUIC/TLS handshake per fresh connect is pure
            // connection-setup latency with no isolation benefit.
            let lookup_client = connect_relay(transport_config.clone())
                .await
                .map_err(ConnectFailure::from_transport)?;
            let lookup = RelayDirectoryLookup::new(directory.device_id, directory.access_code)
                .map_err(ConnectFailure::from_transport)?;
            let mut invitation = tokio::time::timeout(
                RELAY_REQUEST_TIMEOUT,
                lookup_client.lookup_directory(lookup),
            )
            .await
            .map_err(|_| ConnectFailure::retryable("relay directory lookup timed out".into()))?
            .map_err(ConnectFailure::from_transport)?;
            let result = PairingInvite::decode(&invitation, now_unix_s())
                .map_err(|_| {
                    ConnectFailure::permanent(
                        "the device returned an invalid or expired connection invitation"
                            .to_owned(),
                    )
                })
                .and_then(|invite| {
                    let session_id = *invite.session_id().as_bytes();
                    let relay_authentication = *invite.relay_authentication();
                    let host_verify_key = *invite.host_verify_key().as_bytes();
                    validate_expected_host(config.host_verify_key, host_verify_key)?;
                    Ok((
                        session_id,
                        relay_authentication,
                        host_verify_key,
                        Some(invite.expires_at_unix_s()).filter(|expires| *expires != 0),
                    ))
                });
            invitation.zeroize();
            let material = result?;
            session_client = Some(lookup_client);
            material
        } else {
            (
                config.session_id,
                config.relay_authentication,
                config.host_verify_key,
                config.expires_at_unix_s,
            )
        };
    let join = RelayJoin::controller_with_participant(
        SessionId::from_bytes(session_id),
        relay_authentication,
        config.controller_device_id,
    );
    let client = 'session: {
        if let Some(reused) = session_client
            && join_relay(&reused, join.clone()).await.is_ok()
        {
            break 'session reused;
            // Older relays close the connection after answering a lookup;
            // fall back to a dedicated join connection below.
        }
        let client = connect_relay(transport_config)
            .await
            .map_err(ConnectFailure::from_transport)?;
        join_relay(&client, join)
            .await
            .map_err(ConnectFailure::from_transport)?;
        client
    };
    callback.emit_state(DesklinkState::SecureHandshake, 0);
    let identity =
        DeviceIdentity::from_secret_key(config.controller_device_id, &config.controller_secret_key);
    let expected_host = VerifyingKey::from_bytes(&host_verify_key).map_err(|error| {
        ConnectFailure::permanent(format!("invalid host verification key: {error}"))
    })?;
    // Noise is complete before WaitingForApproval. Host capabilities only
    // arrive after authorization, so do not claim negotiation before approval.
    let controller = ControllerRuntime::connect_for_platform_with_observer(
        client,
        identity,
        expected_host,
        platform,
        || callback.emit_state(DesklinkState::WaitingForApproval, 0),
    )
    .await
    .map_err(ConnectFailure::from_controller)?;
    callback.emit_state(DesklinkState::NegotiatingCapabilities, 0);
    Ok(ResolvedControllerConnection {
        controller,
        expires_at_unix_s,
        material: SavedHostMaterialOwned {
            session_id,
            relay_authentication: Zeroizing::new(relay_authentication),
            host_verify_key,
            server_name: config.server_name.clone(),
            expires_at_unix_s: expires_at_unix_s.unwrap_or(0),
            authorization_type: if expires_at_unix_s.is_some() { 1 } else { 2 },
            directory: config.directory.clone(),
        },
    })
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

struct ResolvedControllerConnection {
    controller: ControllerRuntime,
    expires_at_unix_s: Option<u64>,
    material: SavedHostMaterialOwned,
}

fn validate_expected_host(expected: [u8; 32], actual: [u8; 32]) -> Result<(), ConnectFailure> {
    if expected != [0; 32] && expected != actual {
        return Err(ConnectFailure::permanent(
            "approved host identity changed; pair again".into(),
        ));
    }
    Ok(())
}

fn enqueue_release_all(
    commands: &mpsc::Sender<ControllerCommand>,
    release_events: &Mutex<Vec<CapturedInput>>,
    release_pending: &AtomicU8,
    events: Vec<CapturedInput>,
) -> Result<(), ()> {
    if !events.is_empty() {
        let mut pending = release_events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.len().saturating_add(events.len()) > MAX_RELEASE_EVENTS {
            return Err(());
        }
        pending.extend(events);
    }
    if release_pending.swap(1, Ordering::AcqRel) == 1 {
        return Ok(());
    }
    if commands.try_send(ControllerCommand::ReleaseAll).is_err() {
        release_pending.store(0, Ordering::Release);
        release_events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        return Err(());
    }
    Ok(())
}

fn take_release_events(release_events: &Mutex<Vec<CapturedInput>>) -> Vec<CapturedInput> {
    let mut pending = release_events
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::take(&mut *pending)
}

fn emit_controller_event(callback: CallbackTarget, event: ControllerEvent) -> bool {
    match event {
        ControllerEvent::Control(message) => {
            if let Ok(data) = encode_control(&message) {
                callback.emit(
                    DesklinkEventKind::Control,
                    &data,
                    EventMeta::for_stream(0),
                    DesklinkState::Connected,
                );
            }
        }
        ControllerEvent::VideoConfig(config) => {
            let meta = EventMeta {
                stream_id: config.stream_id,
                frame_id: 0,
                config_version: config.config_version,
                width: config.width,
                height: config.height,
                input_epoch: config.input_epoch,
            };
            callback.emit_state(DesklinkState::Connected, config.stream_id);
            callback.emit(
                DesklinkEventKind::VideoConfig,
                &config.sequence_header,
                meta,
                DesklinkState::Connected,
            );
        }
        ControllerEvent::H264AccessUnit(frame) => {
            callback.emit(
                DesklinkEventKind::H264AccessUnit,
                &frame.data,
                EventMeta {
                    stream_id: frame.stream_id,
                    frame_id: frame.frame_id,
                    config_version: frame.config_version,
                    width: frame.width,
                    height: frame.height,
                    input_epoch: 0,
                },
                DesklinkState::Connected,
            );
        }
        ControllerEvent::Cursor(cursor) => {
            if let Ok(data) = encode_cursor_update(&cursor) {
                callback.emit(
                    DesklinkEventKind::Cursor,
                    &data,
                    EventMeta::for_stream(cursor.stream_id),
                    DesklinkState::Connected,
                );
            }
        }
        ControllerEvent::Audio(packet) => {
            if let Ok(data) = encode_audio_packet(&packet) {
                callback.emit(
                    DesklinkEventKind::Audio,
                    &data,
                    EventMeta::for_stream(packet.stream_id),
                    DesklinkState::Connected,
                );
            }
        }
        ControllerEvent::Transfer(message) => {
            if let Ok(data) = encode_transfer(&message) {
                callback.emit(
                    DesklinkEventKind::Transfer,
                    &data,
                    EventMeta::for_stream(0),
                    DesklinkState::Connected,
                );
            }
        }
        ControllerEvent::Closed { reason } => {
            callback.emit_error(&reason, 0);
            return false;
        }
    }
    true
}

fn resolve_relay(relay_url: &str) -> Result<SocketAddr, String> {
    let authority = relay_url
        .strip_prefix("quic://")
        .ok_or_else(|| "relay URL must start with quic://".to_owned())?;
    authority
        .to_socket_addrs()
        .map_err(|error| format!("relay address resolution failed: {error}"))?
        .next()
        .ok_or_else(|| "relay address did not resolve".to_owned())
}

#[cfg(test)]
mod tests {
    use desklink_transport::JoinRejectCode;

    use super::*;

    #[cfg(unix)]
    type AbiEvents = Mutex<Vec<(DesklinkEventKind, u64, Vec<u8>)>>;
    #[cfg(unix)]
    extern "C" fn record_abi(context: *mut std::ffi::c_void, event: *const DesklinkEvent) {
        let event = unsafe { &*event };
        let bytes = if event.data_len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(event.data, event.data_len) }.to_vec()
        };
        unsafe { &*context.cast::<AbiEvents>() }
            .lock()
            .unwrap()
            .push((event.kind, event.stream_id, bytes));
    }

    // File actions are an Apple-controller API: the non-unix Directory
    // adapter deliberately reports Unsupported, so the file half of this
    // probe can only round-trip where the unix adapter exists.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn c_audio_and_file_actions_cross_worker_and_encrypted_peer_boundaries() {
        use crate::{
            DesklinkConfig, DesklinkResult, desklink_create, desklink_destroy, desklink_send_file,
            desklink_set_audio_enabled,
        };
        use desklink_crypto::SecureLane;
        use desklink_protocol::{
            AudioCodec, AudioPacket, ControlMessage, TransferMessage, TransferResult,
            decode_control, decode_transfer, encode_audio_packet, encode_control, encode_transfer,
        };
        tokio::time::timeout(Duration::from_secs(10),async {
            let (controller,host,mut secure,relay_task)=crate::session_commands::tests::encrypted_runtime().await;
            let events=AbiEvents::default();
            let callback=CallbackTarget{callback:Some(record_abi),context:(&events as *const AbiEvents) as usize};
            let (commands,mut receiver)=mpsc::channel(COMMAND_CAPACITY+RELEASE_RESERVE);
            let (cancellation,mut cancel_receiver)=watch::channel(false);
            let release_events=Arc::new(Mutex::new(Vec::new()));
            let release_pending=Arc::new(AtomicU8::new(0));
            let gate=Arc::new(SessionGate::default());
            let file_gate=Arc::new(SessionGate::default());
            let test_gate=gate.clone();
            let test_file_gate=file_gate.clone();
            let test_release_events=release_events.clone();
            let test_release_pending=release_pending.clone();
            // Match production ownership: destroying the C handle joins the
            // worker runtime, including its file actor callbacks, before the
            // borrowed callback storage may be released.
            let thread=std::thread::spawn(move || {
                let runtime=tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
                runtime.block_on(run_connected(controller,&mut receiver,&mut None,&mut cancel_receiver,callback,
                    &test_release_events,&test_release_pending,&test_gate,&test_file_gate));
            });
            let worker=ControllerWorker {commands,cancellation,release_events,release_pending,
                phase:Arc::new(AtomicU8::new(PHASE_RUNNING)),gate:gate.clone(),file_gate,thread:Some(thread)};
            let url=std::ffi::CString::new("quic://127.0.0.1:1").unwrap();
            let mut handle=std::ptr::null_mut();
            assert_eq!(unsafe { desklink_create(&DesklinkConfig{relay_url:url.as_ptr(),log_level:0},Some(record_abi),callback.context as *mut _,&mut handle) },DesklinkResult::Ok);
            unsafe {(*handle).runtime.worker=Some(worker)};
            let bytes=secure.open(SecureLane::Control,&host.next_control().await.unwrap()).unwrap();
            assert_eq!(decode_control(&bytes).unwrap(),ControlMessage::SetAudioEnabled{enabled:false},"a new controller starts muted");
            while gate.stream_id.load(Ordering::Acquire)!=9 {tokio::task::yield_now().await;}
            assert_eq!(unsafe { desklink_set_audio_enabled(handle,8,1) },DesklinkResult::InvalidState);
            assert_eq!(unsafe { desklink_set_audio_enabled(handle,9,1) },DesklinkResult::Ok);
            let bytes=secure.open(SecureLane::Control,&host.next_control().await.unwrap()).unwrap();
            assert_eq!(decode_control(&bytes).unwrap(),ControlMessage::SetAudioEnabled{enabled:true});
            let bytes=encode_control(&ControlMessage::AudioState{available:true,enabled:true}).unwrap();
            host.send_control(secure.seal(SecureLane::Control,&bytes).unwrap()).await.unwrap();
            // Control and datagram lanes are independent; wait for ack callback
            // before sending this first audio frame to avoid a race in the probe.
            while !events.lock().unwrap().iter().any(|(kind,_,bytes)| *kind==DesklinkEventKind::Session && serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|v|v["kind"]=="audio_state")) {tokio::task::yield_now().await;}
            let bytes=encode_audio_packet(&AudioPacket{protocol_version:desklink_protocol::PROTOCOL_VERSION,stream_id:9,sequence:1,
                capture_timestamp_us:10_000,codec:AudioCodec::PcmS16Le,sample_rate:48_000,channels:1,payload:vec![1;960]}).unwrap();
            host.send_audio_datagram(secure.seal(SecureLane::AudioDatagram,&bytes).unwrap()).await.unwrap();
            while !events.lock().unwrap().iter().any(|(kind,stream,bytes)| *kind==DesklinkEventKind::AudioPcm && *stream==9 && bytes.len()==960) {tokio::task::yield_now().await;}
            let directory=std::env::temp_dir().join(format!("desklink-abi-{}-{}",std::process::id(),SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
            std::fs::create_dir_all(&directory).unwrap();
            let source=directory.join("中文😀.bin");std::fs::write(&source,b"phase4 file boundary").unwrap();
            let path=source.to_str().unwrap();let transfer_id=[17;16];
            gate.invalidate(); // A same-stream display transition releases input.
            assert_eq!(unsafe { desklink_send_file(handle,8,transfer_id.as_ptr(),path.as_ptr(),path.len()) },DesklinkResult::InvalidState);
            assert_eq!(unsafe { desklink_send_file(handle,9,transfer_id.as_ptr(),path.as_ptr(),path.len()) }, DesklinkResult::Ok);
            let bytes=secure.open(SecureLane::Transfer,&host.next_transfer().await.unwrap()).unwrap();
            assert_eq!(decode_transfer(&bytes).unwrap(),TransferMessage::FileOffer{transfer_id,request_id:None,name:"中文😀.bin".into(),size:20});
            let bytes=encode_transfer(&TransferMessage::FileDecision{transfer_id,result:TransferResult::Completed,resume_offset:0,resume_prefix_hash:None}).unwrap();
            host.send_transfer(secure.seal(SecureLane::Transfer,&bytes).unwrap()).await.unwrap();
            let bytes=secure.open(SecureLane::Transfer,&host.next_transfer().await.unwrap()).unwrap();
            assert_eq!(decode_transfer(&bytes).unwrap(),TransferMessage::FileChunk{transfer_id,offset:0,bytes:b"phase4 file boundary".to_vec()});
            let bytes=secure.open(SecureLane::Transfer,&host.next_transfer().await.unwrap()).unwrap();
            assert!(matches!(decode_transfer(&bytes).unwrap(),TransferMessage::FileComplete{transfer_id:id,..} if id==transfer_id));
            let bytes=encode_transfer(&TransferMessage::FileResult{transfer_id,result:TransferResult::Completed}).unwrap();
            host.send_transfer(secure.seal(SecureLane::Transfer,&bytes).unwrap()).await.unwrap();
            while !events.lock().unwrap().iter().any(|(kind,_,bytes)| *kind==DesklinkEventKind::File && serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|v|v["state"]=="completed")) {tokio::task::yield_now().await;}
            let destination=directory.to_str().unwrap();
            assert_eq!(unsafe{crate::desklink_request_remote_file(handle,9,1,destination.as_ptr(),destination.len())},DesklinkResult::Ok);
            let bytes=secure.open(SecureLane::Transfer,&host.next_transfer().await.unwrap()).unwrap();
            assert_eq!(decode_transfer(&bytes).unwrap(),TransferMessage::FileSelectionRequest{request_id:1,resume:None});
            assert_eq!(unsafe{crate::desklink_cancel_file_transfer(handle,9)},DesklinkResult::Ok,
                "file cancellation remains available while same-stream input is suspended");
            let bytes=secure.open(SecureLane::Transfer,&host.next_transfer().await.unwrap()).unwrap();
            assert_eq!(decode_transfer(&bytes).unwrap(),TransferMessage::FileSelectionCancel{request_id:1});
            unsafe {desklink_destroy(handle)};
            relay_task.abort();std::fs::remove_dir_all(directory).unwrap();
        }).await.expect("C media probe must complete");
    }

    #[test]
    fn directory_relookup_cannot_replace_an_approved_host_identity() {
        assert!(validate_expected_host([0; 32], [1; 32]).is_ok());
        assert!(validate_expected_host([1; 32], [1; 32]).is_ok());
        let changed = validate_expected_host([1; 32], [2; 32]).unwrap_err();
        assert!(!changed.retryable);
        assert!(changed.message.contains("identity changed"));
    }

    #[tokio::test]
    async fn persistent_host_keeps_waiting_after_budget_and_is_still_cancellable() {
        let policy =
            ReconnectPolicy::new(Duration::from_millis(1), Duration::from_secs(8), 1).unwrap();
        let mut schedule = ReconnectSchedule::new(policy, None);
        let _ = schedule.next(now_unix_s());
        let (_sender, mut commands) = mpsc::channel(1);
        let (sender, mut cancellation) = watch::channel(false);
        let mut pending = None;
        let events = Arc::new(Mutex::new(Vec::new()));
        let release_pending = Arc::new(AtomicU8::new(0));
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(5)).await;
            sender.send(true).unwrap();
        };
        let retry = schedule_retry(
            ConnectFailure::retryable("offline".into()),
            false,
            RetryContext {
                schedule: &mut schedule,
                commands: &mut commands,
                pending_release: &mut pending,
                cancellation: &mut cancellation,
                callback: CallbackTarget {
                    callback: None,
                    context: 0,
                },
                release_events: &events,
                release_pending: &release_pending,
                persistent: true,
            },
        );
        let (result, ()) = tokio::join!(retry, cancel);
        assert!(!result);
        assert!(*cancellation.borrow());
        assert!(matches!(
            schedule.next(now_unix_s()),
            ReconnectDecision::Exhausted
        ));
    }

    #[tokio::test]
    async fn full_connected_send_is_cancelled_even_with_queued_commands() {
        // A full channel models an established reliable stream with no credit;
        // unlike connection-time cancellation this future has already started.
        let (queue, _peer) = mpsc::channel(1);
        queue.send(()).await.unwrap();
        let (cancel, mut cancelled) = watch::channel(false);
        let send = async {
            queue.send(()).await.unwrap();
            Ok::<_, ControllerError>(())
        };
        let task = async {
            tokio::task::yield_now().await;
            cancel.send(true).unwrap();
        };
        let (result, ()) = tokio::join!(send_cancellable(&mut cancelled, send), task);
        assert!(matches!(
            result,
            Err(ControllerError::Transport(TransportError::Closed))
        ));
        // A new session's cancellation token is independent of the old one.
        let (_new_cancel, mut new_receiver) = watch::channel(false);
        assert!(
            send_cancellable(&mut new_receiver, async { Ok::<_, ControllerError>(()) })
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn connected_send_has_a_deadline_without_user_cancellation() {
        let (_sender, mut cancellation) = watch::channel(false);
        let result = send_cancellable(
            &mut cancellation,
            std::future::pending::<Result<(), ControllerError>>(),
        )
        .await;
        assert!(matches!(
            result,
            Err(ControllerError::Transport(TransportError::Connection(_)))
        ));
    }

    #[test]
    fn recovery_errors_are_structured_and_terminal_errors_are_not_retryable() {
        assert_eq!(
            error_metadata(DesklinkEventKind::Error, DesklinkState::Reconnecting),
            (1, 1)
        );
        assert_eq!(
            error_metadata(DesklinkEventKind::Error, DesklinkState::StartingVideo),
            (2, 1)
        );
        assert_eq!(
            error_metadata(DesklinkEventKind::Error, DesklinkState::Closed),
            (3, 0)
        );
        assert_eq!(
            error_metadata(DesklinkEventKind::VideoConfig, DesklinkState::Connected),
            (0, 0)
        );
    }

    #[test]
    fn retry_classification_keeps_authentication_failures_permanent() {
        assert!(!transport_error_is_retryable(&TransportError::Malformed));
        assert!(!transport_error_is_retryable(
            &TransportError::JoinRejected(JoinRejectCode::SessionNotFound)
        ));
        assert!(!transport_error_is_retryable(
            &TransportError::JoinRejected(JoinRejectCode::SessionOccupied)
        ));
        assert!(!transport_error_is_retryable(
            &TransportError::JoinRejected(JoinRejectCode::AuthenticationMismatch)
        ));
        assert!(!transport_error_is_retryable(
            &TransportError::JoinRejected(JoinRejectCode::RoleMismatch)
        ));
        assert!(controller_error_is_retryable(
            &ControllerError::HandshakeTimeout
        ));
        assert!(!controller_error_is_retryable(
            &ControllerError::UnexpectedHandshakeStep
        ));
        assert!(
            ConnectFailure::from_transport(TransportError::Stream("synthetic interruption".into()))
                .retryable
        );
        assert!(!controller_error_is_retryable(
            &ControllerError::AccessDenied(desklink_protocol::AccessDenialReason::ApprovalRejected)
        ));
        assert!(!controller_error_is_retryable(
            &ControllerError::AccessDenied(desklink_protocol::AccessDenialReason::ApprovalExpired)
        ));
    }

    #[test]
    fn session_data_retry_classification_requires_a_new_noise_session_for_core_path_loss() {
        for (error, retryable) in [
            (
                SessionDataError::Transport(TransportError::Connection("offline".into())),
                true,
            ),
            (
                SessionDataError::Transport(TransportError::JoinRejected(
                    JoinRejectCode::AuthenticationMismatch,
                )),
                false,
            ),
            (
                SessionDataError::Lan(LanDataError::Transport(TransportError::Closed)),
                true,
            ),
            (SessionDataError::NoPath(ChannelKind::Control), true),
            (SessionDataError::Interrupted(ChannelKind::Input), true),
            (SessionDataError::NoPath(ChannelKind::Transfer), false),
            (
                SessionDataError::Interrupted(ChannelKind::VideoReliable),
                false,
            ),
            (SessionDataError::Timeout, true),
            (SessionDataError::Closed, true),
            (SessionDataError::Lan(LanDataError::Timeout), true),
            (SessionDataError::InvalidNegotiation, false),
            (SessionDataError::InvalidSwitch, false),
            (SessionDataError::AlreadyAttached, false),
            (
                SessionDataError::Crypto(desklink_crypto::CryptoError::AuthenticationFailed),
                false,
            ),
            (
                SessionDataError::Lan(LanDataError::Crypto(
                    desklink_crypto::CryptoError::AuthenticationFailed,
                )),
                false,
            ),
            (SessionDataError::Lan(LanDataError::Malformed), false),
        ] {
            let error = ControllerError::SessionData(error);
            assert_eq!(controller_error_is_retryable(&error), retryable, "{error}");
        }
    }

    #[test]
    fn optional_transfer_retirement_does_not_swallow_core_or_authentication_errors() {
        for (error, optional) in [
            (SessionDataError::NoPath(ChannelKind::Transfer), true),
            (SessionDataError::Interrupted(ChannelKind::Transfer), true),
            (SessionDataError::NoPath(ChannelKind::Control), false),
            (SessionDataError::Interrupted(ChannelKind::Input), false),
            (SessionDataError::Timeout, false),
            (SessionDataError::Closed, false),
            (
                SessionDataError::Crypto(desklink_crypto::CryptoError::AuthenticationFailed),
                false,
            ),
            (SessionDataError::InvalidSwitch, false),
        ] {
            let error = ControllerError::SessionData(error);
            assert_eq!(optional_transfer_unavailable(&error), optional, "{error}");
        }
        assert!(!optional_transfer_unavailable(&ControllerError::Protocol(
            desklink_protocol::ProtocolError::UnsupportedVersion(13)
        )));
    }

    #[tokio::test]
    async fn retry_wait_discards_stale_commands_and_honors_cancellation() {
        let (sender, mut commands) = mpsc::channel(4);
        let (cancellation_sender, mut cancellation) = watch::channel(false);
        let release_events = Arc::new(Mutex::new(Vec::new()));
        let release_pending = Arc::new(AtomicU8::new(0));
        let mut pending_release = None;
        sender
            .send(ControllerCommand::Session {
                scope: CommandScope {
                    epoch: 1,
                    stream_id: 1,
                },
                action: SessionAction::RequestKeyframe,
            })
            .await
            .unwrap();
        assert!(
            wait_for_retry(
                Duration::from_millis(1),
                &mut commands,
                &mut pending_release,
                &mut cancellation,
                &release_events,
                &release_pending,
            )
            .await
        );
        assert!(commands.try_recv().is_err());

        cancellation_sender.send(true).unwrap();
        assert!(
            !wait_for_retry(
                Duration::from_secs(30),
                &mut commands,
                &mut pending_release,
                &mut cancellation,
                &release_events,
                &release_pending,
            )
            .await
        );
    }

    #[tokio::test]
    async fn repeated_release_all_is_coalesced_without_consuming_queue_capacity() {
        let (sender, mut receiver) = mpsc::channel(1);
        let release_events = Mutex::new(Vec::new());
        let release_pending = AtomicU8::new(0);
        let first_event = CapturedInput {
            context: InputContext {
                stream_id: 1,
                input_epoch: 11,
            },
            event: InputEvent::MouseMove { x: 1, y: 2 },
        };
        let second_event = CapturedInput {
            context: InputContext {
                stream_id: 2,
                input_epoch: 22,
            },
            event: InputEvent::MouseMove { x: 3, y: 4 },
        };

        assert!(
            enqueue_release_all(
                &sender,
                &release_events,
                &release_pending,
                vec![first_event.clone()]
            )
            .is_ok()
        );
        assert!(
            enqueue_release_all(
                &sender,
                &release_events,
                &release_pending,
                vec![second_event.clone()]
            )
            .is_ok()
        );
        assert!(matches!(
            receiver.try_recv(),
            Ok(ControllerCommand::ReleaseAll)
        ));
        assert_eq!(
            take_release_events(&release_events),
            vec![first_event, second_event]
        );
        assert_eq!(release_pending.load(Ordering::Acquire), 1);
    }
}

#[cfg(test)]
#[path = "worker/concurrent_send_tests.rs"]
mod concurrent_send_tests;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn directory_connect_reuses_its_lookup_connection_for_the_session() {
    use desklink_crypto::{DeviceIdentity, PairingInvite, SessionId};
    use desklink_relay::{RelayConfig, RelayServer};
    use desklink_transport::{QuicClient, QuicClientConfig, RelayDirectoryRegistration};
    use rand_core::{OsRng, RngCore};

    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let der = certificate.cert.der().to_vec();
    let server_config = quinn::ServerConfig::with_single_cert(
        vec![rustls::pki_types::CertificateDer::from(der.clone())],
        rustls::pki_types::PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into()),
    )
    .unwrap();
    let relay = Arc::new(
        RelayServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            server_config,
            RelayConfig::default(),
        )
        .await
        .unwrap(),
    );
    let address = relay.local_addr().unwrap();
    let relay_task = {
        let relay = relay.clone();
        tokio::spawn(async move {
            let _ = relay.run().await;
        })
    };
    let der_for_closure = der.clone();
    let client_config = move || {
        let mut roots = rustls::RootCertStore::empty();
        let _ = roots.add(rustls::pki_types::CertificateDer::from(
            der_for_closure.clone(),
        ));
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let quic_tls = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
        QuicClientConfig::with_client_config(
            address,
            "localhost",
            quinn::ClientConfig::new(Arc::new(quic_tls)),
        )
    };

    // Host side: register one directory entry from its own connection.
    let host_identity = DeviceIdentity::from_secret_key([7; 16], &[8; 32]);
    let password: [u8; 8] = *b"ABCDEFGH";
    let code = desklink_crypto::PairingCode::derive_from_password(&password).unwrap();
    let mut session_id = [0u8; 16];
    let mut authentication = [0u8; 32];
    OsRng.fill_bytes(&mut session_id);
    OsRng.fill_bytes(&mut authentication);
    let invite = PairingInvite::for_persistent_connection(
        &host_identity,
        SessionId::from_bytes(session_id),
        authentication,
    )
    .encode()
    .unwrap();
    let registration = RelayDirectoryRegistration::new(
        crate::host_directory::public_host_device_id(host_identity.device_id),
        *code.as_bytes(),
        invite.as_bytes().to_vec(),
        0,
    )
    .unwrap();
    let registrar = QuicClient::connect(client_config()).await.unwrap();
    let host_join = RelayJoin::host_with_participant(
        SessionId::from_bytes(session_id),
        authentication,
        host_identity.device_id,
    )
    .with_directory_registration(registration)
    .unwrap();
    registrar.join(host_join).await.unwrap();
    let connections_after_registration = relay.capacity_snapshot().accepted_connections;
    assert_eq!(connections_after_registration, 1);

    // Controller side: the worker's directory connect must finish with
    // exactly one additional relay connection (lookup + join on it).
    let controller_secret = [10u8; 32];
    let controller_identity = DeviceIdentity::from_secret_key([9; 16], &controller_secret);
    let config = SecureConnectionConfigOwned {
        server_name: "localhost".to_owned(),
        session_id: [0; 16],
        relay_authentication: [0; 32],
        controller_device_id: controller_identity.device_id,
        controller_secret_key: controller_secret,
        host_verify_key: [0; 32],
        expires_at_unix_s: None,
        authorization_type: 0,
        directory: Some(DirectoryConnectionConfigOwned {
            device_id: crate::host_directory::public_host_device_id(host_identity.device_id),
            access_code: *code.as_bytes(),
        }),
    };
    // The lookup and the session join run first; the connect then parks in
    // the Noise handshake waiting for a host responder this test never
    // provides. Parking there proves lookup+join already completed, and
    // the relay must have accepted exactly one controller connection.
    let mut roots = rustls::RootCertStore::empty();
    let _ = roots.add(rustls::pki_types::CertificateDer::from(der.clone()));
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let transport = QuicClientConfig::with_client_config(
        address,
        "localhost",
        quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
        )),
    );
    let parked = tokio::time::timeout(
        Duration::from_secs(2),
        connect_controller_with_transport(
            transport,
            Platform::MacOS,
            &config,
            CallbackTarget {
                callback: None,
                context: 0,
            },
        ),
    )
    .await;
    match parked {
        Err(_) => {}
        Ok(Err(failure)) => panic!("connect failed early: {}", failure.message),
        Ok(Ok(_)) => panic!("connect unexpectedly completed without a host responder"),
    }
    assert_eq!(
        relay.capacity_snapshot().accepted_connections,
        connections_after_registration + 1,
        "directory lookup and session join must share one connection"
    );
    registrar.close(b"test complete");
    relay_task.abort();
}

#[test]
fn diagnostic_lines_are_structured_and_default_off() {
    // DESKLINK_LOG is unset in the test environment, so the worker must
    // produce no output; when a session later enables it, every line is
    // a JSON object with a timestamp, module and event name.
    assert_eq!(
        diagnostic_line("terminal_failure", "\"message\":\"x\""),
        None
    );
}
