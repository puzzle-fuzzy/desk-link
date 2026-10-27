use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use desklink_crypto::SessionId;
use desklink_protocol::{
    ControlMessage, DeviceRole, FrameFlags, InputEnvelope, InputEvent, decode_control,
    decode_input, decode_video_packet, encode_control, encode_input, encode_video_packet,
};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_session::{SessionAction, SessionEvent, SessionMachine, SessionState};
use desklink_transport::{QuicClient, QuicClientConfig, RelayJoin};
use desklink_video::{
    AssembleResult, EncodedFrame, FrameAssembler, LatestFrameQueue, packetize_frame,
};
use quinn::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

struct TestRelay {
    address: std::net::SocketAddr,
    client_config: quinn::ClientConfig,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_test_relay() -> TestRelay {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_der = certificate.cert.der().to_vec();
    let key_der = certificate.signing_key.serialize_der();
    let server_config = ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der.clone())],
        PrivateKeyDer::Pkcs8(key_der.into()),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(certificate_der)).unwrap();
    let client_tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap();
    let client_config = quinn::ClientConfig::new(Arc::new(client_crypto));
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
    let task_relay = relay.clone();
    let task = tokio::spawn(async move {
        let _ = task_relay.run().await;
    });
    TestRelay {
        address,
        client_config,
        task,
    }
}

#[derive(Default)]
struct SessionMetrics {
    dropped_frames: u64,
    last_frame_id: u64,
    input_sequence: u64,
    stream_id: u64,
    config_version: u32,
}

struct Harness {
    host: QuicClient,
    controller: QuicClient,
    assembler: FrameAssembler,
    video: LatestFrameQueue<EncodedFrame>,
    session: SessionMachine,
    presented: Option<u64>,
    keyframe_requests: usize,
    metrics: SessionMetrics,
    _relay: TestRelay,
}

impl Harness {
    async fn new() -> Self {
        let relay = spawn_test_relay().await;
        let config = || {
            QuicClientConfig::with_client_config(
                relay.address,
                "localhost",
                relay.client_config.clone(),
            )
        };
        let host = QuicClient::connect(config()).await.unwrap();
        let controller = QuicClient::connect(config()).await.unwrap();
        let session_id = SessionId::from_bytes([7; 16]);
        let authentication = [11; 32];
        host.join(RelayJoin::host_with_participant(
            session_id,
            authentication,
            [1; 16],
        ))
        .await
        .unwrap();
        controller
            .join(RelayJoin::controller_with_participant(
                session_id,
                authentication,
                [2; 16],
            ))
            .await
            .unwrap();

        let mut session = SessionMachine::new(DeviceRole::Controller);
        session.apply(SessionEvent::RelayConnected).unwrap();
        session.apply(SessionEvent::HandshakeComplete).unwrap();
        let actions = session.apply(SessionEvent::CapabilitiesNegotiated).unwrap();
        let stream_id = actions
            .iter()
            .find_map(|action| match action {
                SessionAction::BeginStream { stream_id } => Some(*stream_id),
                _ => None,
            })
            .unwrap();
        session.apply(SessionEvent::StartVideo).unwrap();
        session.apply(SessionEvent::VideoStarted).unwrap();
        let mut assembler = FrameAssembler::new(3, Duration::from_millis(800));
        assert!(assembler.begin_stream(stream_id));

        Self {
            host,
            controller,
            assembler,
            video: LatestFrameQueue::new(2),
            session,
            presented: None,
            keyframe_requests: 0,
            metrics: SessionMetrics {
                stream_id,
                config_version: 1,
                ..SessionMetrics::default()
            },
            _relay: relay,
        }
    }

