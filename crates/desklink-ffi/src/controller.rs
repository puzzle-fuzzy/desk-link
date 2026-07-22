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
    AccessDenialReason, AudioPacket, Codec, ControlMessage, CursorUpdate, DeviceCapabilities,
    DeviceRole, DirectLanCandidate, H264Profile, InputEnvelope, InputEvent, NoiseHandshake,
    NoiseHandshakeStep, PROTOCOL_VERSION, Platform, ProtocolError, TransferMessage, VideoConfig,
    VideoQualityPreference, decode_audio_packet, decode_control, decode_cursor_update,
    decode_noise_handshake, decode_transfer, decode_video_config, decode_video_packet,
    encode_control, encode_input, encode_noise_handshake, encode_transfer,
};
use desklink_session::InputSequencer;
use desklink_transport::{
    DirectLanConnection, DirectLanEndpoint, DirectLanProbeResult, DirectLanSession,
    DirectVideoPathAction, DirectVideoPathEvent, DirectVideoPathMachine, DirectVideoPathState,
    QuicClient, TransportError, TransportEvent, VideoPathKind, VideoPathQuality,
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
    Audio(AudioPacket),
    Transfer(TransferMessage),
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
    video_continuity: VideoContinuity,
    video_path: DirectVideoPathMachine,
    direct_session: Option<DirectLanSession>,
    direct_connection: Option<Arc<DirectLanConnection>>,
}

#[derive(Clone)]
pub struct ControllerTransferSender {
    client: Arc<QuicClient>,
    secure: Arc<Mutex<SecureSession>>,
}

