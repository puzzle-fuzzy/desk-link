mod cursor_send;
mod media_diagnostics;
pub use cursor_send::CursorDisableReason;
pub use media_diagnostics::{
    DirectListenerStopReason, HostMediaDiagnostic, HostMediaDiagnosticsObserver,
    rejected_host_media_diagnostics,
};
mod adaptive_bitrate;
#[cfg(test)]
mod direct_arbitration_tests;
mod direct_path_slot;
use adaptive_bitrate::AdaptiveBitrate;
mod configuration_queue;
use configuration_queue::{
    ConfigurationAction, ConfigurationKind, ConfigurationOperation, ConfigurationQueue,
};

use std::{
    collections::VecDeque,
    num::{NonZeroU32, NonZeroU64},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use desklink_crypto::{
    CryptoError, DeviceIdentity, NoiseResponder, PeerIdentity, SecureLane, SecureRole,
    SecureSession,
};
use desklink_protocol::{
    AccessDenialReason, AudioCodec, AudioPacket, Codec, ControlMessage, CursorUpdate,
    DeviceCapabilities, DeviceRole, FrameFlags, H264Profile, InputContext, InputEnvelope,
    InputEvent, MAX_INPUT_AGE_US, MAX_INPUT_FUTURE_SKEW_US, NoiseHandshake, NoiseHandshakeStep,
    PROTOCOL_VERSION, Platform, ProtocolError, RemoteDisplay, TransferId, TransferMessage,
    TransferResult, VideoConfig, VideoQualityPreference, VideoQualityPreset,
    VideoReliableBatchBuilder, decode_control, decode_noise_handshake, decode_session_input,
    decode_transfer, encode_audio_packet, encode_control, encode_cursor_update,
    encode_noise_handshake, encode_transfer, encode_video_config,
};
use desklink_session::{DesktopRect, ReconnectDecision, ReconnectPolicy, ReconnectSchedule};
use desklink_transport::{
    DirectLanConnection, DirectLanSession, DirectLanVideoPath, JoinRejectCode, QuicClient,
    QuicClientConfig, RelayDirectoryRegistration, RelayJoin, RelayVideoPath, TransportError,
    VideoDatagramBackend, VideoDatagramRoute,
};
use desklink_video::{EncodedFrame as WireEncodedFrame, LatestFrameQueue, encode_video_frame};
use ed25519_dalek::VerifyingKey;
use rand_core::{OsRng, RngCore};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore, mpsc, oneshot, watch};
use zeroize::Zeroizing;

use crate::{
    audio::{
        AUDIO_QUEUE_CAPACITY, AudioCaptureState, AudioFrameQueue, RemoteAudioEncoder,
        run_loopback_capture,
    },
    capture::{
        CaptureError, CaptureMediaFrame, CapturedFrame, DesktopCapturer, DisplayDescriptor,
        DisplayTopologySnapshot, DxgiDesktopCapturer, available_displays, display_topology,
    },
    encoder::{EncodedFrame, EncoderError, H264Encoder, H264EncoderSettings},
    input::{InputInjectionError, InputInjector, VirtualDesktop},
    transfer::{
        IncomingFileWorker, OutgoingFile, OutgoingFileReader, confirm_file_offer,
        notify_file_received, pick_outgoing_file, prepare_outgoing_file, read_clipboard_text,
        write_clipboard_text,
    },
    window::ApprovalState,
};

const CAPTURE_TIMEOUT: Duration = Duration::from_millis(50);
const DISPLAY_TOPOLOGY_INTERVAL: Duration = Duration::from_secs(1);
const STATIC_KEYFRAME_INTERVAL: Duration = Duration::from_millis(250);
const INITIAL_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const INITIAL_ENCODED_FRAME_TIMEOUT: Duration = Duration::from_secs(10);
const CURSOR_INTERVAL: Duration = Duration::from_millis(16);
const RELAY_CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(15);
// The authenticated handshake includes the native local-approval dialog. Give
// the person at the host enough time to inspect and approve the controller.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(150);
const DENIAL_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const FILE_TRANSFER_SEND_TIMEOUT: Duration = Duration::from_secs(30);
// Only one host-side file sender can be active for a session. Keep its
// completion notification bounded so a late task cannot retain an unbounded
// backlog while the transfer loop is reconnecting or shutting down.
const HOST_OUTGOING_EVENT_QUEUE_CAPACITY: usize = 1;
// Capture commands are low-frequency, but they must not grow without bound if
// a capture worker stalls while the controller keeps requesting changes.
const CAPTURE_COMMAND_QUEUE_CAPACITY: usize = 4;
// A normal DirectLan negotiation has one candidate per peer. Keep a small
// bounded allowance for retries, while preventing an authenticated peer from
// turning repeated candidate offers into unbounded probe tasks.
const DIRECT_PROBE_TASK_CAPACITY: usize = 2;
const VIDEO_QUEUE_CAPACITY: usize = 2;
// A single transient datagram failure is expected on a lossy path. Rebuild
// the peer attempt only after a short burst of failures, otherwise a wedged
// QUIC datagram queue leaves the host connected but permanently black.
const MAX_CONSECUTIVE_VIDEO_DATAGRAM_FAILURES: u8 = 3;
const DEFAULT_VIDEO_QUALITY: VideoQualityPreset = VideoQualityPreset::Sharp;
const DEFAULT_VIDEO_QUALITY_PREFERENCE: VideoQualityPreference = VideoQualityPreference::Automatic;
const VIDEO_FEEDBACK_MIN_PACKETS: u32 = 40;
const VIDEO_HEALTHY_LOSS_BASIS_POINTS: u64 = 50;
const VIDEO_DEGRADED_LOSS_BASIS_POINTS: u64 = 500;
const VIDEO_SEVERE_LOSS_BASIS_POINTS: u64 = 1_500;
const VIDEO_DEGRADED_SAMPLES: u8 = 2;
const VIDEO_HEALTHY_SAMPLES: u8 = 12;
const VIDEO_SWITCH_COOLDOWN_SAMPLES: u8 = 8;
const VIDEO_DEGRADED_DECODE_QUEUE: u16 = 5;
const VIDEO_HEALTHY_DECODE_QUEUE: u16 = 2;
const VIDEO_FRAME_PACING_TOLERANCE_US: u64 = 2_000;

fn next_direct_candidate_id() -> u64 {
    let mut candidate_id = 0_u64;
    while candidate_id == 0 {
        candidate_id = OsRng.next_u64();
    }
    candidate_id
}

fn video_quality_settings(preset: VideoQualityPreset) -> H264EncoderSettings {
    match preset {
        VideoQualityPreset::Smooth => H264EncoderSettings {
            max_width: 1280,
            max_height: 720,
            fps: 30,
            bitrate: 1_500_000,
            ..H264EncoderSettings::default()
        },
        VideoQualityPreset::Balanced => H264EncoderSettings {
            max_width: 1920,
            max_height: 1080,
            fps: 30,
            bitrate: 2_500_000,
            ..H264EncoderSettings::default()
        },
        VideoQualityPreset::Sharp => H264EncoderSettings::default(),
    }
}

