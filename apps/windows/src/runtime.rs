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
    AccessDenialReason, Codec, ControlMessage, CursorUpdate, DeviceCapabilities, DeviceRole,
    FrameFlags, InputEvent, MAX_INPUT_AGE_US, MAX_INPUT_FUTURE_SKEW_US, NoiseHandshake,
    NoiseHandshakeStep, PROTOCOL_VERSION, Platform, ProtocolError, RemoteDisplay, VideoConfig,
    decode_control, decode_noise_handshake, decode_session_input, encode_control,
    encode_cursor_update, encode_noise_handshake, encode_video_config, encode_video_packet,
};
use desklink_session::{DesktopRect, ReconnectDecision, ReconnectPolicy, ReconnectSchedule};
use desklink_transport::{
    JoinRejectCode, QuicClient, QuicClientConfig, RelayDirectoryRegistration, RelayJoin,
    TransportError,
};
use desklink_video::{EncodedFrame as WireEncodedFrame, LatestFrameQueue, packetize_frame};
use ed25519_dalek::VerifyingKey;
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, Notify, oneshot};
use zeroize::Zeroizing;

use crate::{
    capture::{
        CaptureError, CapturedFrame, DesktopCapturer, DxgiDesktopCapturer, available_displays,
        display_topology,
    },
    encoder::{EncodedFrame, EncoderError, H264Encoder, fit_h264_dimensions},
    input::{InputInjectionError, InputInjector, VirtualDesktop},
    window::ApprovalState,
};

