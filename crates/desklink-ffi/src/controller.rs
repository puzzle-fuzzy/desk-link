use std::{
    collections::VecDeque,
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
    AccessDenialReason, AudioPacket, Codec, ControlMessage, CursorUpdate, DeviceCapabilities,
    DeviceRole, DirectLanCandidate, H264Profile, InputContext, InputEnvelope, InputEvent,
    NoiseHandshake, NoiseHandshakeStep, PROTOCOL_VERSION, Platform, ProtocolError, TransferMessage,
    VideoConfig, VideoPacket, VideoQualityPreference, decode_audio_packet, decode_control,
    decode_cursor_update, decode_noise_handshake, decode_transfer, decode_video_config,
    decode_video_packet, decode_video_reliable_batch, encode_control, encode_input,
    encode_noise_handshake, encode_transfer,
};
use desklink_session::InputSequencer;
use desklink_transport::{
    ChannelKind, DirectLanConnection, DirectLanEndpoint, DirectLanProbeResult, DirectLanSession,
    DirectVideoPathAction, DirectVideoPathEvent, DirectVideoPathFallbackReason,
    DirectVideoPathMachine, DirectVideoPathState, NegotiatedLanData, QuicClient, QuicRelayBackend,
    SessionDataError, SessionDataPlane, SessionLaneHealth, TransportError, TransportEvent,
    VideoPathKind, VideoPathQuality,
};
use desklink_video::{
    AssembleResult, EncodedFrame, FrameAssembler, VideoContinuity, VideoContinuityAction,
};
use ed25519_dalek::VerifyingKey;
use rand_core::{OsRng, RngCore};

fn supported_h264_profiles(platform: Platform) -> Vec<H264Profile> {
    match platform {
        Platform::Windows => vec![H264Profile::Main, H264Profile::High],
        _ => vec![H264Profile::Main],
    }
}
use thiserror::Error;
use tokio::sync::Mutex;

mod lan_runtime;
use lan_runtime::{ControllerInboundEvent, ControllerLanRuntime, ControllerPayload};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(150);
// The host opens its local approval dialog after the Noise handshake. The
// controller waits for capability negotiation while that dialog is visible.
const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(120);
const ASSEMBLY_CAPACITY: usize = 3;
// Datagram frames are low-latency and must not retain stale partials for long.
const ASSEMBLY_MAX_AGE: Duration = Duration::from_secs(10);
// The first relay keyframe is delivered on an ordered reliable stream and may
// be paced by a public relay. Keep that one bounded but separate from lossy
// datagram assembly so a slow burst cannot expire just before completion.
const RELIABLE_ASSEMBLY_MAX_AGE: Duration = Duration::from_secs(30);
const PENDING_VIDEO_DATAGRAM_CAPACITY: usize = 256;
// The config and the first relay keyframe use separate QUIC streams. Give
// that keyframe a short grace window before asking the host to generate a
// newer frame, which could otherwise overtake and invalidate the in-flight
// assembly. The reliable assembler has its own longer expiry below.
const INITIAL_KEYFRAME_GRACE: Duration = Duration::from_millis(1500);
// A successful authenticated probe only proves that the endpoints can open a
// datagram socket. If a firewall silently drops the subsequent video packets,
// do not leave the controller on a permanently black direct surface.
const DIRECT_VIDEO_SILENCE_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const CONTROLLER_SEND_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
enum VideoPacketSource {
    Datagram,
    Reliable,
}

