use std::{
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use desklink_crypto::{
    CryptoError, DeviceIdentity, NoiseResponder, PeerIdentity, SecureLane, SecureRole,
    SecureSession,
};
use desklink_protocol::{
    Codec, ControlMessage, CursorUpdate, DeviceCapabilities, DeviceRole, FrameFlags, InputEvent,
    NoiseHandshake, NoiseHandshakeStep, PROTOCOL_VERSION, Platform, ProtocolError, VideoConfig,
    decode_control, decode_input, decode_noise_handshake, encode_control, encode_cursor_update,
    encode_noise_handshake, encode_video_config, encode_video_packet,
};
use desklink_session::{DesktopRect, ReconnectDecision, ReconnectPolicy, ReconnectSchedule};
use desklink_transport::{
    JoinRejectCode, QuicClient, QuicClientConfig, RelayJoin, TransportError, TransportEvent,
};
use desklink_video::{EncodedFrame as WireEncodedFrame, LatestFrameQueue, packetize_frame};
use ed25519_dalek::VerifyingKey;
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, Notify, oneshot};
use zeroize::Zeroizing;

use crate::{
    capture::{CaptureError, CapturedFrame, DesktopCapturer, DxgiDesktopCapturer},
    encoder::{EncodedFrame, EncoderError, H264Encoder, fit_h264_dimensions},
    input::{InputInjectionError, InputInjector, VirtualDesktop},
    window::ApprovalState,
};

