use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use desklink_crypto::{
    CryptoError, DeviceIdentity, NoiseInitiator, SecureLane, SecureRole, SecureSession,
};
use desklink_protocol::{
    AccessDenialReason, Codec, ControlMessage, CursorUpdate, DeviceCapabilities, DeviceRole,
    InputEnvelope, InputEvent, NoiseHandshake, NoiseHandshakeStep, PROTOCOL_VERSION, Platform,
    ProtocolError, VideoConfig, decode_control, decode_cursor_update, decode_noise_handshake,
    decode_video_config, decode_video_packet, encode_control, encode_input, encode_noise_handshake,
};
use desklink_session::InputSequencer;
use desklink_transport::{QuicClient, TransportError, TransportEvent};
use desklink_video::{AssembleResult, EncodedFrame, FrameAssembler};
use ed25519_dalek::VerifyingKey;
use thiserror::Error;
use tokio::sync::Mutex;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(150);
// The host opens its local approval dialog after the Noise handshake. The
// controller waits for capability negotiation while that dialog is visible.
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(120);
const ASSEMBLY_CAPACITY: usize = 3;
const ASSEMBLY_MAX_AGE: Duration = Duration::from_millis(500);

#[derive(Debug, Error)]
pub enum ControllerError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("cryptographic session error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("authenticated Noise handshake timed out")]
    HandshakeTimeout,
    #[error("capability negotiation timed out")]
    NegotiationTimeout,
    #[error("received an unexpected Noise handshake step")]
    UnexpectedHandshakeStep,
    #[error("host capabilities are invalid or incompatible")]
    InvalidHostCapabilities,
    #[error("video configuration changed without advancing its version")]
    InconsistentVideoConfig,
    #[error("no active video stream is available")]
    NoActiveStream,
    #[error("host sent data on a controller-only transport lane")]
    UnexpectedTransportLane,
    #[error("the host denied remote-control access: {0:?}")]
    AccessDenied(AccessDenialReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControllerEvent {
    Control(ControlMessage),
    VideoConfig(VideoConfig),
    H264AccessUnit(EncodedFrame),
    Cursor(CursorUpdate),
    Closed { reason: String },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ControllerMetrics {
    pub received_video_packets: u64,
    pub dropped_video_packets: u64,
    pub completed_frames: u64,
}

#[derive(Default)]
struct AtomicControllerMetrics {
    received_video_packets: AtomicU64,
    dropped_video_packets: AtomicU64,
    completed_frames: AtomicU64,
}

impl AtomicControllerMetrics {
    fn snapshot(&self) -> ControllerMetrics {
        ControllerMetrics {
            received_video_packets: self.received_video_packets.load(Ordering::Relaxed),
            dropped_video_packets: self.dropped_video_packets.load(Ordering::Relaxed),
            completed_frames: self.completed_frames.load(Ordering::Relaxed),
        }
    }
}

pub struct ControllerRuntime {
    client: Arc<QuicClient>,
    secure: Arc<Mutex<SecureSession>>,
    input_sequence: Mutex<InputSequencer>,
    assembler: FrameAssembler,
    video_config: Option<VideoConfig>,
    active_stream: AtomicU64,
    metrics: AtomicControllerMetrics,
    keyframe_needed_after_config: bool,
    awaiting_keyframe: bool,
    keyframe_request_outstanding: bool,
}

impl ControllerRuntime {
    pub async fn connect(
        client: QuicClient,
        identity: DeviceIdentity,
        expected_host: VerifyingKey,
    ) -> Result<Self, ControllerError> {
        Self::connect_for_platform(client, identity, expected_host, Platform::MacOS).await
    }

    pub async fn connect_for_platform(
        client: QuicClient,
        identity: DeviceIdentity,
        expected_host: VerifyingKey,
        platform: Platform,
    ) -> Result<Self, ControllerError> {
        Self::connect_for_platform_with_observer(client, identity, expected_host, platform, || {})
            .await
    }

    pub async fn connect_for_platform_with_observer(
        client: QuicClient,
        identity: DeviceIdentity,
        expected_host: VerifyingKey,
        platform: Platform,
        on_encrypted: impl FnOnce(),
    ) -> Result<Self, ControllerError> {
        let client = Arc::new(client);
        let secure = Arc::new(Mutex::new(
            perform_noise_handshake(&client, identity, expected_host).await?,
        ));
        on_encrypted();
        negotiate_host(&client, &secure, platform).await?;
        Ok(Self {
            client,
            secure,
            input_sequence: Mutex::new(InputSequencer::new()),
            assembler: FrameAssembler::new(ASSEMBLY_CAPACITY, ASSEMBLY_MAX_AGE),
            video_config: None,
            active_stream: AtomicU64::new(0),
            metrics: AtomicControllerMetrics::default(),
            keyframe_needed_after_config: false,
            awaiting_keyframe: false,
            keyframe_request_outstanding: false,
        })
    }

    pub fn metrics(&self) -> ControllerMetrics {
        self.metrics.snapshot()
    }

    pub fn active_stream_id(&self) -> Option<u64> {
        match self.active_stream.load(Ordering::Acquire) {
            0 => None,
            stream_id => Some(stream_id),
        }
    }

    pub async fn send_input(&self, event: InputEvent) -> Result<(), ControllerError> {
        let sequence = self.input_sequence.lock().await.next_sequence();
        let plaintext = encode_input(&InputEnvelope {
            sequence,
            timestamp_us: now_micros(),
            event,
        })?;
        let ciphertext = seal(&self.secure, SecureLane::Input, &plaintext).await?;
        self.client.send_input(ciphertext).await?;
        Ok(())
    }

    pub async fn request_keyframe(&self) -> Result<(), ControllerError> {
        let stream_id = self
            .active_stream_id()
            .ok_or(ControllerError::NoActiveStream)?;
        self.request_keyframe_for(stream_id).await
    }

    pub async fn select_display(&self, display_id: u32) -> Result<(), ControllerError> {
        let plaintext = encode_control(&ControlMessage::SelectDisplay { display_id })?;
        let ciphertext = seal(&self.secure, SecureLane::Control, &plaintext).await?;
        self.client.send_control(ciphertext).await?;
        Ok(())
    }

    pub async fn next_event(&mut self) -> Result<ControllerEvent, ControllerError> {
        loop {
            match self.client.next_event().await? {
                TransportEvent::Control(ciphertext) => {
                    let plaintext = open(&self.secure, SecureLane::Control, &ciphertext).await?;
                    let message = decode_control(&plaintext)?;
                    if let ControlMessage::AccessDenied { reason } = message {
                        return Err(ControllerError::AccessDenied(reason));
                    }
                    return Ok(ControllerEvent::Control(message));
                }
                TransportEvent::Input(_) => {
                    return Err(ControllerError::UnexpectedTransportLane);
                }
                TransportEvent::VideoConfig(ciphertext) => {
                    let plaintext =
                        open(&self.secure, SecureLane::VideoConfig, &ciphertext).await?;
                    let config = decode_video_config(&plaintext)?;
                    let config_changed = self.video_config.as_ref() != Some(&config);
                    if let Some(current) = &self.video_config {
                        if config.stream_id == current.stream_id
                            && config.config_version < current.config_version
                        {
                            continue;
                        }
                        if config.stream_id == current.stream_id
                            && config.config_version == current.config_version
                            && config != *current
                        {
                            return Err(ControllerError::InconsistentVideoConfig);
                        }
                    }
                    if self.active_stream_id() != Some(config.stream_id) {
                        if !self.assembler.begin_stream(config.stream_id) {
                            continue;
                        }
                        self.active_stream
                            .store(config.stream_id, Ordering::Release);
                    }
                    self.video_config = Some(config.clone());
                    if config_changed {
                        self.awaiting_keyframe = true;
                        self.keyframe_request_outstanding = false;
                    }
                    if self.keyframe_needed_after_config {
                        self.request_keyframe_for(config.stream_id).await?;
                        self.keyframe_needed_after_config = false;
                        self.keyframe_request_outstanding = true;
                    }
                    return Ok(ControllerEvent::VideoConfig(config));
                }
                TransportEvent::VideoDatagram(ciphertext) => {
                    self.metrics
                        .received_video_packets
                        .fetch_add(1, Ordering::Relaxed);
                    let plaintext =
                        open(&self.secure, SecureLane::VideoDatagram, &ciphertext).await?;
                    let packet = decode_video_packet(&plaintext)?;
                    let Some(config) = &self.video_config else {
                        self.drop_video_packet();
                        self.keyframe_needed_after_config = true;
                        continue;
                    };
                    if packet.header.stream_id != config.stream_id
                        || packet.header.config_version != config.config_version
                        || packet.header.width != config.width
                        || packet.header.height != config.height
                    {
                        self.drop_video_packet();
                        continue;
                    }
                    match self.assembler.push(Instant::now(), packet) {
                        AssembleResult::Pending => {}
                        AssembleResult::Dropped(_) => self.drop_video_packet(),
                        AssembleResult::Complete(frame) => {
                            let is_keyframe =
                                frame.flags.0 & desklink_protocol::FrameFlags::KEYFRAME.0 != 0;
                            if self.awaiting_keyframe && !is_keyframe {
                                self.drop_video_packet();
                                if !self.keyframe_request_outstanding {
                                    self.request_keyframe_for(config.stream_id).await?;
                                    self.keyframe_request_outstanding = true;
                                }
                                continue;
                            }
                            if self.assembler.accept_for_present(frame.clone()) {
                                if is_keyframe {
                                    self.awaiting_keyframe = false;
                                    self.keyframe_request_outstanding = false;
                                }
                                self.metrics
                                    .completed_frames
                                    .fetch_add(1, Ordering::Relaxed);
                                return Ok(ControllerEvent::H264AccessUnit(frame));
                            }
                            self.drop_video_packet();
                        }
                    }
                }
                TransportEvent::CursorDatagram(ciphertext) => {
                    let plaintext =
                        open(&self.secure, SecureLane::CursorDatagram, &ciphertext).await?;
                    let cursor = decode_cursor_update(&plaintext)?;
                    if self
                        .active_stream_id()
                        .is_some_and(|id| id != cursor.stream_id)
                    {
                        continue;
                    }
                    return Ok(ControllerEvent::Cursor(cursor));
                }
                TransportEvent::PeerDisconnected { channel } => {
                    return Ok(ControllerEvent::Closed {
                        reason: format!("远端会话已断开（{channel:?}）"),
                    });
                }
                TransportEvent::Closed { reason } => {
                    return Ok(ControllerEvent::Closed { reason });
                }
            }
        }
    }

    async fn request_keyframe_for(&self, stream_id: u64) -> Result<(), ControllerError> {
        let plaintext = encode_control(&ControlMessage::RequestKeyframe { stream_id })?;
        let ciphertext = seal(&self.secure, SecureLane::Control, &plaintext).await?;
        self.client.send_control(ciphertext).await?;
        Ok(())
    }

    fn drop_video_packet(&self) {
        self.metrics
            .dropped_video_packets
            .fetch_add(1, Ordering::Relaxed);
    }
}

async fn perform_noise_handshake(
    client: &QuicClient,
    identity: DeviceIdentity,
    expected_host: VerifyingKey,
) -> Result<SecureSession, ControllerError> {
    let handshake = async {
        let (mut initiator, message_1) = NoiseInitiator::start(identity, expected_host)?;
        client
            .send_control(encode_noise_handshake(&NoiseHandshake {
                protocol_version: PROTOCOL_VERSION,
                step: NoiseHandshakeStep::InitiatorHello,
                payload: message_1,
            })?)
            .await?;
        let response = decode_noise_handshake(&client.next_control().await?)?;
        if response.step != NoiseHandshakeStep::ResponderHello {
            return Err(ControllerError::UnexpectedHandshakeStep);
        }
        let message_3 = initiator.receive(&response.payload)?;
        client
            .send_control(encode_noise_handshake(&NoiseHandshake {
                protocol_version: PROTOCOL_VERSION,
                step: NoiseHandshakeStep::InitiatorFinish,
                payload: message_3,
            })?)
            .await?;
        Ok(initiator
            .finish()?
            .into_secure_session(SecureRole::Initiator))
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
        .await
        .map_err(|_| ControllerError::HandshakeTimeout)?
}

async fn negotiate_host(
    client: &QuicClient,
    secure: &Arc<Mutex<SecureSession>>,
    platform: Platform,
) -> Result<(), ControllerError> {
    send_control(
        client,
        secure,
        &ControlMessage::Hello {
            platform,
            role: DeviceRole::Controller,
        },
    )
    .await?;
    send_control(
        client,
        secure,
        &ControlMessage::Capabilities(DeviceCapabilities {
            platform,
            role: DeviceRole::Controller,
            codecs: vec![Codec::H264],
            width: 1920,
            height: 1080,
        }),
    )
    .await?;

    let negotiation = async {
        let mut received_host_hello = false;
        loop {
            let ciphertext = client.next_control().await?;
            let plaintext = open(secure, SecureLane::Control, &ciphertext).await?;
            match decode_control(&plaintext)? {
                ControlMessage::Hello {
                    role: DeviceRole::Host,
                    ..
                } => received_host_hello = true,
                ControlMessage::Capabilities(capabilities)
                    if received_host_hello
                        && capabilities.role == DeviceRole::Host
                        && capabilities.codecs.contains(&Codec::H264) =>
                {
                    return Ok(());
                }
                ControlMessage::Capabilities(_) => {
                    return Err(ControllerError::InvalidHostCapabilities);
                }
                ControlMessage::AccessDenied { reason } => {
                    return Err(ControllerError::AccessDenied(reason));
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
        .map_err(|_| ControllerError::NegotiationTimeout)?
}

async fn send_control(
    client: &QuicClient,
    secure: &Arc<Mutex<SecureSession>>,
    message: &ControlMessage,
) -> Result<(), ControllerError> {
    let plaintext = encode_control(message)?;
    client
        .send_control(seal(secure, SecureLane::Control, &plaintext).await?)
        .await?;
    Ok(())
}

async fn seal(
    secure: &Arc<Mutex<SecureSession>>,
    lane: SecureLane,
    plaintext: &[u8],
) -> Result<Vec<u8>, ControllerError> {
    Ok(secure.lock().await.seal(lane, plaintext)?)
}

async fn open(
    secure: &Arc<Mutex<SecureSession>>,
    lane: SecureLane,
    ciphertext: &[u8],
) -> Result<Vec<u8>, ControllerError> {
    Ok(secure.lock().await.open(lane, ciphertext)?)
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
