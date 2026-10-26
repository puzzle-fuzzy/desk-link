//! Real local transport regressions for the controller's receive multiplexer.
//! QUIC, relay forwarding and Noise-authenticated direct probes are real; these
//! tests do not claim codec/display, hardware-firewall or cross-device proof.
use super::*;
use desklink_crypto::{NoiseResponder, SessionId};
use desklink_protocol::{CursorUpdate, FrameFlags, encode_cursor_update, encode_video_packet};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{QuicClientConfig, QuicFlowControlLimits, RelayJoin};
use desklink_video::packetize_frame;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

struct TestTask(tokio::task::JoinHandle<()>);

impl Drop for TestTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct TestRelay {
    address: std::net::SocketAddr,
    client_config: quinn::ClientConfig,
    _task: TestTask,
}

pub(crate) struct DirectFixture {
    pub(crate) runtime: ControllerRuntime,
    pub(crate) host: Arc<QuicClient>,
    pub(crate) host_secure: Arc<Mutex<SecureSession>>,
    direct_peer: Arc<DirectLanConnection>,
    _controller_endpoint: Arc<DirectLanEndpoint>,
    _peer_endpoint: Arc<DirectLanEndpoint>,
    _relay: TestRelay,
    _cleanup: ConnectionCleanup,
}

struct ConnectionCleanup {
    client: Arc<QuicClient>,
    host: Arc<QuicClient>,
    direct: Arc<DirectLanConnection>,
    direct_peer: Arc<DirectLanConnection>,
}

impl Drop for ConnectionCleanup {
    fn drop(&mut self) {
        self.client.close(b"direct silence test complete");
        self.host.close(b"direct silence test complete");
        self.direct.close(b"direct silence test complete");
        self.direct_peer.close(b"direct silence test complete");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_blackhole_falls_back_despite_continuous_relay_cursor() {
    let mut fixture = connected_direct_fixture().await;
    let _cursor = relay_cursor_flood(fixture.host.clone(), fixture.host_secure.clone());
    let started = Instant::now();
    // Keep the production five-second budget. Only allow local scheduling slack.
    let budget = DIRECT_VIDEO_SILENCE_TIMEOUT + Duration::from_millis(750);
    let deadline = tokio::time::Instant::now() + budget;
    let mut relay_events = 0;
    loop {
        assert!(
            started.elapsed() <= budget,
            "relay cursor events kept resetting the direct silence deadline"
        );
        let event = tokio::time::timeout_at(deadline, fixture.runtime.next_transport_event(true))
            .await
            .expect("direct silence fallback did not complete within the production budget")
            .unwrap();
        if matches!(event, TransportEvent::CursorDatagram(_)) {
            relay_events += 1;
        }
        if fixture.runtime.video_path_kind() == VideoPathKind::Relay {
            assert_eq!(
                fixture.runtime.video_path_fallback_reason(),
                Some(DirectVideoPathFallbackReason::DatagramSilence)
            );
            assert!(fixture.runtime.direct_connection.is_none());
            assert!(
                relay_events >= 20,
                "the real relay must keep forwarding cursor traffic during the blackhole"
            );
            eprintln!(
                "DL2-061 blackhole fallback: elapsed_ms={} relay_cursor_events={relay_events}",
                started.elapsed().as_millis()
            );
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn active_direct_datagrams_do_not_time_out_while_relay_cursor_arrives() {
    let mut fixture = connected_direct_fixture().await;
    let _cursor = relay_cursor_flood(fixture.host.clone(), fixture.host_secure.clone());
    let _direct = direct_video_flood(fixture.direct_peer.clone(), fixture.host_secure.clone());
    let started = Instant::now();
    // Run beyond the initial production deadline; actual direct receive activity
    // must extend it, while relay events must have no say in that deadline.
    let duration = DIRECT_VIDEO_SILENCE_TIMEOUT + Duration::from_secs(1);
    let deadline = tokio::time::Instant::now() + duration;
    let mut direct_events = 0;
    let mut relay_events = 0;
    while started.elapsed() < duration {
        let event =
            match tokio::time::timeout_at(deadline, fixture.runtime.next_transport_event(true)).await {
                Ok(event) => event.unwrap(),
                Err(_) => break,
            };
        match event {
            TransportEvent::VideoDatagram(_) => direct_events += 1,
            TransportEvent::CursorDatagram(_) => relay_events += 1,
            _ => {}
        }
        assert_eq!(fixture.runtime.video_path_kind(), VideoPathKind::DirectLan);
        assert_eq!(fixture.runtime.video_path_fallback_reason(), None);
    }
    assert!(started.elapsed() >= DIRECT_VIDEO_SILENCE_TIMEOUT);
    assert!(
        direct_events >= 20 && relay_events >= 20,
        "both real transport lanes must stay active"
    );
    assert_eq!(fixture.runtime.video_path_kind(), VideoPathKind::DirectLan);
    eprintln!(
        "DL2-061 active direct: elapsed_ms={} direct_events={direct_events} relay_cursor_events={relay_events}",
        started.elapsed().as_millis()
    );
}

fn relay_cursor_flood(host: Arc<QuicClient>, secure: Arc<Mutex<SecureSession>>) -> TestTask {
    TestTask(tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(20));
        let mut sequence = 0;
        loop {
            interval.tick().await;
            sequence += 1;
            let plaintext = encode_cursor_update(&CursorUpdate {
                protocol_version: PROTOCOL_VERSION,
                stream_id: 1,
                sequence,
                timestamp_us: now_micros(),
                x_millionths: 500_000,
                y_millionths: 500_000,
                visible: true,
                shape_id: 0,
            })
            .unwrap();
            let ciphertext = secure
                .lock()
                .await
                .seal(SecureLane::CursorDatagram, &plaintext)
                .unwrap();
            host.send_cursor_datagram(ciphertext).await.unwrap();
        }
    }))
}

fn direct_video_flood(
    peer: Arc<DirectLanConnection>,
    secure: Arc<Mutex<SecureSession>>,
) -> TestTask {
    TestTask(tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        let mut frame_id = 0;
        loop {
            interval.tick().await;
            frame_id += 1;
            let frame = EncodedFrame {
                stream_id: 1,
                frame_id,
                config_version: 1,
                capture_timestamp_us: now_micros(),
                width: 16,
                height: 16,
                flags: FrameFlags::KEYFRAME,
                data: vec![0x61; 64],
            };
            for packet in packetize_frame(&frame).unwrap() {
                let plaintext = encode_video_packet(&packet).unwrap();
                let ciphertext = secure
                    .lock()
                    .await
                    .seal(SecureLane::VideoDatagram, &plaintext)
                    .unwrap();
                peer.send_datagram(ciphertext).unwrap();
            }
        }
    }))
}