const CAPTURE_TIMEOUT: Duration = Duration::from_millis(50);
const CURSOR_INTERVAL: Duration = Duration::from_millis(16);
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(15);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const VIDEO_QUEUE_CAPACITY: usize = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedVideo {
    pub video_config: Option<Vec<u8>>,
    pub datagrams: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrepareVideo {
    NeedKeyframe,
    Ready(PreparedVideo),
}

#[derive(Debug)]
pub enum CaptureOutcome {
    Frame(CapturedFrame),
    Idle,
    Recovered,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum HostRuntimeError {
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("cryptographic session error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("capture error: {0:?}")]
    Capture(CaptureError),
    #[error("video dimensions exceed the negotiated protocol bounds")]
    InvalidDimensions,
    #[error("an encoder configuration version changed dimensions without advancing")]
    InconsistentVideoConfig,
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("encoder error: {0:?}")]
    Encoder(EncoderError),
    #[error("input injection error: {0:?}")]
    Input(InputInjectionError),
    #[error("controller capabilities are invalid or incompatible")]
    InvalidControllerCapabilities,
    #[error("controller capability negotiation timed out")]
    NegotiationTimeout,
    #[error("authenticated Noise handshake timed out")]
    HandshakeTimeout,
    #[error("received an unexpected Noise handshake step")]
    UnexpectedHandshakeStep,
    #[error("transport closed: {0}")]
    TransportClosed(String),
    #[error("capture worker stopped unexpectedly")]
    CaptureWorkerStopped,
    #[error("capture worker panicked")]
    CaptureWorkerPanicked,
    #[error("host approval is required before starting capture or input injection")]
    ApprovalRequired,
    #[error("the authenticated controller is not trusted")]
    UntrustedController,
    #[error("a trusted controller device ID presented a different public key")]
    ControllerKeyChanged,
    #[error("the host rejected the controller pairing request")]
    PairingRejected,
    #[error("the controller pairing invitation expired")]
    PairingExpired,
    #[error("controller authorization backend failed: {0}")]
    AuthorizationBackend(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostLifecycleEvent {
    Connecting {
        attempt: u32,
        stream_id: u64,
    },
    Connected {
        stream_id: u64,
    },
    Reconnecting {
        retry: u32,
        maximum_retries: u32,
        delay: Duration,
        reason: String,
    },
    Stopped {
        reason: String,
    },
}

pub trait HostLifecycleObserver: Send + Sync {
    fn publish(&self, event: HostLifecycleEvent);
}

impl<F> HostLifecycleObserver for F
where
    F: Fn(HostLifecycleEvent) + Send + Sync,
{
    fn publish(&self, event: HostLifecycleEvent) {
        self(event);
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum HostSupervisorError {
    #[error("host connection failed permanently: {0}")]
    Permanent(HostRuntimeError),
    #[error("host reconnect retry budget exhausted after {retries} retries: {last_error}")]
    RetryBudgetExhausted { retries: u32, last_error: String },
    #[error("pairing invitation expired before host reconnect")]
    PairingExpired,
    #[error("video stream identifier space is exhausted")]
    StreamIdExhausted,
}

pub struct HostSupervisor {
    transport: QuicClientConfig,
    session_id: desklink_crypto::SessionId,
    relay_authentication: Zeroizing<[u8; 32]>,
    initial_stream_id: u64,
    host_device_id: [u8; 16],
    host_secret_key: Zeroizing<[u8; 32]>,
    authorizer: Arc<dyn ControllerAuthorizer>,
    reconnect_policy: ReconnectPolicy,
    expires_at_unix_s: Option<u64>,
    observer: Option<Arc<dyn HostLifecycleObserver>>,
}

impl HostSupervisor {
    pub fn new(
        transport: QuicClientConfig,
        session_id: desklink_crypto::SessionId,
        relay_authentication: [u8; 32],
        initial_stream_id: u64,
        identity: DeviceIdentity,
        authorizer: Arc<dyn ControllerAuthorizer>,
        expires_at_unix_s: Option<u64>,
    ) -> Result<Self, HostRuntimeError> {
        if initial_stream_id == 0 {
            return Err(HostRuntimeError::InvalidControllerCapabilities);
        }
        let host_secret_key = identity.with_secret_key_bytes(|secret| Zeroizing::new(*secret));
        Ok(Self {
            transport,
            session_id,
            relay_authentication: Zeroizing::new(relay_authentication),
            initial_stream_id,
            host_device_id: identity.device_id,
            host_secret_key,
            authorizer,
            reconnect_policy: ReconnectPolicy::default(),
            expires_at_unix_s,
            observer: None,
        })
    }

    pub fn with_reconnect_policy(mut self, policy: ReconnectPolicy) -> Self {
        self.reconnect_policy = policy;
        self
    }

    pub fn with_observer(mut self, observer: Arc<dyn HostLifecycleObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    pub async fn run(self) -> Result<(), HostSupervisorError> {
        let mut schedule = ReconnectSchedule::new(self.reconnect_policy, self.expires_at_unix_s);
        let mut stream_id = self.initial_stream_id;
        loop {
            self.publish(HostLifecycleEvent::Connecting {
                attempt: schedule.retries_used().saturating_add(1),
                stream_id,
            });
            let stable = AtomicBool::new(false);
            let outcome = self.run_attempt(stream_id, &stable).await;
            let error = match outcome {
                Ok(()) => return Ok(()),
                Err(error) if !host_error_is_retryable(&error) => {
                    self.publish(HostLifecycleEvent::Stopped {
                        reason: error.to_string(),
                    });
                    return Err(HostSupervisorError::Permanent(error));
                }
                Err(error) => error,
            };
            if stable.load(Ordering::Acquire) {
                schedule.reset();
            }
            let reason = error.to_string();
            match schedule.next(now_unix_s()) {
                ReconnectDecision::RetryAfter { retry, delay } => {
                    stream_id = match stream_id.checked_add(1).filter(|stream_id| *stream_id != 0) {
                        Some(stream_id) => stream_id,
                        None => {
                            self.publish(HostLifecycleEvent::Stopped {
                                reason: "video stream identifier space is exhausted".to_owned(),
                            });
                            return Err(HostSupervisorError::StreamIdExhausted);
                        }
                    };
                    self.publish(HostLifecycleEvent::Reconnecting {
                        retry,
                        maximum_retries: schedule.max_retries(),
                        delay,
                        reason,
                    });
                    tokio::time::sleep(delay).await;
                }
                ReconnectDecision::Exhausted => {
                    let retries = schedule.retries_used();
                    self.publish(HostLifecycleEvent::Stopped {
                        reason: format!(
                            "reconnect retry budget exhausted after {retries} retries: {reason}"
                        ),
                    });
                    return Err(HostSupervisorError::RetryBudgetExhausted {
                        retries,
                        last_error: reason,
                    });
                }
                ReconnectDecision::SessionExpired => {
                    self.publish(HostLifecycleEvent::Stopped {
                        reason: "pairing invitation expired before host reconnect".to_owned(),
                    });
                    return Err(HostSupervisorError::PairingExpired);
                }
            }
        }
    }

    async fn run_attempt(
        &self,
        stream_id: u64,
        stable: &AtomicBool,
    ) -> Result<(), HostRuntimeError> {
        let client = QuicClient::connect(self.transport.clone()).await?;
        client
            .join(RelayJoin::host(self.session_id, *self.relay_authentication))
            .await?;
        let identity = DeviceIdentity::from_secret_key(self.host_device_id, &self.host_secret_key);
        HostRuntime::with_authorizer(client, stream_id, identity, self.authorizer.clone())?
            .run_tracking_stability(stable, |stream_id| {
                self.publish(HostLifecycleEvent::Connected { stream_id });
            })
            .await
    }

    fn publish(&self, event: HostLifecycleEvent) {
        if let Some(observer) = &self.observer {
            observer.publish(event);
        }
    }
}

pub fn host_error_is_retryable(error: &HostRuntimeError) -> bool {
    match error {
        HostRuntimeError::Transport(error) => transport_error_is_retryable(error),
        HostRuntimeError::TransportClosed(_)
        | HostRuntimeError::HandshakeTimeout
        | HostRuntimeError::NegotiationTimeout => true,
        HostRuntimeError::Protocol(_)
        | HostRuntimeError::Crypto(_)
        | HostRuntimeError::Capture(_)
        | HostRuntimeError::InvalidDimensions
        | HostRuntimeError::InconsistentVideoConfig
        | HostRuntimeError::Encoder(_)
        | HostRuntimeError::Input(_)
        | HostRuntimeError::InvalidControllerCapabilities
        | HostRuntimeError::UnexpectedHandshakeStep
        | HostRuntimeError::CaptureWorkerStopped
        | HostRuntimeError::CaptureWorkerPanicked
        | HostRuntimeError::ApprovalRequired
        | HostRuntimeError::UntrustedController
        | HostRuntimeError::ControllerKeyChanged
        | HostRuntimeError::PairingRejected
        | HostRuntimeError::PairingExpired
        | HostRuntimeError::AuthorizationBackend(_) => false,
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
        | TransportError::Malformed
        | TransportError::InvalidConfig(_) => false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerAuthorization {
    Authorized,
    Unknown,
    KeyChanged,
    Rejected,
    Expired,
}

pub trait ControllerAuthorizer: Send + Sync {
    /// A pinned key lets the Noise layer reject the peer before completing the
    /// handshake. Returning `None` is appropriate for a multi-entry trust store;
    /// the exact authenticated identity is then checked by `authorize`.
    fn pinned_verify_key(&self) -> Option<VerifyingKey> {
        None
    }

    fn authorize(&self, identity: PeerIdentity) -> Result<ControllerAuthorization, String>;
}

#[derive(Clone, Copy, Debug)]
struct PinnedControllerAuthorizer {
    expected: VerifyingKey,
}

impl ControllerAuthorizer for PinnedControllerAuthorizer {
    fn pinned_verify_key(&self) -> Option<VerifyingKey> {
        Some(self.expected)
    }

    fn authorize(&self, identity: PeerIdentity) -> Result<ControllerAuthorization, String> {
        Ok(if identity.verify_key() == self.expected {
            ControllerAuthorization::Authorized
        } else {
            ControllerAuthorization::Unknown
        })
    }
}

pub struct HostVideoPipeline {
    stream_id: u64,
    sent_config: Option<(u32, u16, u16)>,
}

impl HostVideoPipeline {
    pub fn new(stream_id: u64) -> Self {
        Self {
            stream_id,
            sent_config: None,
        }
    }

    pub fn prepare(
        &mut self,
        frame: EncodedFrame,
        width: u32,
        height: u32,
    ) -> Result<PrepareVideo, HostRuntimeError> {
        let width = u16::try_from(width).map_err(|_| HostRuntimeError::InvalidDimensions)?;
        let height = u16::try_from(height).map_err(|_| HostRuntimeError::InvalidDimensions)?;
        if width == 0 || height == 0 {
            return Err(HostRuntimeError::InvalidDimensions);
        }
        if self
            .sent_config
            .is_some_and(|(version, sent_width, sent_height)| {
                version == frame.config_version && (sent_width, sent_height) != (width, height)
            })
        {
            return Err(HostRuntimeError::InconsistentVideoConfig);
        }

        let needs_config = self
            .sent_config
            .is_none_or(|(version, _, _)| version != frame.config_version);
        if needs_config && (!frame.keyframe || frame.sequence_header.is_none()) {
            return Ok(PrepareVideo::NeedKeyframe);
        }
        let video_config = if needs_config {
            Some(encode_video_config(&VideoConfig {
                protocol_version: PROTOCOL_VERSION,
                stream_id: self.stream_id,
                config_version: frame.config_version,
                codec: desklink_protocol::Codec::H264,
                width,
                height,
                sequence_header: frame.sequence_header.clone().unwrap_or_default(),
            })?)
        } else {
            None
        };
        let mut flags = 0_u16;
        if frame.keyframe {
            flags |= FrameFlags::KEYFRAME.0;
        }
        if needs_config {
            flags |= FrameFlags::CONFIG.0;
        }
        let flags = FrameFlags(flags);
        let wire_frame = WireEncodedFrame {
            stream_id: self.stream_id,
            frame_id: frame.frame_id,
            config_version: frame.config_version,
            capture_timestamp_us: frame.timestamp_us,
            width,
            height,
            flags,
            data: frame.access_unit,
        };
        let datagrams = packetize_frame(&wire_frame)?
            .iter()
            .map(encode_video_packet)
            .collect::<Result<Vec<_>, _>>()?;
        if needs_config {
            self.sent_config = Some((frame.config_version, width, height));
        }
        Ok(PrepareVideo::Ready(PreparedVideo {
            video_config,
            datagrams,
        }))
    }
}

pub struct HostInboundPolicy {
    stream_id: u64,
    last_input_sequence: u64,
}

impl HostInboundPolicy {
    pub fn new(stream_id: u64) -> Self {
        Self {
            stream_id,
            last_input_sequence: 0,
        }
    }

    pub fn handle_control(&self, bytes: &[u8]) -> Result<bool, HostRuntimeError> {
        Ok(matches!(
            decode_control(bytes)?,
            ControlMessage::RequestKeyframe { stream_id } if stream_id == self.stream_id
        ))
    }

    pub fn decode_input(
        &mut self,
        bytes: &[u8],
        now_us: u64,
    ) -> Result<Option<InputEvent>, HostRuntimeError> {
        let envelope = decode_input(bytes, now_us)?;
        if !sequence_is_newer(envelope.sequence, self.last_input_sequence) {
            return Ok(None);
        }
        self.last_input_sequence = envelope.sequence;
        Ok(Some(envelope.event))
    }
}

pub fn next_frame_with_recovery<C: DesktopCapturer>(
    capture: &mut C,
    timeout: Duration,
) -> Result<CaptureOutcome, CaptureError> {
    match capture.next_frame(timeout) {
        Ok(frame) => Ok(CaptureOutcome::Frame(frame)),
        Err(CaptureError::Timeout) => Ok(CaptureOutcome::Idle),
        Err(CaptureError::AccessLost) => {
            capture.recover()?;
            Ok(CaptureOutcome::Recovered)
        }
        Err(error) => Err(error),
    }
}

pub fn normalize_cursor(desktop: DesktopRect, x: i32, y: i32) -> (i32, i32) {
    let normalize = |coordinate: i32, origin: i32, length: u32| {
        if length == 0 {
            return 0;
        }
        (i64::from(coordinate) - i64::from(origin))
            .saturating_mul(1_000_000)
            .saturating_div(i64::from(length))
            .clamp(0, 1_000_000) as i32
    };
    (
        normalize(x, desktop.left, desktop.width),
        normalize(y, desktop.top, desktop.height),
    )
}

fn sequence_is_newer(sequence: u64, previous: u64) -> bool {
    if sequence == 0 {
        return false;
    }
    if previous == 0 {
        return true;
    }
    let distance = sequence.wrapping_sub(previous);
    distance != 0 && distance < 1_u64 << 63
}

struct EncodedDesktopFrame {
    frame: EncodedFrame,
    width: u32,
    height: u32,
}

pub struct HostRuntime {
    client: Arc<QuicClient>,
    stream_id: u64,
    identity: DeviceIdentity,
    authorizer: Arc<dyn ControllerAuthorizer>,
}

impl HostRuntime {
    pub fn new(
        client: QuicClient,
        stream_id: u64,
        approval: ApprovalState,
        identity: DeviceIdentity,
        expected_controller: VerifyingKey,
    ) -> Result<Self, HostRuntimeError> {
        if stream_id == 0 {
            return Err(HostRuntimeError::InvalidControllerCapabilities);
        }
        if approval != ApprovalState::Accepted {
            return Err(HostRuntimeError::ApprovalRequired);
        }
        Self::with_authorizer(
            client,
            stream_id,
            identity,
            Arc::new(PinnedControllerAuthorizer {
                expected: expected_controller,
            }),
        )
    }

    pub fn with_authorizer(
        client: QuicClient,
        stream_id: u64,
        identity: DeviceIdentity,
        authorizer: Arc<dyn ControllerAuthorizer>,
    ) -> Result<Self, HostRuntimeError> {
        if stream_id == 0 {
            return Err(HostRuntimeError::InvalidControllerCapabilities);
        }
        Ok(Self {
            client: Arc::new(client),
            stream_id,
            identity,
            authorizer,
        })
    }

    pub async fn run(self) -> Result<(), HostRuntimeError> {
        let stable = AtomicBool::new(false);
        self.run_tracking_stability(&stable, |_| {}).await
    }

    async fn run_tracking_stability(
        self,
        stable: &AtomicBool,
        on_connected: impl FnOnce(u64),
    ) -> Result<(), HostRuntimeError> {
        let Self {
            client,
            stream_id,
            identity,
            authorizer,
        } = self;
        let secure = Arc::new(AsyncMutex::new(
            perform_noise_handshake(&client, identity, authorizer.as_ref()).await?,
        ));
        let force_keyframe = Arc::new(AtomicBool::new(true));
        let shutdown = Arc::new(AtomicBool::new(false));
        let video_queue = Arc::new(Mutex::new(LatestFrameQueue::new(VIDEO_QUEUE_CAPACITY)));
        let video_notify = Arc::new(Notify::new());
        let shutdown_guard = RuntimeShutdown {
            shutdown: shutdown.clone(),
            video_notify: video_notify.clone(),
        };
        let (ready_sender, ready_receiver) = oneshot::channel();
        let (worker_sender, worker_receiver) = oneshot::channel();
        let (start_sender, start_receiver) = std::sync::mpsc::sync_channel(0);
        let worker_shutdown = shutdown.clone();
        let worker_force = force_keyframe.clone();
        let worker_queue = video_queue.clone();
        let worker_notify = video_notify.clone();
        let capture_worker = std::thread::Builder::new()
            .name("desklink-capture".into())
            .spawn(move || {
                let result = capture_worker_entry(
                    ready_sender,
                    start_receiver,
                    worker_queue,
                    worker_notify,
                    worker_force,
                    worker_shutdown,
                );
                let _ = worker_sender.send(result);
            })
            .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?;
        let ready = match ready_receiver.await {
            Ok(Ok(ready)) => ready,
            Ok(Err(error)) => {
                let _ = capture_worker.join();
                return Err(error);
            }
            Err(_) => {
                let _ = capture_worker.join();
                return Err(HostRuntimeError::CaptureWorkerStopped);
            }
        };
        if let Err(error) = negotiate_controller(
            &client,
            &secure,
            ready.width,
            ready.height,
            force_keyframe.clone(),
            stream_id,
        )
        .await
        {
            drop(start_sender);
            shutdown.store(true, Ordering::Release);
            let _ = capture_worker.join();
            return Err(error);
        }
        start_sender
            .send(())
            .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?;
        let input = InputInjector::new(VirtualDesktop {
            rect: ready.desktop,
        });
        stable.store(true, Ordering::Release);
        on_connected(stream_id);

        let mut video = Box::pin(send_video_loop(
            client.clone(),
            secure.clone(),
            stream_id,
            video_queue,
            video_notify.clone(),
            force_keyframe.clone(),
        ));
        let mut inbound = Box::pin(receive_input_and_control(
            client.clone(),
            secure.clone(),
            stream_id,
            input,
            force_keyframe,
        ));
        let mut cursor = Box::pin(send_cursor_loop(client, secure, stream_id, ready.desktop));
        let mut worker_receiver = Box::pin(worker_receiver);

        let outcome = tokio::select! {
            result = &mut video => result,
            result = &mut inbound => result,
            result = &mut cursor => result,
            result = &mut worker_receiver => result.unwrap_or(Err(HostRuntimeError::CaptureWorkerStopped)),
        };
        shutdown.store(true, Ordering::Release);
        video_notify.notify_waiters();
        drop(video);
        drop(inbound);
        drop(cursor);
        drop(worker_receiver);
        if capture_worker.join().is_err() {
            return Err(HostRuntimeError::CaptureWorkerPanicked);
        }
        drop(shutdown_guard);
        outcome
    }
}

struct RuntimeShutdown {
    shutdown: Arc<AtomicBool>,
    video_notify: Arc<Notify>,
}

impl Drop for RuntimeShutdown {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.video_notify.notify_waiters();
    }
}

#[derive(Clone, Copy)]
struct CaptureWorkerReady {
    desktop: DesktopRect,
    width: u32,
    height: u32,
}

type SharedSecureSession = Arc<AsyncMutex<SecureSession>>;

async fn perform_noise_handshake(
    client: &QuicClient,
    identity: DeviceIdentity,
    authorizer: &dyn ControllerAuthorizer,
) -> Result<SecureSession, HostRuntimeError> {
    let handshake = async {
        let first = decode_noise_handshake(&client.next_control().await?)?;
        if first.step != NoiseHandshakeStep::InitiatorHello {
            return Err(HostRuntimeError::UnexpectedHandshakeStep);
        }
        let (mut responder, response) = match authorizer.pinned_verify_key() {
            Some(expected) => NoiseResponder::accept(&first.payload, identity, expected)?,
            None => NoiseResponder::accept_pairing(&first.payload, identity)?,
        };
        client
            .send_control(encode_noise_handshake(&NoiseHandshake {
                protocol_version: PROTOCOL_VERSION,
                step: NoiseHandshakeStep::ResponderHello,
                payload: response,
            })?)
            .await?;

        let finish = decode_noise_handshake(&client.next_control().await?)?;
        if finish.step != NoiseHandshakeStep::InitiatorFinish {
            return Err(HostRuntimeError::UnexpectedHandshakeStep);
        }
        responder.receive(&finish.payload)?;
        let secure = responder
            .finish()?
            .into_secure_session(SecureRole::Responder);
        match authorizer
            .authorize(secure.peer_identity())
            .map_err(HostRuntimeError::AuthorizationBackend)?
        {
            ControllerAuthorization::Authorized => Ok(secure),
            ControllerAuthorization::Unknown => Err(HostRuntimeError::UntrustedController),
            ControllerAuthorization::KeyChanged => Err(HostRuntimeError::ControllerKeyChanged),
            ControllerAuthorization::Rejected => Err(HostRuntimeError::PairingRejected),
            ControllerAuthorization::Expired => Err(HostRuntimeError::PairingExpired),
        }
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
        .await
        .map_err(|_| HostRuntimeError::HandshakeTimeout)?
}

async fn seal(
    secure: &SharedSecureSession,
    lane: SecureLane,
    plaintext: &[u8],
) -> Result<Vec<u8>, HostRuntimeError> {
    Ok(secure.lock().await.seal(lane, plaintext)?)
}

async fn open(
    secure: &SharedSecureSession,
    lane: SecureLane,
    ciphertext: &[u8],
) -> Result<Vec<u8>, HostRuntimeError> {
    Ok(secure.lock().await.open(lane, ciphertext)?)
}

fn capture_worker_entry(
    ready: oneshot::Sender<Result<CaptureWorkerReady, HostRuntimeError>>,
    start: std::sync::mpsc::Receiver<()>,
    queue: Arc<Mutex<LatestFrameQueue<EncodedDesktopFrame>>>,
    notify: Arc<Notify>,
    force_keyframe: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) -> Result<(), HostRuntimeError> {
    let capture = match DxgiDesktopCapturer::new_primary() {
        Ok(capture) => capture,
        Err(error) => {
            let _ = ready.send(Err(HostRuntimeError::Capture(error)));
            return Ok(());
        }
    };
    let (source_width, source_height) = capture.dimensions();
    let (width, height) = match fit_h264_dimensions(source_width, source_height) {
        Ok(dimensions) => dimensions,
        Err(error) => {
            let _ = ready.send(Err(HostRuntimeError::Encoder(error)));
            return Ok(());
        }
    };
    let encoder = match H264Encoder::new(width, height, 30) {
        Ok(encoder) => encoder,
        Err(error) => {
            let _ = ready.send(Err(HostRuntimeError::Encoder(error)));
            return Ok(());
        }
    };
    let worker_ready = CaptureWorkerReady {
        desktop: capture.desktop_rect(),
        width,
        height,
    };
    if ready.send(Ok(worker_ready)).is_err() || start.recv().is_err() {
        return Ok(());
    }
    capture_encode_loop(capture, encoder, queue, notify, force_keyframe, shutdown)
}

async fn negotiate_controller(
    client: &QuicClient,
    secure: &SharedSecureSession,
    width: u32,
    height: u32,
    force_keyframe: Arc<AtomicBool>,
    stream_id: u64,
) -> Result<(), HostRuntimeError> {
    let width = u16::try_from(width).map_err(|_| HostRuntimeError::InvalidDimensions)?;
    let height = u16::try_from(height).map_err(|_| HostRuntimeError::InvalidDimensions)?;
    let hello = encode_control(&ControlMessage::Hello {
        platform: Platform::Windows,
        role: DeviceRole::Host,
    })?;
    client
        .send_control(seal(secure, SecureLane::Control, &hello).await?)
        .await?;
    let capabilities = encode_control(&ControlMessage::Capabilities(DeviceCapabilities {
        platform: Platform::Windows,
        role: DeviceRole::Host,
        codecs: vec![Codec::H264],
        width,
        height,
    }))?;
    client
        .send_control(seal(secure, SecureLane::Control, &capabilities).await?)
        .await?;

    let negotiation = async {
        loop {
            let encrypted = client.next_control().await?;
            let plaintext = open(secure, SecureLane::Control, &encrypted).await?;
            match decode_control(&plaintext)? {
                ControlMessage::Capabilities(capabilities)
                    if capabilities.role == DeviceRole::Controller
                        && capabilities.codecs.contains(&Codec::H264) =>
                {
                    return Ok(());
                }
                ControlMessage::Capabilities(_) => {
                    return Err(HostRuntimeError::InvalidControllerCapabilities);
                }
                ControlMessage::RequestKeyframe {
                    stream_id: requested_stream,
                } if requested_stream == stream_id => {
                    force_keyframe.store(true, Ordering::Release);
                }
                ControlMessage::Hello { .. } | ControlMessage::RequestKeyframe { .. } => {}
            }
        }
    };
    tokio::time::timeout(NEGOTIATION_TIMEOUT, negotiation)
        .await
        .map_err(|_| HostRuntimeError::NegotiationTimeout)?
}

fn capture_encode_loop(
    mut capture: DxgiDesktopCapturer,
    mut encoder: H264Encoder,
    queue: Arc<Mutex<LatestFrameQueue<EncodedDesktopFrame>>>,
    notify: Arc<Notify>,
    force_keyframe: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) -> Result<(), HostRuntimeError> {
    while !shutdown.load(Ordering::Acquire) {
        let frame = match next_frame_with_recovery(&mut capture, CAPTURE_TIMEOUT)
            .map_err(HostRuntimeError::Capture)?
        {
            CaptureOutcome::Frame(frame) => frame,
            CaptureOutcome::Idle => continue,
            CaptureOutcome::Recovered => {
                force_keyframe.store(true, Ordering::Release);
                continue;
            }
        };
        let request_keyframe = force_keyframe.swap(false, Ordering::AcqRel);
        let encoded = match encoder.encode(frame, request_keyframe) {
            Ok(encoded) => encoded,
            Err(EncoderError::NeedMoreInput) => continue,
            Err(error) => return Err(HostRuntimeError::Encoder(error)),
        };
        let (width, height) = encoder.dimensions();
        lock_queue(&queue).push_latest(EncodedDesktopFrame {
            frame: encoded,
            width,
            height,
        });
        notify.notify_one();
    }
    Ok(())
}

async fn send_video_loop(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    stream_id: u64,
    queue: Arc<Mutex<LatestFrameQueue<EncodedDesktopFrame>>>,
    notify: Arc<Notify>,
    force_keyframe: Arc<AtomicBool>,
) -> Result<(), HostRuntimeError> {
    let mut pipeline = HostVideoPipeline::new(stream_id);
    loop {
        notify.notified().await;
        let next = lock_queue(&queue).drain_newest_first().into_iter().next();
        let Some(next) = next else {
            continue;
        };
        match pipeline.prepare(next.frame, next.width, next.height)? {
            PrepareVideo::NeedKeyframe => {
                force_keyframe.store(true, Ordering::Release);
            }
            PrepareVideo::Ready(prepared) => {
                if let Some(config) = prepared.video_config {
                    client
                        .send_video_config(seal(&secure, SecureLane::VideoConfig, &config).await?)
                        .await?;
                }
                for (index, datagram) in prepared.datagrams.into_iter().enumerate() {
                    client
                        .send_video_datagram(
                            seal(&secure, SecureLane::VideoDatagram, &datagram).await?,
                        )
                        .await?;
                    if index % 16 == 15 {
                        tokio::task::yield_now().await;
                    }
                }
            }
        }
    }
}

async fn receive_input_and_control(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    stream_id: u64,
    mut input: InputInjector,
    force_keyframe: Arc<AtomicBool>,
) -> Result<(), HostRuntimeError> {
    let mut policy = HostInboundPolicy::new(stream_id);
    loop {
        match client.next_event().await? {
            TransportEvent::Control(bytes) => {
                let plaintext = open(&secure, SecureLane::Control, &bytes).await?;
                if policy.handle_control(&plaintext)? {
                    force_keyframe.store(true, Ordering::Release);
                }
            }
            TransportEvent::Input(bytes) => {
                let plaintext = open(&secure, SecureLane::Input, &bytes).await?;
                if let Some(event) = policy.decode_input(&plaintext, now_micros())? {
                    input.apply(event).map_err(HostRuntimeError::Input)?;
                }
            }
            TransportEvent::Closed { reason } => {
                return Err(HostRuntimeError::TransportClosed(reason));
            }
            TransportEvent::VideoConfig(_)
            | TransportEvent::VideoDatagram(_)
            | TransportEvent::CursorDatagram(_) => {}
        }
    }
}

async fn send_cursor_loop(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    stream_id: u64,
    desktop: DesktopRect,
) -> Result<(), HostRuntimeError> {
    let mut interval = tokio::time::interval(CURSOR_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sequence = 0_u64;
    loop {
        interval.tick().await;
        sequence = sequence.wrapping_add(1).max(1);
        let Some(cursor) = sample_cursor(desktop, stream_id, sequence) else {
            continue;
        };
        let plaintext = encode_cursor_update(&cursor)?;
        client
            .send_cursor_datagram(seal(&secure, SecureLane::CursorDatagram, &plaintext).await?)
            .await?;
    }
}

#[cfg(windows)]
fn sample_cursor(desktop: DesktopRect, stream_id: u64, sequence: u64) -> Option<CursorUpdate> {
    use std::mem::size_of;
    use windows::Win32::UI::WindowsAndMessaging::{CURSOR_SHOWING, CURSORINFO, GetCursorInfo};

    let mut info = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..CURSORINFO::default()
    };
    unsafe { GetCursorInfo(&mut info) }.ok()?;
    let (x_millionths, y_millionths) =
        normalize_cursor(desktop, info.ptScreenPos.x, info.ptScreenPos.y);
    Some(CursorUpdate {
        protocol_version: PROTOCOL_VERSION,
        stream_id,
        sequence,
        timestamp_us: now_micros(),
        x_millionths,
        y_millionths,
        visible: info.flags.0 & CURSOR_SHOWING.0 != 0,
        shape_id: info.hCursor.0 as usize as u64,
    })
}

#[cfg(not(windows))]
fn sample_cursor(desktop: DesktopRect, stream_id: u64, sequence: u64) -> Option<CursorUpdate> {
    let (x_millionths, y_millionths) = normalize_cursor(desktop, 0, 0);
    Some(CursorUpdate {
        protocol_version: PROTOCOL_VERSION,
        stream_id,
        sequence,
        timestamp_us: now_micros(),
        x_millionths,
        y_millionths,
        visible: false,
        shape_id: 0,
    })
}

fn lock_queue<T>(queue: &Mutex<LatestFrameQueue<T>>) -> MutexGuard<'_, LatestFrameQueue<T>> {
    match queue.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration
                .as_secs()
                .saturating_mul(1_000_000)
                .saturating_add(u64::from(duration.subsec_micros()))
        })
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