#[derive(Debug, Error)]
pub enum ControllerError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("session data transport error: {0}")]
    SessionData(#[from] SessionDataError),
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
    #[error("video configuration changed without advancing its version or geometry epoch")]
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
    Audio(AudioPacket),
    Transfer(TransferMessage),
    Closed { reason: String },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ControllerMetrics {
    pub received_video_packets: u64,
    pub received_video_reliable_packets: u64,
    pub dropped_video_packets: u64,
    pub completed_frames: u64,
}

#[derive(Default)]
struct AtomicControllerMetrics {
    received_video_packets: AtomicU64,
    received_video_reliable_packets: AtomicU64,
    dropped_video_packets: AtomicU64,
    completed_frames: AtomicU64,
}

impl AtomicControllerMetrics {
    fn snapshot(&self) -> ControllerMetrics {
        ControllerMetrics {
            received_video_packets: self.received_video_packets.load(Ordering::Relaxed),
            received_video_reliable_packets: self
                .received_video_reliable_packets
                .load(Ordering::Relaxed),
            dropped_video_packets: self.dropped_video_packets.load(Ordering::Relaxed),
            completed_frames: self.completed_frames.load(Ordering::Relaxed),
        }
    }
}

pub struct ControllerRuntime {
    data_plane: Option<Arc<SessionDataPlane>>,
    host_capabilities: DeviceCapabilities,
    client: Arc<QuicClient>,
    secure: Arc<Mutex<SecureSession>>,
    input_sequence: Arc<Mutex<InputSequencer>>,
    control_order: Arc<Mutex<()>>,
    #[cfg(test)]
    received_transport_events: Arc<AtomicU64>,
    #[cfg(test)]
    input_writes_started: Arc<AtomicU64>,
    assembler: FrameAssembler,
    reliable_assembler: FrameAssembler,
    video_config: Option<VideoConfig>,
    pending_video_datagrams: VecDeque<ControllerPayload>,
    pending_video_reliable: VecDeque<ControllerPayload>,
    active_stream: AtomicU64,
    metrics: AtomicControllerMetrics,
    keyframe_needed_after_config: bool,
    keyframe_fallback_deadline: Option<Instant>,
    video_continuity: VideoContinuity,
    last_cursor_sequence: u64,
    reliable_trace_started: Option<Instant>,
    video_path: DirectVideoPathMachine,
    direct_session: Option<DirectLanSession>,
    direct_connection: Option<Arc<DirectLanConnection>>,
    last_direct_datagram_at: Option<Instant>,
    lan: Option<ControllerLanRuntime>,
}

#[cfg(test)]
pub(crate) struct ControllerReceiveTestState {
    pub(crate) client: Arc<QuicClient>,
    pub(crate) dequeued: Arc<AtomicU64>,
    pub(crate) secure: Arc<Mutex<SecureSession>>,
    pub(crate) input_order: Arc<Mutex<InputSequencer>>,
    pub(crate) writes_started: Arc<AtomicU64>,
}

#[derive(Clone)]
pub struct ControllerInputSender {
    data_plane: Option<Arc<SessionDataPlane>>,
    client: Arc<QuicClient>,
    secure: Arc<Mutex<SecureSession>>,
    input_sequence: Arc<Mutex<InputSequencer>>,
    #[cfg(test)]
    input_writes_started: Arc<AtomicU64>,
}

impl ControllerInputSender {
    /// Sends exactly the context captured when input was admitted. The sender
    /// does not adopt a newer presentation epoch or cancel an in-flight frame
    /// when that epoch changes; the owning session controls cancellation.
    pub async fn send_input_with_context(
        &self,
        context: InputContext,
        event: InputEvent,
    ) -> Result<(), ControllerError> {
        send_session_input(
            &self.client,
            &self.secure,
            &self.input_sequence,
            self.data_plane.as_deref(),
            context,
            event,
            #[cfg(test)]
            &self.input_writes_started,
        )
        .await
    }
}

#[derive(Clone)]
pub(crate) struct ControllerControlSender {
    data_plane: Option<Arc<SessionDataPlane>>,
    client: Arc<QuicClient>,
    secure: Arc<Mutex<SecureSession>>,
    order: Arc<Mutex<()>>,
}

impl ControllerControlSender {
    pub(crate) async fn send(&self, message: ControlMessage) -> Result<(), ControllerError> {
        // Runtime-generated recovery requests and worker commands share one
        // reliable lane. Preserve seal/wire order without holding crypto at I/O.
        match tokio::time::timeout(CONTROLLER_SEND_TIMEOUT, async {
            let _order = self.order.lock().await;
            if let Some(plane) = &self.data_plane {
                plane
                    .send(ChannelKind::Control, encode_control(&message)?.into())
                    .await?;
                Ok(())
            } else {
                send_control(&self.client, &self.secure, &message).await
            }
        })
        .await
        {
            Ok(result) => result,
            Err(_) => {
                // A partial reliable write may not be reused by the next
                // sender. Retire the entire session before exposing timeout.
                self.close();
                Err(
                    TransportError::Connection("controller control send deadline exceeded".into())
                        .into(),
                )
            }
        }
    }

    pub(crate) fn close(&self) {
        if let Some(plane) = &self.data_plane {
            plane.close();
        }
        self.client.close(b"desklink controller session retired");
    }
}

#[derive(Clone)]
pub struct ControllerTransferSender {
    data_plane: Option<Arc<SessionDataPlane>>,
    client: Arc<QuicClient>,
    secure: Arc<Mutex<SecureSession>>,
}

impl ControllerTransferSender {
    pub async fn send(&self, message: TransferMessage) -> Result<(), ControllerError> {
        let plaintext = encode_transfer(&message)?;
        if let Some(plane) = &self.data_plane {
            plane.send(ChannelKind::Transfer, plaintext.into()).await?;
        } else {
            let ciphertext = seal(&self.secure, SecureLane::Transfer, &plaintext).await?;
            self.client.send_transfer(ciphertext).await?;
        }
        Ok(())
    }
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
        let host_capabilities = negotiate_host(&client, &secure, platform).await?;
        let video_path_binding = secure.lock().await.video_path_binding();
        let agreement = NegotiatedLanData::from_authenticated_caps(
            &lan_runtime::controller_capabilities(platform),
            &host_capabilities,
            video_path_binding,
            client.peer_generation(),
        )?;
        let data_plane = if let Some(agreement) = agreement {
            let relay = Arc::new(QuicRelayBackend::new(
                client.clone(),
                client.peer_generation(),
            )?);
            Some(SessionDataPlane::relay_only(agreement, secure.clone(), relay).await?)
        } else {
            None
        };
        let lan = agreement.map(ControllerLanRuntime::new);
        let mut video_path = DirectVideoPathMachine::new(video_path_binding);
        let direct_session = if platform == Platform::Windows && data_plane.is_none() {
            video_path = video_path.with_direct_probe();
            let candidate_id = next_direct_candidate_id();
            match DirectLanSession::bind_for_client(
                "0.0.0.0:0".parse().expect("valid wildcard bind address"),
                &client,
                candidate_id,
                video_path_binding,
                now_unix_s(),
            ) {
                Ok(session) => {
                    let actions = video_path.apply(DirectVideoPathEvent::StartOffer {
                        candidate: session.candidate().clone(),
                        now_unix_s: now_unix_s(),
                    });
                    let mut sent = false;
                    for action in actions {
                        if let DirectVideoPathAction::SendOffer(candidate) = action
                            && send_control(
                                &client,
                                &secure,
                                &ControlMessage::VideoPathCandidateOffer { candidate },
                            )
                            .await
                            .is_ok()
                        {
                            sent = true;
                        }
                    }
                    sent.then_some(session)
                }
                Err(_) => None,
            }
        } else {
            None
        };
        Ok(Self {
            data_plane,
            lan,
            host_capabilities,
            client,
            secure,
            input_sequence: Arc::new(Mutex::new(InputSequencer::new())),
            control_order: Arc::new(Mutex::new(())),
            #[cfg(test)]
            received_transport_events: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            input_writes_started: Arc::new(AtomicU64::new(0)),
            assembler: FrameAssembler::new(ASSEMBLY_CAPACITY, ASSEMBLY_MAX_AGE),
            reliable_assembler: FrameAssembler::new(ASSEMBLY_CAPACITY, RELIABLE_ASSEMBLY_MAX_AGE),
            video_config: None,
            pending_video_datagrams: VecDeque::new(),
            pending_video_reliable: VecDeque::new(),
            active_stream: AtomicU64::new(0),
            metrics: AtomicControllerMetrics::default(),
            keyframe_needed_after_config: false,
            keyframe_fallback_deadline: None,
            video_continuity: VideoContinuity::default(),
            last_cursor_sequence: 0,
            reliable_trace_started: None,
            video_path,
            direct_session,
            direct_connection: None,
            last_direct_datagram_at: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn receive_test_state(&self) -> ControllerReceiveTestState {
        ControllerReceiveTestState {
            client: self.client.clone(),
            dequeued: self.received_transport_events.clone(),
            secure: self.secure.clone(),
            input_order: self.input_sequence.clone(),
            writes_started: self.input_writes_started.clone(),
        }
    }

    pub fn video_path_state(&self) -> DirectVideoPathState {
        if self.data_plane.is_some() {
            return if self.video_path_kind() == VideoPathKind::DirectLan {
                DirectVideoPathState::Direct {
                    candidate_id: self
                        .lan
                        .as_ref()
                        .and_then(|lan| lan.candidate.as_ref())
                        .map_or(0, DirectLanCandidate::candidate_id),
                    allows_experimental_4k: false,
                }
            } else {
                DirectVideoPathState::Relay
            };
        }
        self.video_path.state().clone()
    }

    pub fn host_capabilities(&self) -> &DeviceCapabilities {
        &self.host_capabilities
    }

    pub(crate) async fn send_controller_control(
        &self,
        message: ControlMessage,
    ) -> Result<(), ControllerError> {
        self.control_sender().send(message).await
    }

    pub(crate) fn control_sender(&self) -> ControllerControlSender {
        ControllerControlSender {
            data_plane: self.data_plane.clone(),
            client: self.client.clone(),
            secure: self.secure.clone(),
            order: self.control_order.clone(),
        }
    }

    /// Reports the path currently carrying video datagrams. The authenticated
    /// direct connection wins over the negotiation state because the responder
    /// may establish the QUIC data path before the control answer is observed.
    pub fn video_path_kind(&self) -> VideoPathKind {
        if let Some(health) = self.session_data_health() {
            return if !health.closed
                && health.direct_path.is_some()
                && matches!(
                    health.lanes[ChannelKind::VideoDatagram as usize - 1],
                    SessionLaneHealth::Direct(_)
                ) {
                VideoPathKind::DirectLan
            } else {
                VideoPathKind::Relay
            };
        }
        if self.direct_connection.is_some()
            || matches!(self.video_path.state(), DirectVideoPathState::Direct { .. })
        {
            VideoPathKind::DirectLan
        } else {
            VideoPathKind::Relay
        }
    }

    /// Returns the most recent authenticated DirectLan probe quality. The
    /// value remains available after fallback so diagnostics can explain why
    /// a session is currently relayed without reporting relay as direct.
    pub fn video_path_quality(&self) -> Option<VideoPathQuality> {
        if let Some(lan) = &self.lan {
            return lan.quality;
        }
        self.video_path.last_quality()
    }

    pub fn video_path_fallback_reason(&self) -> Option<DirectVideoPathFallbackReason> {
        self.video_path.last_fallback_reason()
    }

    pub fn direct_candidate(&self) -> Option<&DirectLanCandidate> {
        self.direct_session
            .as_ref()
            .map(DirectLanSession::candidate)
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
        // Legacy callers are deliberately fail-closed until they capture a
        // presentation context. No network command acquires a current epoch.
        self.send_input_with_context(
            InputContext {
                stream_id: 0,
                input_epoch: 0,
            },
            event,
        )
        .await
    }

    pub async fn send_input_with_context(
        &self,
        context: InputContext,
        event: InputEvent,
    ) -> Result<(), ControllerError> {
        send_session_input(
            &self.client,
            &self.secure,
            &self.input_sequence,
            self.data_plane.as_deref(),
            context,
            event,
            #[cfg(test)]
            &self.input_writes_started,
        )
        .await
    }

    /// Clone the input lane once for a session-owned send future that can run
    /// alongside `next_event`. Runtime and all clones share one wire sequence.
    pub fn input_sender(&self) -> ControllerInputSender {
        ControllerInputSender {
            data_plane: self.data_plane.clone(),
            client: self.client.clone(),
            secure: self.secure.clone(),
            input_sequence: self.input_sequence.clone(),
            #[cfg(test)]
            input_writes_started: self.input_writes_started.clone(),
        }
    }

    pub async fn request_keyframe(&self) -> Result<(), ControllerError> {
        let stream_id = self
            .active_stream_id()
            .ok_or(ControllerError::NoActiveStream)?;
        self.request_keyframe_for(stream_id).await
    }

    pub async fn select_display(&self, display_id: u32) -> Result<(), ControllerError> {
        self.send_controller_control(ControlMessage::SelectDisplay { display_id })
            .await
    }

    pub async fn set_audio_enabled(&self, enabled: bool) -> Result<(), ControllerError> {
        self.send_controller_control(ControlMessage::SetAudioEnabled { enabled })
            .await
    }

    pub async fn set_video_quality(
        &self,
        preference: VideoQualityPreference,
    ) -> Result<(), ControllerError> {
        self.send_controller_control(ControlMessage::SetVideoQuality { preference })
            .await
    }

    pub async fn set_video_profile(&self, profile: H264Profile) -> Result<(), ControllerError> {
        self.send_controller_control(ControlMessage::SetVideoProfile { profile })
            .await
    }

    pub async fn report_video_network_feedback(
        &self,
        received_packets: u32,
        dropped_packets: u32,
        decode_queue_peak: u16,
        freshness_recoveries: u16,
    ) -> Result<(), ControllerError> {
        self.send_controller_control(ControlMessage::VideoNetworkFeedback {
            received_packets,
            dropped_packets,
            decode_queue_peak,
            freshness_recoveries,
        })
        .await
    }

    pub async fn send_transfer(&self, message: TransferMessage) -> Result<(), ControllerError> {
        self.transfer_sender().send(message).await
    }

    pub fn transfer_sender(&self) -> ControllerTransferSender {
        ControllerTransferSender {
            data_plane: self.data_plane.clone(),
            client: self.client.clone(),
            secure: self.secure.clone(),
        }
    }

    pub async fn next_event(&mut self) -> Result<ControllerEvent, ControllerError> {
        self.next_event_with_transfer(true).await
    }

    pub async fn next_event_with_transfer(
        &mut self,
        include_transfer: bool,
    ) -> Result<ControllerEvent, ControllerError> {
        loop {
            if self.video_config.is_some()
                && let Some(ciphertext) = self.pending_video_reliable.pop_front()
            {
                if let Some(event) = self.handle_video_reliable(ciphertext).await? {
                    return Ok(event);
                }
                continue;
            }
            if self.video_config.is_some()
                && let Some(ciphertext) = self.pending_video_datagrams.pop_front()
            {
                if let Some(event) = self.handle_video_datagram(ciphertext).await? {
                    return Ok(event);
                }
                continue;
            }
            let transport_event = if let Some(deadline) = self.keyframe_fallback_deadline {
                match tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    self.next_inbound_event(include_transfer),
                )
                .await
                {
                    Ok(event) => event?,
                    Err(_) => {
                        self.keyframe_fallback_deadline = None;
                        if self.video_continuity.awaiting_keyframe()
                            && self.pending_video_reliable.is_empty()
                            && self.pending_video_datagrams.is_empty()
                            && let Some(stream_id) = self.active_stream_id()
                        {
                            self.request_keyframe_for(stream_id).await?;
                            self.video_continuity.note_keyframe_request(Instant::now());
                        }
                        continue;
                    }
                }
            } else {
                self.next_inbound_event(include_transfer).await?
            };
            #[cfg(test)]
            self.received_transport_events
                .fetch_add(1, Ordering::SeqCst);
            match transport_event {
                ControllerInboundEvent::Data(ChannelKind::Control, ciphertext) => {
                    let plaintext = ciphertext.open(&self.secure, SecureLane::Control).await?;
                    let message = decode_control(&plaintext)?;
                    if let ControlMessage::AccessDenied { reason } = message {
                        return Err(ControllerError::AccessDenied(reason));
                    }
                    if self.data_plane.is_some() && self.handle_lan_control(&message).await? {
                        continue;
                    }
                    if let ControlMessage::LanDataSwitch { .. } = message {
                        return Err(SessionDataError::InvalidSwitch.into());
                    }
                    if let ControlMessage::VideoPathCandidateOffer { candidate } = message {
                        let local_candidate = self.direct_candidate().cloned();
                        let actions = self.video_path.apply(DirectVideoPathEvent::ReceiveOffer {
                            candidate,
                            local_candidate,
                            now_unix_s: now_unix_s(),
                        });
                        self.apply_video_path_actions(actions).await?;
                        continue;
                    }
                    if let ControlMessage::VideoPathCandidateAnswer {
                        candidate_id,
                        accepted,
                        candidate,
                    } = message
                    {
                        let actions = self.video_path.apply(DirectVideoPathEvent::ReceiveAnswer {
                            candidate_id,
                            accepted,
                            candidate,
                            now_unix_s: now_unix_s(),
                        });
                        self.apply_video_path_actions(actions).await?;
                        continue;
                    }
                    return Ok(ControllerEvent::Control(message));
                }
                ControllerInboundEvent::Data(ChannelKind::Input, _) => {
                    return Err(ControllerError::UnexpectedTransportLane);
                }
                ControllerInboundEvent::Data(ChannelKind::VideoConfig, ciphertext) => {
                    let plaintext = ciphertext
                        .open(&self.secure, SecureLane::VideoConfig)
                        .await?;
                    let config = decode_video_config(&plaintext)?;
                    let config_changed = self.video_config.as_ref() != Some(&config);
                    if let Some(current) = &self.video_config {
                        if config.stream_id == current.stream_id
                            && config.config_version < current.config_version
                        {
                            continue;
                        }
                        if !input_geometry_change_is_valid(current, &config) {
                            return Err(ControllerError::InconsistentVideoConfig);
                        }
                        if config.stream_id == current.stream_id
                            && config.config_version == current.config_version
                            && config != *current
                        {
                            return Err(ControllerError::InconsistentVideoConfig);
                        }
                    }
                    if self.active_stream_id() != Some(config.stream_id) {
                        if !self.assembler.begin_stream(config.stream_id)
                            || !self.reliable_assembler.begin_stream(config.stream_id)
                        {
                            continue;
                        }
                        self.active_stream
                            .store(config.stream_id, Ordering::Release);
                        self.last_cursor_sequence = 0;
                    }
                    self.video_config = Some(config.clone());
                    if config_changed {
                        self.video_continuity.reset_for_config();
                    }
                    // Config and the first keyframe use separate reliable
                    // streams. Keep a short fallback deadline instead of
                    // requesting immediately; otherwise a newer datagram
                    // keyframe can overtake and clear the reliable frame in
                    // the assembler while it is still arriving.
                    if config_changed || self.keyframe_needed_after_config {
                        self.keyframe_fallback_deadline =
                            Some(Instant::now() + INITIAL_KEYFRAME_GRACE);
                        self.video_continuity.note_keyframe_request(Instant::now());
                    }
                    self.keyframe_needed_after_config = false;
                    return Ok(ControllerEvent::VideoConfig(config));
                }
                ControllerInboundEvent::Data(ChannelKind::VideoReliable, ciphertext) => {
                    if self.video_config.is_none() {
                        if self.pending_video_reliable.len() >= PENDING_VIDEO_DATAGRAM_CAPACITY {
                            self.pending_video_reliable.pop_front();
                            self.drop_video_packet();
                        }
                        self.pending_video_reliable.push_back(ciphertext);
                        self.keyframe_needed_after_config = true;
                        continue;
                    }
                    if let Some(event) = self.handle_video_reliable(ciphertext).await? {
                        return Ok(event);
                    }
                }
                ControllerInboundEvent::Data(ChannelKind::VideoDatagram, ciphertext) => {
                    self.metrics
                        .received_video_packets
                        .fetch_add(1, Ordering::Relaxed);
                    if self.video_config.is_none() {
                        if self.pending_video_datagrams.len() >= PENDING_VIDEO_DATAGRAM_CAPACITY {
                            self.pending_video_datagrams.pop_front();
                            self.drop_video_packet();
                        }
                        self.pending_video_datagrams.push_back(ciphertext);
                        self.keyframe_needed_after_config = true;
                        continue;
                    }
                    if let Some(event) = self.handle_video_datagram(ciphertext).await? {
                        return Ok(event);
                    }
                }
                ControllerInboundEvent::Data(ChannelKind::CursorDatagram, ciphertext) => {
                    let Some(plaintext) = ciphertext
                        .open_datagram(&self.secure, SecureLane::CursorDatagram)
                        .await?
                    else {
                        continue;
                    };
                    let cursor = decode_cursor_update(&plaintext)?;
                    if self
                        .active_stream_id()
                        .is_some_and(|id| id != cursor.stream_id)
                    {
                        continue;
                    }
                    if !cursor_sequence_is_newer(self.last_cursor_sequence, cursor.sequence) {
                        // Relay datagrams are forwarded by independent tasks
                        // and may arrive out of order. A stale cursor must
                        // never move the local pointer backwards after a
                        // newer update has already been rendered.
                        continue;
                    }
                    self.last_cursor_sequence = cursor.sequence;
                    return Ok(ControllerEvent::Cursor(cursor));
                }
                ControllerInboundEvent::Data(ChannelKind::AudioDatagram, ciphertext) => {
                    let Some(plaintext) = ciphertext
                        .open_datagram(&self.secure, SecureLane::AudioDatagram)
                        .await?
                    else {
                        continue;
                    };
                    let packet = decode_audio_packet(&plaintext)?;
                    if self
                        .active_stream_id()
                        .is_some_and(|id| id != packet.stream_id)
                    {
                        continue;
                    }
                    return Ok(ControllerEvent::Audio(packet));
                }
                ControllerInboundEvent::Data(ChannelKind::Transfer, ciphertext) => {
                    let plaintext = ciphertext.open(&self.secure, SecureLane::Transfer).await?;
                    return Ok(ControllerEvent::Transfer(decode_transfer(&plaintext)?));
                }
                ControllerInboundEvent::PeerDisconnected { channel } => {
                    return Ok(ControllerEvent::Closed {
                        reason: format!("远端会话已断开（{channel:?}）"),
                    });
                }
                ControllerInboundEvent::Closed { reason } => {
                    return Ok(ControllerEvent::Closed { reason });
                }
            }
        }
    }

    async fn handle_video_datagram(
        &mut self,
        ciphertext: ControllerPayload,
    ) -> Result<Option<ControllerEvent>, ControllerError> {
        let Some(plaintext) = ciphertext
            .open_datagram(&self.secure, SecureLane::VideoDatagram)
            .await?
        else {
            // QUIC datagrams may be duplicated by the network or by a relay
            // retry. The packet is authenticated and the replay window has
            // rejected it, so dropping it is safe and keeps a transient
            // duplicate from tearing down the whole session.
            self.drop_video_packet();
            return Ok(None);
        };
        self.handle_video_packet(
            decode_video_packet(&plaintext)?,
            VideoPacketSource::Datagram,
        )
        .await
    }

    async fn handle_video_reliable(
        &mut self,
        ciphertext: ControllerPayload,
    ) -> Result<Option<ControllerEvent>, ControllerError> {
        let plaintext = ciphertext
            .open(&self.secure, SecureLane::VideoReliable)
            .await?;
        let packets = match decode_video_reliable_batch(&plaintext) {
            Ok(packets) => packets,
            Err(_) => vec![decode_video_packet(&plaintext)?],
        };
        self.metrics
            .received_video_reliable_packets
            .fetch_add(packets.len() as u64, Ordering::Relaxed);
        for packet in packets {
            if let Some(event) = self
                .handle_video_packet(packet, VideoPacketSource::Reliable)
                .await?
            {
                return Ok(Some(event));
            }
        }
        Ok(None)
    }

    async fn handle_video_packet(
        &mut self,
        packet: VideoPacket,
        source: VideoPacketSource,
    ) -> Result<Option<ControllerEvent>, ControllerError> {
        let Some(config) = &self.video_config else {
            self.drop_video_packet();
            self.keyframe_needed_after_config = true;
            return Ok(None);
        };
        if packet.header.stream_id != config.stream_id
            || packet.header.config_version != config.config_version
            || packet.header.width != config.width
            || packet.header.height != config.height
        {
            self.drop_video_packet();
            return Ok(None);
        }
        let is_keyframe = packet.header.flags.0 & desklink_protocol::FrameFlags::KEYFRAME.0 != 0;
        if self.video_continuity.awaiting_keyframe() {
            if !is_keyframe {
                self.drop_video_packet();
                if self
                    .keyframe_fallback_deadline
                    .is_some_and(|deadline| Instant::now() < deadline)
                {
                    return Ok(None);
                }
                if self.video_continuity.retry_keyframe_request(Instant::now()) {
                    self.request_keyframe_for(config.stream_id).await?;
                }
                return Ok(None);
            }
            if packet.header.chunk_index == 0 {
                match source {
                    VideoPacketSource::Datagram => self
                        .assembler
                        .discard_pending_before(packet.header.stream_id, packet.header.frame_id),
                    VideoPacketSource::Reliable => self
                        .reliable_assembler
                        .discard_pending_before(packet.header.stream_id, packet.header.frame_id),
                }
            }
        }
        let stream_id = config.stream_id;
        if matches!(source, VideoPacketSource::Reliable) {
            if packet.header.chunk_index == 0 && self.reliable_trace_started.is_none() {
                self.reliable_trace_started = Some(Instant::now());
                eprintln!(
                    "DeskLink reliable keyframe assembly started (stream_id={} frame_id={} chunks={})",
                    packet.header.stream_id, packet.header.frame_id, packet.header.chunk_count,
                );
            } else if packet.header.chunk_index + 1 == packet.header.chunk_count
                && let Some(started) = self.reliable_trace_started
            {
                eprintln!(
                    "DeskLink reliable keyframe assembly ended (stream_id={} frame_id={} elapsed_ms={})",
                    packet.header.stream_id,
                    packet.header.frame_id,
                    started.elapsed().as_millis(),
                );
            }
        }
        let assembler = match source {
            VideoPacketSource::Datagram => &mut self.assembler,
            VideoPacketSource::Reliable => &mut self.reliable_assembler,
        };
        let assembled = assembler.push(Instant::now(), packet);
        let dropped_chunks = assembler.take_dropped_chunks();
        if dropped_chunks > 0 {
            self.metrics
                .dropped_video_packets
                .fetch_add(dropped_chunks, Ordering::Relaxed);
            self.video_continuity.note_transport_loss();
        }
        match assembled {
            AssembleResult::Pending => Ok(None),
            AssembleResult::Dropped(_) => {
                self.drop_video_packet();
                Ok(None)
            }
            AssembleResult::Complete(frame) => {
                let is_keyframe = frame.flags.0 & desklink_protocol::FrameFlags::KEYFRAME.0 != 0;
                match self.video_continuity.observe_frame(
                    frame.frame_id,
                    is_keyframe,
                    Instant::now(),
                ) {
                    VideoContinuityAction::Present => {
                        if assembler.accept_for_present(frame.clone()) {
                            self.keyframe_fallback_deadline = None;
                            self.metrics
                                .completed_frames
                                .fetch_add(1, Ordering::Relaxed);
                            Ok(Some(ControllerEvent::H264AccessUnit(frame)))
                        } else {
                            self.drop_video_packet();
                            Ok(None)
                        }
                    }
                    VideoContinuityAction::Drop => {
                        self.drop_video_packet();
                        Ok(None)
                    }
                    VideoContinuityAction::DropAndRequestKeyframe => {
                        self.drop_video_packet();
                        self.request_keyframe_for(stream_id).await?;
                        Ok(None)
                    }
                }
            }
        }
    }

    async fn next_transport_event(
        &mut self,
        include_transfer: bool,
    ) -> Result<TransportEvent, ControllerError> {
        loop {
            if let Some(connection) = self.direct_connection.clone() {
                let last_datagram = *self
                    .last_direct_datagram_at
                    .get_or_insert_with(Instant::now);
                let deadline = last_datagram + DIRECT_VIDEO_SILENCE_TIMEOUT;
                let direct = if Instant::now() >= deadline {
                    // At expiry, an already queued direct packet still proves
                    // liveness. Otherwise fall back before servicing more relay
                    // events, so continuous cursor traffic cannot defer expiry.
                    tokio::select! {
                        biased;
                        direct = connection.recv_datagram() => Some(direct),
                        () = std::future::ready(()) => None,
                    }
                } else {
                    // Preserve fairness between live direct and relay lanes.
                    // The timeout is absolute and survives every relay return.
                    tokio::select! {
                        relay = self.client.next_event_with_transfer(include_transfer) => return relay.map_err(ControllerError::from),
                        direct = connection.recv_datagram() => Some(direct),
                        () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => continue,
                    }
                };
                match direct {
                    Some(Ok(bytes)) => {
                        self.last_direct_datagram_at = Some(Instant::now());
                        return Ok(TransportEvent::VideoDatagram(bytes));
                    }
                    Some(Err(_)) => {
                        self.direct_connection = None;
                        self.last_direct_datagram_at = None;
                        let actions = self.video_path.apply(DirectVideoPathEvent::Stop);
                        self.apply_video_path_actions(actions).await?;
                    }
                    None => {
                        self.direct_connection = None;
                        self.last_direct_datagram_at = None;
                        connection.close(b"desklink direct video silence timeout");
                        let actions = self.video_path.apply(DirectVideoPathEvent::DatagramSilence);
                        self.apply_video_path_actions(actions).await?;
                    }
                }
                continue;
            }

            let Some(session) = self.direct_session.as_ref() else {
                return self
                    .client
                    .next_event_with_transfer(include_transfer)
                    .await
                    .map_err(ControllerError::from);
            };
            let endpoint = session.endpoint();
            let expected_candidate_id = session.candidate().candidate_id();
            let expected_binding = *session.session_binding();
            tokio::select! {
                relay = self.client.next_event_with_transfer(include_transfer) => return relay.map_err(ControllerError::from),
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else {
                        self.direct_session = None;
                        continue;
                    };
                    let Ok(connection) = incoming else {
                        continue;
                    };
                    let accepted = endpoint
                            .accept_probe_connection_shared(
                                connection,
                                expected_candidate_id,
                                &expected_binding,
                                &self.secure,
                                now_unix_s(),
                            )
                            .await;
                    if let Ok((connection, result)) = accepted {
                        self.direct_connection = Some(Arc::new(connection));
                        self.last_direct_datagram_at = Some(Instant::now());
                        let actions = self.video_path.apply(DirectVideoPathEvent::ProbeSucceeded {
                            candidate_id: self.probe_candidate_id(result.candidate_id),
                            quality: VideoPathQuality {
                                kind: VideoPathKind::DirectLan,
                                rtt_ms: result.rtt_ms,
                                // The handshake measures RTT only. Until the
                                // video datagram feedback loop supplies a real
                                // loss sample, keep the 4K gate closed.
                                loss_basis_points: u16::MAX,
                            },
                        });
                        self.apply_video_path_actions(actions).await?;
                    }
                }
            }
        }
    }

    async fn apply_video_path_actions(
        &mut self,
        actions: Vec<DirectVideoPathAction>,
    ) -> Result<(), ControllerError> {
        let mut pending: Vec<_> = actions.into_iter().rev().collect();
        while let Some(action) = pending.pop() {
            match action {
                DirectVideoPathAction::SendOffer(candidate) => {
                    self.send_controller_control(ControlMessage::VideoPathCandidateOffer {
                        candidate,
                    })
                    .await?;
                }
                DirectVideoPathAction::SendAnswer {
                    candidate_id,
                    accepted,
                    candidate,
                } => {
                    self.send_controller_control(ControlMessage::VideoPathCandidateAnswer {
                        candidate_id,
                        accepted,
                        candidate,
                    })
                    .await?;
                }
                DirectVideoPathAction::StartProbe { candidate, .. } => {
                    let Some(session) = self.direct_session.as_ref() else {
                        pending.extend(self.video_path.apply(DirectVideoPathEvent::ProbeFailed {
                            candidate_id: candidate.candidate_id(),
                        }));
                        continue;
                    };
                    let endpoint = session.endpoint();
                    let own_candidate_id = session.candidate().candidate_id();
                    let binding = *session.session_binding();
                    let result = connect_or_accept_direct(
                        endpoint,
                        candidate.clone(),
                        own_candidate_id,
                        binding,
                        self.secure.clone(),
                    )
                    .await;
                    match result {
                        Ok((connection, probe)) => {
                            self.direct_connection = Some(Arc::new(connection));
                            self.last_direct_datagram_at = Some(Instant::now());
                            pending.extend(self.video_path.apply(
                                DirectVideoPathEvent::ProbeSucceeded {
                                    candidate_id: self.probe_candidate_id(probe.candidate_id),
                                    quality: VideoPathQuality {
                                        kind: VideoPathKind::DirectLan,
                                        rtt_ms: probe.rtt_ms,
                                        // A successful probe does not prove
                                        // that the video path has acceptable
                                        // packet loss for experimental 4K.
                                        loss_basis_points: u16::MAX,
                                    },
                                },
                            ));
                        }
                        Err(_) => {
                            pending.extend(self.video_path.apply(
                                DirectVideoPathEvent::ProbeFailed {
                                    candidate_id: candidate.candidate_id(),
                                },
                            ));
                        }
                    }
                }
                DirectVideoPathAction::ActivateDirect { .. } => {}
                DirectVideoPathAction::UseRelay { .. } => {
                    self.last_direct_datagram_at = None;
                    if let Some(connection) = self.direct_connection.take() {
                        connection.close(b"desklink direct video fallback");
                    }
                }
            }
        }
        Ok(())
    }

    fn probe_candidate_id(&self, fallback: u64) -> u64 {
        match self.video_path.state() {
            DirectVideoPathState::Offering { candidate }
            | DirectVideoPathState::Probing { candidate, .. } => candidate.candidate_id(),
            _ => fallback,
        }
    }

    async fn request_keyframe_for(&self, stream_id: u64) -> Result<(), ControllerError> {
        self.send_controller_control(ControlMessage::RequestKeyframe { stream_id })
            .await
    }

    fn drop_video_packet(&self) {
        self.metrics
            .dropped_video_packets
            .fetch_add(1, Ordering::Relaxed);
    }
}

async fn connect_or_accept_direct(
    endpoint: Arc<DirectLanEndpoint>,
    candidate: DirectLanCandidate,
    own_candidate_id: u64,
    binding: [u8; 16],
    secure: Arc<Mutex<SecureSession>>,
) -> Result<(DirectLanConnection, DirectLanProbeResult), ()> {
    // Both Windows peers advertise a candidate and the host normally
    // initiates the QUIC probe. Give an incoming probe a short grace window
    // first; holding the shared Noise session while an outgoing QUIC connect
    // waits would otherwise starve the responder and create a false timeout.
    let accept_deadline = tokio::time::Instant::now() + Duration::from_millis(300);
    loop {
        let remaining = accept_deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let incoming = match tokio::time::timeout(remaining, endpoint.accept()).await {
            Ok(Some(incoming)) => incoming,
            Ok(None) | Err(_) => break,
        };
        let Ok(connection) = incoming else {
            continue;
        };
        match endpoint
            .accept_probe_connection_shared(
                connection,
                own_candidate_id,
                &binding,
                &secure,
                now_unix_s(),
            )
            .await
        {
            Ok(result) => return Ok(result),
            Err(_) => continue,
        }
    }

    endpoint
        .connect_shared(&candidate, &binding, &secure, now_unix_s())
        .await
        .map_err(|_| ())
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
) -> Result<DeviceCapabilities, ControllerError> {
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
        &ControlMessage::Capabilities(lan_runtime::controller_capabilities(platform)),
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
                        && capabilities.supports_h264_profile(H264Profile::Main) =>
                {
                    return Ok(capabilities);
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
                ControlMessage::LanDataSwitch { .. } => {
                    return Err(SessionDataError::InvalidSwitch.into());
                }
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

async fn send_session_input(
    client: &QuicClient,
    secure: &Arc<Mutex<SecureSession>>,
    input_sequence: &Mutex<InputSequencer>,
    data_plane: Option<&SessionDataPlane>,
    context: InputContext,
    event: InputEvent,
    #[cfg(test)] writes_started: &AtomicU64,
) -> Result<(), ControllerError> {
    context.validate()?;
    // This input-only guard preserves allocation and wire order across Runtime
    // and cloned senders. The crypto guard inside seal is released before I/O.
    let mut sequence = input_sequence.lock().await;
    let plaintext = encode_input(&InputEnvelope {
        sequence: sequence.next_sequence(),
        timestamp_us: now_micros(),
        stream_id: context.stream_id,
        input_epoch: context.input_epoch,
        event,
    })?;
    if let Some(plane) = data_plane {
        let mut pending = PendingPlaneInput {
            plane,
            client,
            completed: false,
        };
        #[cfg(test)]
        writes_started.fetch_add(1, Ordering::SeqCst);
        plane.send(ChannelKind::Input, plaintext.into()).await?;
        pending.completed = true;
    } else {
        let ciphertext = seal(secure, SecureLane::Input, &plaintext).await?;
        #[cfg(test)]
        writes_started.fetch_add(1, Ordering::SeqCst);
        client.send_input(ciphertext).await?;
    }
    drop(sequence);
    Ok(())
}

struct PendingPlaneInput<'a> {
    plane: &'a SessionDataPlane,
    client: &'a QuicClient,
    completed: bool,
}

impl Drop for PendingPlaneInput<'_> {
    fn drop(&mut self) {
        if !self.completed {
            // An uncertain keyDown must not leave a healthy-looking session
            // accepting later keyUp on another path. Closing both transports
            // lets the Host's existing input exit guard release held input.
            self.plane.close();
            self.client.close(b"controller input write interrupted");
        }
    }
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

async fn open_datagram(
    secure: &Arc<Mutex<SecureSession>>,
    lane: SecureLane,
    ciphertext: &[u8],
) -> Result<Option<Vec<u8>>, ControllerError> {
    match secure.lock().await.open(lane, ciphertext) {
        Ok(plaintext) => Ok(Some(plaintext)),
        Err(CryptoError::ReplayRejected) => Ok(None),
        Err(error) => Err(ControllerError::Crypto(error)),
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
    now_micros() / 1_000_000
}

fn cursor_sequence_is_newer(last_sequence: u64, candidate_sequence: u64) -> bool {
    candidate_sequence > last_sequence
}

fn input_geometry_change_is_valid(previous: &VideoConfig, next: &VideoConfig) -> bool {
    previous.stream_id != next.stream_id
        || previous.input_epoch != next.input_epoch
        || (previous.width, previous.height) == (next.width, next.height)
}

fn next_direct_candidate_id() -> u64 {
    let mut candidate_id = 0_u64;
    while candidate_id == 0 {
        candidate_id = OsRng.next_u64();
    }
    candidate_id
}

#[cfg(test)]
mod tests {
    use super::*;
    use desklink_crypto::{DeviceIdentity, NoiseInitiator, NoiseResponder, SecureRole};
    use desklink_protocol::DirectLanCandidate;
    use rand_core::OsRng;

    #[test]
    fn quality_reconfiguration_keeps_geometry_epoch_but_resize_cannot_reuse_it() {
        let original = VideoConfig {
            protocol_version: PROTOCOL_VERSION,
            stream_id: 7,
            input_epoch: 91,
            config_version: 1,
            codec: Codec::H264,
            width: 1920,
            height: 1080,
            sequence_header: vec![1],
        };
        let mut next = original.clone();
        next.config_version = 2;
        next.sequence_header = vec![2];
        assert!(input_geometry_change_is_valid(&original, &next));
        next.width = 1280;
        next.height = 720;
        assert!(!input_geometry_change_is_valid(&original, &next));
        next.input_epoch = 92;
        assert!(input_geometry_change_is_valid(&original, &next));
        next.input_epoch = 1; // Identity is not ordered across a host restart.
        assert!(input_geometry_change_is_valid(&original, &next));
    }

    #[test]
    fn cursor_updates_reject_duplicates_and_out_of_order_datagrams() {
        assert!(cursor_sequence_is_newer(0, 1));
        assert!(cursor_sequence_is_newer(41, 42));
        assert!(!cursor_sequence_is_newer(42, 42));
        assert!(!cursor_sequence_is_newer(42, 41));
    }

    #[tokio::test]
    async fn direct_probe_accepts_peer_initiated_connection_without_blocking() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let binding = [31; 16];
        let now = now_unix_s();
        let controller_endpoint =
            Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let peer_endpoint =
            Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let controller_candidate = DirectLanCandidate::new(
            2,
            controller_endpoint.local_addr().unwrap(),
            now + 10,
            binding,
            now,
        )
        .unwrap();
        let unused_port = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let peer_candidate =
            DirectLanCandidate::new(1, unused_port, now + 10, binding, now).unwrap();
        let (mut peer_secure, controller_secure) = connected_secure_sessions();
        let controller_secure = Arc::new(Mutex::new(controller_secure));
        let controller_candidate_for_peer = controller_candidate.clone();
        let controller_candidate_id = controller_candidate.candidate_id();
        let peer_endpoint_for_task = peer_endpoint.clone();
        let peer_task = tokio::spawn(async move {
            peer_endpoint_for_task
                .connect(
                    &controller_candidate_for_peer,
                    &binding,
                    &mut peer_secure,
                    now,
                )
                .await
        });

        let (connection, result) = tokio::time::timeout(
            Duration::from_secs(3),
            connect_or_accept_direct(
                controller_endpoint.clone(),
                peer_candidate,
                controller_candidate_id,
                binding,
                controller_secure,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let (peer_connection, _) = peer_task.await.unwrap().unwrap();
        assert_eq!(result.candidate_id, controller_candidate_id);
        peer_connection.send_datagram(vec![4, 5, 6]).unwrap();
        let datagram = tokio::time::timeout(Duration::from_secs(1), connection.recv_datagram())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(datagram, vec![4, 5, 6]);
    }

    fn connected_secure_sessions() -> (SecureSession, SecureSession) {
        let mut rng = OsRng;
        let initiator_identity = DeviceIdentity::generate(&mut rng);
        let responder_identity = DeviceIdentity::generate(&mut rng);
        let initiator_verify_key = initiator_identity.verify_key();
        let responder_verify_key = responder_identity.verify_key();
        let (mut initiator, message_1) =
            NoiseInitiator::start(initiator_identity, responder_verify_key).unwrap();
        let (mut responder, message_2) =
            NoiseResponder::accept(&message_1, responder_identity, initiator_verify_key).unwrap();
        let message_3 = initiator.receive(&message_2).unwrap();
        responder.receive(&message_3).unwrap();
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

#[cfg(test)]
#[path = "controller/direct_silence_tests.rs"]
pub(crate) mod direct_silence_tests;