async fn connected_direct_fixture() -> DirectFixture {
    connected_fixture(false).await
}

pub(crate) async fn connected_flow_control_fixture() -> DirectFixture {
    connected_fixture(true).await
}

fn fixture_flow_control_limits() -> QuicFlowControlLimits {
    // A 2 KiB flight fills the peer's 128-message receive queue promptly on
    // loopback, while retaining finite buffers on both relay hops. A 128-byte
    // flight instead tested ACK pacing before the receive queue became full.
    QuicFlowControlLimits::new(2048, 16384, 2048).unwrap()
}

async fn connected_fixture(backpressure: bool) -> DirectFixture {
    let relay = spawn_relay(backpressure).await;
    let config = || {
        let config = QuicClientConfig::with_client_config(
            relay.address,
            "localhost",
            relay.client_config.clone(),
        );
        if backpressure {
            config.with_flow_control_limits(fixture_flow_control_limits())
        } else {
            config
        }
    };
    let host = Arc::new(QuicClient::connect(config()).await.unwrap());
    let client = Arc::new(QuicClient::connect(config()).await.unwrap());
    let session_id = SessionId::from_bytes([101; 16]);
    host.join(RelayJoin::host_with_participant(
        session_id, [102; 32], [103; 16],
    ))
    .await
    .unwrap();
    client
        .join(RelayJoin::controller_with_participant(
            session_id, [102; 32], [104; 16],
        ))
        .await
        .unwrap();
    let (controller_secure, host_secure) = secure_sessions();
    let binding = controller_secure.video_path_binding();
    let secure = Arc::new(Mutex::new(controller_secure));
    let host_secure = Arc::new(Mutex::new(host_secure));
    let controller_endpoint =
        Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let peer_endpoint = Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let now = now_unix_s();
    let local = DirectLanCandidate::new(
        201,
        controller_endpoint.local_addr().unwrap(),
        now + 10,
        binding,
        now,
    )
    .unwrap();
    let remote = DirectLanCandidate::new(
        202,
        peer_endpoint.local_addr().unwrap(),
        now + 10,
        binding,
        now,
    )
    .unwrap();
    let accepting = peer_endpoint.clone();
    let peer_crypto = host_secure.clone();
    let accepted = tokio::spawn(async move {
        let connection = accepting.accept().await.unwrap().unwrap();
        accepting
            .accept_probe_connection_shared(connection, 202, &binding, &peer_crypto, now)
            .await
            .unwrap()
    });
    let (direct, quality) = tokio::time::timeout(
        Duration::from_secs(3),
        controller_endpoint.connect_shared(&remote, &binding, &secure, now),
    )
    .await
    .unwrap()
    .unwrap();
    let (peer, _) = accepted.await.unwrap();
    let mut video_path = DirectVideoPathMachine::new(binding).with_direct_probe();
    video_path.apply(DirectVideoPathEvent::StartOffer {
        candidate: local.clone(),
        now_unix_s: now,
    });
    video_path.apply(DirectVideoPathEvent::ReceiveAnswer {
        candidate_id: local.candidate_id(),
        accepted: true,
        candidate: Some(remote),
        now_unix_s: now,
    });
    video_path.apply(DirectVideoPathEvent::ProbeSucceeded {
        candidate_id: quality.candidate_id,
        quality: VideoPathQuality {
            kind: VideoPathKind::DirectLan,
            rtt_ms: quality.rtt_ms,
            loss_basis_points: 0,
        },
    });
    assert!(matches!(
        video_path.state(),
        DirectVideoPathState::Direct { .. }
    ));
    let direct = Arc::new(direct);
    let peer = Arc::new(peer);
    let cleanup = ConnectionCleanup {
        client: client.clone(),
        host: host.clone(),
        direct: direct.clone(),
        direct_peer: peer.clone(),
    };
    let runtime = ControllerRuntime {
        host_capabilities: DeviceCapabilities {
            platform: Platform::Windows,
            role: DeviceRole::Host,
            codecs: vec![Codec::H264],
            h264_profiles: vec![H264Profile::Main],
            width: 16,
            height: 16,
            lan_data: None,
        },
        client,
        secure,
        input_sequence: Arc::new(Mutex::new(InputSequencer::new())),
        control_order: Arc::new(Mutex::new(())),
        received_transport_events: Arc::new(AtomicU64::new(0)),
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
        direct_session: None,
        direct_connection: Some(direct),
        last_direct_datagram_at: Some(Instant::now()),
    };
    DirectFixture {
        runtime,
        host,
        host_secure,
        direct_peer: peer,
        _controller_endpoint: controller_endpoint,
        _peer_endpoint: peer_endpoint,
        _relay: relay,
        _cleanup: cleanup,
    }
}