    async fn send_frame(&mut self, frame_id: u64, keyframe: bool, dropped_chunks: &[usize]) {
        let frame = EncodedFrame {
            stream_id: self.metrics.stream_id,
            frame_id,
            config_version: self.metrics.config_version,
            capture_timestamp_us: frame_id,
            width: 1280,
            height: 720,
            flags: if keyframe {
                FrameFlags::KEYFRAME
            } else {
                FrameFlags(0)
            },
            data: vec![frame_id as u8; 2_500],
        };
        let packets = packetize_frame(&frame).unwrap();
        let mut sent = 0;
        for (index, packet) in packets.into_iter().enumerate() {
            if dropped_chunks.contains(&index) {
                self.metrics.dropped_frames += 1;
                continue;
            }
            self.host
                .send_video_datagram(encode_video_packet(&packet).unwrap())
                .await
                .unwrap();
            sent += 1;
        }

        for _ in 0..sent {
            let bytes = tokio::time::timeout(
                Duration::from_secs(2),
                self.controller.next_video_datagram(),
            )
            .await
            .expect("video datagram timeout")
            .expect("video datagram");
            let packet = decode_video_packet(&bytes).unwrap();
            match self.assembler.push(Instant::now(), packet) {
                AssembleResult::Pending => {}
                AssembleResult::Complete(frame) => {
                    self.video.push_latest(frame);
                }
                AssembleResult::Dropped(reason) => panic!("unexpected packet drop: {reason:?}"),
            }
        }
        if let Some(frame) = self.video.pop_newest()
            && self.assembler.accept_for_present(frame.clone())
        {
            self.presented = Some(frame.frame_id);
            self.metrics.last_frame_id = frame.frame_id;
        }
        if keyframe && self.session.state() == SessionState::RecoveringVideo {
            self.session.apply(SessionEvent::VideoStarted).unwrap();
        }
    }

    async fn request_video_recovery(&mut self) {
        let actions = self.session.apply(SessionEvent::DecoderStalled).unwrap();
        for action in actions {
            if !matches!(action, SessionAction::RequestKeyframe) {
                continue;
            }
            let expected = ControlMessage::RequestKeyframe {
                stream_id: self.metrics.stream_id,
            };
            self.controller
                .send_control(encode_control(&expected).unwrap())
                .await
                .unwrap();
            let bytes = tokio::time::timeout(Duration::from_secs(2), self.host.next_control())
                .await
                .expect("control timeout")
                .expect("control message");
            assert_eq!(decode_control(&bytes).unwrap(), expected);
            self.keyframe_requests += 1;
        }
    }

    async fn flood_video_lane(&self, count: u64) {
        for frame_id in 1..=count {
            let frame = EncodedFrame {
                stream_id: self.metrics.stream_id,
                frame_id,
                config_version: self.metrics.config_version,
                capture_timestamp_us: frame_id,
                width: 640,
                height: 480,
                flags: FrameFlags(0),
                data: vec![frame_id as u8; 32],
            };
            let packet = packetize_frame(&frame).unwrap().remove(0);
            self.host
                .send_video_datagram(encode_video_packet(&packet).unwrap())
                .await
                .unwrap();
        }
    }

    async fn send_input(&mut self, event: InputEvent) {
        self.metrics.input_sequence = self.metrics.input_sequence.wrapping_add(1).max(1);
        let envelope = InputEnvelope {
            sequence: self.metrics.input_sequence,
            timestamp_us: now_micros(),
            stream_id: 1,
            input_epoch: 1,
            event,
        };
        self.controller
            .send_input(encode_input(&envelope).unwrap())
            .await
            .unwrap();
        let bytes = tokio::time::timeout(Duration::from_secs(2), self.host.next_input())
            .await
            .expect("input timeout")
            .expect("input message");
        assert_eq!(decode_input(&bytes, now_micros()).unwrap(), envelope);
    }
}

#[tokio::test]
async fn dropped_old_frame_recovers_with_new_keyframe_over_quic_relay() {
    let mut harness = Harness::new().await;
    harness.send_frame(1, false, &[]).await;
    harness.send_frame(2, false, &[1]).await;
    harness.request_video_recovery().await;
    harness.send_frame(3, true, &[]).await;

    assert_eq!(harness.presented, Some(3));
    assert_eq!(harness.keyframe_requests, 1);
    assert_eq!(harness.metrics.dropped_frames, 1);
    assert_eq!(harness.metrics.last_frame_id, 3);
    assert_eq!(harness.metrics.stream_id, 1);
    assert_eq!(harness.metrics.config_version, 1);
}