impl ControllerTransferSender {
    pub async fn send(&self, message: TransferMessage) -> Result<(), ControllerError> {
        let plaintext = encode_transfer(&message)?;
        let ciphertext = seal(&self.secure, SecureLane::Transfer, &plaintext).await?;
        self.client.send_transfer(ciphertext).await?;
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
        negotiate_host(&client, &secure, platform).await?;
        let video_path_binding = secure.lock().await.video_path_binding();
        let mut video_path = DirectVideoPathMachine::new(video_path_binding);
        let direct_session = if platform == Platform::Windows {
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
            client,
            secure,
            input_sequence: Mutex::new(InputSequencer::new()),
            assembler: FrameAssembler::new(ASSEMBLY_CAPACITY, ASSEMBLY_MAX_AGE),
            video_config: None,
            active_stream: AtomicU64::new(0),
            metrics: AtomicControllerMetrics::default(),
            keyframe_needed_after_config: false,
            video_continuity: VideoContinuity::default(),
            video_path,
            direct_session,
            direct_connection: None,
        })
    }

    pub fn video_path_state(&self) -> DirectVideoPathState {
        self.video_path.state().clone()
    }

    /// Reports the path currently carrying video datagrams. The authenticated
    /// direct connection wins over the negotiation state because the responder
    /// may establish the QUIC data path before the control answer is observed.
    pub fn video_path_kind(&self) -> VideoPathKind {
        if self.direct_connection.is_some()
            || matches!(self.video_path.state(), DirectVideoPathState::Direct { .. })
        {
            VideoPathKind::DirectLan
        } else {
            VideoPathKind::Relay
        }
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

    pub async fn set_audio_enabled(&self, enabled: bool) -> Result<(), ControllerError> {
        let plaintext = encode_control(&ControlMessage::SetAudioEnabled { enabled })?;
        let ciphertext = seal(&self.secure, SecureLane::Control, &plaintext).await?;
        self.client.send_control(ciphertext).await?;
        Ok(())
    }

    pub async fn set_video_quality(
        &self,
        preference: VideoQualityPreference,
    ) -> Result<(), ControllerError> {
        let plaintext = encode_control(&ControlMessage::SetVideoQuality { preference })?;
        let ciphertext = seal(&self.secure, SecureLane::Control, &plaintext).await?;
        self.client.send_control(ciphertext).await?;
        Ok(())
    }

    pub async fn set_video_profile(&self, profile: H264Profile) -> Result<(), ControllerError> {
        let plaintext = encode_control(&ControlMessage::SetVideoProfile { profile })?;
        let ciphertext = seal(&self.secure, SecureLane::Control, &plaintext).await?;
        self.client.send_control(ciphertext).await?;
        Ok(())
    }

    pub async fn report_video_network_feedback(
        &self,
        received_packets: u32,
        dropped_packets: u32,
        decode_queue_peak: u16,
        freshness_recoveries: u16,
    ) -> Result<(), ControllerError> {
        let plaintext = encode_control(&ControlMessage::VideoNetworkFeedback {
            received_packets,
            dropped_packets,
            decode_queue_peak,
            freshness_recoveries,
        })?;
        let ciphertext = seal(&self.secure, SecureLane::Control, &plaintext).await?;
        self.client.send_control(ciphertext).await?;
        Ok(())
    }

    pub async fn send_transfer(&self, message: TransferMessage) -> Result<(), ControllerError> {
        self.transfer_sender().send(message).await
    }

    pub fn transfer_sender(&self) -> ControllerTransferSender {
        ControllerTransferSender {
            client: self.client.clone(),
            secure: self.secure.clone(),
        }
    }

    pub async fn next_event(&mut self) -> Result<ControllerEvent, ControllerError> {
        loop {
            match self.next_transport_event().await? {
                TransportEvent::Control(ciphertext) => {
                    let plaintext = open(&self.secure, SecureLane::Control, &ciphertext).await?;
                    let message = decode_control(&plaintext)?;
                    if let ControlMessage::AccessDenied { reason } = message {
                        return Err(ControllerError::AccessDenied(reason));
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
                        self.video_continuity.reset_for_config();
                    }
                    if self.keyframe_needed_after_config {
                        self.request_keyframe_for(config.stream_id).await?;
                        self.keyframe_needed_after_config = false;
                        self.video_continuity.note_keyframe_request(Instant::now());
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
                    let assembled = self.assembler.push(Instant::now(), packet);
                    let dropped_chunks = self.assembler.take_dropped_chunks();
                    if dropped_chunks > 0 {
                        self.metrics
                            .dropped_video_packets
                            .fetch_add(dropped_chunks, Ordering::Relaxed);
                        self.video_continuity.note_transport_loss();
                    }
                    match assembled {
                        AssembleResult::Pending => {}
                        AssembleResult::Dropped(_) => self.drop_video_packet(),
                        AssembleResult::Complete(frame) => {
                            let is_keyframe =
                                frame.flags.0 & desklink_protocol::FrameFlags::KEYFRAME.0 != 0;
                            match self.video_continuity.observe_frame(
                                frame.frame_id,
                                is_keyframe,
                                Instant::now(),
                            ) {
                                VideoContinuityAction::Present => {
                                    if self.assembler.accept_for_present(frame.clone()) {
                                        self.metrics
                                            .completed_frames
                                            .fetch_add(1, Ordering::Relaxed);
                                        return Ok(ControllerEvent::H264AccessUnit(frame));
                                    }
                                    self.drop_video_packet();
                                }
                                VideoContinuityAction::Drop => self.drop_video_packet(),
                                VideoContinuityAction::DropAndRequestKeyframe => {
                                    self.drop_video_packet();
                                    self.request_keyframe_for(config.stream_id).await?;
                                }
                            }
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
                TransportEvent::AudioDatagram(ciphertext) => {
                    let plaintext =
                        open(&self.secure, SecureLane::AudioDatagram, &ciphertext).await?;
                    let packet = decode_audio_packet(&plaintext)?;
                    if self
                        .active_stream_id()
                        .is_some_and(|id| id != packet.stream_id)
                    {
                        continue;
                    }
                    return Ok(ControllerEvent::Audio(packet));
                }
                TransportEvent::Transfer(ciphertext) => {
                    let plaintext = open(&self.secure, SecureLane::Transfer, &ciphertext).await?;
                    return Ok(ControllerEvent::Transfer(decode_transfer(&plaintext)?));
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

    async fn next_transport_event(&mut self) -> Result<TransportEvent, ControllerError> {
        loop {
            if let Some(connection) = self.direct_connection.clone() {
                tokio::select! {
                    relay = self.client.next_event() => return relay.map_err(ControllerError::from),
                        direct = connection.recv_datagram() => match direct {
                            Ok(bytes) => return Ok(TransportEvent::VideoDatagram(bytes)),
                            Err(_) => {
                                self.direct_connection = None;
                                let actions = self.video_path.apply(DirectVideoPathEvent::Stop);
                                self.apply_video_path_actions(actions).await?;
                            }
                        },
                }
                continue;
            }

            let Some(session) = self.direct_session.as_ref() else {
                return self
                    .client
                    .next_event()
                    .await
                    .map_err(ControllerError::from);
            };
            let endpoint = session.endpoint();
            let expected_candidate_id = session.candidate().candidate_id();
            let expected_binding = *session.session_binding();
            tokio::select! {
                relay = self.client.next_event() => return relay.map_err(ControllerError::from),
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else {
                        self.direct_session = None;
                        continue;
                    };
                    let Ok(connection) = incoming else {
                        continue;
                    };
                    let accepted = {
                        let mut secure = self.secure.lock().await;
                        endpoint
                            .accept_probe_connection(
                                connection,
                                expected_candidate_id,
                                &expected_binding,
                                &mut secure,
                                now_unix_s(),
                            )
                            .await
                    };
                    if let Ok((connection, result)) = accepted {
                        self.direct_connection = Some(Arc::new(connection));
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
                    send_control(
                        &self.client,
                        &self.secure,
                        &ControlMessage::VideoPathCandidateOffer { candidate },
                    )
                    .await?;
                }
                DirectVideoPathAction::SendAnswer {
                    candidate_id,
                    accepted,
                    candidate,
                } => {
                    send_control(
                        &self.client,
                        &self.secure,
                        &ControlMessage::VideoPathCandidateAnswer {
                            candidate_id,
                            accepted,
                            candidate,
                        },
                    )
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
        let mut secure = secure.lock().await;
        match endpoint
            .accept_probe_connection(
                connection,
                own_candidate_id,
                &binding,
                &mut secure,
                now_unix_s(),
            )
            .await
        {
            Ok(result) => return Ok(result),
            Err(_) => continue,
        }
    }

    let mut secure = secure.lock().await;
    endpoint
        .connect(&candidate, &binding, &mut secure, now_unix_s())
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
            h264_profiles: supported_h264_profiles(platform),
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
                        && capabilities.supports_h264_profile(H264Profile::Main) =>
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
                | ControlMessage::SelectDisplay { .. }
                | ControlMessage::SetAudioEnabled { .. }
                | ControlMessage::AudioState { .. }
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

fn now_unix_s() -> u64 {
    now_micros() / 1_000_000
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