fn video_quality_settings_with_profile(
    preset: VideoQualityPreset,
    profile: H264Profile,
) -> H264EncoderSettings {
    H264EncoderSettings {
        profile,
        ..video_quality_settings(preset)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AdaptiveVideoQuality {
    preference: VideoQualityPreference,
    preset: VideoQualityPreset,
    degraded_samples: u8,
    healthy_samples: u8,
    cooldown_samples: u8,
}

impl AdaptiveVideoQuality {
    fn new() -> Self {
        Self {
            preference: DEFAULT_VIDEO_QUALITY_PREFERENCE,
            preset: DEFAULT_VIDEO_QUALITY,
            degraded_samples: 0,
            healthy_samples: 0,
            cooldown_samples: 0,
        }
    }

    fn set_preference(&mut self, preference: VideoQualityPreference) -> Option<VideoQualityPreset> {
        self.preference = preference;
        self.reset_samples();
        self.cooldown_samples = 0;
        manual_video_quality(preference).filter(|preset| *preset != self.preset)
    }

    fn observe(
        &mut self,
        received_packets: u32,
        dropped_packets: u32,
        decode_queue_peak: u16,
        freshness_recoveries: u16,
    ) -> Option<VideoQualityPreset> {
        if self.preference != VideoQualityPreference::Automatic {
            return None;
        }

        if self.cooldown_samples > 0 {
            self.cooldown_samples -= 1;
            return None;
        }
        if freshness_recoveries > 0 {
            return self.request_lower_preset();
        }

        let total_packets = received_packets.saturating_add(dropped_packets);
        let has_network_sample = total_packets >= VIDEO_FEEDBACK_MIN_PACKETS;
        let loss_basis_points = has_network_sample
            .then(|| u64::from(dropped_packets).saturating_mul(10_000) / u64::from(total_packets));
        if loss_basis_points.is_some_and(|loss| loss >= VIDEO_SEVERE_LOSS_BASIS_POINTS) {
            return self.request_lower_preset();
        }
        let playback_degraded = decode_queue_peak >= VIDEO_DEGRADED_DECODE_QUEUE;
        let network_degraded =
            loss_basis_points.is_some_and(|loss| loss >= VIDEO_DEGRADED_LOSS_BASIS_POINTS);
        if playback_degraded || network_degraded {
            self.healthy_samples = 0;
            self.degraded_samples = self.degraded_samples.saturating_add(1);
            if self.degraded_samples >= VIDEO_DEGRADED_SAMPLES {
                return self.request_lower_preset();
            }
            return None;
        }
        if loss_basis_points.is_some_and(|loss| loss <= VIDEO_HEALTHY_LOSS_BASIS_POINTS)
            && decode_queue_peak <= VIDEO_HEALTHY_DECODE_QUEUE
        {
            self.degraded_samples = 0;
            self.healthy_samples = self.healthy_samples.saturating_add(1);
            if self.healthy_samples >= VIDEO_HEALTHY_SAMPLES {
                return self.request_higher_preset();
            }
            return None;
        }

        self.reset_samples();
        None
    }

    fn record_applied(&mut self, preset: VideoQualityPreset) {
        self.preset = preset;
        self.reset_samples();
    }

    fn request_lower_preset(&mut self) -> Option<VideoQualityPreset> {
        let next = match self.preset {
            VideoQualityPreset::Sharp => Some(VideoQualityPreset::Balanced),
            VideoQualityPreset::Balanced => Some(VideoQualityPreset::Smooth),
            VideoQualityPreset::Smooth => None,
        };
        self.finish_switch_decision(next)
    }

    fn request_higher_preset(&mut self) -> Option<VideoQualityPreset> {
        let next = match self.preset {
            VideoQualityPreset::Smooth => Some(VideoQualityPreset::Balanced),
            VideoQualityPreset::Balanced => Some(VideoQualityPreset::Sharp),
            VideoQualityPreset::Sharp => None,
        };
        self.finish_switch_decision(next)
    }

    fn finish_switch_decision(
        &mut self,
        next: Option<VideoQualityPreset>,
    ) -> Option<VideoQualityPreset> {
        self.reset_samples();
        if next.is_some() {
            self.cooldown_samples = VIDEO_SWITCH_COOLDOWN_SAMPLES;
        }
        next
    }

    fn reset_samples(&mut self) {
        self.degraded_samples = 0;
        self.healthy_samples = 0;
    }
}

fn bitrate_feedback_decision(
    preference: VideoQualityPreference,
    policy: &mut AdaptiveBitrate,
    received: u32,
    dropped: u32,
    decode_queue: u16,
    freshness: u16,
) -> (Option<u32>, bool) {
    if preference != VideoQualityPreference::Automatic {
        return (None, false);
    }
    let requested = policy.observe(received, dropped, decode_queue, freshness);
    let handled = policy.can_handle_network_pressure(received, dropped, decode_queue, freshness)
        || policy.should_recover_rate_first(received, dropped, decode_queue, freshness);
    (requested, handled)
}

fn manual_video_quality(preference: VideoQualityPreference) -> Option<VideoQualityPreset> {
    match preference {
        VideoQualityPreference::Automatic => None,
        VideoQualityPreference::Smooth => Some(VideoQualityPreset::Smooth),
        VideoQualityPreference::Balanced => Some(VideoQualityPreset::Balanced),
        VideoQualityPreference::Sharp => Some(VideoQualityPreset::Sharp),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VideoFramePacer {
    fps: u32,
    next_due_us: Option<u64>,
}

impl VideoFramePacer {
    fn new(fps: u32) -> Self {
        Self {
            fps: fps.max(1),
            next_due_us: None,
        }
    }

    fn reset(&mut self, fps: u32) {
        self.fps = fps.max(1);
        self.next_due_us = None;
    }

    fn should_encode(&mut self, timestamp_us: u64, force: bool) -> bool {
        let interval_us = (1_000_000_u64 / u64::from(self.fps)).max(1);
        let Some(next_due_us) = self.next_due_us else {
            self.next_due_us = Some(timestamp_us.saturating_add(interval_us));
            return true;
        };
        if force {
            self.next_due_us = Some(timestamp_us.saturating_add(interval_us));
            return true;
        }
        if timestamp_us < next_due_us {
            let early_us = next_due_us - timestamp_us;
            if early_us > interval_us {
                self.next_due_us = Some(timestamp_us.saturating_add(interval_us));
                return true;
            }
            if early_us > VIDEO_FRAME_PACING_TOLERANCE_US {
                return false;
            }
        }

        let intervals = if timestamp_us < next_due_us {
            1
        } else {
            timestamp_us
                .saturating_sub(next_due_us)
                .checked_div(interval_us)
                .unwrap_or(0)
                .saturating_add(1)
        };
        self.next_due_us = Some(next_due_us.saturating_add(interval_us.saturating_mul(intervals)));
        true
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedVideo {
    pub video_config: Option<Vec<u8>>,
    pub datagrams: Vec<Vec<u8>>,
    pub keyframe: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrepareVideo {
    NeedKeyframe,
    Ready(PreparedVideo),
}

#[derive(Debug)]
pub enum CaptureOutcome<T = CapturedFrame> {
    Frame(T),
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
    #[error("the process input epoch allocator is exhausted")]
    InputEpochExhausted,
    #[error("secure randomness for the process input epoch is unavailable")]
    InputEpochRandomUnavailable,
    #[error("secure randomness produced an invalid zero input epoch")]
    InputEpochZeroSeed,
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
    media_observer: Option<HostMediaDiagnosticsObserver>,
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
            media_observer: None,
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

    /// Delivery is queued to one bounded worker, never inline on media/input.
    pub fn with_media_diagnostics_observer(
        mut self,
        observer: HostMediaDiagnosticsObserver,
    ) -> Self {
        self.media_observer = Some(observer);
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
        let client = Arc::new(
            tokio::time::timeout(
                RELAY_CONNECT_TIMEOUT,
                QuicClient::connect(self.transport.clone()),
            )
            .await
            .map_err(|_| TransportError::Connection("relay connection timed out".to_owned()))??,
        );
        let mut join = RelayJoin::host_with_participant(
            self.session_id,
            *self.relay_authentication,
            self.host_device_id,
        );
        if let Some(registration) = &self.directory_registration {
            join = join.with_directory_registration(registration.clone())?;
        }
        tokio::time::timeout(RELAY_CONNECT_TIMEOUT, client.join(join))
            .await
            .map_err(|_| TransportError::Connection("relay join timed out".to_owned()))??;
        self.publish(HostLifecycleEvent::Available {
            stream_id: *stream_id,
        });
        loop {
            let identity =
                DeviceIdentity::from_secret_key(self.host_device_id, &self.host_secret_key);
            let mut runtime = HostRuntime::with_shared_authorizer(
                client.clone(),
                *stream_id,
                identity,
                self.authorizer.clone(),
            )?;
            runtime.media_observer = self.media_observer.clone();
            let outcome = runtime
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
        | HostRuntimeError::InputEpochExhausted
        | HostRuntimeError::InputEpochRandomUnavailable
        | HostRuntimeError::InputEpochZeroSeed
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
        | TransportError::DirectoryProtocolMismatch { .. }
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
    sent_config: Option<(u32, u16, u16, u64)>,
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
        input_epoch: u64,
    ) -> Result<PrepareVideo, HostRuntimeError> {
        InputContext {
            stream_id: self.stream_id,
            input_epoch,
        }
        .validate()?;
        let width = u16::try_from(width).map_err(|_| HostRuntimeError::InvalidDimensions)?;
        let height = u16::try_from(height).map_err(|_| HostRuntimeError::InvalidDimensions)?;
        if width == 0 || height == 0 {
            return Err(HostRuntimeError::InvalidDimensions);
        }
        if self
            .sent_config
            .is_some_and(|(version, sent_width, sent_height, sent_epoch)| {
                frame.config_version < version
                    || (version == frame.config_version
                        && ((sent_width, sent_height) != (width, height)
                            || sent_epoch != input_epoch))
            })
        {
            return Err(HostRuntimeError::InconsistentVideoConfig);
        }

        let needs_config = self.sent_config.is_none_or(|(version, _, _, sent_epoch)| {
            version != frame.config_version || sent_epoch != input_epoch
        });
        if needs_config && (!frame.keyframe || frame.sequence_header.is_none()) {
            return Ok(PrepareVideo::NeedKeyframe);
        }
        let video_config = if needs_config {
            Some(encode_video_config(&VideoConfig {
                protocol_version: PROTOCOL_VERSION,
                stream_id: self.stream_id,
                input_epoch,
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
        let datagrams = encode_video_frame(&wire_frame)?;
        if needs_config {
            self.sent_config = Some((frame.config_version, width, height, input_epoch));
        }
        Ok(PrepareVideo::Ready(PreparedVideo {
            video_config,
            datagrams,
            keyframe: frame.keyframe,
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
        let context = envelope.context();
        Ok(self
            .admit_input_envelope(envelope, now_us, context)
            .map(|envelope| envelope.event))
    }

    /// Keep the admitted wire identity intact until the selected-desktop lock
    /// validates and injects it. Rejected identities cannot advance ordering.
    pub fn decode_input_envelope(
        &mut self,
        bytes: &[u8],
        now_us: u64,
        expected_context: InputContext,
    ) -> Result<Option<InputEnvelope>, HostRuntimeError> {
        expected_context.validate()?;
        Ok(self.admit_input_envelope(decode_session_input(bytes)?, now_us, expected_context))
    }

    fn admit_input_envelope(
        &mut self,
        envelope: InputEnvelope,
        now_us: u64,
        expected_context: InputContext,
    ) -> Option<InputEnvelope> {
        if envelope.stream_id != self.stream_id || envelope.context() != expected_context {
            return None;
        }
        if !sequence_is_newer(envelope.sequence, self.last_input_sequence) {
            return None;
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
        Some(envelope)
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

fn next_media_frame_with_recovery(
    capture: &mut DxgiDesktopCapturer,
    timeout: Duration,
    prefer_gpu: bool,
) -> Result<CaptureOutcome<CaptureMediaFrame>, CaptureError> {
    match capture.next_media_frame(timeout, prefer_gpu) {
        Ok(frame) => Ok(CaptureOutcome::Frame(frame)),
        Err(CaptureError::Timeout) => Ok(CaptureOutcome::Idle),
        Err(CaptureError::AccessLost | CaptureError::Native(_)) => {
            capture.recover()?;
            Ok(CaptureOutcome::Recovered)
        }
        Err(error) => Err(error),
    }
}

fn encode_media_frame(
    encoder: &mut H264Encoder,
    frame: CaptureMediaFrame,
    force_keyframe: bool,
) -> Result<EncodedFrame, EncoderError> {
    match frame {
        CaptureMediaFrame::Cpu(frame) => encoder.encode(frame, force_keyframe),
        #[cfg(windows)]
        CaptureMediaFrame::Gpu(frame) => encoder.encode_gpu(&frame, force_keyframe),
    }
}

fn prefer_gpu_input(encoder: &H264Encoder) -> bool {
    #[cfg(windows)]
    {
        encoder.wants_gpu_input()
    }
    #[cfg(not(windows))]
    {
        let _ = encoder;
        false
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
    input_epoch: u64,
}

#[derive(Default)]
struct FrameInputContexts {
    configuration: Option<(u32, InputContext)>,
    pending: VecDeque<(u64, u32, InputContext)>,
}

impl FrameInputContexts {
    // Independent of the native encoder's smaller pending-input budget. Never
    // let a stalled backend accumulate input authorization metadata unboundedly.
    const CAPACITY: usize = 32;

    fn record_submission(
        &mut self,
        sample: crate::encoder::EncoderPerformanceSample,
        context: InputContext,
    ) -> bool {
        if !context.is_valid() || sample.frame_id == 0 || sample.config_version == 0 {
            return false;
        }
        let configuration = (sample.config_version, context);
        if self.configuration != Some(configuration) {
            self.pending.clear();
            self.configuration = Some(configuration);
        }
        if self.pending.len() == Self::CAPACITY
            || self
                .pending
                .iter()
                .any(|(id, version, _)| *id == sample.frame_id && *version == sample.config_version)
        {
            self.pending.clear();
            return false;
        }
        self.pending
            .push_back((sample.frame_id, sample.config_version, context));
        true
    }

    fn take_output(&mut self, frame: &EncodedFrame) -> Option<InputContext> {
        let index = self.pending.iter().position(|(id, version, _)| {
            *id == frame.frame_id && *version == frame.config_version
        })?;
        self.pending.remove(index).map(|(_, _, context)| context)
    }
}

pub struct HostRuntime {
    client: Arc<QuicClient>,
    stream_id: u64,
    identity: DeviceIdentity,
    authorizer: Arc<dyn ControllerAuthorizer>,
    media_observer: Option<HostMediaDiagnosticsObserver>,
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
        media_diagnostics::initialize();
        Ok(Self {
            client,
            stream_id,
            identity,
            authorizer,
            media_observer: None,
        })
    }

    pub fn with_media_diagnostics_observer(
        mut self,
        observer: HostMediaDiagnosticsObserver,
    ) -> Self {
        self.media_observer = Some(observer);
        self
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
            media_observer,
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
        let negotiated_profile = receive_controller_capabilities(
            &client,
            &secure,
            peer_generation,
            force_keyframe.clone(),
            stream_id,
        )
        .await?;
        let direct_session = DirectLanSession::bind_for_client(
            "0.0.0.0:0".parse().expect("valid wildcard bind address"),
            &client,
            next_direct_candidate_id(),
            secure.lock().await.video_path_binding(),
            now_unix_s(),
        )
        .ok()
        .map(Arc::new);
        let direct_connection: Arc<AsyncMutex<Option<Arc<DirectLanConnection>>>> =
            Arc::new(AsyncMutex::new(None));
        let shutdown = Arc::new(AtomicBool::new(false));
        let video_queue = Arc::new(Mutex::new(LatestFrameQueue::new(VIDEO_QUEUE_CAPACITY)));
        let video_notify = Arc::new(Notify::new());
        let (ready_sender, ready_receiver) = oneshot::channel();
        let (worker_sender, worker_receiver) = oneshot::channel();
        let (start_sender, start_receiver) = std::sync::mpsc::sync_channel(0);
        let (capture_command_sender, capture_command_receiver) =
            std::sync::mpsc::sync_channel(CAPTURE_COMMAND_QUEUE_CAPACITY);
        let selected_desktop = Arc::new(Mutex::new(SelectedDesktop::unconfigured(stream_id)?));
        let input = Arc::new(Mutex::new(InputInjector::new(VirtualDesktop::single(
            DesktopRect::new(0, 0, 1, 1),
        ))));
        let input_suspended = Arc::new(AtomicBool::new(false));
        let input_epochs = process_input_epochs()?;
        let (topology_updates, mut topology_receiver) = watch::channel(0_u64);
        let capture_context = CaptureWorkerContext {
            commands: capture_command_receiver,
            queue: video_queue.clone(),
            notify: video_notify.clone(),
            force_keyframe: force_keyframe.clone(),
            shutdown: shutdown.clone(),
            selected_desktop: selected_desktop.clone(),
            input: input.clone(),
            input_epochs,
            input_suspended: input_suspended.clone(),
            topology_updates,
            profile: negotiated_profile,
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
            ready.profile,
            &ready.displays,
            ready.active_display_id,
        )
        .await
        {
            let _ = capture_worker.shutdown_and_join();
            return Err(error);
        }
        if let Some(session) = direct_session.as_ref() {
            let offer = encode_control(&ControlMessage::VideoPathCandidateOffer {
                candidate: session.candidate().clone(),
            })?;
            client
                .send_control_for_generation(
                    peer_generation,
                    seal(&secure, SecureLane::Control, &offer).await?,
                )
                .await?;
        }
        // Initial capabilities already contained this generation's display list.
        topology_receiver.borrow_and_update();
        capture_worker.start()?;
        let audio_queue = Arc::new(Mutex::new(AudioFrameQueue::new(AUDIO_QUEUE_CAPACITY)));
        let audio_notify = Arc::new(Notify::new());
        let audio_enabled = Arc::new(AtomicBool::new(true));
        let (audio_state_sender, audio_state_receiver) =
            watch::channel(AudioCaptureState::Starting);
        let (audio_enabled_updates, audio_enabled_receiver) = watch::channel(true);
        let audio_thread_state = audio_state_sender.clone();
        let audio_thread = std::thread::Builder::new()
            .name("desklink-audio".into())
            .spawn({
                let audio_queue = audio_queue.clone();
                let audio_notify = audio_notify.clone();
                let shutdown = shutdown.clone();
                let audio_enabled = audio_enabled.clone();
                move || {
                    run_loopback_capture(
                        audio_queue,
                        audio_notify,
                        shutdown,
                        audio_enabled,
                        audio_thread_state,
                    );
                }
            });
        let mut audio_worker = match audio_thread {
            Ok(thread) => Some(AudioWorkerGuard::new(
                thread,
                shutdown.clone(),
                audio_notify.clone(),
            )),
            Err(_) => {
                audio_state_sender.send_replace(AudioCaptureState::Unavailable);
                None
            }
        };
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
            direct_connection.clone(),
        ));
        let mut inbound = Box::pin(receive_input_and_control(
            client.clone(),
            secure.clone(),
            peer_generation,
            stream_id,
            input.clone(),
            force_keyframe,
            HostInboundContext {
                capture_commands: capture_command_sender,
                selected_desktop: selected_desktop.clone(),
                input,
                input_suspended,
                topology_updates: topology_receiver,
                audio_enabled: audio_enabled.clone(),
                audio_enabled_updates,
                audio_queue: audio_queue.clone(),
                audio_notify: audio_notify.clone(),
                direct_session: direct_session.clone(),
                direct_connection: direct_connection.clone(),
                media_observer: media_observer.clone(),
            },
        ));
        let mut cursor = Box::pin(send_cursor_loop(
            client.clone(),
            secure.clone(),
            peer_generation,
            stream_id,
            selected_desktop,
            media_observer,
        ));
        let mut transfer = Box::pin(receive_transfer_loop(
            client.clone(),
            secure.clone(),
            peer_generation,
        ));
        let mut audio = Box::pin(send_audio_loop(
            client.clone(),
            secure.clone(),
            peer_generation,
            stream_id,
            audio_queue,
            audio_notify,
            audio_state_receiver,
            audio_enabled_receiver,
        ));
        let mut worker_receiver = Box::pin(worker_receiver);

        let outcome = tokio::select! {
            result = &mut video => result,
            result = &mut inbound => result,
            result = &mut cursor => result,
            result = &mut transfer => result,
            result = &mut audio => result,
            result = &mut worker_receiver => result.unwrap_or(Err(HostRuntimeError::CaptureWorkerStopped)),
        };
        drop(video);
        drop(inbound);
        drop(cursor);
        drop(transfer);
        drop(audio);
        drop(worker_receiver);
        let outcome = match capture_worker.shutdown_and_join() {
            Ok(()) => outcome,
            Err(error) => Err(error),
        };
        if let Some(worker) = audio_worker.take() {
            worker.shutdown_and_join();
        }
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

struct AudioWorkerGuard {
    thread: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl AudioWorkerGuard {
    fn new(
        thread: std::thread::JoinHandle<()>,
        shutdown: Arc<AtomicBool>,
        notify: Arc<Notify>,
    ) -> Self {
        Self {
            thread: Some(thread),
            shutdown,
            notify,
        }
    }

    fn signal_shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn shutdown_and_join(mut self) {
        self.signal_shutdown();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AudioWorkerGuard {
    fn drop(&mut self) {
        self.signal_shutdown();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
struct CaptureWorkerReady {
    width: u32,
    height: u32,
    displays: Vec<RemoteDisplay>,
    active_display_id: u32,
    profile: H264Profile,
}

#[derive(Clone)]
struct SelectedDesktop {
    stream_id: NonZeroU64,
    input_epoch: Option<NonZeroU64>,
    input_config_anchor: Option<NonZeroU32>,
    display_id: u32,
    desktop: VirtualDesktop,
    topology: Option<DisplayTopologySnapshot>,
    selection_pending: bool,
    input_invalidated: bool,
    input_closed: bool,
}

struct InputEpochAllocator(AtomicU64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputEpochSeedError {
    RandomUnavailable,
    Zero,
}

impl InputEpochAllocator {
    #[cfg(test)]
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    fn from_seed(first: u64) -> Result<Self, InputEpochSeedError> {
        let before_first = first.checked_sub(1).ok_or(InputEpochSeedError::Zero)?;
        Ok(Self(AtomicU64::new(before_first)))
    }

    fn from_rng(random: &mut impl RngCore) -> Result<Self, InputEpochSeedError> {
        let mut bytes = [0_u8; 8];
        random
            .try_fill_bytes(&mut bytes)
            .map_err(|_| InputEpochSeedError::RandomUnavailable)?;
        Self::from_seed(u64::from_be_bytes(bytes))
    }

    fn allocate(&self) -> Result<NonZeroU64, HostRuntimeError> {
        let previous = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| HostRuntimeError::InputEpochExhausted)?;
        NonZeroU64::new(previous + 1).ok_or(HostRuntimeError::InputEpochExhausted)
    }
}

static INPUT_EPOCHS: OnceLock<Result<InputEpochAllocator, InputEpochSeedError>> = OnceLock::new();

fn process_input_epochs() -> Result<&'static InputEpochAllocator, HostRuntimeError> {
    // Stream IDs persist across restarts, and old plaintext can be encrypted
    // again by a new transport. Draw one full-width nonzero process seed, then
    // issue checked monotonic epochs from it. Restart separation is random,
    // not a persistence guarantee. Zero/RNG failure and exhaustion fail closed;
    // never mask, clamp, wrap, redraw, or fall back to a fixed active epoch.
    match INPUT_EPOCHS.get_or_init(|| InputEpochAllocator::from_rng(&mut OsRng)) {
        Ok(epochs) => Ok(epochs),
        Err(InputEpochSeedError::RandomUnavailable) => {
            Err(HostRuntimeError::InputEpochRandomUnavailable)
        }
        Err(InputEpochSeedError::Zero) => Err(HostRuntimeError::InputEpochZeroSeed),
    }
}

impl SelectedDesktop {
    fn unconfigured(stream_id: u64) -> Result<Self, HostRuntimeError> {
        Ok(Self {
            stream_id: NonZeroU64::new(stream_id).ok_or(ProtocolError::InvalidInput)?,
            input_epoch: None,
            input_config_anchor: None,
            display_id: 0,
            desktop: VirtualDesktop::single(DesktopRect::new(0, 0, 1, 1)),
            topology: None,
            selection_pending: false,
            input_invalidated: true,
            input_closed: false,
        })
    }

    fn from_snapshot(
        topology: DisplayTopologySnapshot,
        stream_id: NonZeroU64,
        input_epoch: NonZeroU64,
        input_config_anchor: NonZeroU32,
    ) -> Self {
        Self {
            stream_id,
            input_epoch: Some(input_epoch),
            input_config_anchor: Some(input_config_anchor),
            display_id: topology.active_display_id(),
            desktop: VirtualDesktop::new(
                topology.active_display().rect,
                topology.virtual_desktop(),
            ),
            topology: Some(topology),
            selection_pending: false,
            input_invalidated: false,
            input_closed: false,
        }
    }

    fn input_context(&self) -> Option<InputContext> {
        self.input_epoch.map(|input_epoch| InputContext {
            stream_id: self.stream_id.get(),
            input_epoch: input_epoch.get(),
        })
    }
}

enum CaptureWorkerCommand {
    SelectDisplay {
        display_id: u32,
        reply: oneshot::Sender<bool>,
    },
    SetVideoQuality {
        preset: VideoQualityPreset,
        reply: oneshot::Sender<(VideoQualityPreset, u32)>,
    },
    SetVideoProfile {
        profile: H264Profile,
        reply: oneshot::Sender<(H264Profile, u32)>,
    },
    SetVideoBitrate {
        bitrate: u32,
        reply: oneshot::Sender<Option<u32>>,
    },
}

struct CaptureWorkerContext {
    commands: std::sync::mpsc::Receiver<CaptureWorkerCommand>,
    queue: Arc<Mutex<LatestFrameQueue<EncodedDesktopFrame>>>,
    notify: Arc<Notify>,
    force_keyframe: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    selected_desktop: Arc<Mutex<SelectedDesktop>>,
    input: Arc<Mutex<InputInjector>>,
    input_epochs: &'static InputEpochAllocator,
    input_suspended: Arc<AtomicBool>,
    topology_updates: watch::Sender<u64>,
    profile: H264Profile,
}

struct HostInboundContext {
    capture_commands: std::sync::mpsc::SyncSender<CaptureWorkerCommand>,
    selected_desktop: Arc<Mutex<SelectedDesktop>>,
    input: Arc<Mutex<InputInjector>>,
    input_suspended: Arc<AtomicBool>,
    topology_updates: watch::Receiver<u64>,
    audio_enabled: Arc<AtomicBool>,
    audio_enabled_updates: watch::Sender<bool>,
    audio_queue: Arc<Mutex<AudioFrameQueue>>,
    audio_notify: Arc<Notify>,
    direct_session: Option<Arc<DirectLanSession>>,
    direct_connection: Arc<AsyncMutex<Option<Arc<DirectLanConnection>>>>,
    media_observer: Option<HostMediaDiagnosticsObserver>,
}

struct CaptureDesktopCoordinator<
    'a,
    B: crate::input::InputBackend = crate::input::NativeInputBackend,
> {
    selected: &'a Mutex<SelectedDesktop>,
    input: &'a Mutex<InputInjector<B>>,
    epochs: &'a InputEpochAllocator,
    input_suspended: &'a AtomicBool,
    updates: &'a watch::Sender<u64>,
}

impl<B: crate::input::InputBackend> CaptureDesktopCoordinator<'_, B> {
    fn current(&self) -> Result<DisplayTopologySnapshot, HostRuntimeError> {
        lock_unpoisoned(self.selected)
            .topology
            .clone()
            .ok_or(HostRuntimeError::CaptureWorkerStopped)
    }

    fn suspend(&self) -> bool {
        let mut selected = lock_unpoisoned(self.selected);
        self.input_suspended.store(true, Ordering::Release);
        selected.input_invalidated = true;
        lock_unpoisoned(self.input).release_all().is_ok()
    }

    fn publish(
        &self,
        topology: DisplayTopologySnapshot,
        encoder_config_version: u32,
    ) -> Result<bool, HostRuntimeError> {
        let generation = topology.generation();
        let mut selected = lock_unpoisoned(self.selected);
        if selected.input_closed {
            return Ok(false);
        }
        let selection_pending = selected.selection_pending;
        let boundary = selected.input_invalidated
            || selected
                .topology
                .as_ref()
                .is_none_or(|previous| topology.requires_capture_reconfigure(previous));
        let encoder_config_version = NonZeroU32::new(encoder_config_version)
            .ok_or(HostRuntimeError::InconsistentVideoConfig)?;
        let previous_anchor = selected.input_config_anchor;
        if let Some(anchor) = previous_anchor
            && (encoder_config_version < anchor || (boundary && encoder_config_version <= anchor))
        {
            return Err(HostRuntimeError::InconsistentVideoConfig);
        }
        let mut input = lock_unpoisoned(self.input);
        let input_epoch = if boundary {
            self.input_suspended.store(true, Ordering::Release);
            selected.input_invalidated = true;
            if input.release_all().is_err() {
                return Ok(false);
            }
            self.epochs.allocate()?
        } else {
            selected
                .input_epoch
                .ok_or(HostRuntimeError::CaptureWorkerStopped)?
        };
        let stream_id = selected.stream_id;
        let input_config_anchor = if boundary {
            encoder_config_version
        } else {
            previous_anchor.ok_or(HostRuntimeError::InconsistentVideoConfig)?
        };
        input.set_desktop(VirtualDesktop::new(
            topology.active_display().rect,
            topology.virtual_desktop(),
        ));
        *selected =
            SelectedDesktop::from_snapshot(topology, stream_id, input_epoch, input_config_anchor);
        selected.selection_pending = selection_pending;
        self.input_suspended.store(false, Ordering::Release);
        self.updates.send_replace(generation);
        Ok(true)
    }
}

fn refreshed_display_topology(
    current: &DisplayTopologySnapshot,
) -> Result<Option<DisplayTopologySnapshot>, HostRuntimeError> {
    let displays = available_displays().map_err(HostRuntimeError::Capture)?;
    remote_displays(&displays)?;
    let bounds = display_topology()
        .map_err(HostRuntimeError::Capture)?
        .virtual_desktop;
    current
        .refreshed(displays, bounds)
        .map_err(HostRuntimeError::Capture)
}

fn prepare_capture_topology(
    capture: &mut DxgiDesktopCapturer,
    encoder: &mut H264Encoder,
    topology: &DisplayTopologySnapshot,
    settings: H264EncoderSettings,
    retain_recovered_capture: bool,
) -> Result<(), HostRuntimeError> {
    let next_capture = if retain_recovered_capture
        && capture.display_id() == topology.active_display_id()
        && capture.desktop_rect() == topology.active_display().rect
    {
        None
    } else {
        Some(
            DxgiDesktopCapturer::new_display(topology.active_display_id())
                .map_err(HostRuntimeError::Capture)?,
        )
    };
    let source = next_capture.as_ref().unwrap_or(capture);
    if source.desktop_rect() != topology.active_display().rect {
        return Err(HostRuntimeError::Capture(CaptureError::AccessLost));
    }
    let observed_bounds = display_topology()
        .map_err(HostRuntimeError::Capture)?
        .virtual_desktop;
    let observed_displays = available_displays().map_err(HostRuntimeError::Capture)?;
    if topology
        .refreshed(observed_displays, observed_bounds)
        .map_err(HostRuntimeError::Capture)?
        .is_some()
    {
        // Resource creation can take long enough for another hotplug/rotation.
        // Reject a candidate prepared from an already superseded OS snapshot.
        return Err(HostRuntimeError::Capture(CaptureError::AccessLost));
    }
    let (width, height) = source.dimensions();
    // reconfigure_for_source commits its backend/settings only after successful
    // creation. Keep the old capture and published coordinates on any error.
    encoder
        .reconfigure_for_source(
            width,
            height,
            H264EncoderSettings {
                bitrate: encoder.bitrate(),
                ..settings
            },
        )
        .map_err(HostRuntimeError::Encoder)?;
    if refreshed_display_topology(topology)?.is_some() {
        // Encoder/driver creation can also span another display change. Keep
        // input gated and retry instead of publishing superseded geometry.
        return Err(HostRuntimeError::Capture(CaptureError::AccessLost));
    }
    if let Some(next_capture) = next_capture {
        *capture = next_capture;
    }
    Ok(())
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

fn remote_displays(displays: &[DisplayDescriptor]) -> Result<Vec<RemoteDisplay>, HostRuntimeError> {
    displays
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
        .collect()
}

fn selected_display_list(
    selected_desktop: &Mutex<SelectedDesktop>,
) -> Result<(Vec<RemoteDisplay>, u32), HostRuntimeError> {
    let selected = lock_unpoisoned(selected_desktop);
    let topology = selected
        .topology
        .as_ref()
        .ok_or(HostRuntimeError::CaptureWorkerStopped)?;
    Ok((remote_displays(topology.displays())?, selected.display_id))
}

fn capture_worker_entry(
    ready: oneshot::Sender<Result<CaptureWorkerReady, HostRuntimeError>>,
    start: std::sync::mpsc::Receiver<()>,
    context: CaptureWorkerContext,
) -> Result<(), HostRuntimeError> {
    let profile = context.profile;
    let display_descriptors = match available_displays() {
        Ok(displays) => displays,
        Err(error) => {
            let _ = ready.send(Err(HostRuntimeError::Capture(error)));
            return Ok(());
        }
    };
    let displays = match remote_displays(&display_descriptors) {
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
    let encoder = match H264Encoder::new_with_settings(
        source_width,
        source_height,
        video_quality_settings_with_profile(DEFAULT_VIDEO_QUALITY, profile),
    ) {
        Ok(encoder) => encoder,
        Err(error) => {
            let _ = ready.send(Err(HostRuntimeError::Encoder(error)));
            return Ok(());
        }
    };
    let (width, height) = encoder.dimensions();
    let coordinate_space = match display_topology() {
        Ok(topology) => topology.virtual_desktop,
        Err(error) => {
            let _ = ready.send(Err(HostRuntimeError::Capture(error)));
            return Ok(());
        }
    };
    let active_display_id = capture.display_id();
    let topology = match DisplayTopologySnapshot::new(
        display_descriptors,
        active_display_id,
        coordinate_space,
    ) {
        Ok(topology) if topology.active_display().rect == capture.desktop_rect() => topology,
        Ok(_) => {
            let _ = ready.send(Err(HostRuntimeError::Capture(CaptureError::AccessLost)));
            return Ok(());
        }
        Err(error) => {
            let _ = ready.send(Err(HostRuntimeError::Capture(error)));
            return Ok(());
        }
    };
    let coordinator = CaptureDesktopCoordinator {
        selected: &context.selected_desktop,
        input: &context.input,
        epochs: context.input_epochs,
        input_suspended: &context.input_suspended,
        updates: &context.topology_updates,
    };
    match coordinator.publish(topology, encoder.config_version()) {
        Ok(true) => {}
        Ok(false) => {
            let _ = ready.send(Err(HostRuntimeError::CaptureWorkerStopped));
            return Ok(());
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            return Ok(());
        }
    }
    let worker_ready = CaptureWorkerReady {
        width,
        height,
        displays,
        active_display_id,
        profile: encoder.profile(),
    };
    if ready.send(Ok(worker_ready)).is_err() || start.recv().is_err() {
        return Ok(());
    }
    let outcome = capture_encode_loop(capture, encoder, context);
    #[cfg(windows)]
    let _ = crate::gpu::trim_retired_gpu_surfaces();
    outcome
}

async fn receive_controller_capabilities(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    force_keyframe: Arc<AtomicBool>,
    stream_id: u64,
) -> Result<H264Profile, HostRuntimeError> {
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
                        && capabilities.supports_h264_profile(H264Profile::Main) =>
                {
                    return Ok(if capabilities.supports_h264_profile(H264Profile::High) {
                        H264Profile::High
                    } else {
                        H264Profile::Main
                    });
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
    tokio::time::timeout(NEGOTIATION_TIMEOUT, negotiation)
        .await
        .map_err(|_| HostRuntimeError::NegotiationTimeout)?
}

#[allow(clippy::too_many_arguments)]
async fn send_host_capabilities(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    width: u32,
    height: u32,
    profile: H264Profile,
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
        h264_profiles: if profile == H264Profile::High {
            vec![H264Profile::Main, H264Profile::High]
        } else {
            vec![H264Profile::Main]
        },
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
    send_video_quality_state(
        client,
        secure,
        peer_generation,
        DEFAULT_VIDEO_QUALITY_PREFERENCE,
        DEFAULT_VIDEO_QUALITY,
    )
    .await?;
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

fn recoverable_configuration<T>(
    result: Result<T, HostRuntimeError>,
) -> Result<Option<T>, HostRuntimeError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error @ HostRuntimeError::Encoder(EncoderError::ConfigVersionExhausted)) => Err(error),
        Err(_) => Ok(None),
    }
}

fn capture_encode_loop(
    mut capture: DxgiDesktopCapturer,
    mut encoder: H264Encoder,
    context: CaptureWorkerContext,
) -> Result<(), HostRuntimeError> {
    let CaptureWorkerContext {
        commands,
        queue,
        notify,
        force_keyframe,
        shutdown,
        selected_desktop,
        input,
        input_epochs,
        input_suspended,
        topology_updates,
        profile: _,
    } = context;
    let desktop_state = CaptureDesktopCoordinator {
        selected: &selected_desktop,
        input: &input,
        epochs: input_epochs,
        input_suspended: &input_suspended,
        updates: &topology_updates,
    };
    let mut next_topology_poll = Instant::now() + DISPLAY_TOPOLOGY_INTERVAL;
    let mut topology_recovery_pending = false;
    let mut encoder_recovery_attempts = 0_u8;
    let mut frame_input_contexts = FrameInputContexts::default();
    let mut video_quality = DEFAULT_VIDEO_QUALITY;
    let mut video_profile = encoder.profile();
    let mut reported_first_encoded_frame = false;
    let mut frame_pacer =
        VideoFramePacer::new(video_quality_settings_with_profile(video_quality, video_profile).fps);
    let mut first_capture_deadline = Instant::now() + INITIAL_FRAME_TIMEOUT;
    let mut static_recovery = StaticKeyframeRecovery::default();
    let mut captured_any_frame = false;
    let mut first_encoded_frame_deadline = None;
    let mut encoded_any_frame = false;
    while !shutdown.load(Ordering::Acquire) {
        {
            let selected = lock_unpoisoned(&selected_desktop);
            if selected.input_closed {
                break;
            }
            topology_recovery_pending |= selected.input_invalidated;
        }
        if Instant::now() >= next_topology_poll {
            next_topology_poll = Instant::now() + DISPLAY_TOPOLOGY_INTERVAL;
            let current = desktop_state.current()?;
            let next = match refreshed_display_topology(&current) {
                Ok(Some(next)) => Some(next),
                Ok(None) if topology_recovery_pending => {
                    Some(current.renewed().map_err(HostRuntimeError::Capture)?)
                }
                Ok(None) => None,
                Err(_) => {
                    // A transient empty/inconsistent OS enumeration must not
                    // replace the last coherent list or inject into new bounds.
                    desktop_state.suspend();
                    topology_recovery_pending = true;
                    None
                }
            };
            if let Some(next) = next {
                let reset_media =
                    topology_recovery_pending || next.requires_capture_reconfigure(&current);
                if reset_media {
                    desktop_state.suspend();
                    topology_recovery_pending = true;
                    if recoverable_configuration(prepare_capture_topology(
                        &mut capture,
                        &mut encoder,
                        &next,
                        video_quality_settings_with_profile(video_quality, video_profile),
                        false,
                    ))?
                    .is_some()
                    {
                        video_profile = encoder.profile();
                        frame_pacer.reset(
                            video_quality_settings_with_profile(video_quality, video_profile).fps,
                        );
                        lock_queue(&queue).clear();
                        force_keyframe.store(true, Ordering::Release);
                        if !captured_any_frame {
                            first_capture_deadline = Instant::now() + INITIAL_FRAME_TIMEOUT;
                        }
                        if !encoded_any_frame {
                            first_encoded_frame_deadline = None;
                        }
                        if desktop_state.publish(next, encoder.config_version())? {
                            notify.notify_one();
                            topology_recovery_pending = false;
                        }
                    }
                } else {
                    // Hotplug of an unused output or a primary/name change may
                    // update the list without rebuilding unchanged media.
                    if !desktop_state.publish(next, encoder.config_version())? {
                        topology_recovery_pending = true;
                    }
                }
            }
        }
        while let Ok(command) = commands.try_recv() {
            match command {
                CaptureWorkerCommand::SelectDisplay { display_id, reply } => {
                    if topology_recovery_pending {
                        let _ = reply.send(false);
                        continue;
                    }
                    let current = desktop_state.current()?;
                    let candidate = current.select(display_id);
                    let switched = match candidate {
                        Ok(None) => true,
                        Ok(Some(next)) => {
                            desktop_state.suspend();
                            let prepared = recoverable_configuration(prepare_capture_topology(
                                &mut capture,
                                &mut encoder,
                                &next,
                                video_quality_settings_with_profile(video_quality, video_profile),
                                false,
                            ))?;
                            if prepared.is_some() {
                                video_profile = encoder.profile();
                                frame_pacer.reset(
                                    video_quality_settings_with_profile(
                                        video_quality,
                                        video_profile,
                                    )
                                    .fps,
                                );
                                lock_queue(&queue).clear();
                                force_keyframe.store(true, Ordering::Release);
                                if !captured_any_frame {
                                    first_capture_deadline = Instant::now() + INITIAL_FRAME_TIMEOUT;
                                }
                                if !encoded_any_frame {
                                    first_encoded_frame_deadline = None;
                                }
                                if desktop_state.publish(next, encoder.config_version())? {
                                    notify.notify_one();
                                    true
                                } else {
                                    topology_recovery_pending = true;
                                    false
                                }
                            } else {
                                topology_recovery_pending = true;
                                next_topology_poll = Instant::now();
                                false
                            }
                        }
                        Err(_) => false,
                    };
                    let _ = reply.send(switched);
                }
                CaptureWorkerCommand::SetVideoQuality { preset, reply } => {
                    if preset != video_quality
                        || encoder.bitrate() != video_quality_settings(preset).bitrate
                    {
                        let (source_width, source_height) = capture.dimensions();
                        if recoverable_configuration(
                            encoder
                                .reconfigure_for_source(
                                    source_width,
                                    source_height,
                                    video_quality_settings_with_profile(preset, video_profile),
                                )
                                .map_err(HostRuntimeError::Encoder),
                        )?
                        .is_some()
                        {
                            video_quality = preset;
                            video_profile = encoder.profile();
                            frame_pacer.reset(
                                video_quality_settings_with_profile(video_quality, video_profile)
                                    .fps,
                            );
                            lock_queue(&queue).clear();
                            force_keyframe.store(true, Ordering::Release);
                            notify.notify_one();
                        }
                    }
                    let _ = reply.send((video_quality, encoder.bitrate()));
                }
                CaptureWorkerCommand::SetVideoProfile { profile, reply } => {
                    if profile != video_profile {
                        let (source_width, source_height) = capture.dimensions();
                        if recoverable_configuration(
                            encoder
                                .reconfigure_for_source(
                                    source_width,
                                    source_height,
                                    H264EncoderSettings {
                                        bitrate: encoder.bitrate(),
                                        ..video_quality_settings_with_profile(
                                            video_quality,
                                            profile,
                                        )
                                    },
                                )
                                .map_err(HostRuntimeError::Encoder),
                        )?
                        .is_some()
                        {
                            video_profile = encoder.profile();
                            lock_queue(&queue).clear();
                            force_keyframe.store(true, Ordering::Release);
                            notify.notify_one();
                        }
                    }
                    let _ = reply.send((video_profile, encoder.bitrate()));
                }
                CaptureWorkerCommand::SetVideoBitrate { bitrate, reply } => {
                    let actual = match encoder.try_set_bitrate(bitrate) {
                        Ok(true) => Some(encoder.bitrate()),
                        Ok(false) => None,
                        Err(_) => {
                            // Native rollback was uncertain. Recreate the last
                            // verified settings before encoding any more frames.
                            let (width, height) = encoder.dimensions();
                            encoder
                                .rebuild(width, height)
                                .map_err(HostRuntimeError::Encoder)?;
                            video_profile = encoder.profile();
                            lock_queue(&queue).clear();
                            force_keyframe.store(true, Ordering::Release);
                            notify.notify_one();
                            None
                        }
                    };
                    let _ = reply.send(actual);
                }
            }
        }
        if topology_recovery_pending {
            std::thread::sleep(CAPTURE_TIMEOUT);
            continue;
        }
        let captured_context = lock_unpoisoned(&selected_desktop)
            .input_context()
            .ok_or(HostRuntimeError::CaptureWorkerStopped)?;
        let outcome = match next_media_frame_with_recovery(
            &mut capture,
            CAPTURE_TIMEOUT,
            prefer_gpu_input(&encoder),
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                // An active monitor can disappear between the once-per-second
                // checks. Try the new topology before treating capture as fatal.
                let current = desktop_state.current()?;
                if matches!(refreshed_display_topology(&current), Ok(None)) {
                    return Err(HostRuntimeError::Capture(error));
                }
                desktop_state.suspend();
                topology_recovery_pending = true;
                next_topology_poll = Instant::now();
                continue;
            }
        };
        let frame = match outcome {
            CaptureOutcome::Frame(frame) => frame,
            CaptureOutcome::Idle => {
                if initial_frame_deadline_expired(
                    captured_any_frame,
                    first_capture_deadline,
                    Instant::now(),
                ) {
                    return Err(HostRuntimeError::Capture(CaptureError::Native(
                        "desktop capture did not produce an initial frame within 10 seconds"
                            .to_owned(),
                    )));
                }
                if initial_encoded_frame_deadline_expired(
                    encoded_any_frame,
                    first_encoded_frame_deadline,
                    Instant::now(),
                ) {
                    return Err(HostRuntimeError::Encoder(EncoderError::Native(
                        "desktop encoder did not produce an initial frame within 10 seconds"
                            .to_owned(),
                    )));
                }
                // DXGI reports only changes. Recover a decoder using a fresh
                // owned snapshot, without copying every normal captured frame.
                if static_recovery
                    .snapshot_due(force_keyframe.load(Ordering::Acquire), Instant::now())
                {
                    capture
                        .snapshot_media_frame(prefer_gpu_input(&encoder))
                        .map_err(HostRuntimeError::Capture)?
                } else {
                    continue;
                }
            }
            CaptureOutcome::Recovered => {
                desktop_state.suspend();
                topology_recovery_pending = true;
                let current = desktop_state.current()?;
                let next = match refreshed_display_topology(&current) {
                    Ok(Some(next)) => next,
                    Ok(None) => current.renewed().map_err(HostRuntimeError::Capture)?,
                    Err(_) => {
                        next_topology_poll = Instant::now() + DISPLAY_TOPOLOGY_INTERVAL;
                        continue;
                    }
                };
                if recoverable_configuration(prepare_capture_topology(
                    &mut capture,
                    &mut encoder,
                    &next,
                    video_quality_settings_with_profile(video_quality, video_profile),
                    true,
                ))?
                .is_none()
                {
                    next_topology_poll = Instant::now() + DISPLAY_TOPOLOGY_INTERVAL;
                    continue;
                }
                if !captured_any_frame {
                    first_capture_deadline = Instant::now() + INITIAL_FRAME_TIMEOUT;
                }
                if !encoded_any_frame {
                    first_encoded_frame_deadline = None;
                }
                video_profile = encoder.profile();
                frame_pacer
                    .reset(video_quality_settings_with_profile(video_quality, video_profile).fps);
                lock_queue(&queue).clear();
                force_keyframe.store(true, Ordering::Release);
                if desktop_state.publish(next, encoder.config_version())? {
                    notify.notify_one();
                    topology_recovery_pending = false;
                }
                continue;
            }
        };
        captured_any_frame = true;
        if first_encoded_frame_deadline.is_none() {
            first_encoded_frame_deadline = Some(Instant::now() + INITIAL_ENCODED_FRAME_TIMEOUT);
        }
        if initial_encoded_frame_deadline_expired(
            encoded_any_frame,
            first_encoded_frame_deadline,
            Instant::now(),
        ) {
            return Err(HostRuntimeError::Encoder(EncoderError::Native(
                "desktop encoder did not produce an initial frame within 10 seconds".to_owned(),
            )));
        }
        let request_keyframe = force_keyframe.swap(false, Ordering::AcqRel);
        if !frame_pacer.should_encode(frame.timestamp_us(), request_keyframe) {
            continue;
        }
        let result = encode_media_frame(&mut encoder, frame, request_keyframe);
        if !encoder
            .last_performance_sample()
            .is_some_and(|sample| frame_input_contexts.record_submission(sample, captured_context))
        {
            force_keyframe.store(true, Ordering::Release);
        }
        let (encoded, encoded_context) = match result {
            Ok(encoded) => {
                let Some(output_context) = frame_input_contexts
                    .take_output(&encoded)
                    .filter(|context| *context == captured_context)
                else {
                    // An asynchronous MFT may return an older buffered frame.
                    // Never relabel it with the current geometry's input epoch.
                    force_keyframe.store(true, Ordering::Release);
                    continue;
                };
                encoder_recovery_attempts = 0;
                if !reported_first_encoded_frame {
                    eprintln!(
                        "DeskLink video capture produced its first encoded frame (bytes={} keyframe={} config_version={})",
                        encoded.access_unit.len(),
                        encoded.keyframe,
                        encoded.config_version,
                    );
                    reported_first_encoded_frame = true;
                }
                encoded_any_frame = true;
                (encoded, output_context)
            }
            Err(EncoderError::NeedMoreInput) => {
                // Some MFTs need another submission before returning output.
                // Preserve recovery intent so an idle DXGI desktop can supply
                // another bounded snapshot instead of losing the request.
                static_recovery.encoder_buffered_output(
                    &force_keyframe,
                    request_keyframe,
                    encoded_any_frame,
                );
                continue;
            }
            Err(error)
                if encoder_error_is_recoverable(&error)
                    && encoder_recovery_attempts < MAX_ENCODER_RECOVERY_ATTEMPTS =>
            {
                encoder_recovery_attempts += 1;
                let (width, height) = encoder.dimensions();
                encoder
                    .rebuild(width, height)
                    .map_err(HostRuntimeError::Encoder)?;
                video_profile = encoder.profile();
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
            input_epoch: encoded_context.input_epoch,
        });
        notify.notify_one();
    }
    Ok(())
}

#[derive(Default)]
struct StaticKeyframeRecovery {
    last_snapshot: Option<Instant>,
}

impl StaticKeyframeRecovery {
    fn encoder_buffered_output(&self, requested: &AtomicBool, keyframe: bool, encoded_any: bool) {
        if keyframe || !encoded_any {
            requested.store(true, Ordering::Release);
        }
    }

    fn snapshot_due(&mut self, requested: bool, now: Instant) -> bool {
        if !requested
            || self
                .last_snapshot
                .is_some_and(|last| now.saturating_duration_since(last) < STATIC_KEYFRAME_INTERVAL)
        {
            return false;
        }
        self.last_snapshot = Some(now);
        true
    }
}

fn initial_frame_deadline_expired(
    captured_any_frame: bool,
    deadline: Instant,
    now: Instant,
) -> bool {
    !captured_any_frame && now >= deadline
}

fn initial_encoded_frame_deadline_expired(
    encoded_any_frame: bool,
    deadline: Option<Instant>,
    now: Instant,
) -> bool {
    !encoded_any_frame && deadline.is_some_and(|deadline| now >= deadline)
}

#[allow(clippy::too_many_arguments)]
async fn send_video_loop(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    peer_generation: u64,
    stream_id: u64,
    queue: Arc<Mutex<LatestFrameQueue<EncodedDesktopFrame>>>,
    notify: Arc<Notify>,
    force_keyframe: Arc<AtomicBool>,
    direct_connection: Arc<AsyncMutex<Option<Arc<DirectLanConnection>>>>,
) -> Result<(), HostRuntimeError> {
    let mut pipeline = HostVideoPipeline::new(stream_id);
    let relay_video_path = RelayVideoPath::new(client.clone(), peer_generation);
    let mut consecutive_datagram_failures = 0_u8;
    let mut reported_first_ready = false;
    let mut reported_need_keyframe = false;
    let mut reported_first_datagram_stage = false;
    loop {
        notify.notified().await;
        let next = lock_queue(&queue).take_newest();
        let Some(next) = next else {
            continue;
        };
        match pipeline.prepare(next.frame, next.width, next.height, next.input_epoch)? {
            PrepareVideo::NeedKeyframe => {
                if !reported_need_keyframe {
                    eprintln!(
                        "DeskLink video pipeline is waiting for a keyframe (stream_id={stream_id})"
                    );
                    reported_need_keyframe = true;
                }
                force_keyframe.store(true, Ordering::Release);
            }
            PrepareVideo::Ready(prepared) => {
                // Any keyframe is large enough to span many QUIC datagrams.
                // Relay pacing or loss during recovery must not leave the
                // controller without a decodable reference frame, so carry
                // every relay keyframe over the bounded reliable batches.
                // Direct-LAN peers stay on datagrams throughout.
                let reliable_relay_keyframe =
                    prepared.keyframe && direct_connection.lock().await.is_none();
                if !reported_first_ready {
                    eprintln!(
                        "DeskLink video pipeline prepared its first frame (stream_id={} datagrams={} config={})",
                        stream_id,
                        prepared.datagrams.len(),
                        prepared.video_config.is_some(),
                    );
                    reported_first_ready = true;
                }
                if let Some(config) = prepared.video_config {
                    client
                        .send_video_config_for_generation(
                            peer_generation,
                            seal(&secure, SecureLane::VideoConfig, &config).await?,
                        )
                        .await?;
                }
                if reliable_relay_keyframe {
                    let batch_count = send_video_reliable_batches(
                        &client,
                        &secure,
                        peer_generation,
                        prepared.datagrams,
                    )
                    .await?;
                    if !reported_first_datagram_stage {
                        eprintln!(
                            "DeskLink video reliable batch send returned (stream_id={} batches={} ok=true)",
                            stream_id, batch_count,
                        );
                        reported_first_datagram_stage = true;
                    }
                    consecutive_datagram_failures = 0;
                    continue;
                }
                for (index, datagram) in prepared.datagrams.into_iter().enumerate() {
                    if !reported_first_datagram_stage {
                        eprintln!(
                            "DeskLink video datagram stage entered (stream_id={} plaintext_bytes={} index={})",
                            stream_id,
                            datagram.len(),
                            index,
                        );
                    }
                    let direct_snapshot = direct_connection.lock().await.clone();
                    let mut direct_path = direct_snapshot.clone().map(DirectLanVideoPath::new);
                    let send_result = if reliable_relay_keyframe && direct_path.is_none() {
                        let ciphertext =
                            seal(&secure, SecureLane::VideoReliable, &datagram).await?;
                        if !reported_first_datagram_stage {
                            eprintln!(
                                "DeskLink video keyframe encrypted (stream_id={} lane=video_reliable ciphertext_bytes={})",
                                stream_id,
                                ciphertext.len(),
                            );
                        }
                        client
                            .send_video_reliable_for_generation(peer_generation, ciphertext)
                            .await
                            .map(|()| VideoDatagramRoute::Relay)
                    } else {
                        let ciphertext =
                            seal(&secure, SecureLane::VideoDatagram, &datagram).await?;
                        if !reported_first_datagram_stage {
                            eprintln!(
                                "DeskLink video datagram encrypted (stream_id={} lane=video_datagram ciphertext_bytes={})",
                                stream_id,
                                ciphertext.len(),
                            );
                        }
                        send_video_datagram_with_fallback(
                            &mut direct_path,
                            &relay_video_path,
                            ciphertext,
                        )
                        .await
                    };
                    if !reported_first_datagram_stage {
                        eprintln!(
                            "DeskLink video datagram send returned (stream_id={} route={:?} ok={})",
                            stream_id,
                            send_result.as_ref().ok(),
                            send_result.is_ok(),
                        );
                        reported_first_datagram_stage = true;
                    }
                    if direct_path.is_none()
                        && let Some(failed) = direct_snapshot.as_ref()
                    {
                        let retired = {
                            let mut active = direct_connection.lock().await;
                            direct_path_slot::retire_if_current(&mut active, failed)
                        };
                        if let Some(retired) = retired {
                            retired.close(b"desklink direct egress failed");
                        }
                    }
                    match send_result {
                        Ok(_) => {
                            consecutive_datagram_failures = 0;
                        }
                        Err(error @ TransportError::Datagram(_)) => {
                            // QUIC datagrams are intentionally lossy. A
                            // transient congestion/MTU drop must not tear
                            // down the entire authenticated host runtime;
                            // request a fresh keyframe and let the next
                            // encoded frame recover the controller instead.
                            consecutive_datagram_failures =
                                consecutive_datagram_failures.saturating_add(1);
                            force_keyframe.store(true, Ordering::Release);
                            if consecutive_datagram_failures
                                >= MAX_CONSECUTIVE_VIDEO_DATAGRAM_FAILURES
                            {
                                eprintln!(
                                    "DeskLink video datagram path requires a peer rebuild after {consecutive_datagram_failures} consecutive send failures (stream_id={stream_id}): {error}"
                                );
                                return Err(error.into());
                            }
                            break;
                        }
                        Err(error) => return Err(error.into()),
                    }
                    if index % 16 == 15 {
                        tokio::task::yield_now().await;
                    }
                }
            }
        }
    }
}

async fn send_video_reliable_batches(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    datagrams: Vec<Vec<u8>>,
) -> Result<usize, HostRuntimeError> {
    let mut batches = 0_usize;
    let mut batch = VideoReliableBatchBuilder::new();
    for datagram in datagrams {
        if !batch.try_push(&datagram)? {
            send_video_reliable_batch(client, secure, peer_generation, batch.bytes()).await?;
            batches += 1;
            batch.clear();
            // Execute the push in release too; the debug assertion is only
            // proof that a legal single packet fits in an empty batch.
            let added = batch.try_push(&datagram)?;
            debug_assert!(added);
        }
    }
    if !batch.is_empty() {
        send_video_reliable_batch(client, secure, peer_generation, batch.bytes()).await?;
        batches += 1;
    }
    Ok(batches)
}

async fn send_video_reliable_batch(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    plaintext: &[u8],
) -> Result<(), HostRuntimeError> {
    let ciphertext = seal(secure, SecureLane::VideoReliable, plaintext).await?;
    client
        .send_video_reliable_for_generation(peer_generation, ciphertext)
        .await?;
    Ok(())
}

async fn send_video_datagram_with_fallback<D, R>(
    direct: &mut Option<D>,
    relay: &R,
    bytes: Vec<u8>,
) -> Result<VideoDatagramRoute, TransportError>
where
    D: VideoDatagramBackend,
    R: VideoDatagramBackend,
{
    let direct_failed = if let Some(path) = direct.as_ref() {
        let route = path.route();
        if path.send(bytes.clone()).await.is_ok() {
            return Ok(route);
        }
        true
    } else {
        false
    };
    if direct_failed {
        *direct = None;
    }
    relay.send(bytes).await?;
    Ok(relay.route())
}

#[allow(clippy::too_many_arguments)]
async fn send_audio_loop(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    peer_generation: u64,
    stream_id: u64,
    queue: Arc<Mutex<AudioFrameQueue>>,
    notify: Arc<Notify>,
    mut capture_state: watch::Receiver<AudioCaptureState>,
    mut enabled: watch::Receiver<bool>,
) -> Result<(), HostRuntimeError> {
    let mut encoder = match RemoteAudioEncoder::new() {
        Ok(encoder) => encoder,
        Err(_) => {
            // Audio is optional. A codec initialization failure must not tear
            // down the video, input, clipboard, or file-transfer session.
            let audio_enabled = *enabled.borrow();
            let _ = send_audio_state(&client, &secure, peer_generation, false, audio_enabled).await;
            std::future::pending::<()>().await;
            unreachable!("pending audio codec fallback returned")
        }
    };
    let mut sequence = 0_u64;
    let mut announced = None;
    loop {
        let available = *capture_state.borrow() == AudioCaptureState::Available;
        let is_enabled = *enabled.borrow();
        let current = (available, is_enabled);
        if announced != Some(current) {
            send_audio_state(&client, &secure, peer_generation, available, is_enabled).await?;
            announced = Some(current);
        }

        if !available || !is_enabled {
            tokio::select! {
                changed = capture_state.changed() => {
                    if changed.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
                changed = enabled.changed() => {
                    if changed.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
            }
            continue;
        }

        tokio::select! {
            _ = notify.notified() => {}
            changed = capture_state.changed() => {
                if changed.is_err() {
                    std::future::pending::<()>().await;
                }
                continue;
            }
            changed = enabled.changed() => {
                if changed.is_err() {
                    std::future::pending::<()>().await;
                }
                continue;
            }
        }
        if *capture_state.borrow() != AudioCaptureState::Available || !*enabled.borrow() {
            lock_unpoisoned(&queue).clear();
            continue;
        }

        let frames = { lock_unpoisoned(&queue).drain() };
        for frame in frames {
            let payload = match encoder.encode_pcm_s16_le(&frame.payload) {
                Ok(payload) => payload,
                // Malformed capture frames or a transient codec failure are
                // isolated to this lossy lane. The next 10 ms frame retries.
                Err(_) => continue,
            };
            sequence = sequence.wrapping_add(1).max(1);
            let packet = AudioPacket {
                protocol_version: PROTOCOL_VERSION,
                stream_id,
                sequence,
                capture_timestamp_us: frame.capture_timestamp_us,
                codec: AudioCodec::Opus,
                sample_rate: desklink_protocol::AUDIO_SAMPLE_RATE,
                channels: desklink_protocol::AUDIO_CHANNELS,
                payload,
            };
            let plaintext = encode_audio_packet(&packet)?;
            let send = client
                .send_audio_datagram_for_generation(
                    peer_generation,
                    seal(&secure, SecureLane::AudioDatagram, &plaintext).await?,
                )
                .await;
            match send {
                Ok(()) => {}
                // QUIC datagram congestion is an expected lossy boundary. Drop
                // this sound packet instead of turning optional audio pressure
                // into a remote-control reconnect.
                Err(TransportError::Datagram(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
    }
}

async fn send_audio_state(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    available: bool,
    enabled: bool,
) -> Result<(), HostRuntimeError> {
    let plaintext = encode_control(&ControlMessage::AudioState { available, enabled })?;
    client
        .send_control_for_generation(
            peer_generation,
            seal(secure, SecureLane::Control, &plaintext).await?,
        )
        .await?;
    Ok(())
}

async fn send_video_quality_state(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    preference: VideoQualityPreference,
    preset: VideoQualityPreset,
) -> Result<(), HostRuntimeError> {
    let plaintext = encode_control(&ControlMessage::VideoQualityState { preference, preset })?;
    client
        .send_control_for_generation(
            peer_generation,
            seal(secure, SecureLane::Control, &plaintext).await?,
        )
        .await?;
    Ok(())
}

async fn apply_video_quality(
    capture_commands: &std::sync::mpsc::SyncSender<CaptureWorkerCommand>,
    preset: VideoQualityPreset,
) -> Result<Option<(VideoQualityPreset, u32)>, HostRuntimeError> {
    let (reply, applied) = oneshot::channel();
    capture_commands
        .try_send(CaptureWorkerCommand::SetVideoQuality { preset, reply })
        .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?;
    Ok(Some(
        applied
            .await
            .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?,
    ))
}

async fn apply_video_profile(
    capture_commands: &std::sync::mpsc::SyncSender<CaptureWorkerCommand>,
    profile: H264Profile,
) -> Result<Option<(H264Profile, u32)>, HostRuntimeError> {
    let (reply, applied) = oneshot::channel();
    capture_commands
        .try_send(CaptureWorkerCommand::SetVideoProfile { profile, reply })
        .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?;
    Ok(Some(
        applied
            .await
            .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?,
    ))
}

enum ConfigurationApplied {
    Display,
    Quality(Option<(VideoQualityPreset, u32)>),
    Profile(Option<(H264Profile, u32)>),
    Bitrate(Option<u32>),
}

type ConfigurationFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<
                Output = Result<(ConfigurationOperation, ConfigurationApplied), HostRuntimeError>,
            > + Send,
    >,
>;

struct PendingConfiguration {
    operation: ConfigurationOperation,
    future: ConfigurationFuture,
}

fn start_configuration(
    commands: std::sync::mpsc::SyncSender<CaptureWorkerCommand>,
    operation: ConfigurationOperation,
) -> PendingConfiguration {
    PendingConfiguration {
        operation,
        future: Box::pin(async move {
            let applied = match operation.action {
                ConfigurationAction::SelectDisplay(display_id) => {
                    let (reply, switched) = oneshot::channel();
                    commands
                        .try_send(CaptureWorkerCommand::SelectDisplay { display_id, reply })
                        .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?;
                    let _ = switched
                        .await
                        .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?;
                    ConfigurationApplied::Display
                }
                ConfigurationAction::SetVideoQuality(preset) => {
                    ConfigurationApplied::Quality(apply_video_quality(&commands, preset).await?)
                }
                ConfigurationAction::SetVideoProfile(profile) => {
                    ConfigurationApplied::Profile(apply_video_profile(&commands, profile).await?)
                }
                ConfigurationAction::SetVideoBitrate(bitrate) => {
                    let (reply, applied) = oneshot::channel();
                    commands
                        .try_send(CaptureWorkerCommand::SetVideoBitrate { bitrate, reply })
                        .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?;
                    ConfigurationApplied::Bitrate(
                        applied
                            .await
                            .map_err(|_| HostRuntimeError::CaptureWorkerStopped)?,
                    )
                }
            };
            Ok((operation, applied))
        }),
    }
}

fn queue_media_configuration(
    queue: &mut ConfigurationQueue,
    bitrate: &mut AdaptiveBitrate,
    action: ConfigurationAction,
) {
    queue.invalidate_kind(ConfigurationKind::Bitrate);
    bitrate.rejected();
    queue.request(action);
}

fn configuration_blocks_bitrate(
    queue: &ConfigurationQueue,
    pending: &Option<PendingConfiguration>,
) -> bool {
    queue.has_pending(ConfigurationKind::Quality)
        || queue.has_pending(ConfigurationKind::Profile)
        || pending.as_ref().is_some_and(|work| {
            matches!(
                work.operation.action.kind(),
                ConfigurationKind::Quality | ConfigurationKind::Profile
            )
        })
}

async fn next_configuration(
    pending: &mut Option<PendingConfiguration>,
) -> Result<(ConfigurationOperation, ConfigurationApplied), HostRuntimeError> {
    match pending {
        Some(operation) => operation.future.as_mut().await,
        None => std::future::pending().await,
    }
}

#[allow(clippy::too_many_arguments)]
async fn receive_input_and_control(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    peer_generation: u64,
    stream_id: u64,
    input: Arc<Mutex<InputInjector>>,
    force_keyframe: Arc<AtomicBool>,
    context: HostInboundContext,
) -> Result<(), HostRuntimeError> {
    let input_suspended = context.input_suspended.clone();
    run_inbound_lanes(
        receive_host_input(
            client.clone(),
            secure.clone(),
            peer_generation,
            stream_id,
            input,
            context.selected_desktop.clone(),
            input_suspended.clone(),
        ),
        receive_host_control(
            client,
            secure,
            peer_generation,
            stream_id,
            force_keyframe,
            context,
        ),
    )
    .await
}

async fn run_inbound_lanes<I, C>(input: I, control: C) -> Result<(), HostRuntimeError>
where
    I: std::future::Future<Output = Result<(), HostRuntimeError>>,
    C: std::future::Future<Output = Result<(), HostRuntimeError>>,
{
    // Dropping either lane also drops the other: no detached injector, stale
    // configuration completion or direct probe survives the peer generation.
    tokio::select! {
        biased;
        result = input => result,
        result = control => result,
    }
}

struct HostInputExitGuard<B: crate::input::InputBackend = crate::input::NativeInputBackend> {
    selected: Arc<Mutex<SelectedDesktop>>,
    input: Arc<Mutex<InputInjector<B>>>,
    input_suspended: Arc<AtomicBool>,
}

impl<B: crate::input::InputBackend> Drop for HostInputExitGuard<B> {
    fn drop(&mut self) {
        let mut selected = lock_unpoisoned(&self.selected);
        selected.input_closed = true;
        selected.input_invalidated = true;
        self.input_suspended.store(true, Ordering::Release);
        // Intentional host teardown, not a remotely supplied release command.
        // The capture worker may still own an Arc, so injector Drop is too late.
        let _ = lock_unpoisoned(&self.input).release_all();
    }
}

fn begin_desktop_selection<B: crate::input::InputBackend>(
    selected: &Mutex<SelectedDesktop>,
    input: &Mutex<InputInjector<B>>,
    input_suspended: &AtomicBool,
) {
    let mut selected = lock_unpoisoned(selected);
    selected.selection_pending = true;
    if lock_unpoisoned(input).release_all().is_err() {
        selected.input_invalidated = true;
        input_suspended.store(true, Ordering::Release);
    }
}

#[allow(clippy::too_many_arguments)]
async fn receive_host_input(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    peer_generation: u64,
    stream_id: u64,
    input: Arc<Mutex<InputInjector>>,
    selected_desktop: Arc<Mutex<SelectedDesktop>>,
    input_suspended: Arc<AtomicBool>,
) -> Result<(), HostRuntimeError> {
    let _exit = HostInputExitGuard {
        selected: selected_desktop.clone(),
        input: input.clone(),
        input_suspended: input_suspended.clone(),
    };
    let mut policy = HostInboundPolicy::new(stream_id);
    loop {
        tokio::select! {
            biased;
            reason = client.next_closed_reason() => {
                return Err(HostRuntimeError::TransportClosed(reason));
            }
            result = client.next_input_for_generation(peer_generation) => {
                let bytes = result?;
                let plaintext = open(&secure, SecureLane::Input, &bytes).await?;
                inject_desktop_input(
                    &mut policy, &input, &plaintext, now_micros(),
                    &selected_desktop, &input_suspended,
                )?;
            }
        }
    }
}

fn inject_desktop_input<B: crate::input::InputBackend>(
    policy: &mut HostInboundPolicy,
    input: &Mutex<InputInjector<B>>,
    plaintext: &[u8],
    now_us: u64,
    selected_desktop: &Mutex<SelectedDesktop>,
    input_suspended: &AtomicBool,
) -> Result<bool, HostRuntimeError> {
    // Context is checked before ordering state, release handling, or touching
    // the injector. A delayed release from A cannot release a key pressed in B.
    let selected = lock_unpoisoned(selected_desktop);
    let Some(context) = selected.input_context() else {
        return Ok(false);
    };
    if selected.input_closed {
        return Ok(false);
    }
    let Some(envelope) = policy.decode_input_envelope(plaintext, now_us, context)? else {
        return Ok(false);
    };
    let release = matches!(
        envelope.event,
        InputEvent::Key { pressed: false, .. } | InputEvent::MouseButton { pressed: false, .. }
    );
    if !release && (input_suspended.load(Ordering::Acquire) || selected.selection_pending) {
        return Ok(false);
    }
    // Maintain one lock order everywhere: selected mapping, then injector.
    let mut input = lock_unpoisoned(input);
    input.set_desktop(selected.desktop);
    apply_input_event(&mut input, envelope.event)?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn receive_host_control(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    peer_generation: u64,
    stream_id: u64,
    force_keyframe: Arc<AtomicBool>,
    context: HostInboundContext,
) -> Result<(), HostRuntimeError> {
    let HostInboundContext {
        capture_commands,
        selected_desktop,
        input,
        input_suspended,
        mut topology_updates,
        audio_enabled,
        audio_enabled_updates,
        audio_queue,
        audio_notify,
        mut direct_session,
        direct_connection,
        media_observer,
    } = context;
    let policy = HostInboundPolicy::new(stream_id);
    let mut video_quality = AdaptiveVideoQuality::new();
    let mut bitrate_policy = AdaptiveBitrate::new(video_quality.preset);
    let direct_probe_slots = Arc::new(Semaphore::new(DIRECT_PROBE_TASK_CAPACITY));
    let mut direct_probes = tokio::task::JoinSet::new();
    let direct_enabled = Arc::new(AtomicBool::new(true));
    let mut configurations = ConfigurationQueue::default();
    let mut pending_configuration = None;
    loop {
        if pending_configuration.is_none()
            && let Some(operation) = configurations.pop_next()
        {
            pending_configuration = Some(start_configuration(capture_commands.clone(), operation));
        }
        tokio::select! {
            completed = next_configuration(&mut pending_configuration) => {
                let (operation, applied) = completed?;
                pending_configuration = None;
                if configurations.is_current(&operation) {
                    match applied {
                        ConfigurationApplied::Display => {
                            if !configurations.has_pending_display() {
                                lock_unpoisoned(&selected_desktop).selection_pending = false;
                            }
                            let (displays, active_display_id) = selected_display_list(&selected_desktop)?;
                            send_display_list(&client, &secure, peer_generation, &displays, active_display_id).await?;
                        }
                        ConfigurationApplied::Quality(Some((actual, bitrate))) => {
                            video_quality.record_applied(actual);
                            bitrate_policy.reset_to_actual(actual, bitrate);
                            send_video_quality_state(&client, &secure, peer_generation, video_quality.preference, actual).await?;
                        }
                        ConfigurationApplied::Profile(Some((actual, bitrate))) => {
                            bitrate_policy.reset_to_actual(video_quality.preset, bitrate);
                            if operation.action != ConfigurationAction::SetVideoProfile(actual) {
                                force_keyframe.store(true, Ordering::Release);
                            }
                        }
                        ConfigurationApplied::Bitrate(Some(actual)) => { bitrate_policy.record_applied(actual); }
                        ConfigurationApplied::Bitrate(None) => { bitrate_policy.rejected(); }
                        ConfigurationApplied::Quality(None) | ConfigurationApplied::Profile(None) => {}
                    }
                }
            }
            updated = topology_updates.changed() => {
                updated.map_err(|_| HostRuntimeError::CaptureWorkerStopped)?;
                let (displays, active_display_id) = selected_display_list(&selected_desktop)?;
                send_display_list(&client, &secure, peer_generation, &displays, active_display_id).await?;
            }
            _ = direct_probes.join_next(), if !direct_probes.is_empty() => {}
            direct = accept_host_direct_connection(direct_session.clone(), secure.clone()) => {
                if let Some(connection) = direct {
                    install_direct_connection(&direct_connection, connection, &direct_enabled).await;
                } else {
                    direct_probes.abort_all();
                    stop_direct_listener(&mut direct_session, &direct_connection, &direct_enabled,
                        &force_keyframe, &media_observer, stream_id).await;
                }
            }
            result = client.next_control_for_generation(peer_generation) => {
                let bytes = result?;
                let plaintext = open(&secure, SecureLane::Control, &bytes).await?;
                if policy.handle_control(&plaintext)? {
                    force_keyframe.store(true, Ordering::Release);
                }
                match decode_control(&plaintext)? {
                    ControlMessage::SelectDisplay { display_id } => {
                        let known_display = lock_unpoisoned(&selected_desktop)
                            .topology.as_ref().is_some_and(|topology| {
                                topology.displays().iter().any(|display| display.id == display_id)
                            });
                        if known_display {
                            begin_desktop_selection(&selected_desktop, &input, &input_suspended);
                            configurations.request(ConfigurationAction::SelectDisplay(display_id));
                        } else {
                            let (displays, active_display_id) = selected_display_list(&selected_desktop)?;
                            send_display_list(&client, &secure, peer_generation, &displays, active_display_id).await?;
                        }
                    }
                    ControlMessage::SetAudioEnabled { enabled } => {
                        audio_enabled.store(enabled, Ordering::Release);
                        audio_enabled_updates.send_replace(enabled);
                        if !enabled {
                            lock_unpoisoned(&audio_queue).clear();
                        }
                        audio_notify.notify_waiters();
                    }
                    ControlMessage::SetVideoQuality { preference } => {
                        let requested = video_quality.set_preference(preference);
                        // Reassert the applied tier even for Automatic/same-tier
                        // choices: a previous slow native operation may be in flight.
                        configurations.invalidate_kind(ConfigurationKind::Quality);
                        queue_media_configuration(&mut configurations, &mut bitrate_policy, ConfigurationAction::SetVideoQuality(requested.unwrap_or(video_quality.preset)));
                    }
                    ControlMessage::SetVideoProfile { profile } => {
                        queue_media_configuration(&mut configurations, &mut bitrate_policy, ConfigurationAction::SetVideoProfile(profile));
                    }
                    ControlMessage::VideoNetworkFeedback {
                        received_packets, dropped_packets, decode_queue_peak, freshness_recoveries,
                    } => {
                        if configuration_blocks_bitrate(&configurations, &pending_configuration) { continue; }
                        let (rate, handled) = bitrate_feedback_decision(video_quality.preference, &mut bitrate_policy,
                            received_packets, dropped_packets, decode_queue_peak, freshness_recoveries);
                        if let Some(bitrate) = rate {
                            configurations.request(ConfigurationAction::SetVideoBitrate(bitrate));
                        }
                        if handled { continue; }
                        if let Some(preset) = video_quality.observe(
                            received_packets, dropped_packets, decode_queue_peak, freshness_recoveries,
                        ) {
                            queue_media_configuration(&mut configurations, &mut bitrate_policy, ConfigurationAction::SetVideoQuality(preset));
                        }
                    }
                    ControlMessage::VideoPathCandidateOffer { candidate } => {
                        let has_active = direct_connection.lock().await.as_ref().is_some_and(|path| !path.is_closed());
                        let answer = if let Some(session) = direct_session.as_ref()
                            && !has_active
                            && candidate
                                .validate(now_unix_s(), session.session_binding())
                                .is_ok()
                        {
                            let endpoint = session.endpoint();
                            let expected_binding = *session.session_binding();
                            let local_candidate = session.candidate().clone();
                            let secure_for_probe = secure.clone();
                            let direct_connection_for_probe = direct_connection.clone();
                            let direct_enabled_for_probe = direct_enabled.clone();
                            let candidate_for_probe = candidate.clone();
                            let accepted = if let Ok(permit) = direct_probe_slots
                                .clone()
                                .try_acquire_owned()
                            {
                                // JoinSet aborts pending probes when this peer
                                // generation exits; no task can publish an old path.
                                direct_probes.spawn(async move {
                                    let _permit = permit;
                                    let result = endpoint
                                        .connect_shared(
                                            &candidate_for_probe,
                                            &expected_binding,
                                            &secure_for_probe,
                                            now_unix_s(),
                                        )
                                        .await;
                                    if let Ok((connection, _)) = result {
                                        install_direct_connection(&direct_connection_for_probe, Arc::new(connection), &direct_enabled_for_probe).await;
                                    }
                                });
                                true
                            } else {
                                false
                            };
                            ControlMessage::VideoPathCandidateAnswer {
                                candidate_id: candidate.candidate_id(),
                                accepted,
                                candidate: accepted.then_some(local_candidate),
                            }
                        } else {
                            ControlMessage::VideoPathCandidateAnswer {
                                candidate_id: candidate.candidate_id(),
                                accepted: false,
                                candidate: None,
                            }
                        };
                        let answer = encode_control(&answer)?;
                        client
                            .send_control_for_generation(
                                peer_generation,
                                seal(&secure, SecureLane::Control, &answer).await?,
                            )
                            .await?;
                    }
                    _ => {}
                }
            }
            reason = client.next_closed_reason() => {
                return Err(HostRuntimeError::TransportClosed(reason));
            }
        }
    }
}

/// Input is a lossy edge of a remote session. A single sample can become
/// invalid while the controller is resizing, switching displays, or crossing
/// a stale WebView event boundary. It must never tear down the host worker or
/// make the host appear stopped; the next valid sample will continue normally.
fn apply_input_event<B: crate::input::InputBackend>(
    input: &mut InputInjector<B>,
    event: InputEvent,
) -> Result<(), HostRuntimeError> {
    match input.apply(event) {
        Ok(())
        | Err(InputInjectionError::Blocked)
        | Err(InputInjectionError::UnsupportedKey)
        | Err(InputInjectionError::InvalidInput) => Ok(()),
    }
}

async fn install_direct_connection(
    slot: &Arc<AsyncMutex<Option<Arc<DirectLanConnection>>>>,
    candidate: Arc<DirectLanConnection>,
    enabled: &AtomicBool,
) -> bool {
    let outcome = {
        let mut active = slot.lock().await;
        if enabled.load(Ordering::Acquire) {
            direct_path_slot::install(&mut active, candidate, DirectLanConnection::is_closed)
        } else {
            direct_path_slot::InstallOutcome::Retained {
                rejected: candidate,
            }
        }
    };
    // No network close/drop while holding the path arbitration mutex.
    match outcome {
        direct_path_slot::InstallOutcome::Installed { retired } => {
            if let Some(retired) = retired {
                retired.close(b"desklink retired closed direct path");
            }
            true
        }
        direct_path_slot::InstallOutcome::Retained { rejected } => {
            rejected.close(b"desklink direct path not selected");
            false
        }
        direct_path_slot::InstallOutcome::AlreadyCurrent => true,
    }
}

async fn stop_direct_listener(
    session: &mut Option<Arc<DirectLanSession>>,
    slot: &Arc<AsyncMutex<Option<Arc<DirectLanConnection>>>>,
    enabled: &AtomicBool,
    force_keyframe: &AtomicBool,
    observer: &Option<HostMediaDiagnosticsObserver>,
    stream_id: u64,
) {
    if session.take().is_none() {
        return;
    }
    let retired = {
        let mut active = slot.lock().await;
        // Serializes with late successful probes: abort_all alone cannot stop
        // a task already executing its installation step on another worker.
        enabled.store(false, Ordering::Release);
        active.take()
    };
    let retired_active = retired.is_some();
    if let Some(connection) = retired {
        connection.close(b"desklink direct listener closed");
    }
    force_keyframe.store(true, Ordering::Release);
    media_diagnostics::emit(
        observer,
        HostMediaDiagnostic::DirectListenerStopped {
            stream_id,
            retired_active,
            reason: DirectListenerStopReason::ListenerClosed,
        },
    );
}

async fn accept_host_direct_connection(
    session: Option<Arc<DirectLanSession>>,
    secure: SharedSecureSession,
) -> Option<Arc<DirectLanConnection>> {
    let Some(session) = session else {
        return std::future::pending::<Option<Arc<DirectLanConnection>>>().await;
    };
    let endpoint = session.endpoint();
    let expected_candidate_id = session.candidate().candidate_id();
    let expected_binding = *session.session_binding();
    loop {
        let incoming = endpoint.accept().await?;
        let Ok(connection) = incoming else {
            continue;
        };
        let accepted = endpoint
            .accept_probe_connection_shared(
                connection,
                expected_candidate_id,
                &expected_binding,
                &secure,
                now_unix_s(),
            )
            .await;
        if let Ok((connection, _)) = accepted {
            return Some(Arc::new(connection));
        }
    }
}

async fn receive_transfer_loop(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    peer_generation: u64,
) -> Result<(), HostRuntimeError> {
    let mut incoming = None;
    let result = receive_transfer_loop_inner(client, secure, peer_generation, &mut incoming).await;
    if let Some(file) = incoming.take() {
        let _ = file.preserve().await;
    }
    result
}

async fn receive_transfer_loop_inner(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    peer_generation: u64,
    incoming: &mut Option<IncomingFileWorker>,
) -> Result<(), HostRuntimeError> {
    let mut outgoing: Option<HostOutgoingTransfer> = None;
    let (outgoing_events, mut pending_outgoing_events) =
        mpsc::channel::<HostOutgoingEvent>(HOST_OUTGOING_EVENT_QUEUE_CAPACITY);
    loop {
        let encrypted = tokio::select! {
            event = pending_outgoing_events.recv() => {
                let Some(event) = event else {
                    continue;
                };
                if outgoing.as_ref().is_some_and(|transfer| {
                    transfer.file.transfer_id == event.transfer_id
                }) {
                    let transfer = outgoing.take().expect("checked outgoing transfer");
                    if let Err(result) = event.result
                        && result != TransferResult::Cancelled
                    {
                        let _ = send_transfer_response(
                            &client,
                            &secure,
                            peer_generation,
                            TransferMessage::FileResult {
                                transfer_id: transfer.file.transfer_id,
                                result,
                            },
                        ).await;
                    }
                }
                continue;
            }
            encrypted = client.next_transfer_for_generation(peer_generation) => encrypted?,
        };
        let plaintext = open(&secure, SecureLane::Transfer, &encrypted).await?;
        match decode_transfer(&plaintext)? {
            TransferMessage::ClipboardSet { request_id, text } => {
                let result = tokio::task::spawn_blocking(move || write_clipboard_text(&text))
                    .await
                    .unwrap_or(Err(TransferResult::IoFailed))
                    .map(|_| TransferResult::Completed)
                    .unwrap_or_else(|error| error);
                send_transfer_response(
                    &client,
                    &secure,
                    peer_generation,
                    TransferMessage::ClipboardResult { request_id, result },
                )
                .await?;
            }
            TransferMessage::ClipboardRequest { request_id } => {
                let result = tokio::task::spawn_blocking(read_clipboard_text)
                    .await
                    .unwrap_or(Err(TransferResult::IoFailed));
                let response = match result {
                    Ok(text) => TransferMessage::ClipboardData { request_id, text },
                    Err(result) => TransferMessage::ClipboardResult { request_id, result },
                };
                send_transfer_response(&client, &secure, peer_generation, response).await?;
            }
            TransferMessage::FileSelectionRequest { request_id, resume } => {
                if incoming.is_some() || outgoing.is_some() {
                    send_transfer_response(
                        &client,
                        &secure,
                        peer_generation,
                        TransferMessage::FileSelectionResult {
                            request_id,
                            result: TransferResult::Busy,
                        },
                    )
                    .await?;
                    continue;
                }
                let selected = tokio::task::spawn_blocking(|| {
                    match pick_outgoing_file("选择要发送给控制端的文件") {
                        Ok(Some(path)) => prepare_outgoing_file(path).map(Some),
                        Ok(None) => Ok(None),
                        Err(result) => Err(result),
                    }
                })
                .await
                .unwrap_or(Err(TransferResult::IoFailed));
                let mut file = match selected {
                    Ok(Some(file)) => file,
                    Ok(None) => {
                        send_transfer_response(
                            &client,
                            &secure,
                            peer_generation,
                            TransferMessage::FileSelectionResult {
                                request_id,
                                result: TransferResult::Cancelled,
                            },
                        )
                        .await?;
                        continue;
                    }
                    Err(result) => {
                        send_transfer_response(
                            &client,
                            &secure,
                            peer_generation,
                            TransferMessage::FileSelectionResult { request_id, result },
                        )
                        .await?;
                        continue;
                    }
                };
                if resume
                    .as_ref()
                    .is_some_and(|resume| resume.name == file.name && resume.size == file.size)
                {
                    file.transfer_id = resume.expect("checked resume metadata").transfer_id;
                }
                send_transfer_response(
                    &client,
                    &secure,
                    peer_generation,
                    TransferMessage::FileOffer {
                        transfer_id: file.transfer_id,
                        request_id: Some(request_id),
                        name: file.name.clone(),
                        size: file.size,
                    },
                )
                .await?;
                outgoing = Some(HostOutgoingTransfer {
                    request_id,
                    file,
                    cancellation: Arc::new(AtomicBool::new(false)),
                    phase: HostOutgoingPhase::AwaitingDecision,
                    sender_task: None,
                });
            }
            TransferMessage::FileSelectionCancel { request_id } => {
                if outgoing
                    .as_ref()
                    .is_some_and(|transfer| transfer.request_id == request_id)
                {
                    let transfer = outgoing.take().expect("checked outgoing transfer");
                    transfer.cancellation.store(true, Ordering::Release);
                }
                let _ = send_transfer_response(
                    &client,
                    &secure,
                    peer_generation,
                    TransferMessage::FileSelectionResult {
                        request_id,
                        result: TransferResult::Cancelled,
                    },
                )
                .await;
            }
            TransferMessage::FileOffer {
                transfer_id,
                request_id,
                name,
                size,
            } => {
                let (result, resume_offset, resume_prefix_hash) = if request_id.is_some() {
                    (TransferResult::Rejected, 0, None)
                } else if incoming.is_some() || outgoing.is_some() {
                    (TransferResult::Busy, 0, None)
                } else {
                    let prompt_name = name.clone();
                    let accepted =
                        tokio::task::spawn_blocking(move || confirm_file_offer(&prompt_name, size))
                            .await
                            .unwrap_or(false);
                    if !accepted {
                        (TransferResult::Rejected, 0, None)
                    } else {
                        match IncomingFileWorker::create(transfer_id, name, size).await {
                            Ok(file) => {
                                let resume_offset = file.resume_offset();
                                let resume_prefix_hash = file.resume_prefix_hash();
                                *incoming = Some(file);
                                (TransferResult::Completed, resume_offset, resume_prefix_hash)
                            }
                            Err(result) => (result, 0, None),
                        }
                    }
                };
                send_transfer_response(
                    &client,
                    &secure,
                    peer_generation,
                    TransferMessage::FileDecision {
                        transfer_id,
                        result,
                        resume_offset,
                        resume_prefix_hash,
                    },
                )
                .await?;
            }
            TransferMessage::FileDecision {
                transfer_id,
                result,
                resume_offset,
                resume_prefix_hash,
            } => {
                let matches = outgoing.as_ref().is_some_and(|transfer| {
                    transfer.file.transfer_id == transfer_id
                        && transfer.phase == HostOutgoingPhase::AwaitingDecision
                });
                if !matches {
                    continue;
                }
                if result != TransferResult::Completed
                    || outgoing
                        .as_ref()
                        .is_some_and(|transfer| resume_offset > transfer.file.size)
                {
                    outgoing.take();
                    continue;
                }
                let transfer = outgoing.as_mut().expect("checked outgoing transfer");
                transfer.phase = HostOutgoingPhase::Sending;
                let file = transfer.file.clone();
                let cancellation = transfer.cancellation.clone();
                let client = client.clone();
                let secure = secure.clone();
                let outgoing_events = outgoing_events.clone();
                transfer.sender_task = Some(tokio::spawn(async move {
                    let result = stream_host_outgoing_file(
                        client,
                        secure,
                        peer_generation,
                        file,
                        resume_offset,
                        resume_prefix_hash,
                        cancellation,
                    )
                    .await;
                    let _ = outgoing_events
                        .send(HostOutgoingEvent {
                            transfer_id,
                            result,
                        })
                        .await;
                }));
            }
            TransferMessage::FileChunk {
                transfer_id,
                offset,
                bytes,
            } => {
                let result = match incoming
                    .as_mut()
                    .filter(|file| file.transfer_id() == transfer_id)
                {
                    Some(file) => file.write_chunk(offset, bytes).await,
                    None => Err(TransferResult::InvalidData),
                };
                if let Err(result) = result {
                    if let Some(file) = incoming.take() {
                        file.cancel();
                    }
                    send_transfer_response(
                        &client,
                        &secure,
                        peer_generation,
                        TransferMessage::FileResult {
                            transfer_id,
                            result,
                        },
                    )
                    .await?;
                }
            }
            TransferMessage::FileComplete {
                transfer_id,
                content_hash,
            } => {
                let result = match incoming.take() {
                    Some(file) if file.transfer_id() == transfer_id => {
                        file.finish(content_hash).await
                    }
                    _ => Err(TransferResult::InvalidData),
                };
                let transfer_result = match result {
                    Ok(path) => {
                        tokio::task::spawn_blocking(move || notify_file_received(&path));
                        TransferResult::Completed
                    }
                    Err(error) => error,
                };
                send_transfer_response(
                    &client,
                    &secure,
                    peer_generation,
                    TransferMessage::FileResult {
                        transfer_id,
                        result: transfer_result,
                    },
                )
                .await?;
            }
            TransferMessage::Cancel { transfer_id } => {
                if incoming
                    .as_ref()
                    .is_some_and(|file| file.transfer_id() == transfer_id)
                    && let Some(file) = incoming.take()
                {
                    file.cancel();
                }
                if outgoing
                    .as_ref()
                    .is_some_and(|transfer| transfer.file.transfer_id == transfer_id)
                {
                    let transfer = outgoing.take().expect("checked outgoing transfer");
                    transfer.cancellation.store(true, Ordering::Release);
                }
                send_transfer_response(
                    &client,
                    &secure,
                    peer_generation,
                    TransferMessage::FileResult {
                        transfer_id,
                        result: TransferResult::Cancelled,
                    },
                )
                .await?;
            }
            TransferMessage::FileResult {
                transfer_id,
                result: _,
            } => {
                if outgoing
                    .as_ref()
                    .is_some_and(|transfer| transfer.file.transfer_id == transfer_id)
                {
                    let transfer = outgoing.take().expect("checked outgoing transfer");
                    transfer.cancellation.store(true, Ordering::Release);
                }
            }
            TransferMessage::ClipboardData { .. }
            | TransferMessage::ClipboardResult { .. }
            | TransferMessage::FileSelectionResult { .. } => {
                // These are host-to-controller responses. Ignore an unexpected
                // response without taking down input or video.
            }
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum HostOutgoingPhase {
    AwaitingDecision,
    Sending,
}

struct HostOutgoingTransfer {
    request_id: u64,
    file: OutgoingFile,
    cancellation: Arc<AtomicBool>,
    phase: HostOutgoingPhase,
    sender_task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for HostOutgoingTransfer {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::Release);
        // Dropping the async receiver also wakes a disk producer blocked on
        // the bounded chunk queue. Prefix scans observe cancellation per chunk.
        if let Some(task) = self.sender_task.take() {
            task.abort();
        }
    }
}

struct HostOutgoingEvent {
    transfer_id: TransferId,
    result: Result<(), TransferResult>,
}

enum HostFileReadEvent {
    Chunk { offset: u64, bytes: Vec<u8> },
    Complete([u8; 32]),
    Failed(TransferResult),
}

async fn stream_host_outgoing_file(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    peer_generation: u64,
    file: OutgoingFile,
    resume_offset: u64,
    resume_prefix_hash: Option<[u8; 32]>,
    cancellation: Arc<AtomicBool>,
) -> Result<(), TransferResult> {
    if resume_offset > file.size {
        return Err(TransferResult::InvalidData);
    }
    let (events, mut receiver) = mpsc::channel(2);
    let producer_cancellation = cancellation.clone();
    let source = file.clone();
    tokio::task::spawn_blocking(move || {
        let result = (|| {
            // Prefix proof and complete-content hashing share one opened source
            // and hasher. A retry no longer reads the prefix twice.
            let mut reader = OutgoingFileReader::open(
                &source,
                resume_offset,
                resume_prefix_hash,
                producer_cancellation,
            )?;
            while let Some((offset, bytes)) = reader.read_chunk()? {
                events
                    .blocking_send(HostFileReadEvent::Chunk { offset, bytes })
                    .map_err(|_| TransferResult::IoFailed)?;
            }
            events
                .blocking_send(HostFileReadEvent::Complete(reader.finish()?))
                .map_err(|_| TransferResult::IoFailed)?;
            Ok::<(), TransferResult>(())
        })();
        if let Err(error) = result {
            let _ = events.blocking_send(HostFileReadEvent::Failed(error));
        }
    });

    while let Some(event) = receiver.recv().await {
        if cancellation.load(Ordering::Acquire) {
            return Err(TransferResult::Cancelled);
        }
        let message = match event {
            HostFileReadEvent::Chunk { offset, bytes } => TransferMessage::FileChunk {
                transfer_id: file.transfer_id,
                offset,
                bytes,
            },
            HostFileReadEvent::Complete(content_hash) => TransferMessage::FileComplete {
                transfer_id: file.transfer_id,
                content_hash,
            },
            HostFileReadEvent::Failed(error) => return Err(error),
        };
        let complete = matches!(message, TransferMessage::FileComplete { .. });
        tokio::time::timeout(
            FILE_TRANSFER_SEND_TIMEOUT,
            send_transfer_response(&client, &secure, peer_generation, message),
        )
        .await
        .map_err(|_| TransferResult::IoFailed)?
        .map_err(|_| TransferResult::IoFailed)?;
        if complete {
            return Ok(());
        }
    }
    Err(TransferResult::IoFailed)
}

async fn send_transfer_response(
    client: &QuicClient,
    secure: &SharedSecureSession,
    peer_generation: u64,
    message: TransferMessage,
) -> Result<(), HostRuntimeError> {
    let plaintext = encode_transfer(&message)?;
    let ciphertext = seal(secure, SecureLane::Transfer, &plaintext).await?;
    client
        .send_transfer_for_generation(peer_generation, ciphertext)
        .await?;
    Ok(())
}

async fn send_cursor_loop(
    client: Arc<QuicClient>,
    secure: SharedSecureSession,
    peer_generation: u64,
    stream_id: u64,
    selected_desktop: Arc<Mutex<SelectedDesktop>>,
    observer: Option<HostMediaDiagnosticsObserver>,
) -> Result<(), HostRuntimeError> {
    send_cursor_samples(stream_id, observer, |sequence| {
        let desktop = lock_unpoisoned(&selected_desktop).desktop.rect;
        let cursor = sample_cursor(desktop, stream_id, sequence);
        let client = &client;
        let secure = &secure;
        async move {
            let Some(cursor) = cursor else {
                return Ok(None);
            };
            let plaintext = encode_cursor_update(&cursor)?;
            let sent = client
                .send_cursor_datagram_for_generation(
                    peer_generation,
                    seal(secure, SecureLane::CursorDatagram, &plaintext).await?,
                )
                .await;
            Ok(Some(sent))
        }
    })
    .await
}

async fn send_cursor_samples<F, Fut>(
    stream_id: u64,
    observer: Option<HostMediaDiagnosticsObserver>,
    mut sample_and_send: F,
) -> Result<(), HostRuntimeError>
where
    F: FnMut(u64) -> Fut,
    Fut: std::future::Future<Output = Result<Option<Result<(), TransportError>>, HostRuntimeError>>,
{
    let mut interval = tokio::time::interval(CURSOR_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut sequence = 0_u64;
    let mut state = cursor_send::CursorSendState::default();
    loop {
        if state.is_disabled() {
            // QUIC capability/size limits are fixed for this peer. Park this
            // optional future; connection closure is consumed by inbound.
            return std::future::pending().await;
        }
        interval.tick().await;
        sequence = sequence.wrapping_add(1).max(1);
        let Some(sent) = sample_and_send(sequence).await? else {
            continue;
        };
        let outcome = state.record(sent)?;
        if outcome.should_report()
            && let cursor_send::CursorSendOutcome::OptionalDisabled { reason, .. } = outcome
        {
            let snapshot = outcome.snapshot();
            media_diagnostics::emit(
                &observer,
                HostMediaDiagnostic::CursorSend {
                    stream_id,
                    send_attempts: snapshot.send_attempts,
                    send_errors: snapshot.send_errors,
                    disabled: snapshot.disabled,
                    reason,
                },
            );
        }
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
    use std::time::Instant;

    use crate::{
        capture::CaptureError,
        encoder::EncoderError,
        input::{InputBackend, InputInjectionError, InputInjector, VirtualDesktop},
    };

    use desklink_protocol::AccessDenialReason;
    use desklink_transport::TransportError;

    use super::{
        AdaptiveVideoQuality, HostRuntimeError, VideoFramePacer, encoder_error_is_recoverable,
        host_error_is_retryable, host_error_is_retryable_for_session,
        host_error_rearms_on_connected_relay, host_runtime_denial_reason,
        initial_encoded_frame_deadline_expired, initial_frame_deadline_expired,
        video_quality_settings,
    };
    use desklink_protocol::{KeyCode, Modifiers, VideoQualityPreference, VideoQualityPreset};
    use desklink_session::DesktopRect;

    struct RejectingInputBackend(InputInjectionError);

    use desklink_protocol::{
        InputContext, InputEnvelope, InputEvent, decode_video_config, encode_input,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    type RecordedTopologyInputs = Arc<Mutex<Vec<(InputEvent, VirtualDesktop)>>>;

    #[derive(Clone)]
    struct RecordingTopologyInputBackend {
        sent: RecordedTopologyInputs,
        block_releases: Arc<AtomicBool>,
    }

    impl InputBackend for RecordingTopologyInputBackend {
        fn send(
            &mut self,
            event: &InputEvent,
            desktop: VirtualDesktop,
        ) -> Result<(), InputInjectionError> {
            if self.block_releases.load(Ordering::Acquire)
                && matches!(
                    event,
                    InputEvent::Key { pressed: false, .. }
                        | InputEvent::MouseButton { pressed: false, .. }
                )
            {
                return Err(InputInjectionError::Blocked);
            }
            self.sent.lock().unwrap().push((event.clone(), desktop));
            Ok(())
        }
    }

    fn topology_pair() -> crate::capture::DisplayTopologySnapshot {
        crate::capture::DisplayTopologySnapshot::new(
            vec![
                crate::capture::DisplayDescriptor {
                    id: 7,
                    name: "secondary".into(),
                    rect: DesktopRect::new(-1280, 0, 1280, 720),
                    primary: false,
                },
                crate::capture::DisplayDescriptor {
                    id: 19,
                    name: "primary".into(),
                    rect: DesktopRect::new(0, 0, 1920, 1080),
                    primary: true,
                },
            ],
            7,
            DesktopRect::new(-1280, 0, 3200, 1080),
        )
        .unwrap()
    }

    fn key_event(pressed: bool) -> InputEvent {
        InputEvent::Key {
            code: KeyCode::Character('a'),
            pressed,
            modifiers: Modifiers::default(),
        }
    }

    fn test_encoded_frame(
        frame_id: u64,
        config_version: u32,
        keyframe: bool,
    ) -> crate::encoder::EncodedFrame {
        crate::encoder::EncodedFrame {
            frame_id,
            config_version,
            keyframe,
            timestamp_us: frame_id * 1_000,
            access_unit: vec![frame_id as u8; 2_500],
            sequence_header: Some(vec![0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2]),
        }
    }

    struct EpochFixture<'a> {
        epochs: &'a super::InputEpochAllocator,
        selected: Arc<Mutex<super::SelectedDesktop>>,
        input: Arc<Mutex<InputInjector<RecordingTopologyInputBackend>>>,
        suspended: Arc<AtomicBool>,
        updates: tokio::sync::watch::Sender<u64>,
        sent: RecordedTopologyInputs,
        block_releases: Arc<AtomicBool>,
        pipeline: super::HostVideoPipeline,
        policy: super::HostInboundPolicy,
        next_frame_id: u64,
        #[cfg(windows)]
        encoder: crate::encoder::H264Encoder,
        #[cfg(not(windows))]
        model_config_version: u32,
    }

    impl EpochFixture<'static> {
        fn new() -> Self {
            Self::with_epochs(super::process_input_epochs().unwrap())
        }
    }

    impl<'a> EpochFixture<'a> {
        fn with_epochs(epochs: &'a super::InputEpochAllocator) -> Self {
            let sent = Arc::new(Mutex::new(Vec::new()));
            let block_releases = Arc::new(AtomicBool::new(false));
            let (updates, _) = tokio::sync::watch::channel(0);
            let fixture = Self {
                epochs,
                selected: Arc::new(Mutex::new(super::SelectedDesktop::unconfigured(9).unwrap())),
                input: Arc::new(Mutex::new(InputInjector::with_backend(
                    VirtualDesktop::single(DesktopRect::new(0, 0, 1, 1)),
                    RecordingTopologyInputBackend {
                        sent: sent.clone(),
                        block_releases: block_releases.clone(),
                    },
                ))),
                suspended: Arc::new(AtomicBool::new(false)),
                updates,
                sent,
                block_releases,
                pipeline: super::HostVideoPipeline::new(9),
                policy: super::HostInboundPolicy::new(9),
                next_frame_id: 1,
                #[cfg(windows)]
                encoder: crate::encoder::H264Encoder::new_with_settings_and_backend(
                    1280,
                    720,
                    crate::encoder::H264EncoderSettings::default(),
                    crate::encoder::EncoderBackendPreference::SoftwareOnly,
                )
                .unwrap(),
                #[cfg(not(windows))]
                model_config_version: 1,
            };
            assert!(
                fixture
                    .coordinator()
                    .publish(topology_pair(), fixture.encoder_version())
                    .unwrap()
            );
            fixture
        }

        fn coordinator(
            &self,
        ) -> super::CaptureDesktopCoordinator<'_, RecordingTopologyInputBackend> {
            super::CaptureDesktopCoordinator {
                selected: &self.selected,
                input: &self.input,
                epochs: self.epochs,
                input_suspended: &self.suspended,
                updates: &self.updates,
            }
        }

        fn encoder_version(&self) -> u32 {
            #[cfg(windows)]
            {
                self.encoder.config_version()
            }
            #[cfg(not(windows))]
            {
                self.model_config_version
            }
        }

        fn reconfigure_encoder(&mut self, rect: DesktopRect) {
            #[cfg(windows)]
            self.encoder
                .reconfigure_for_source(
                    rect.width,
                    rect.height,
                    crate::encoder::H264EncoderSettings::default(),
                )
                .unwrap();
            #[cfg(not(windows))]
            {
                // Portable policy tests have no Media Foundation backend. The
                // native Windows fixture above uses the real encoder getter.
                let _ = rect;
                self.model_config_version = self.model_config_version.checked_add(1).unwrap();
            }
        }

        fn publish_prepared(
            &mut self,
            topology: crate::capture::DisplayTopologySnapshot,
        ) -> Result<bool, HostRuntimeError> {
            self.reconfigure_encoder(topology.active_display().rect);
            self.coordinator().publish(topology, self.encoder_version())
        }

        fn advance_codec_only(&mut self) {
            let rect = self.selected.lock().unwrap().desktop.rect;
            self.reconfigure_encoder(rect);
        }

        fn wire_context(&mut self) -> InputContext {
            let selected = self.selected.lock().unwrap();
            let context = selected.input_context().unwrap();
            let rect = selected.desktop.rect;
            drop(selected);
            let frame = test_encoded_frame(self.next_frame_id, self.encoder_version(), true);
            self.next_frame_id += 1;
            let super::PrepareVideo::Ready(prepared) = self
                .pipeline
                .prepare(frame, rect.width, rect.height, context.input_epoch)
                .unwrap()
            else {
                panic!("expected host video configuration");
            };
            let configuration =
                decode_video_config(prepared.video_config.as_deref().expect("new configuration"))
                    .unwrap();
            // All strict-input tests take their identity from the real host
            // pipeline's encoded/decoded VideoConfig, never a default epoch.
            InputContext {
                stream_id: configuration.stream_id,
                input_epoch: configuration.input_epoch,
            }
        }

        #[cfg(windows)]
        fn actual_media_context(
            &mut self,
            media: crate::capture::CaptureMediaFrame,
            timestamp_base: u64,
        ) -> (
            InputContext,
            crate::encoder::EncodedFrame,
            crate::encoder::EncoderPerformanceSample,
        ) {
            let expected = self.selected.lock().unwrap().input_context().unwrap();
            let mut identities = super::FrameInputContexts::default();
            for tick in 0..32_u64 {
                let timestamp_us = timestamp_base + tick * 33_333;
                let frame = match &media {
                    crate::capture::CaptureMediaFrame::Gpu(frame) => {
                        crate::capture::CaptureMediaFrame::Gpu(frame.with_timestamp(timestamp_us))
                    }
                    crate::capture::CaptureMediaFrame::Cpu(frame) => {
                        crate::capture::CaptureMediaFrame::Cpu(crate::capture::CapturedFrame {
                            timestamp_us,
                            ..frame.clone()
                        })
                    }
                };
                let result = super::encode_media_frame(&mut self.encoder, frame, true);
                let sample = self
                    .encoder
                    .last_performance_sample()
                    .expect("submission identity");
                assert!(identities.record_submission(sample, expected));
                let output = match result {
                    Ok(output) => output,
                    Err(EncoderError::NeedMoreInput) => continue,
                    Err(error) => panic!("actual media encode failed: {error:?}"),
                };
                let captured = identities
                    .take_output(&output)
                    .expect("actual output identity");
                assert_eq!(captured, expected);
                let (width, height) = self.encoder.dimensions();
                let super::PrepareVideo::Ready(prepared) = self
                    .pipeline
                    .prepare(output.clone(), width, height, captured.input_epoch)
                    .unwrap()
                else {
                    continue;
                };
                let configuration = decode_video_config(
                    prepared
                        .video_config
                        .as_deref()
                        .expect("new actual encoder config"),
                )
                .unwrap();
                assert_eq!(configuration.config_version, output.config_version);
                let context = InputContext {
                    stream_id: configuration.stream_id,
                    input_epoch: configuration.input_epoch,
                };
                assert_eq!(context, expected);
                return (context, output, sample);
            }
            panic!("no actual media output within bounded submissions");
        }

        fn inject(&mut self, context: InputContext, sequence: u64, event: InputEvent) -> bool {
            let envelope = InputEnvelope {
                stream_id: context.stream_id,
                input_epoch: context.input_epoch,
                sequence,
                timestamp_us: 1_000_000 + sequence,
                event,
            };
            super::inject_desktop_input(
                &mut self.policy,
                &self.input,
                &encode_input(&envelope).unwrap(),
                1_000_000 + sequence,
                &self.selected,
                &self.suspended,
            )
            .unwrap()
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "opt-in actual GPU surface encoding and synthetic epoch test; no desktop/input injection"]
    fn gpu_media_backend_changes_preserve_epochs_and_same_size_geometry_rejects_old_outputs() {
        use crate::{
            capture::{CaptureMediaFrame, CapturedFrame},
            gpu::{GpuBgraPool, create_device, create_synthetic_bgra, enumerate_adapters},
        };
        let adapter = enumerate_adapters()
            .unwrap()
            .into_iter()
            .find(|a| !a.software)
            .expect("hardware adapter");
        let device = create_device(adapter.index).unwrap();
        let pixels = [24u8, 100, 200, 255].repeat(1280 * 720);
        let source = create_synthetic_bgra(&device, 1280, 720, &pixels).unwrap();
        let pool = GpuBgraPool::from_device(device.clone(), 1280, 720, 2).unwrap();
        let gpu = pool.copy_from(&source, 1).unwrap();
        let mut fixture = EpochFixture::new();
        fixture.encoder = crate::encoder::H264Encoder::new_with_settings(
            1280,
            720,
            crate::encoder::H264EncoderSettings::default(),
        )
        .unwrap();
        let (initial, old_output, old_sample) =
            fixture.actual_media_context(CaptureMediaFrame::Gpu(gpu.clone()), 10_000_000);
        assert!(
            old_sample.gpu_surface_input,
            "{:?}",
            fixture.encoder.gpu_status()
        );
        assert_eq!(fixture.encoder.gpu_status().cpu_fallbacks, 0);
        assert!(fixture.inject(initial, 1, key_event(true)));
        let initial_version = old_output.config_version;
        let (cpu_context, cpu_output, cpu_sample) = fixture.actual_media_context(
            CaptureMediaFrame::Cpu(CapturedFrame {
                width: 1280,
                height: 720,
                timestamp_us: 0,
                pixels,
            }),
            20_000_000,
        );
        assert_eq!(cpu_context, initial);
        assert!(!cpu_sample.gpu_surface_input);
        assert!(cpu_output.keyframe && cpu_output.config_version > initial_version);
        assert!(fixture.inject(cpu_context, 2, key_event(false)));
        assert!(
            super::prefer_gpu_input(&fixture.encoder),
            "normal CPU transition must not latch a GPU failure"
        );
        let (rebound, rebound_output, rebound_sample) =
            fixture.actual_media_context(CaptureMediaFrame::Gpu(gpu), 30_000_000);
        assert_eq!(rebound, initial);
        assert!(
            rebound_sample.gpu_surface_input
                && rebound_output.config_version > cpu_output.config_version
        );
        assert!(fixture.inject(rebound, 3, key_event(true)));
        fixture.advance_codec_only();
        let codec_source =
            create_synthetic_bgra(&device, 1280, 720, &[80u8, 30, 140, 255].repeat(1280 * 720))
                .unwrap();
        let codec_frame = pool.copy_from(&codec_source, 1).unwrap();
        let (codec_context, _, codec_sample) =
            fixture.actual_media_context(CaptureMediaFrame::Gpu(codec_frame), 40_000_000);
        assert_eq!(codec_context, initial);
        assert!(codec_sample.gpu_surface_input);
        let current = fixture.coordinator().current().unwrap();
        let mut displays = current.displays().to_vec();
        displays
            .iter_mut()
            .find(|d| d.id == current.active_display_id())
            .unwrap()
            .rect
            .left += 10;
        let next = current
            .refreshed(displays, current.virtual_desktop())
            .unwrap()
            .unwrap();
        assert_eq!(
            next.active_display().rect.width,
            current.active_display().rect.width
        );
        fixture.coordinator().suspend();
        assert!(fixture.publish_prepared(next).unwrap());
        let next_source = create_synthetic_bgra(
            &device,
            1280,
            720,
            &[112u8, 66, 170, 255].repeat(1280 * 720),
        )
        .unwrap();
        let next_frame = pool.copy_from(&next_source, 1).unwrap();
        let (renewed, new_output, new_sample) =
            fixture.actual_media_context(CaptureMediaFrame::Gpu(next_frame), 50_000_000);
        assert_ne!(renewed.input_epoch, initial.input_epoch);
        assert_eq!(renewed.stream_id, initial.stream_id);
        assert!(
            new_sample.gpu_surface_input && new_output.config_version > old_output.config_version
        );
        let mut queued = super::FrameInputContexts::default();
        assert!(queued.record_submission(old_sample, initial));
        assert!(queued.record_submission(new_sample, renewed));
        assert!(
            queued.take_output(&old_output).is_none(),
            "an old actual GPU output cannot acquire the new geometry context"
        );
        assert!(
            fixture
                .pipeline
                .prepare(old_output, 1280, 720, renewed.input_epoch)
                .is_err()
        );
        assert!(fixture.inject(renewed, 4, key_event(true)));
        assert!(!fixture.inject(initial, 9000, key_event(false)));
        assert!(
            fixture.inject(renewed, 5, key_event(false)),
            "stale release must not consume the new context sequence"
        );
    }

    #[test]
    fn topology_publication_keeps_list_active_output_and_input_context_together() {
        let mut fixture = EpochFixture::new();
        let before = fixture.wire_context();
        let current = fixture.coordinator().current().unwrap();
        let surviving = current
            .displays()
            .iter()
            .find(|display| display.id == 19)
            .unwrap()
            .clone();
        let next = current
            .refreshed(vec![surviving.clone()], surviving.rect)
            .unwrap()
            .unwrap();
        assert!(fixture.coordinator().suspend());
        assert_eq!(fixture.selected.lock().unwrap().display_id, 7);
        assert!(fixture.publish_prepared(next).unwrap());
        let after = fixture.wire_context();
        assert_ne!(before.input_epoch, after.input_epoch);
        let published = fixture.selected.lock().unwrap();
        assert_eq!(published.display_id, 19);
        assert_eq!(published.desktop, VirtualDesktop::single(surviving.rect));
        assert_eq!(published.input_context(), Some(after));
        drop(published);
        let (list, active) = super::selected_display_list(&fixture.selected).unwrap();
        assert_eq!(active, 19);
        assert_eq!(
            (list.len(), list[0].id, list[0].width, list[0].height),
            (1, 19, 1920, 1080)
        );
        assert!(!fixture.suspended.load(Ordering::Acquire));
    }

    #[test]
    fn old_wire_releases_and_pending_samples_cannot_touch_new_epoch_or_reused_display() {
        let mut fixture = EpochFixture::new();
        let first_a = fixture.wire_context();
        assert!(fixture.inject(first_a, 1, key_event(true)));
        let b = fixture
            .coordinator()
            .current()
            .unwrap()
            .select(19)
            .unwrap()
            .unwrap();
        assert!(fixture.coordinator().suspend());
        assert!(fixture.publish_prepared(b).unwrap());
        let context_b = fixture.wire_context();
        assert!(fixture.inject(context_b, 2, key_event(true)));
        let before = fixture.sent.lock().unwrap().len();
        let old_events = [
            InputEvent::MouseMove {
                x: 500_000,
                y: 500_000,
            },
            key_event(true),
            key_event(false),
            InputEvent::Key {
                code: KeyCode::Character('中'),
                pressed: true,
                modifiers: Modifiers::default(),
            },
            InputEvent::Key {
                code: KeyCode::Character('中'),
                pressed: false,
                modifiers: Modifiers::default(),
            },
            InputEvent::MouseButton {
                button: desklink_protocol::MouseButton::Left,
                pressed: true,
            },
            InputEvent::MouseButton {
                button: desklink_protocol::MouseButton::Left,
                pressed: false,
            },
            InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 120,
            },
        ];
        // Includes the key/button-up expansion used by a queued ReleaseAll.
        for (index, event) in old_events.into_iter().enumerate() {
            assert!(!fixture.inject(first_a, 1_000 + index as u64, event));
        }
        assert_eq!(fixture.sent.lock().unwrap().len(), before);
        assert!(!fixture.inject(
            InputContext {
                stream_id: 8,
                ..context_b
            },
            9_000,
            key_event(false)
        ));
        // Rejected old high sequences must not poison current ordering either.
        assert!(fixture.inject(context_b, 3, InputEvent::MouseMove { x: 100, y: 100 }));
        let second_a = fixture
            .coordinator()
            .current()
            .unwrap()
            .select(7)
            .unwrap()
            .unwrap();
        assert!(fixture.coordinator().suspend());
        assert!(fixture.publish_prepared(second_a).unwrap());
        let second_a = fixture.wire_context();
        assert_ne!(second_a, first_a);
        assert_ne!(second_a, context_b);
        assert!(fixture.inject(second_a, 4, key_event(true)));
        let before = fixture.sent.lock().unwrap().len();
        assert!(!fixture.inject(first_a, 2_000, key_event(false)));
        assert!(!fixture.inject(context_b, 2_001, key_event(false)));
        assert_eq!(fixture.sent.lock().unwrap().len(), before);
        assert!(fixture.inject(second_a, 5, key_event(false)));
    }

    #[test]
    fn metadata_and_media_configuration_versions_preserve_epoch_and_held_keys() {
        let mut fixture = EpochFixture::new();
        let context = fixture.wire_context();
        let initial_anchor = fixture.selected.lock().unwrap().input_config_anchor;
        assert!(fixture.inject(context, 1, key_event(true)));
        let current = fixture.coordinator().current().unwrap();
        let mut displays = current.displays().to_vec();
        displays[0].name = "updated source label".into();
        displays[0].primary = true;
        displays[1].primary = false;
        let next = current
            .refreshed(displays, current.virtual_desktop())
            .unwrap()
            .unwrap();
        assert!(
            fixture
                .coordinator()
                .publish(next, fixture.encoder_version())
                .unwrap()
        );
        assert_eq!(
            fixture.sent.lock().unwrap().len(),
            1,
            "metadata must not release a held key"
        );
        // Quality/profile changes advance codec config versions, not geometry
        // epochs. Both identities come back from actual host VideoConfig bytes.
        fixture.advance_codec_only();
        assert_eq!(fixture.wire_context(), context);
        assert_eq!(
            fixture.selected.lock().unwrap().input_config_anchor,
            initial_anchor
        );
        fixture.advance_codec_only();
        assert_eq!(fixture.wire_context(), context);
        assert_eq!(fixture.sent.lock().unwrap().len(), 1);
        assert!(fixture.inject(context, 2, key_event(false)));
    }

    #[test]
    fn same_dimensions_selection_and_virtual_origin_require_prepared_encoder_version() {
        let mut fixture = EpochFixture::new();
        let first = fixture.wire_context();
        let current = fixture.coordinator().current().unwrap();
        let mut displays = current.displays().to_vec();
        displays[1].rect = DesktopRect::new(0, 0, 1280, 720);
        let same_size = current
            .refreshed(displays, DesktopRect::new(-1280, 0, 2560, 720))
            .unwrap()
            .unwrap();
        assert!(fixture.coordinator().suspend());
        assert!(fixture.publish_prepared(same_size).unwrap());
        let normalized = fixture.wire_context();
        assert_ne!(normalized, first);
        let current = fixture.coordinator().current().unwrap();
        let selected = current.select(19).unwrap().unwrap();
        assert_eq!(
            current.active_display().rect.width,
            selected.active_display().rect.width
        );
        assert_eq!(
            current.active_display().rect.height,
            selected.active_display().rect.height
        );
        let old_version = fixture.encoder_version();
        assert!(fixture.coordinator().suspend());
        assert_eq!(
            fixture.coordinator().publish(selected.clone(), old_version),
            Err(HostRuntimeError::InconsistentVideoConfig)
        );
        assert_eq!(
            fixture.selected.lock().unwrap().input_context(),
            Some(normalized)
        );
        assert_eq!(fixture.selected.lock().unwrap().display_id, 7);
        assert!(fixture.publish_prepared(selected).unwrap());
        assert!(fixture.encoder_version() > old_version);
        let selected_context = fixture.wire_context();
        assert_ne!(selected_context, normalized);
        let current = fixture.coordinator().current().unwrap();
        let mut displays = current.displays().to_vec();
        displays[0].rect.left = -1400;
        let origin_changed = current
            .refreshed(displays, DesktopRect::new(-1400, 0, 2680, 720))
            .unwrap()
            .unwrap();
        assert_eq!(
            origin_changed.active_display().rect,
            current.active_display().rect
        );
        assert!(fixture.coordinator().suspend());
        assert_eq!(
            fixture
                .coordinator()
                .publish(origin_changed.clone(), fixture.encoder_version()),
            Err(HostRuntimeError::InconsistentVideoConfig)
        );
        assert!(fixture.publish_prepared(origin_changed).unwrap());
        let shifted_context = fixture.wire_context();
        assert_ne!(shifted_context, selected_context);
        assert_eq!(
            fixture
                .coordinator()
                .publish(fixture.coordinator().current().unwrap(), 0),
            Err(HostRuntimeError::InconsistentVideoConfig)
        );
        assert!(!fixture.inject(selected_context, 100, key_event(true)));
        assert!(fixture.inject(shifted_context, 1, key_event(true)));
    }

    #[test]
    fn encoder_version_exhaustion_ends_configuration_instead_of_retrying_or_wrapping() {
        let failure = HostRuntimeError::Encoder(EncoderError::ConfigVersionExhausted);
        assert!(!super::encoder_error_is_recoverable(
            &EncoderError::ConfigVersionExhausted
        ));
        assert_eq!(
            super::recoverable_configuration::<()>(Err(failure)),
            Err(HostRuntimeError::Encoder(
                EncoderError::ConfigVersionExhausted
            ))
        );
        assert_eq!(
            super::recoverable_configuration::<()>(Err(HostRuntimeError::Encoder(
                EncoderError::Native("transient".into())
            ))),
            Ok(None)
        );
    }

    #[test]
    fn invalidity_renewal_rotates_epoch_and_blocked_releases_keep_old_mapping_gated() {
        let mut fixture = EpochFixture::new();
        let context = fixture.wire_context();
        assert!(fixture.inject(context, 1, key_event(true)));
        fixture.block_releases.store(true, Ordering::Release);
        let current = fixture.coordinator().current().unwrap();
        let next = current.select(19).unwrap().unwrap();
        assert!(!fixture.coordinator().suspend());
        assert!(!fixture.publish_prepared(next).unwrap());
        assert_eq!(
            fixture.selected.lock().unwrap().input_context(),
            Some(context)
        );
        assert_eq!(fixture.selected.lock().unwrap().display_id, 7);
        assert!(fixture.suspended.load(Ordering::Acquire));
        assert!(!fixture.inject(context, 2, key_event(true)));
        fixture.block_releases.store(false, Ordering::Release);
        // Restore the same physical display after an invalid interval. Identity
        // still advances even though active ID and all geometry are unchanged.
        assert!(
            fixture
                .publish_prepared(current.renewed().unwrap())
                .unwrap()
        );
        let restored = fixture.wire_context();
        assert_ne!(restored, context);
        assert_eq!(fixture.selected.lock().unwrap().display_id, 7);
        assert!(!fixture.inject(context, 1_000, key_event(false)));
        assert!(fixture.inject(restored, 3, key_event(true)));
    }

    #[test]
    fn input_epoch_allocator_never_wraps_or_reuses_an_exhausted_counter() {
        let allocator = super::InputEpochAllocator::new();
        assert_eq!(allocator.allocate().unwrap().get(), 1);
        assert_eq!(allocator.allocate().unwrap().get(), 2);
        allocator.0.store(u64::MAX - 1, Ordering::Release);
        assert_eq!(allocator.allocate().unwrap().get(), u64::MAX);
        assert_eq!(
            allocator.allocate(),
            Err(HostRuntimeError::InputEpochExhausted)
        );
        assert_eq!(
            allocator.allocate(),
            Err(HostRuntimeError::InputEpochExhausted)
        );
        assert_eq!(allocator.0.load(Ordering::Acquire), u64::MAX);
        assert!(!super::host_error_is_retryable(
            &HostRuntimeError::InputEpochExhausted
        ));
    }

    #[test]
    fn process_epoch_seed_is_full_width_fallible_and_never_redrawn_or_clamped() {
        struct FixedRandom {
            seed: u64,
            fail: bool,
            calls: usize,
        }
        impl rand_core::RngCore for FixedRandom {
            fn next_u32(&mut self) -> u32 {
                panic!("use fallible entropy only")
            }
            fn next_u64(&mut self) -> u64 {
                panic!("use fallible entropy only")
            }
            fn fill_bytes(&mut self, _: &mut [u8]) {
                panic!("use fallible entropy only")
            }
            fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
                self.calls += 1;
                if self.fail {
                    return Err(rand_core::Error::from(
                        std::num::NonZeroU32::new(rand_core::Error::CUSTOM_START).unwrap(),
                    ));
                }
                destination.copy_from_slice(&self.seed.to_be_bytes());
                Ok(())
            }
        }
        let mut full_width = FixedRandom {
            seed: u64::MAX,
            fail: false,
            calls: 0,
        };
        let allocator = super::InputEpochAllocator::from_rng(&mut full_width).unwrap();
        assert_eq!(allocator.allocate().unwrap().get(), u64::MAX);
        assert_eq!(
            allocator.allocate(),
            Err(HostRuntimeError::InputEpochExhausted)
        );
        assert_eq!(full_width.calls, 1);
        let mut zero = FixedRandom {
            seed: 0,
            fail: false,
            calls: 0,
        };
        assert!(matches!(
            super::InputEpochAllocator::from_rng(&mut zero),
            Err(super::InputEpochSeedError::Zero)
        ));
        assert_eq!(zero.calls, 1);
        let mut failed = FixedRandom {
            seed: 123,
            fail: true,
            calls: 0,
        };
        assert!(matches!(
            super::InputEpochAllocator::from_rng(&mut failed),
            Err(super::InputEpochSeedError::RandomUnavailable)
        ));
        assert_eq!(failed.calls, 1);
        assert!(!super::host_error_is_retryable(
            &HostRuntimeError::InputEpochZeroSeed
        ));
        assert!(!super::host_error_is_retryable(
            &HostRuntimeError::InputEpochRandomUnavailable
        ));
    }

    #[test]
    fn new_process_seed_and_new_authenticated_attempt_reject_old_plaintext_with_same_stream_id() {
        let old_process = super::InputEpochAllocator::from_seed(9_007_199_254_740_993).unwrap();
        let new_process = super::InputEpochAllocator::from_seed(18_014_398_509_481_987).unwrap();
        let mut old = EpochFixture::with_epochs(&old_process);
        let old_context = old.wire_context();
        assert_eq!(old_context.input_epoch, 9_007_199_254_740_993);
        let mut restarted = EpochFixture::with_epochs(&new_process);
        let new_context = restarted.wire_context();
        assert_eq!(new_context.input_epoch, 18_014_398_509_481_987);
        assert_eq!(old_context.stream_id, new_context.stream_id);
        assert!(!restarted.inject(old_context, 100, key_event(true)));
        assert!(restarted.inject(new_context, 1, key_event(true)));
        let mut new_attempt = EpochFixture::with_epochs(&new_process);
        let next_context = new_attempt.wire_context();
        assert_eq!(next_context.input_epoch, new_context.input_epoch + 1);
        assert!(!new_attempt.inject(old_context, 100, key_event(true)));
        assert!(!new_attempt.inject(new_context, 101, key_event(false)));
        assert!(new_attempt.inject(next_context, 1, key_event(true)));
    }

    #[test]
    fn buffered_old_encoder_output_cannot_be_relabelled_with_new_geometry_epoch() {
        let mut fixture = EpochFixture::new();
        let old_context = fixture.wire_context();
        let old_version = fixture.encoder_version();
        let mut bindings = super::FrameInputContexts::default();
        assert!(bindings.record_submission(
            crate::encoder::EncoderPerformanceSample {
                frame_id: 41,
                config_version: old_version,
                ..Default::default()
            },
            old_context
        ));
        // The native MFT buffers frame 41. Switch geometry before it returns.
        let next = fixture
            .coordinator()
            .current()
            .unwrap()
            .select(19)
            .unwrap()
            .unwrap();
        assert!(fixture.coordinator().suspend());
        assert!(fixture.publish_prepared(next).unwrap());
        let new_version = fixture.encoder_version();
        assert!(new_version > old_version);
        let captured_new_context = fixture.selected.lock().unwrap().input_context().unwrap();
        assert!(bindings.record_submission(
            crate::encoder::EncoderPerformanceSample {
                frame_id: 42,
                config_version: new_version,
                ..Default::default()
            },
            captured_new_context
        ));
        assert_eq!(
            bindings.take_output(&test_encoded_frame(41, old_version, true)),
            None
        );
        // Even a buggy old output with a current config number cannot inherit
        // the new sample's identity just because encode() returned it now.
        assert_eq!(
            bindings.take_output(&test_encoded_frame(41, new_version, true)),
            None
        );
        let output = test_encoded_frame(42, new_version, true);
        let encoded_context = bindings.take_output(&output).unwrap();
        let super::PrepareVideo::Ready(prepared) = fixture
            .pipeline
            .prepare(output, 1920, 1080, encoded_context.input_epoch)
            .unwrap()
        else {
            panic!("new frame must produce configuration");
        };
        let configuration = decode_video_config(prepared.video_config.as_deref().unwrap()).unwrap();
        assert_eq!(configuration.config_version, new_version);
        let actual_wire_context = InputContext {
            stream_id: configuration.stream_id,
            input_epoch: configuration.input_epoch,
        };
        assert_eq!(actual_wire_context, captured_new_context);
        assert_ne!(actual_wire_context, old_context);
        assert!(!fixture.inject(old_context, 100, key_event(true)));
        assert!(fixture.inject(actual_wire_context, 1, key_event(true)));
        assert_eq!(
            bindings.take_output(&test_encoded_frame(42, new_version, true)),
            None
        );
    }

    #[test]
    fn buffered_outputs_keep_submission_identity_and_context_storage_is_bounded() {
        let mut fixture = EpochFixture::new();
        let context = fixture.wire_context();
        let mut bindings = super::FrameInputContexts::default();
        for frame_id in [41, 42] {
            assert!(bindings.record_submission(
                crate::encoder::EncoderPerformanceSample {
                    frame_id,
                    config_version: 1,
                    ..Default::default()
                },
                context
            ));
        }
        // A normal delayed output remains legal within the same epoch.
        assert_eq!(
            bindings.take_output(&test_encoded_frame(41, 1, true)),
            Some(context)
        );
        assert_eq!(
            bindings.take_output(&test_encoded_frame(42, 1, false)),
            Some(context)
        );
        for frame_id in 1..=super::FrameInputContexts::CAPACITY as u64 {
            assert!(bindings.record_submission(
                crate::encoder::EncoderPerformanceSample {
                    frame_id,
                    config_version: 1,
                    ..Default::default()
                },
                context
            ));
        }
        assert!(!bindings.record_submission(
            crate::encoder::EncoderPerformanceSample {
                frame_id: 100,
                config_version: 1,
                ..Default::default()
            },
            context
        ));
        assert!(bindings.pending.is_empty());
        assert!(bindings.record_submission(
            crate::encoder::EncoderPerformanceSample {
                frame_id: 101,
                config_version: 1,
                ..Default::default()
            },
            context
        ));
        assert_eq!(
            bindings.take_output(&test_encoded_frame(101, 1, true)),
            Some(context)
        );
    }

    #[test]
    fn input_exit_releases_pressed_state_while_capture_retains_its_injector_arc() {
        let mut fixture = EpochFixture::new();
        let context = fixture.wire_context();
        assert!(fixture.inject(context, 1, key_event(true)));
        let capture_input = fixture.input.clone();
        let exit = super::HostInputExitGuard {
            selected: fixture.selected.clone(),
            input: fixture.input.clone(),
            input_suspended: fixture.suspended.clone(),
        };
        drop(exit);
        assert_eq!(Arc::strong_count(&capture_input), 2);
        assert_eq!(
            fixture.sent.lock().unwrap().last().unwrap().0,
            key_event(false)
        );
        assert!(!fixture.inject(context, 2, key_event(true)));
        assert!(
            !fixture
                .coordinator()
                .publish(
                    fixture.coordinator().current().unwrap().renewed().unwrap(),
                    fixture.encoder_version()
                )
                .unwrap()
        );
        assert!(fixture.suspended.load(Ordering::Acquire));
    }

    #[test]
    fn topology_publication_lock_remains_held_through_context_check_and_injection() {
        struct BlockingBackend {
            entered: std::sync::mpsc::SyncSender<()>,
            release: std::sync::mpsc::Receiver<()>,
        }
        impl InputBackend for BlockingBackend {
            fn send(
                &mut self,
                _: &InputEvent,
                _: VirtualDesktop,
            ) -> Result<(), InputInjectionError> {
                self.entered.send(()).unwrap();
                self.release
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .unwrap();
                Ok(())
            }
        }
        let mut fixture = EpochFixture::new();
        let context = fixture.wire_context();
        let selected = fixture.selected.clone();
        let desktop = selected.lock().unwrap().desktop;
        let (entered, wait_entered) = std::sync::mpsc::sync_channel(0);
        let (release, wait_release) = std::sync::mpsc::sync_channel(0);
        let inject = std::thread::spawn({
            let selected = selected.clone();
            move || {
                let input = Mutex::new(InputInjector::with_backend(
                    desktop,
                    BlockingBackend {
                        entered,
                        release: wait_release,
                    },
                ));
                let suspended = AtomicBool::new(false);
                let mut policy = super::HostInboundPolicy::new(context.stream_id);
                let envelope = InputEnvelope {
                    stream_id: context.stream_id,
                    input_epoch: context.input_epoch,
                    sequence: 1,
                    timestamp_us: 1_000,
                    event: InputEvent::MouseMove { x: 100, y: 100 },
                };
                super::inject_desktop_input(
                    &mut policy,
                    &input,
                    &encode_input(&envelope).unwrap(),
                    1_000,
                    &selected,
                    &suspended,
                )
                .unwrap()
            }
        });
        wait_entered
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert!(
            selected.try_lock().is_err(),
            "publication must wait for the complete sample"
        );
        release.send(()).unwrap();
        assert!(inject.join().unwrap());
        assert!(selected.try_lock().is_ok());
    }

    impl InputBackend for RejectingInputBackend {
        fn send(
            &mut self,
            _event: &desklink_protocol::InputEvent,
            _desktop: VirtualDesktop,
        ) -> Result<(), InputInjectionError> {
            Err(self.0.clone())
        }
    }

    #[tokio::test]
    async fn bitrate_waits_for_late_verified_native_completion_instead_of_inventing_rejection() {
        let (commands, received) = std::sync::mpsc::sync_channel(1);
        let mut queue = super::ConfigurationQueue::default();
        queue.request(super::ConfigurationAction::SetVideoBitrate(12_000_000));
        let mut pending = Some(super::start_configuration(
            commands,
            queue.pop_next().unwrap(),
        ));
        tokio::select! {
            _ = super::next_configuration(&mut pending) => panic!("must await native worker"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(1)) => {}
        }
        let super::CaptureWorkerCommand::SetVideoBitrate { bitrate, reply } =
            received.try_recv().unwrap()
        else {
            panic!("wrong command")
        };
        let native = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;
            reply.send(Some(bitrate)).unwrap();
        });
        let (_, applied) = tokio::time::timeout(
            std::time::Duration::from_secs(6),
            super::next_configuration(&mut pending),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            applied,
            super::ConfigurationApplied::Bitrate(Some(12_000_000))
        ));
        native.await.unwrap();
    }

    #[test]
    fn preset_or_profile_work_invalidates_old_rate_intent_and_blocks_new_rate_observations() {
        use super::{ConfigurationAction as Action, ConfigurationQueue};
        let mut queue = ConfigurationQueue::default();
        let mut rate = super::AdaptiveBitrate::new(VideoQualityPreset::Sharp);
        rate.observe(90, 10, 1, 0);
        let target = rate.observe(90, 10, 1, 0).unwrap();
        queue.request(Action::SetVideoBitrate(target));
        let old_rate = queue.pop_next().unwrap();
        super::queue_media_configuration(
            &mut queue,
            &mut rate,
            Action::SetVideoProfile(desklink_protocol::H264Profile::High),
        );
        assert!(!queue.is_current(&old_rate));
        assert!(super::configuration_blocks_bitrate(&queue, &None));
        let (commands, _received) = std::sync::mpsc::sync_channel(4);
        let profile = queue.pop_next().unwrap();
        let pending = Some(super::start_configuration(commands, profile));
        assert!(super::configuration_blocks_bitrate(&queue, &pending));
        assert_eq!(rate.observe(90, 10, 1, 0), None);
        drop(pending);
        rate.reset_to_actual(VideoQualityPreset::Sharp, 14_400_000);
        assert!(rate.should_recover_rate_first(1000, 0, 1, 0));
        super::queue_media_configuration(
            &mut queue,
            &mut rate,
            Action::SetVideoQuality(VideoQualityPreset::Balanced),
        );
        assert!(super::configuration_blocks_bitrate(&queue, &None));
    }

    #[test]
    fn bitrate_feedback_keeps_manual_quality_and_render_pressure_out_of_rate_adaptation() {
        let mut policy = super::AdaptiveBitrate::new(VideoQualityPreset::Sharp);
        for preference in [
            VideoQualityPreference::Smooth,
            VideoQualityPreference::Balanced,
            VideoQualityPreference::Sharp,
        ] {
            for _ in 0..100 {
                assert_eq!(
                    super::bitrate_feedback_decision(preference, &mut policy, 80, 20, 1, 0),
                    (None, false)
                );
            }
        }
        assert_eq!(policy.current(), policy.max());
        assert_eq!(
            super::bitrate_feedback_decision(
                VideoQualityPreference::Automatic,
                &mut policy,
                80,
                20,
                5,
                0
            ),
            (None, false)
        );
        assert_eq!(
            super::bitrate_feedback_decision(
                VideoQualityPreference::Automatic,
                &mut policy,
                80,
                20,
                1,
                0
            ),
            (None, true)
        );
        assert!(
            super::bitrate_feedback_decision(
                VideoQualityPreference::Automatic,
                &mut policy,
                80,
                20,
                1,
                0
            )
            .0
            .is_some()
        );
    }

    #[tokio::test]
    async fn outgoing_peer_drop_cancels_source_and_wakes_blocked_disk_producer() {
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (chunks, receiver) = tokio::sync::mpsc::channel(2);
        chunks.try_send(1).unwrap();
        chunks.try_send(2).unwrap();
        let sender_task = tokio::spawn(async move {
            std::future::pending::<()>().await;
            drop(receiver);
        });
        // Ensure the sender owns the receiver before cancelling the peer.
        tokio::task::yield_now().await;
        let (started, reached) = tokio::sync::oneshot::channel();
        let producer = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            chunks.blocking_send(3)
        });
        reached.await.unwrap();
        assert!(
            !producer.is_finished(),
            "full live queue must backpressure the producer"
        );
        let outgoing = super::HostOutgoingTransfer {
            request_id: 1,
            file: crate::transfer::OutgoingFile {
                transfer_id: [1; 16],
                path: "synthetic.bin".into(),
                name: "synthetic.bin".into(),
                size: 1,
                modified_at_unix_ns: 0,
            },
            cancellation: cancelled.clone(),
            phase: super::HostOutgoingPhase::Sending,
            sender_task: Some(sender_task),
        };
        drop(outgoing);
        assert!(cancelled.load(std::sync::atomic::Ordering::Acquire));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(250), producer)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }

    #[tokio::test]
    async fn slow_configuration_coalesces_new_requests_without_restarting_in_flight_work() {
        use super::{ConfigurationAction as Action, ConfigurationQueue};
        let (commands, received) = std::sync::mpsc::sync_channel(4);
        let mut queue = ConfigurationQueue::default();
        queue.request(Action::SelectDisplay(1));
        let first = queue.pop_next().unwrap();
        let mut pending = Some(super::start_configuration(commands.clone(), first));
        // Poll the actual production future until it has queued its native
        // command and is waiting for the slow worker's response.
        tokio::select! {
            result = super::next_configuration(&mut pending) => panic!("unexpected completion: {}", result.is_ok()),
            _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
        }
        let super::CaptureWorkerCommand::SelectDisplay { display_id, reply } =
            received.try_recv().unwrap()
        else {
            panic!("wrong command")
        };
        assert_eq!(display_id, 1);
        for id in 2..=1000 {
            queue.request(Action::SelectDisplay(id));
        }
        assert!(received.try_recv().is_err());
        reply.send(true).unwrap();
        let (completed, _) = super::next_configuration(&mut pending).await.unwrap();
        assert!(!queue.is_current(&completed));
        let latest = queue.pop_next().unwrap();
        assert_eq!(latest.action, Action::SelectDisplay(1000));
        assert!(queue.pop_next().is_none());
        pending = Some(super::start_configuration(commands, latest));
        tokio::select! {
            _ = super::next_configuration(&mut pending) => panic!("must await worker"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(5)) => {}
        }
        let super::CaptureWorkerCommand::SelectDisplay { display_id, reply } =
            received.try_recv().unwrap()
        else {
            panic!("wrong command")
        };
        assert_eq!(display_id, 1000);
        drop(pending);
        assert!(
            reply.send(true).is_err(),
            "cancelled peer must discard late configuration completion"
        );
    }

    #[tokio::test]
    async fn slow_configuration_does_not_stop_input_and_release_consumption() {
        let (sent, mut received) = tokio::sync::mpsc::channel(4);
        let (started, wait_started) = tokio::sync::oneshot::channel();
        let input = async move {
            wait_started.await.unwrap();
            for expected in [1, 2, 3] {
                let next =
                    tokio::time::timeout(std::time::Duration::from_millis(100), received.recv())
                        .await
                        .unwrap();
                assert_eq!(next, Some(expected));
            }
            Ok(())
        };
        let control = async move {
            started.send(()).unwrap();
            for value in [1, 2, 3] {
                sent.send(value).await.unwrap();
            }
            // Simulate a worker stalled for the full production reconfigure
            // deadline. The input future must finish first without waiting.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            panic!("slow control completed before input was consumed");
        };
        tokio::time::timeout(
            std::time::Duration::from_millis(250),
            super::run_inbound_lanes(input, control),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[test]
    fn buffered_keyframe_recovery_can_submit_again_on_an_idle_desktop() {
        let now = Instant::now();
        let requested = std::sync::atomic::AtomicBool::new(true);
        let mut recovery = super::StaticKeyframeRecovery::default();
        assert!(recovery.snapshot_due(requested.load(std::sync::atomic::Ordering::Acquire), now));
        let request_keyframe = requested.swap(false, std::sync::atomic::Ordering::AcqRel);
        assert!(request_keyframe);
        recovery.encoder_buffered_output(&requested, request_keyframe, true);
        assert!(!recovery.snapshot_due(requested.load(std::sync::atomic::Ordering::Acquire), now));
        assert!(recovery.snapshot_due(
            requested.load(std::sync::atomic::Ordering::Acquire),
            now + super::STATIC_KEYFRAME_INTERVAL
        ));
    }

    #[test]
    fn static_keyframe_recovery_is_request_driven_and_rate_limited() {
        let now = Instant::now();
        let mut recovery = super::StaticKeyframeRecovery::default();
        assert!(!recovery.snapshot_due(false, now));
        assert!(recovery.snapshot_due(true, now));
        assert!(!recovery.snapshot_due(true, now));
        assert!(!recovery.snapshot_due(true, now + std::time::Duration::from_millis(249)));
        assert!(recovery.snapshot_due(true, now + super::STATIC_KEYFRAME_INTERVAL));
        assert!(!recovery.snapshot_due(false, now + std::time::Duration::from_secs(10)));
    }

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
    fn one_bad_remote_input_sample_isolated_from_host_lifecycle() {
        let desktop = DesktopRect::new(0, 0, 1920, 1080);

        let mut blocked = InputInjector::with_backend(
            VirtualDesktop::single(desktop),
            RejectingInputBackend(InputInjectionError::Blocked),
        );
        assert!(
            super::apply_input_event(
                &mut blocked,
                desklink_protocol::InputEvent::MouseMove { x: 1, y: 1 },
            )
            .is_ok()
        );

        let mut unsupported = InputInjector::with_backend(
            VirtualDesktop::single(desktop),
            RejectingInputBackend(InputInjectionError::UnsupportedKey),
        );
        assert!(
            super::apply_input_event(
                &mut unsupported,
                desklink_protocol::InputEvent::Key {
                    code: KeyCode::Function(13),
                    pressed: true,
                    modifiers: Modifiers(0),
                },
            )
            .is_ok()
        );

        let mut invalid = InputInjector::with_backend(
            VirtualDesktop::single(desktop),
            RejectingInputBackend(InputInjectionError::Blocked),
        );
        assert!(
            super::apply_input_event(
                &mut invalid,
                desklink_protocol::InputEvent::MouseMove {
                    x: desklink_protocol::MAX_POINTER_COORDINATE + 1,
                    y: 1,
                },
            )
            .is_ok()
        );
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
    fn initial_capture_watchdog_only_fires_before_the_first_frame() {
        let start = Instant::now();
        let deadline = start + std::time::Duration::from_secs(10);
        assert!(!initial_frame_deadline_expired(false, deadline, start));
        assert!(initial_frame_deadline_expired(
            false,
            deadline,
            deadline + std::time::Duration::from_millis(1)
        ));
        assert!(!initial_frame_deadline_expired(
            true,
            deadline + std::time::Duration::from_millis(1),
            deadline + std::time::Duration::from_secs(1)
        ));
    }

    #[test]
    fn initial_encoder_watchdog_only_fires_after_capture_starts() {
        let start = Instant::now();
        let deadline = start + std::time::Duration::from_secs(10);
        assert!(!initial_encoded_frame_deadline_expired(
            false,
            None,
            deadline + std::time::Duration::from_secs(1)
        ));
        assert!(!initial_encoded_frame_deadline_expired(
            false,
            Some(deadline),
            start
        ));
        assert!(initial_encoded_frame_deadline_expired(
            false,
            Some(deadline),
            deadline + std::time::Duration::from_millis(1)
        ));
        assert!(!initial_encoded_frame_deadline_expired(
            true,
            Some(deadline),
            deadline + std::time::Duration::from_secs(1)
        ));
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

    #[test]
    fn quality_profiles_preserve_thirty_fps_while_reducing_bandwidth() {
        let smooth = video_quality_settings(VideoQualityPreset::Smooth);
        let balanced = video_quality_settings(VideoQualityPreset::Balanced);
        let sharp = video_quality_settings(VideoQualityPreset::Sharp);
        assert_eq!(
            (smooth.max_width, smooth.max_height, smooth.fps),
            (1280, 720, 30)
        );
        assert_eq!(
            (balanced.max_width, balanced.max_height, balanced.fps),
            (1920, 1080, 30)
        );
        assert_eq!(sharp.fps, 30);
        assert_eq!(sharp.bitrate, 18_000_000);
        assert!(smooth.bitrate < balanced.bitrate);
        assert!(balanced.bitrate < sharp.bitrate);
    }

    #[test]
    fn automatic_quality_is_the_default_session_policy() {
        let quality = AdaptiveVideoQuality::new();
        assert_eq!(quality.preference, VideoQualityPreference::Automatic);
        assert_eq!(quality.preset, VideoQualityPreset::Sharp);
    }

    #[test]
    fn automatic_quality_requires_sustained_loss_and_steps_down_once() {
        let mut quality = AdaptiveVideoQuality::new();
        assert_eq!(
            quality.set_preference(VideoQualityPreference::Automatic),
            None
        );
        assert_eq!(quality.observe(90, 10, 0, 0), None);
        assert_eq!(
            quality.observe(90, 10, 0, 0),
            Some(VideoQualityPreset::Balanced)
        );
        quality.record_applied(VideoQualityPreset::Balanced);

        for _ in 0..8 {
            assert_eq!(quality.observe(90, 10, 0, 0), None);
        }
        assert_eq!(quality.observe(90, 10, 0, 0), None);
        assert_eq!(
            quality.observe(90, 10, 0, 0),
            Some(VideoQualityPreset::Smooth)
        );
    }

    #[test]
    fn automatic_quality_recovers_only_after_a_long_healthy_window() {
        let mut quality = AdaptiveVideoQuality::new();
        assert_eq!(
            quality.set_preference(VideoQualityPreference::Smooth),
            Some(VideoQualityPreset::Smooth)
        );
        quality.record_applied(VideoQualityPreset::Smooth);
        assert_eq!(
            quality.set_preference(VideoQualityPreference::Automatic),
            None
        );

        for _ in 0..11 {
            assert_eq!(quality.observe(100, 0, 2, 0), None);
        }
        assert_eq!(
            quality.observe(100, 0, 2, 0),
            Some(VideoQualityPreset::Balanced)
        );
    }

    #[test]
    fn automatic_quality_ignores_sparse_feedback_and_manual_modes() {
        let mut quality = AdaptiveVideoQuality::new();
        assert_eq!(quality.observe(10, 10, 0, 0), None);
        assert_eq!(quality.set_preference(VideoQualityPreference::Sharp), None);
        assert_eq!(quality.observe(80, 20, 64, 16), None);
    }

    #[test]
    fn automatic_quality_uses_playback_pressure_without_network_loss() {
        let mut quality = AdaptiveVideoQuality::new();
        assert_eq!(
            quality.observe(0, 0, 7, 1),
            Some(VideoQualityPreset::Balanced)
        );

        let mut quality = AdaptiveVideoQuality::new();
        assert_eq!(quality.observe(0, 0, 5, 0), None);
        assert_eq!(
            quality.observe(0, 0, 5, 0),
            Some(VideoQualityPreset::Balanced)
        );
    }

    #[test]
    fn automatic_quality_requires_a_low_decode_queue_before_recovering() {
        let mut quality = AdaptiveVideoQuality::new();
        assert_eq!(
            quality.set_preference(VideoQualityPreference::Smooth),
            Some(VideoQualityPreset::Smooth)
        );
        quality.record_applied(VideoQualityPreset::Smooth);
        assert_eq!(
            quality.set_preference(VideoQualityPreference::Automatic),
            None
        );

        for _ in 0..12 {
            assert_eq!(quality.observe(100, 0, 3, 0), None);
        }
        assert_eq!(quality.preset, VideoQualityPreset::Smooth);
    }

    #[test]
    fn frame_pacer_does_not_alias_a_sixty_hz_capture_source_to_twenty_fps() {
        let mut pacer = VideoFramePacer::new(30);
        let encoded = (0..60)
            .filter(|index| pacer.should_encode(index * 16_666, false))
            .count();
        assert_eq!(encoded, 30);
    }

    #[test]
    fn frame_pacer_handles_quality_changes_forced_frames_and_clock_rewinds() {
        let mut pacer = VideoFramePacer::new(20);
        assert!(pacer.should_encode(1_000_000, false));
        assert!(!pacer.should_encode(1_020_000, false));
        assert!(pacer.should_encode(1_030_000, true));
        assert!(!pacer.should_encode(1_050_000, false));
        assert!(pacer.should_encode(900_000, false));

        pacer.reset(15);
        let encoded = (0..60)
            .filter(|index| pacer.should_encode(index * 16_666, false))
            .count();
        assert_eq!(encoded, 15);
    }
}

#[cfg(test)]
mod direct_video_tests {
    use std::{
        future::Future,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use desklink_crypto::{DeviceIdentity, NoiseInitiator, NoiseResponder, SecureRole};
    use desklink_transport::{
        DirectLanEndpoint, DirectLanSession, TransportError, VideoDatagramBackend,
        VideoDatagramRoute,
    };
    use rand_core::OsRng;
    use tokio::sync::Mutex as AsyncMutex;

    use super::{accept_host_direct_connection, now_unix_s, send_video_datagram_with_fallback};

    #[derive(Clone)]
    struct RecordingBackend {
        route: VideoDatagramRoute,
        fail: bool,
        sent: Arc<Mutex<Vec<Vec<u8>>>>,
    }

    impl VideoDatagramBackend for RecordingBackend {
        fn route(&self) -> VideoDatagramRoute {
            self.route
        }

        fn send<'a>(
            &'a self,
            bytes: Vec<u8>,
        ) -> impl Future<Output = Result<(), TransportError>> + Send + 'a {
            let sent = self.sent.clone();
            let fail = self.fail;
            async move {
                sent.lock().unwrap().push(bytes);
                if fail {
                    Err(TransportError::Datagram(
                        "simulated direct path failure".to_owned(),
                    ))
                } else {
                    Ok(())
                }
            }
        }
    }

    #[tokio::test]
    async fn host_acceptor_keeps_authenticated_direct_datagram_connection() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let now = now_unix_s();
        let binding = [42; 16];
        let host_session = Arc::new(
            DirectLanSession::bind(
                "127.0.0.1:0".parse().unwrap(),
                "127.0.0.1:9".parse().unwrap(),
                7,
                binding,
                now,
            )
            .unwrap(),
        );
        let controller_endpoint = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let (mut controller_secure, host_secure) = connected_secure_sessions();
        let host_secure = Arc::new(AsyncMutex::new(host_secure));
        let host_accept_task = tokio::spawn(accept_host_direct_connection(
            Some(host_session.clone()),
            host_secure,
        ));

        let (controller_connection, controller_probe) = controller_endpoint
            .connect(
                host_session.candidate(),
                &binding,
                &mut controller_secure,
                now,
            )
            .await
            .unwrap();
        let host_connection = tokio::time::timeout(Duration::from_secs(3), host_accept_task)
            .await
            .unwrap()
            .unwrap()
            .expect("host should accept the authenticated direct probe");

        assert_eq!(controller_probe.candidate_id, 7);
        assert_eq!(host_connection.candidate_id(), 7);
        controller_connection.send_datagram(vec![7, 8, 9]).unwrap();
        let received =
            tokio::time::timeout(Duration::from_secs(1), host_connection.recv_datagram())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(received, vec![7, 8, 9]);
    }

    #[tokio::test]
    async fn failed_direct_video_datagram_falls_back_to_relay_only() {
        let direct_sent = Arc::new(Mutex::new(Vec::new()));
        let relay_sent = Arc::new(Mutex::new(Vec::new()));
        let direct = RecordingBackend {
            route: VideoDatagramRoute::DirectLan { candidate_id: 7 },
            fail: true,
            sent: direct_sent.clone(),
        };
        let relay = RecordingBackend {
            route: VideoDatagramRoute::Relay,
            fail: false,
            sent: relay_sent.clone(),
        };
        let mut selected = Some(direct);

        let route = send_video_datagram_with_fallback(&mut selected, &relay, vec![1, 2, 3])
            .await
            .unwrap();

        assert_eq!(route, VideoDatagramRoute::Relay);
        assert!(selected.is_none(), "failed direct path must be retired");
        assert_eq!(direct_sent.lock().unwrap().as_slice(), &[vec![1, 2, 3]]);
        assert_eq!(relay_sent.lock().unwrap().as_slice(), &[vec![1, 2, 3]]);
    }

    fn connected_secure_sessions() -> (
        desklink_crypto::SecureSession,
        desklink_crypto::SecureSession,
    ) {
        let mut rng = OsRng;
        let controller_identity = DeviceIdentity::generate(&mut rng);
        let host_identity = DeviceIdentity::generate(&mut rng);
        let controller_verify_key = controller_identity.verify_key();
        let host_verify_key = host_identity.verify_key();
        let (mut initiator, hello) =
            NoiseInitiator::start(controller_identity, host_verify_key).unwrap();
        let (mut responder, response) =
            NoiseResponder::accept(&hello, host_identity, controller_verify_key).unwrap();
        let finish = initiator.receive(&response).unwrap();
        responder.receive(&finish).unwrap();
        (
            initiator
                .finish()
                .unwrap()
                .into_secure_session(SecureRole::Initiator),
            responder
                .finish()
                .unwrap()
                .into_secure_session(SecureRole::Responder),
        )
    }
}