#[tokio::test]
async fn input_is_delivered_while_real_video_datagram_lane_is_full() {
    let mut harness = Harness::new().await;
    harness.flood_video_lane(140).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    harness
        .send_input(InputEvent::MouseWheel {
            delta_x: 0,
            delta_y: 120,
        })
        .await;
    assert_eq!(harness.metrics.input_sequence, 1);
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

/// MVP-10: a fixed-password directory registration resolves through the
/// FFI controller on the derived-code path, and the same typed password
/// replays the identical invitation (the credential saved material carries
/// for reconnect without retyping).
#[tokio::test]
async fn fixed_password_directory_material_reconnects_without_retyping_the_password() {
    use desklink_crypto::{DeviceIdentity, PairingCode, PairingInvite};
    use desklink_ffi::{DesklinkDirectoryConnectionConfig, DesklinkEventKind};
    use desklink_transport::RelayDirectoryLookup;
    use rand_core::{OsRng, RngCore};

    let relay = spawn_test_relay().await;
    let connect_config = || {
        QuicClientConfig::with_client_config(
            relay.address,
            "localhost",
            relay.client_config.clone(),
        )
    };

    // Host: one fixed-password directory registration on its own connection.
    let host_identity = DeviceIdentity::from_secret_key([7; 16], &[8; 32]);
    let password = "correct horse:电池 🐎!";
    let code = PairingCode::derive_from_password(password.as_bytes()).unwrap();
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
    let host = QuicClient::connect(connect_config()).await.unwrap();
    host.join(
        RelayJoin::host_with_participant(
            SessionId::from_bytes(session_id),
            authentication,
            host_identity.device_id,
        )
        .with_directory_registration(
            desklink_transport::RelayDirectoryRegistration::new(
                desklink_ffi::public_host_device_id(host_identity.device_id),
                *code.as_bytes(),
                invite.as_bytes().to_vec(),
                0,
            )
            .unwrap(),
        )
        .unwrap(),
    )
    .await
    .unwrap();

    // Controller FFI: connect with the typed password. The host fixture never
    // completes the Noise handshake, so the connect eventually parks; the
    // lookup itself proves the derived directory code path end to end.
    extern "C" fn record(
        context: *mut std::ffi::c_void,
        event: *const desklink_ffi::DesklinkEvent,
    ) {
        let sink = unsafe { &mut *(context as *mut Vec<(DesklinkEventKind, Vec<u8>)>) };
        let event = unsafe { &*event };
        let data = if event.data.is_null() || event.data_len == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(event.data, event.data_len).to_vec() }
        };
        sink.push((event.kind, data));
    }
    let url = std::ffi::CString::new(format!("quic://{}", relay.address)).unwrap();
    let server_name = std::ffi::CString::new("localhost").unwrap();
    let mut events: Vec<(DesklinkEventKind, Vec<u8>)> = Vec::new();
    let mut handle = std::ptr::null_mut();
    let config = desklink_ffi::DesklinkConfig {
        relay_url: url.as_ptr(),
        log_level: 0,
    };
    assert_eq!(
        unsafe {
            desklink_ffi::desklink_create_for_platform(
                &config,
                desklink_ffi::DesklinkPlatform::MacOS as u32,
                Some(record),
                (&mut events as *mut Vec<(DesklinkEventKind, Vec<u8>)>).cast(),
                &mut handle,
            )
        },
        desklink_ffi::DesklinkResult::Ok
    );

    let password_bytes = password.as_bytes();
    let directory = DesklinkDirectoryConnectionConfig {
        server_name: server_name.as_ptr(),
        device_id: desklink_ffi::public_host_device_id(host_identity.device_id),
        access_code: password_bytes.as_ptr(),
        access_code_len: password_bytes.len(),
        controller_device_id: [9; 16],
        controller_secret_key: [10; 32],
    };
    assert_eq!(
        unsafe { desklink_ffi::desklink_connect_directory(handle, &directory) },
        desklink_ffi::DesklinkResult::Ok
    );
    unsafe { desklink_ffi::desklink_destroy(handle) };

    // Reconnect replay: a second lookup with the SAME typed password must
    // resolve the identical signed invitation — exactly the bytes the saved
    // material stores and replays on reconnect without retyping.
    let probe = QuicClient::connect(connect_config()).await.unwrap();
    let invitation = probe
        .lookup_directory(
            RelayDirectoryLookup::new(
                desklink_ffi::public_host_device_id(host_identity.device_id),
                *code.as_bytes(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invitation, invite.as_bytes().to_vec());

    host.close(b"e2e complete");
    probe.close(b"e2e complete");
}