const CAPTURE_TIMEOUT: Duration = Duration::from_millis(50);
const CURSOR_INTERVAL: Duration = Duration::from_millis(16);
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(15);
// The authenticated handshake includes the native local-approval dialog. Give
// the person at the host enough time to inspect and approve the controller.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(150);
const DENIAL_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);
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
    Available {
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
    directory_registration: Option<RelayDirectoryRegistration>,
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
            directory_registration: None,
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

    pub fn with_directory_registration(mut self, registration: RelayDirectoryRegistration) -> Self {
        self.directory_registration = Some(registration);
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
            let outcome = self.run_attempt(&mut stream_id, &stable).await;
            let error = match outcome {
                Ok(()) => return Ok(()),
                Err(error)
                    if !host_error_is_retryable_for_session(
                        &error,
                        self.expires_at_unix_s.is_none(),
                    ) =>
                {
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
                    stream_id = self.next_stream_id(stream_id)?;
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
                    if self.expires_at_unix_s.is_none() {
                        stream_id = self.next_stream_id(stream_id)?;
                        let delay = self.reconnect_policy.max_delay();
                        self.publish(HostLifecycleEvent::Reconnecting {
                            retry: retries,
                            maximum_retries: schedule.max_retries(),
                            delay,
                            reason,
                        });
                        tokio::time::sleep(delay).await;
                        schedule.reset();
                        continue;
                    }
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

    fn next_stream_id(&self, stream_id: u64) -> Result<u64, HostSupervisorError> {
        match stream_id.checked_add(1).filter(|stream_id| *stream_id != 0) {
            Some(stream_id) => Ok(stream_id),
            None => {
                self.publish(HostLifecycleEvent::Stopped {
                    reason: "video stream identifier space is exhausted".to_owned(),
                });
                Err(HostSupervisorError::StreamIdExhausted)
            }
        }
    }

    async fn run_attempt(
        &self,
        stream_id: &mut u64,
        stable: &AtomicBool,
    ) -> Result<(), HostRuntimeError> {
        let client = Arc::new(QuicClient::connect(self.transport.clone()).await?);
        let mut join = RelayJoin::host_with_participant(
            self.session_id,
            *self.relay_authentication,
            self.host_device_id,
        );
        if let Some(registration) = &self.directory_registration {
            join = join.with_directory_registration(registration.clone())?;
        }
        client.join(join).await?;
        self.publish(HostLifecycleEvent::Available {
            stream_id: *stream_id,
        });
        loop {
            let identity =
                DeviceIdentity::from_secret_key(self.host_device_id, &self.host_secret_key);
            let outcome = HostRuntime::with_shared_authorizer(
                client.clone(),
                *stream_id,
                identity,
                self.authorizer.clone(),
            )?
            .run_tracking_stability(stable, |stream_id| {
                self.publish(HostLifecycleEvent::Connected { stream_id });
            })
            .await;
            match outcome {
                Err(error)
                    if self.expires_at_unix_s.is_none()
                        && host_error_rearms_on_connected_relay(&error) =>
                {
                    client.reset_reliable_channels().await;
                    *stream_id = stream_id
                        .checked_add(1)
                        .filter(|stream_id| *stream_id != 0)
                        .ok_or(HostRuntimeError::InvalidControllerCapabilities)?;
                    self.publish(HostLifecycleEvent::Available {
                        stream_id: *stream_id,
                    });
                }
                outcome => return outcome,
            }
        }
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
        | HostRuntimeError::NegotiationTimeout
        | HostRuntimeError::UntrustedController
        | HostRuntimeError::ControllerKeyChanged
        // A malformed, incompatible, or interrupted controller must only end
        // that peer attempt. It must never be able to stop the host service.
        | HostRuntimeError::Protocol(_)
        | HostRuntimeError::Crypto(_)
        | HostRuntimeError::InvalidControllerCapabilities
        | HostRuntimeError::UnexpectedHandshakeStep
        // Capture, encoding, and input backends belong to the current interactive Windows
        // desktop. Display changes, driver resets, lock/unlock, and RDP transitions can make one
        // controller attempt fail without invalidating the host configuration. Rebuild the
        // complete runtime instead of permanently stopping the host service.
        | HostRuntimeError::Capture(_)
        | HostRuntimeError::InvalidDimensions
        | HostRuntimeError::InconsistentVideoConfig
        | HostRuntimeError::Encoder(_)
        | HostRuntimeError::Input(_)
        | HostRuntimeError::CaptureWorkerStopped
        | HostRuntimeError::CaptureWorkerPanicked => true,
        HostRuntimeError::ApprovalRequired
        | HostRuntimeError::PairingRejected
        | HostRuntimeError::PairingExpired
        | HostRuntimeError::AuthorizationBackend(_) => false,
    }
}

fn host_error_is_retryable_for_session(error: &HostRuntimeError, persistent_session: bool) -> bool {
    host_error_is_retryable(error)
        || (persistent_session
            && matches!(
                error,
                HostRuntimeError::ApprovalRequired
                    | HostRuntimeError::PairingRejected
                    | HostRuntimeError::PairingExpired
                    | HostRuntimeError::AuthorizationBackend(_)
            ))
}

fn transport_error_is_retryable(error: &TransportError) -> bool {
    match error {
        TransportError::Connection(_)
        | TransportError::ConnectionLimit
        | TransportError::Stream(_)
        | TransportError::Datagram(_)
        | TransportError::Closed
        | TransportError::PeerDisconnected
        | TransportError::PeerReplaced
        | TransportError::Malformed => true,
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
        | TransportError::InvalidConfig(_) => false,
    }
}

fn host_error_rearms_on_connected_relay(error: &HostRuntimeError) -> bool {
    !matches!(
        error,
        HostRuntimeError::TransportClosed(_)
            | HostRuntimeError::Transport(
                TransportError::Connection(_)
                    | TransportError::ConnectionLimit
                    | TransportError::Closed
                    | TransportError::JoinRejected(_)
                    | TransportError::NotJoined
                    | TransportError::AlreadyJoined
                    | TransportError::InvalidConfig(_)
            )
    )
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
    input_clock_anchor: Option<InputClockAnchor>,
}

#[derive(Clone, Copy)]
struct InputClockAnchor {
    remote_us: u64,
    local_us: u64,
}

impl HostInboundPolicy {
    pub fn new(stream_id: u64) -> Self {
        Self {
            stream_id,
            last_input_sequence: 0,
            input_clock_anchor: None,
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
        let envelope = decode_session_input(bytes)?;
        if !sequence_is_newer(envelope.sequence, self.last_input_sequence) {
            return Ok(None);
        }
        if !self.input_timestamp_is_fresh(envelope.timestamp_us, now_us) {
            // A Windows time synchronization, resume from sleep, or manual clock
            // correction must not tear down the authenticated remote session.
            // Re-anchor on the newest sequence; sequence ordering still rejects
            // every replay within this secure session.
            self.input_clock_anchor = Some(InputClockAnchor {
                remote_us: envelope.timestamp_us,
                local_us: now_us,
            });
        }
        self.last_input_sequence = envelope.sequence;
        Ok(Some(envelope.event))
    }

    fn input_timestamp_is_fresh(&mut self, remote_us: u64, local_us: u64) -> bool {
        let Some(anchor) = self.input_clock_anchor else {
            self.input_clock_anchor = Some(InputClockAnchor {
                remote_us,
                local_us,
            });
            return true;
        };
        let mapped_local_us = if remote_us >= anchor.remote_us {
            anchor
                .local_us
                .saturating_add(remote_us.saturating_sub(anchor.remote_us))
        } else {
            anchor
                .local_us
                .saturating_sub(anchor.remote_us.saturating_sub(remote_us))
        };
        mapped_local_us >= local_us.saturating_sub(MAX_INPUT_AGE_US)
            && mapped_local_us <= local_us.saturating_add(MAX_INPUT_FUTURE_SKEW_US)
    }
}

pub fn next_frame_with_recovery<C: DesktopCapturer>(
    capture: &mut C,
    timeout: Duration,
) -> Result<CaptureOutcome, CaptureError> {
    match capture.next_frame(timeout) {
        Ok(frame) => Ok(CaptureOutcome::Frame(frame)),
        Err(CaptureError::Timeout) => Ok(CaptureOutcome::Idle),
        Err(CaptureError::AccessLost | CaptureError::Native(_)) => {
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
        Self::with_shared_authorizer(Arc::new(client), stream_id, identity, authorizer)
    }

    pub fn with_shared_authorizer(
        client: Arc<QuicClient>,
        stream_id: u64,
        identity: DeviceIdentity,
        authorizer: Arc<dyn ControllerAuthorizer>,
    ) -> Result<Self, HostRuntimeError> {
        if stream_id == 0 {
            return Err(HostRuntimeError::InvalidControllerCapabilities);
        }
        Ok(Self {
            client,
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
        let (secure, peer_generation) =
            perform_noise_handshake(&client, identity, authorizer.as_ref()).await?;
        let secure = Arc::new(AsyncMutex::new(secure));
        let force_keyframe = Arc::new(AtomicBool::new(true));
        // The controller sends its encrypted capabilities immediately after
        // finishing Noise. Consume and authenticate them before creating the
        // capture stack or sending anything back. This proves the approved
        // controller attempt is still the active peer and prevents late
        // approval from writing old ciphertext into a replacement connection.
        receive_controller_capabilities(
            &client,
            &secure,
            peer_generation,
            force_keyframe.clone(),
            stream_id,
        )
        .await?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let video_queue = Arc::new(Mutex::new(LatestFrameQueue::new(VIDEO_QUEUE_CAPACITY)));
        let video_notify = Arc::new(Notify::new());
        let (ready_sender, ready_receiver) = oneshot::channel();
        let (worker_sender, worker_receiver) = oneshot::channel();
        let (start_sender, start_receiver) = std::sync::mpsc::sync_channel(0);
        let (capture_command_sender, capture_command_receiver) = std::sync::mpsc::channel();
        let selected_desktop = Arc::new(Mutex::new(SelectedDesktop {
            display_id: 0,
            desktop: VirtualDesktop::single(DesktopRect::new(0, 0, 1, 1)),
        }));
        let capture_context = CaptureWorkerContext {
            commands: capture_command_receiver,
            queue: video_queue.clone(),
            notify: video_notify.clone(),
            force_keyframe: force_keyframe.clone(),
            shutdown: shutdown.clone(),
            selected_desktop: selected_desktop.clone(),
        };
        let capture_thread = std::thread::Builder::new()
            .name("desklink-capture".into())
            .spawn(move || {
                let result = capture_worker_entry(ready_sender, start_receiver, capture_context);
                let _ = worker_sender.send(result);
            })
            .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?;
        let mut capture_worker = CaptureWorkerGuard::new(
            start_sender,
            capture_thread,
            shutdown.clone(),
            video_notify.clone(),
        );
        let ready = match ready_receiver.await {
            Ok(Ok(ready)) => ready,
            Ok(Err(error)) => {
                let _ = send_host_unavailable(&client, &secure, peer_generation).await;
                let _ = capture_worker.shutdown_and_join();
                return Err(error);
            }
            Err(_) => {
                let _ = send_host_unavailable(&client, &secure, peer_generation).await;
                let _ = capture_worker.shutdown_and_join();
                return Err(HostRuntimeError::CaptureWorkerStopped);
            }
        };
        if let Err(error) = send_host_capabilities(
            &client,
            &secure,
            peer_generation,
            ready.width,
            ready.height,
            &ready.displays,
            ready.active_display_id,
        )
        .await
        {
            let _ = capture_worker.shutdown_and_join();
            return Err(error);
        }
        capture_worker.start()?;
        let input = InputInjector::new(VirtualDesktop::new(ready.desktop, ready.coordinate_space));
        stable.store(true, Ordering::Release);
        on_connected(stream_id);

        let mut video = Box::pin(send_video_loop(
            client.clone(),
            secure.clone(),
            peer_generation,
            stream_id,
            video_queue,
            video_notify.clone(),
            force_keyframe.clone(),
        ));
        let mut inbound = Box::pin(receive_input_and_control(
            client.clone(),
            secure.clone(),
            peer_generation,
            stream_id,
            input,
            force_keyframe,
            HostInboundContext {
                capture_commands: capture_command_sender,
                selected_desktop: selected_desktop.clone(),
                displays: Arc::new(ready.displays),
            },
        ));
        let mut cursor = Box::pin(send_cursor_loop(
            client.clone(),
            secure.clone(),
            peer_generation,
            stream_id,
            selected_desktop,
        ));
        let mut worker_receiver = Box::pin(worker_receiver);

        let outcome = tokio::select! {
            result = &mut video => result,
            result = &mut inbound => result,
            result = &mut cursor => result,
            result = &mut worker_receiver => result.unwrap_or(Err(HostRuntimeError::CaptureWorkerStopped)),
        };
        drop(video);
        drop(inbound);
        drop(cursor);
        drop(worker_receiver);
        let outcome = match capture_worker.shutdown_and_join() {
            Ok(()) => outcome,
            Err(error) => Err(error),
        };
        if let Err(error) = &outcome
            && let Some(reason) = host_runtime_denial_reason(error)
        {
            // This message uses the already authenticated end-to-end encrypted control lane.
            // The controller can stop retrying and show an actionable local-backend failure
            // instead of misreporting the intentional host rearm as a network disconnect.
            let _ = send_runtime_failure(&client, &secure, peer_generation, reason).await;
        }
        outcome
    }
}

struct CaptureWorkerGuard {
    start: Option<std::sync::mpsc::SyncSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    video_notify: Arc<Notify>,
}

impl CaptureWorkerGuard {
    fn new(
        start: std::sync::mpsc::SyncSender<()>,
        thread: std::thread::JoinHandle<()>,
        shutdown: Arc<AtomicBool>,
        video_notify: Arc<Notify>,
    ) -> Self {
        Self {
            start: Some(start),
            thread: Some(thread),
            shutdown,
            video_notify,
        }
    }

    fn start(&mut self) -> Result<(), HostRuntimeError> {
        self.start
            .take()
            .ok_or(HostRuntimeError::CaptureWorkerStopped)?
            .send(())
            .map_err(|_| HostRuntimeError::CaptureWorkerStopped)
    }

    fn signal_shutdown(&mut self) {
        // Dropping the start sender also releases a worker that is still waiting
        // between capture initialization and capability negotiation.
        self.start.take();
        self.shutdown.store(true, Ordering::Release);
        self.video_notify.notify_waiters();
    }

    fn shutdown_and_join(mut self) -> Result<(), HostRuntimeError> {
        self.signal_shutdown();
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread
            .join()
            .map_err(|_| HostRuntimeError::CaptureWorkerPanicked)
    }
}

impl Drop for CaptureWorkerGuard {
    fn drop(&mut self) {
        self.signal_shutdown();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
struct CaptureWorkerReady {
    desktop: DesktopRect,
    coordinate_space: DesktopRect,
    width: u32,
    height: u32,
    displays: Vec<RemoteDisplay>,
    active_display_id: u32,
}

#[derive(Clone, Copy)]
struct SelectedDesktop {
    display_id: u32,
    desktop: VirtualDesktop,
}

enum CaptureWorkerCommand {
    SelectDisplay {
        display_id: u32,
        reply: oneshot::Sender<bool>,
    },
}

struct CaptureWorkerContext {
    commands: std::sync::mpsc::Receiver<CaptureWorkerCommand>,
    queue: Arc<Mutex<LatestFrameQueue<EncodedDesktopFrame>>>,
    notify: Arc<Notify>,
    force_keyframe: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    selected_desktop: Arc<Mutex<SelectedDesktop>>,
}

struct HostInboundContext {
    capture_commands: std::sync::mpsc::Sender<CaptureWorkerCommand>,
    selected_desktop: Arc<Mutex<SelectedDesktop>>,
    displays: Arc<Vec<RemoteDisplay>>,
}

type SharedSecureSession = Arc<AsyncMutex<SecureSession>>;

async fn perform_noise_handshake(
    client: &QuicClient,
    identity: DeviceIdentity,
    authorizer: &dyn ControllerAuthorizer,
) -> Result<(SecureSession, u64), HostRuntimeError> {
    // An idle host is already online at the relay. Waiting for the first controller message is
    // therefore not a failed handshake and must not churn the relay session or the UI state.
    let (peer_generation, first) = next_initiator_hello(client).await?;
    let handshake = async {
        let (mut responder, response) = match authorizer.pinned_verify_key() {
            Some(expected) => NoiseResponder::accept(&first.payload, identity, expected)?,
            None => NoiseResponder::accept_pairing(&first.payload, identity)?,
        };
        client
            .send_control_for_generation(
                peer_generation,
                encode_noise_handshake(&NoiseHandshake {
                    protocol_version: PROTOCOL_VERSION,
                    step: NoiseHandshakeStep::ResponderHello,
                    payload: response,
                })?,
            )
            .await?;

        let finish =
            decode_noise_handshake(&client.next_control_for_generation(peer_generation).await?)?;
        if finish.step != NoiseHandshakeStep::InitiatorFinish {
            return Err(HostRuntimeError::UnexpectedHandshakeStep);
        }
        responder.receive(&finish.payload)?;
        let mut secure = responder
            .finish()?
            .into_secure_session(SecureRole::Responder);
        match authorizer
            .authorize(secure.peer_identity())
            .map_err(HostRuntimeError::AuthorizationBackend)?
        {
            ControllerAuthorization::Authorized => Ok((secure, peer_generation)),
            ControllerAuthorization::Unknown => {
                send_access_denied(
                    client,
                    peer_generation,
                    &mut secure,
                    AccessDenialReason::ControllerNotTrusted,
                )
                .await?;
                Err(HostRuntimeError::UntrustedController)
            }
            ControllerAuthorization::KeyChanged => {
                send_access_denied(
                    client,
                    peer_generation,
                    &mut secure,
                    AccessDenialReason::ControllerIdentityChanged,
                )
                .await?;
                Err(HostRuntimeError::ControllerKeyChanged)
            }
            ControllerAuthorization::Rejected => {
                send_access_denied(
                    client,
                    peer_generation,
                    &mut secure,
                    AccessDenialReason::ApprovalRejected,
                )
                .await?;
                Err(HostRuntimeError::PairingRejected)
            }
            ControllerAuthorization::Expired => {
                send_access_denied(
                    client,
                    peer_generation,
                    &mut secure,
                    AccessDenialReason::ApprovalExpired,
                )
                .await?;
                Err(HostRuntimeError::PairingExpired)
            }
        }
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
        .await
        .map_err(|_| HostRuntimeError::HandshakeTimeout)?
}

async fn next_initiator_hello(
    client: &QuicClient,
) -> Result<(u64, NoiseHandshake), HostRuntimeError> {
    const MAX_STALE_MESSAGES: usize = 32;
    let mut stale_messages = 0;
    loop {
        match client.next_control_with_generation().await {
            Ok((peer_generation, payload)) => match decode_noise_handshake(&payload) {
                Ok(handshake) if handshake.step == NoiseHandshakeStep::InitiatorHello => {
                    return Ok((peer_generation, handshake));
                }
                Ok(_) | Err(_) => {
                    // Messages queued by an already disconnected controller
                    // are not allowed to take the durable host offline. Drain
                    // a bounded stale generation and then rearm the peer lanes.
                    stale_messages += 1;
                    if stale_messages >= MAX_STALE_MESSAGES {
                        client.reset_reliable_channels().await;
                        return Err(HostRuntimeError::UnexpectedHandshakeStep);
                    }
                }
            },
            Err(TransportError::PeerDisconnected) => {
                client.reset_reliable_channels().await;
                stale_messages = 0;
            }
            Err(error) => return Err(HostRuntimeError::Transport(error)),
        }
    }
}

async fn send_access_denied(
    client: &QuicClient,
    peer_generation: u64,
    secure: &mut SecureSession,
    reason: AccessDenialReason,
) -> Result<(), HostRuntimeError> {
    let plaintext = encode_control(&ControlMessage::AccessDenied { reason })?;
    let ciphertext = secure.seal(SecureLane::Control, &plaintext)?;
    client
        .send_control_for_generation(peer_generation, ciphertext)
        .await?;
    let disconnected = async {
        loop {
            match client.next_control_for_generation(peer_generation).await {
                Ok(_) => {}
                Err(TransportError::PeerDisconnected | TransportError::PeerReplaced) => {
                    return Ok(());
                }
                Err(error) => return Err(HostRuntimeError::Transport(error)),
            }
        }
    };
    tokio::time::timeout(DENIAL_DISCONNECT_TIMEOUT, disconnected)
        .await
        .map_err(|_| {
            HostRuntimeError::Transport(TransportError::Connection(
                "controller did not close after access denial".to_owned(),
            ))
        })?
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
    context: CaptureWorkerContext,
) -> Result<(), HostRuntimeError> {
    let display_descriptors = match available_displays() {
        Ok(displays) => displays,
        Err(error) => {
            let _ = ready.send(Err(HostRuntimeError::Capture(error)));
            return Ok(());
        }
    };
    let displays = match display_descriptors
        .iter()
        .map(|display| {
            Ok(RemoteDisplay {
                id: display.id,
                width: u16::try_from(display.rect.width)
                    .map_err(|_| HostRuntimeError::InvalidDimensions)?,
                height: u16::try_from(display.rect.height)
                    .map_err(|_| HostRuntimeError::InvalidDimensions)?,
                primary: display.primary,
            })
        })
        .collect::<Result<Vec<_>, HostRuntimeError>>()
    {
        Ok(displays) => displays,
        Err(error) => {
            let _ = ready.send(Err(error));
            return Ok(());
        }
    };
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
    let coordinate_space = match display_topology() {
        Ok(topology) => topology.virtual_desktop,
        Err(error) => {
            let _ = ready.send(Err(HostRuntimeError::Capture(error)));
            return Ok(());
        }
    };
    let active_display_id = capture.display_id();
    *lock_unpoisoned(&context.selected_desktop) = SelectedDesktop {
        display_id: active_display_id,
        desktop: VirtualDesktop::new(capture.desktop_rect(), coordinate_space),
    };
    let worker_ready = CaptureWorkerReady {
        desktop: capture.desktop_rect(),
        coordinate_space,
        width,
        height,
        displays,
        active_display_id,
    };
    if ready.send(Ok(worker_ready)).is_err() || start.recv().is_err() {
        return Ok(());
    }
    capture_encode_loop(capture, encoder, context, coordinate_space)
}

async fn receive_controller_capabilities(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    force_keyframe: Arc<AtomicBool>,
    stream_id: u64,
) -> Result<(), HostRuntimeError> {
    let negotiation = async {
        let mut received_controller_hello = false;
        loop {
            let encrypted = client.next_control_for_generation(peer_generation).await?;
            let plaintext = open(secure, SecureLane::Control, &encrypted).await?;
            match decode_control(&plaintext)? {
                ControlMessage::Hello {
                    role: DeviceRole::Controller,
                    ..
                } => received_controller_hello = true,
                ControlMessage::Capabilities(capabilities)
                    if received_controller_hello
                        && capabilities.role == DeviceRole::Controller
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
                ControlMessage::AccessDenied { .. } => {
                    return Err(HostRuntimeError::InvalidControllerCapabilities);
                }
                ControlMessage::Hello { .. }
                | ControlMessage::RequestKeyframe { .. }
                | ControlMessage::DisplayList { .. }
                | ControlMessage::SelectDisplay { .. } => {}
            }
        }
    };
    tokio::time::timeout(NEGOTIATION_TIMEOUT, negotiation)
        .await
        .map_err(|_| HostRuntimeError::NegotiationTimeout)?
}

async fn send_host_capabilities(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    width: u32,
    height: u32,
    displays: &[RemoteDisplay],
    active_display_id: u32,
) -> Result<(), HostRuntimeError> {
    let width = u16::try_from(width).map_err(|_| HostRuntimeError::InvalidDimensions)?;
    let height = u16::try_from(height).map_err(|_| HostRuntimeError::InvalidDimensions)?;
    let hello = encode_control(&ControlMessage::Hello {
        platform: Platform::Windows,
        role: DeviceRole::Host,
    })?;
    client
        .send_control_for_generation(
            peer_generation,
            seal(secure, SecureLane::Control, &hello).await?,
        )
        .await?;
    let capabilities = encode_control(&ControlMessage::Capabilities(DeviceCapabilities {
        platform: Platform::Windows,
        role: DeviceRole::Host,
        codecs: vec![Codec::H264],
        width,
        height,
    }))?;
    client
        .send_control_for_generation(
            peer_generation,
            seal(secure, SecureLane::Control, &capabilities).await?,
        )
        .await?;
    send_display_list(client, secure, peer_generation, displays, active_display_id).await?;
    Ok(())
}

async fn send_display_list(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    displays: &[RemoteDisplay],
    active_display_id: u32,
) -> Result<(), HostRuntimeError> {
    let message = encode_control(&ControlMessage::DisplayList {
        displays: displays.to_vec(),
        active_display_id,
    })?;
    client
        .send_control_for_generation(
            peer_generation,
            seal(secure, SecureLane::Control, &message).await?,
        )
        .await?;
    Ok(())
}

async fn send_host_unavailable(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
) -> Result<(), HostRuntimeError> {
    let mut secure = secure.lock().await;
    send_access_denied(
        client,
        peer_generation,
        &mut secure,
        AccessDenialReason::HostUnavailable,
    )
    .await
}

fn host_runtime_denial_reason(error: &HostRuntimeError) -> Option<AccessDenialReason> {
    match error {
        HostRuntimeError::Capture(_)
        | HostRuntimeError::CaptureWorkerStopped
        | HostRuntimeError::CaptureWorkerPanicked => Some(AccessDenialReason::HostCaptureFailed),
        HostRuntimeError::Encoder(_)
        | HostRuntimeError::InvalidDimensions
        | HostRuntimeError::InconsistentVideoConfig => Some(AccessDenialReason::HostEncoderFailed),
        HostRuntimeError::Input(_) => Some(AccessDenialReason::HostInputFailed),
        _ => None,
    }
}

async fn send_runtime_failure(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    reason: AccessDenialReason,
) -> Result<(), HostRuntimeError> {
    let plaintext = encode_control(&ControlMessage::AccessDenied { reason })?;
    let ciphertext = seal(secure, SecureLane::Control, &plaintext).await?;
    client
        .send_control_for_generation(peer_generation, ciphertext)
        .await?;
    Ok(())
}

const MAX_ENCODER_RECOVERY_ATTEMPTS: u8 = 2;

fn encoder_error_is_recoverable(error: &EncoderError) -> bool {
    matches!(
        error,
        EncoderError::BackendUnavailable | EncoderError::Native(_)
    )
}

fn capture_encode_loop(
    mut capture: DxgiDesktopCapturer,
    mut encoder: H264Encoder,
    context: CaptureWorkerContext,
    coordinate_space: DesktopRect,
) -> Result<(), HostRuntimeError> {
    let CaptureWorkerContext {
        commands,
        queue,
        notify,
        force_keyframe,
        shutdown,
        selected_desktop,
    } = context;
    let mut encoder_recovery_attempts = 0_u8;
    while !shutdown.load(Ordering::Acquire) {
        while let Ok(command) = commands.try_recv() {
            match command {
                CaptureWorkerCommand::SelectDisplay { display_id, reply } => {
                    let current_display_id = lock_unpoisoned(&selected_desktop).display_id;
                    if display_id == current_display_id {
                        let _ = reply.send(true);
                        continue;
                    }
                    let switched = (|| {
                        let next_capture = DxgiDesktopCapturer::new_display(display_id)
                            .map_err(HostRuntimeError::Capture)?;
                        let (source_width, source_height) = next_capture.dimensions();
                        let (width, height) = fit_h264_dimensions(source_width, source_height)
                            .map_err(HostRuntimeError::Encoder)?;
                        encoder
                            .rebuild(width, height)
                            .map_err(HostRuntimeError::Encoder)?;
                        capture = next_capture;
                        lock_queue(&queue).drain_newest_first();
                        *lock_unpoisoned(&selected_desktop) = SelectedDesktop {
                            display_id,
                            desktop: VirtualDesktop::new(capture.desktop_rect(), coordinate_space),
                        };
                        force_keyframe.store(true, Ordering::Release);
                        notify.notify_one();
                        Ok::<(), HostRuntimeError>(())
                    })()
                    .is_ok();
                    let _ = reply.send(switched);
                }
            }
        }
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
            Ok(encoded) => {
                encoder_recovery_attempts = 0;
                encoded
            }
            Err(EncoderError::NeedMoreInput) => continue,
            Err(error)
                if encoder_error_is_recoverable(&error)
                    && encoder_recovery_attempts < MAX_ENCODER_RECOVERY_ATTEMPTS =>
            {
                encoder_recovery_attempts += 1;
                let (width, height) = encoder.dimensions();
                encoder
                    .rebuild(width, height)
                    .map_err(HostRuntimeError::Encoder)?;
                force_keyframe.store(true, Ordering::Release);
                continue;
            }
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
    peer_generation: u64,
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
                        .send_video_config_for_generation(
                            peer_generation,
                            seal(&secure, SecureLane::VideoConfig, &config).await?,
                        )
                        .await?;
                }
                for (index, datagram) in prepared.datagrams.into_iter().enumerate() {
                    client
                        .send_video_datagram_for_generation(
                            peer_generation,
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
    peer_generation: u64,
    stream_id: u64,
    mut input: InputInjector,
    force_keyframe: Arc<AtomicBool>,
    context: HostInboundContext,
) -> Result<(), HostRuntimeError> {
    let HostInboundContext {
        capture_commands,
        selected_desktop,
        displays,
    } = context;
    let mut policy = HostInboundPolicy::new(stream_id);
    loop {
        tokio::select! {
            result = client.next_control_for_generation(peer_generation) => {
                let bytes = result?;
                let plaintext = open(&secure, SecureLane::Control, &bytes).await?;
                if policy.handle_control(&plaintext)? {
                    force_keyframe.store(true, Ordering::Release);
                }
                if let ControlMessage::SelectDisplay { display_id } = decode_control(&plaintext)? {
                    let known_display = displays.iter().any(|display| display.id == display_id);
                    if known_display {
                        // A transient Windows input rejection while switching
                        // monitors must not close the video and control lanes.
                        // Retained pressed-state entries are retried by later
                        // release events and once more when the injector drops.
                        let _ = input.release_all();
                        let (reply, switched) = oneshot::channel();
                        capture_commands
                            .send(CaptureWorkerCommand::SelectDisplay { display_id, reply })
                            .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?;
                        if tokio::time::timeout(Duration::from_secs(5), switched)
                            .await
                            .ok()
                            .and_then(Result::ok)
                            .unwrap_or(false)
                        {
                            input.set_desktop(lock_unpoisoned(&selected_desktop).desktop);
                        }
                    }
                    let active_display_id = lock_unpoisoned(&selected_desktop).display_id;
                    send_display_list(
                        &client,
                        &secure,
                        peer_generation,
                        &displays,
                        active_display_id,
                    )
                    .await?;
                }
            }
            result = client.next_input_for_generation(peer_generation) => {
                let bytes = result?;
                let plaintext = open(&secure, SecureLane::Input, &bytes).await?;
                if let Some(event) = policy.decode_input(&plaintext, now_micros())? {
                    match input.apply(event) {
                        Ok(()) => {}
                        // An individual Windows input injection can be rejected
                        // temporarily (secure desktop, focus transition, UIPI),
                        // and an unsupported key belongs only to that event. Do
                        // not convert either case into a transport-wide failure.
                        Err(InputInjectionError::Blocked | InputInjectionError::UnsupportedKey) => {}
                        Err(error @ InputInjectionError::InvalidInput) => {
                            return Err(HostRuntimeError::Input(error));
                        }
                    }
                }
            }
            reason = client.next_closed_reason() => {
                return Err(HostRuntimeError::TransportClosed(reason));
            }
        }
    }
}

async fn send_cursor_loop(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    peer_generation: u64,
    stream_id: u64,
    selected_desktop: Arc<Mutex<SelectedDesktop>>,
) -> Result<(), HostRuntimeError> {
    let mut interval = tokio::time::interval(CURSOR_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sequence = 0_u64;
    loop {
        interval.tick().await;
        sequence = sequence.wrapping_add(1).max(1);
        let desktop = lock_unpoisoned(&selected_desktop).desktop.rect;
        let Some(cursor) = sample_cursor(desktop, stream_id, sequence) else {
            continue;
        };
        let plaintext = encode_cursor_update(&cursor)?;
        client
            .send_cursor_datagram_for_generation(
                peer_generation,
                seal(&secure, SecureLane::CursorDatagram, &plaintext).await?,
            )
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
    lock_unpoisoned(queue)
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
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

#[cfg(test)]
mod retry_policy_tests {
    use crate::{capture::CaptureError, encoder::EncoderError};

    use desklink_protocol::AccessDenialReason;
    use desklink_transport::TransportError;

    use super::{
        HostRuntimeError, encoder_error_is_recoverable, host_error_is_retryable,
        host_error_is_retryable_for_session, host_error_rearms_on_connected_relay,
        host_runtime_denial_reason,
    };

    #[test]
    fn persistent_host_recovers_after_rejecting_a_stale_pairing_request() {
        for error in [
            HostRuntimeError::ApprovalRequired,
            HostRuntimeError::PairingRejected,
            HostRuntimeError::PairingExpired,
            HostRuntimeError::AuthorizationBackend("temporary trust-store failure".into()),
        ] {
            assert!(host_error_is_retryable_for_session(&error, true));
            assert!(!host_error_is_retryable_for_session(&error, false));
        }
    }

    #[test]
    fn interactive_desktop_failures_rearm_the_host_service() {
        for error in [
            HostRuntimeError::Capture(CaptureError::AccessLost),
            HostRuntimeError::Encoder(EncoderError::BackendUnavailable),
            HostRuntimeError::CaptureWorkerStopped,
            HostRuntimeError::CaptureWorkerPanicked,
        ] {
            assert!(
                host_error_is_retryable(&error),
                "runtime desktop failure must not stop persistent hosting: {error}"
            );
        }
    }

    #[test]
    fn peer_attempt_failures_never_reconnect_the_durable_host_transport() {
        for error in [
            HostRuntimeError::HandshakeTimeout,
            HostRuntimeError::NegotiationTimeout,
            HostRuntimeError::UnexpectedHandshakeStep,
            HostRuntimeError::Capture(CaptureError::AccessLost),
            HostRuntimeError::Encoder(EncoderError::BackendUnavailable),
            HostRuntimeError::Transport(TransportError::PeerDisconnected),
            HostRuntimeError::Transport(TransportError::Stream("peer stream reset".into())),
        ] {
            assert!(
                host_error_rearms_on_connected_relay(&error),
                "peer attempt must not take the host offline: {error}"
            );
        }

        for error in [
            HostRuntimeError::TransportClosed("relay connection closed".into()),
            HostRuntimeError::Transport(TransportError::Closed),
            HostRuntimeError::Transport(TransportError::Connection("relay unavailable".into())),
        ] {
            assert!(
                !host_error_rearms_on_connected_relay(&error),
                "a dead relay transport must be rebuilt: {error}"
            );
        }
    }

    #[test]
    fn local_backend_failures_are_reported_without_masking_network_failures() {
        assert_eq!(
            host_runtime_denial_reason(&HostRuntimeError::Capture(CaptureError::AccessLost)),
            Some(AccessDenialReason::HostCaptureFailed)
        );
        assert_eq!(
            host_runtime_denial_reason(&HostRuntimeError::Encoder(EncoderError::Native(
                "MFT failed".into()
            ))),
            Some(AccessDenialReason::HostEncoderFailed)
        );
        assert_eq!(
            host_runtime_denial_reason(&HostRuntimeError::Transport(
                TransportError::PeerDisconnected
            )),
            None
        );
    }

    #[test]
    fn only_transient_encoder_backend_errors_trigger_a_limited_rebuild() {
        assert!(encoder_error_is_recoverable(
            &EncoderError::BackendUnavailable
        ));
        assert!(encoder_error_is_recoverable(&EncoderError::Native(
            "device reset".into()
        )));
        assert!(!encoder_error_is_recoverable(&EncoderError::InvalidFrame));
        assert!(!encoder_error_is_recoverable(&EncoderError::FrameTooLarge));
    }
}