fn secure_sessions() -> (SecureSession, SecureSession) {
    let initiator_identity = DeviceIdentity::generate(&mut OsRng);
    let responder_identity = DeviceIdentity::generate(&mut OsRng);
    let initiator_key = initiator_identity.verify_key();
    let responder_key = responder_identity.verify_key();
    let (mut initiator, message_1) =
        NoiseInitiator::start(initiator_identity, responder_key).unwrap();
    let (mut responder, message_2) =
        NoiseResponder::accept(&message_1, responder_identity, initiator_key).unwrap();
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

async fn spawn_relay(backpressure: bool) -> TestRelay {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_der = certificate.cert.der().to_vec();
    let server_config = quinn::ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der.clone())],
        PrivateKeyDer::Pkcs8(certificate.key_pair.serialize_der().into()),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(certificate_der)).unwrap();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
    ));
    let mut transport = quinn::TransportConfig::default();
    if backpressure {
        fixture_flow_control_limits().apply_to(&mut transport);
    }
    let relay = Arc::new(
        RelayServer::bind_with_transport(
            "127.0.0.1:0".parse().unwrap(),
            server_config,
            RelayConfig::default(),
            transport,
        )
        .await
        .unwrap(),
    );
    let address = relay.local_addr().unwrap();
    let task = TestTask(tokio::spawn(async move {
        let _ = relay.run().await;
    }));
    TestRelay {
        address,
        client_config,
        _task: task,
    }
}
