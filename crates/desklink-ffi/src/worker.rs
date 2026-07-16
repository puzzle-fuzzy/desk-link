use std::{
    net::{SocketAddr, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use desklink_crypto::{DeviceIdentity, SessionId};
use desklink_protocol::{InputEvent, encode_control, encode_cursor_update};
use desklink_session::{ReconnectDecision, ReconnectPolicy, ReconnectSchedule};
use desklink_transport::{JoinRejectCode, QuicClient, QuicClientConfig, RelayJoin, TransportError};
use ed25519_dalek::VerifyingKey;
use tokio::sync::{mpsc, watch};
use zeroize::Zeroize;

use crate::{
    ControllerError, ControllerEvent, ControllerRuntime, DesklinkEvent, DesklinkEventCallback,
    DesklinkEventKind, DesklinkState, EventMeta,
};

const COMMAND_CAPACITY: usize = 1_024;
const RELEASE_RESERVE: usize = 1;
const PHASE_CONNECTING: u8 = 0;
const PHASE_RUNNING: u8 = 1;
const PHASE_FINISHED: u8 = 2;

pub(crate) struct SecureConnectionConfigOwned {
    pub server_name: String,
    pub session_id: [u8; 16],
    pub relay_authentication: [u8; 32],
    pub controller_device_id: [u8; 16],
    pub controller_secret_key: [u8; 32],
    pub host_verify_key: [u8; 32],
    pub expires_at_unix_s: Option<u64>,
}

impl Drop for SecureConnectionConfigOwned {
    fn drop(&mut self) {
        self.relay_authentication.zeroize();
        self.controller_secret_key.zeroize();
        self.host_verify_key.zeroize();
    }
}

#[derive(Debug)]
pub(crate) enum ControllerCommand {
    SendInput(InputEvent),
    ReleaseAll(Vec<InputEvent>),
    RequestKeyframe,
    Shutdown,
}

#[derive(Clone, Copy)]
struct CallbackTarget {
    callback: Option<DesklinkEventCallback>,
    context: usize,
}

impl CallbackTarget {
    fn emit(&self, kind: DesklinkEventKind, data: &[u8], meta: EventMeta, state: DesklinkState) {
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

pub(crate) struct ControllerWorker {
    commands: mpsc::Sender<ControllerCommand>,
    cancellation: watch::Sender<bool>,
    phase: Arc<AtomicU8>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ControllerWorker {
    pub(crate) fn start(
        relay_url: String,
        config: SecureConnectionConfigOwned,
        callback: Option<DesklinkEventCallback>,
        callback_context: *mut std::ffi::c_void,
        material_invalidator: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<Self, std::io::Error> {
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY + RELEASE_RESERVE);
        let (cancellation, cancellation_receiver) = watch::channel(false);
        let phase = Arc::new(AtomicU8::new(PHASE_CONNECTING));
        let worker_phase = phase.clone();
        let callback = CallbackTarget {
            callback,
            context: callback_context as usize,
        };
        let worker_material_invalidator = material_invalidator.clone();
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
                        config,
                        receiver,
                        cancellation_receiver,
                        worker_phase.clone(),
                        callback,
                        worker_material_invalidator,
                    )),
                    Err(error) => callback.emit_error(&error.to_string(), 0),
                }
                worker_phase.store(PHASE_FINISHED, Ordering::Release);
            })?;
        Ok(Self {
            commands,
            cancellation,
            phase,
            thread: Some(thread),
        })
    }

    pub(crate) fn send(&self, command: ControllerCommand) -> Result<(), ()> {
        if self.commands.capacity() <= RELEASE_RESERVE {
            return Err(());
        }
        self.commands.try_send(command).map_err(|_| ())
    }

    pub(crate) fn release_all(&self, events: Vec<InputEvent>) -> Result<(), ()> {
        self.commands
            .try_send(ControllerCommand::ReleaseAll(events))
            .map_err(|_| ())
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

async fn run_worker(
    relay_url: String,
    config: SecureConnectionConfigOwned,
    mut commands: mpsc::Receiver<ControllerCommand>,
    mut cancellation: watch::Receiver<bool>,
    phase: Arc<AtomicU8>,
    callback: CallbackTarget,
    material_invalidator: Option<Arc<dyn Fn() + Send + Sync>>,
) {
    let mut schedule = ReconnectSchedule::new(ReconnectPolicy::default(), config.expires_at_unix_s);
    let mut first_attempt = true;
    let mut pending_release: Option<Vec<InputEvent>> = None;
    loop {
        if !first_attempt && let Some(invalidate) = &material_invalidator {
            invalidate();
        }
        phase.store(PHASE_CONNECTING, Ordering::Release);
        callback.emit_state(
            if first_attempt {
                DesklinkState::ConnectingRelay
            } else {
                DesklinkState::Reconnecting
            },
            0,
        );
        let connection = connect_controller(&relay_url, &config, callback);
        let controller = tokio::select! {
            result = connection => match result {
                Ok(controller) => controller,
                Err(failure) => {
                    if schedule_retry(
                        failure,
                        false,
                        &mut schedule,
                        &mut commands,
                        &mut pending_release,
                        &mut cancellation,
                        callback,
                    ).await {
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
        callback.emit_state(DesklinkState::StartingVideo, 0);
        match run_connected(
            controller,
            &mut commands,
            &mut pending_release,
            &mut cancellation,
            callback,
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
                    &mut schedule,
                    &mut commands,
                    &mut pending_release,
                    &mut cancellation,
                    callback,
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

async fn run_connected(
    mut controller: ControllerRuntime,
    commands: &mut mpsc::Receiver<ControllerCommand>,
    pending_release: &mut Option<Vec<InputEvent>>,
    cancellation: &mut watch::Receiver<bool>,
    callback: CallbackTarget,
) -> ConnectedOutcome {
    let mut stable = false;
    if let Some(events) = pending_release.take() {
        for event in events {
            if let Err(error) = controller.send_input(event).await {
                return ConnectedOutcome::Failed {
                    failure: ConnectFailure::from_controller(error),
                    stream_id: controller.active_stream_id().unwrap_or(0),
                    stable,
                };
            }
        }
    }
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(ControllerCommand::ReleaseAll(events)) => {
                    for event in events {
                        if let Err(error) = controller.send_input(event).await {
                            return ConnectedOutcome::Failed {
                                failure: ConnectFailure::from_controller(error),
                                stream_id: controller.active_stream_id().unwrap_or(0),
                                stable,
                            };
                        }
                    }
                }
                Some(ControllerCommand::SendInput(event)) => {
                    if let Err(error) = controller.send_input(event).await {
                        return ConnectedOutcome::Failed {
                            failure: ConnectFailure::from_controller(error),
                            stream_id: controller.active_stream_id().unwrap_or(0),
                            stable,
                        };
                    }
                }
                Some(ControllerCommand::RequestKeyframe) => {
                    if let Err(error) = controller.request_keyframe().await {
                        if matches!(error, ControllerError::NoActiveStream) {
                            callback.emit_error_with_state(
                                &error.to_string(),
                                0,
                                DesklinkState::StartingVideo,
                            );
                            continue;
                        }
                        return ConnectedOutcome::Failed {
                            failure: ConnectFailure::from_controller(error),
                            stream_id: controller.active_stream_id().unwrap_or(0),
                            stable,
                        };
                    }
                }
                Some(ControllerCommand::Shutdown) | None => {
                    return ConnectedOutcome::Shutdown {
                        stream_id: controller.active_stream_id().unwrap_or(0),
                    };
                }
            },
            changed = cancellation.changed() => {
                let _ = changed;
                return ConnectedOutcome::Shutdown {
                    stream_id: controller.active_stream_id().unwrap_or(0),
                };
            }
            event = controller.next_event() => match event {
                Ok(ControllerEvent::Closed { reason }) => {
                    return ConnectedOutcome::Failed {
                        failure: ConnectFailure::retryable(format!("transport closed: {reason}")),
                        stream_id: controller.active_stream_id().unwrap_or(0),
                        stable,
                    };
                }
                Ok(event) => {
                    if matches!(event, ControllerEvent::VideoConfig(_)) {
                        stable = true;
                    }
                    let _ = emit_controller_event(callback, event);
                }
                Err(error) => {
                    return ConnectedOutcome::Failed {
                        failure: ConnectFailure::from_controller(error),
                        stream_id: controller.active_stream_id().unwrap_or(0),
                        stable,
                    };
                }
            }
        }
    }
}

async fn schedule_retry(
    failure: ConnectFailure,
    stable: bool,
    schedule: &mut ReconnectSchedule,
    commands: &mut mpsc::Receiver<ControllerCommand>,
    pending_release: &mut Option<Vec<InputEvent>>,
    cancellation: &mut watch::Receiver<bool>,
    callback: CallbackTarget,
) -> bool {
    if !failure.retryable {
        callback.emit_error(&failure.message, 0);
        return false;
    }
    if stable {
        schedule.reset();
    }
    match schedule.next(now_unix_s()) {
        ReconnectDecision::RetryAfter { retry, delay } => {
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
            wait_for_retry(delay, commands, pending_release, cancellation).await
        }
        ReconnectDecision::Exhausted => {
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
    pending_release: &mut Option<Vec<InputEvent>>,
    cancellation: &mut watch::Receiver<bool>,
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
                Some(ControllerCommand::ReleaseAll(events)) => {
                    if let Some(pending) = pending_release {
                        pending.extend(events);
                    } else {
                        *pending_release = Some(events);
                    }
                }
                Some(ControllerCommand::SendInput(_))
                | Some(ControllerCommand::RequestKeyframe) => {}
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
        ControllerError::HandshakeTimeout | ControllerError::NegotiationTimeout => true,
        ControllerError::Protocol(_)
        | ControllerError::Crypto(_)
        | ControllerError::UnexpectedHandshakeStep
        | ControllerError::InvalidHostCapabilities
        | ControllerError::InconsistentVideoConfig
        | ControllerError::NoActiveStream
        | ControllerError::UnexpectedTransportLane => false,
    }
}

fn transport_error_is_retryable(error: &TransportError) -> bool {
    match error {
        TransportError::Connection(_)
        | TransportError::ConnectionLimit
        | TransportError::Stream(_)
        | TransportError::Datagram(_)
        | TransportError::Closed => true,
        TransportError::JoinRejected(code) => matches!(
            code,
            JoinRejectCode::SessionNotFound
                | JoinRejectCode::SessionOccupied
                | JoinRejectCode::Internal
                | JoinRejectCode::ConnectionLimit
                | JoinRejectCode::SessionLimit
        ),
        TransportError::MessageTooLarge { .. }
        | TransportError::NotJoined
        | TransportError::AlreadyJoined
        | TransportError::DirectoryNotFound
        | TransportError::DirectoryRateLimited
        | TransportError::Malformed
        | TransportError::InvalidConfig(_) => false,
    }
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

async fn connect_controller(
    relay_url: &str,
    config: &SecureConnectionConfigOwned,
    callback: CallbackTarget,
) -> Result<ControllerRuntime, ConnectFailure> {
    let relay_addr = resolve_relay(relay_url).map_err(ConnectFailure::permanent)?;
    let transport_config = QuicClientConfig::new(relay_addr, config.server_name.clone())
        .map_err(ConnectFailure::from_transport)?;
    let client = QuicClient::connect(transport_config)
        .await
        .map_err(ConnectFailure::from_transport)?;
    client
        .join(RelayJoin::controller_with_participant(
            SessionId::from_bytes(config.session_id),
            config.relay_authentication,
            config.controller_device_id,
        ))
        .await
        .map_err(ConnectFailure::from_transport)?;
    callback.emit_state(DesklinkState::SecureHandshake, 0);
    let identity =
        DeviceIdentity::from_secret_key(config.controller_device_id, &config.controller_secret_key);
    let expected_host = VerifyingKey::from_bytes(&config.host_verify_key).map_err(|error| {
        ConnectFailure::permanent(format!("invalid host verification key: {error}"))
    })?;
    callback.emit_state(DesklinkState::NegotiatingCapabilities, 0);
    ControllerRuntime::connect(client, identity, expected_host)
        .await
        .map_err(ConnectFailure::from_controller)
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
    use super::*;

    #[test]
    fn retry_classification_keeps_authentication_failures_permanent() {
        assert!(transport_error_is_retryable(&TransportError::JoinRejected(
            JoinRejectCode::SessionNotFound
        )));
        assert!(transport_error_is_retryable(&TransportError::JoinRejected(
            JoinRejectCode::SessionOccupied
        )));
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
    }

    #[tokio::test]
    async fn retry_wait_discards_stale_commands_and_honors_cancellation() {
        let (sender, mut commands) = mpsc::channel(4);
        let (cancellation_sender, mut cancellation) = watch::channel(false);
        let mut pending_release = None;
        sender
            .send(ControllerCommand::RequestKeyframe)
            .await
            .unwrap();
        assert!(
            wait_for_retry(
                Duration::from_millis(1),
                &mut commands,
                &mut pending_release,
                &mut cancellation,
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
            )
            .await
        );
    }
}
