use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use desklink_crypto::{
    DeviceIdentity, MAX_PACKET_PLAINTEXT_BYTES, NoiseInitiator, NoiseResponder, SecureRole,
    SecureSession, SessionId,
};
use desklink_protocol::{
    Codec, ControlMessage, DeviceCapabilities, DeviceRole, DirectLanCandidate, H264Profile,
    InputEnvelope, InputEvent, LAN_DATA_ENVELOPE_VERSION, LAN_DATA_KNOWN_CHANNELS,
    LanDataCapabilities, Platform, decode_control, decode_session_input, encode_control,
    encode_input,
};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{
    ChannelKind, DirectLanConnection, DirectLanEndpoint, LanDataConfig, LanDataRoute,
    LanSwitchAction, LanSwitchNegotiator, NegotiatedLanData, QuicClient, QuicClientConfig,
    QuicFlowControlLimits, QuicRelayBackend, RelayJoin, SessionDataError, SessionDataPlane,
    SessionLaneHealth,
};
use quinn::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::Mutex;

const BUDGET: Duration = Duration::from_secs(3);

#[path = "business_transport/backpressure.rs"]
mod backpressure;

struct Pair {
    relay: Arc<RelayServer>,
    task: tokio::task::JoinHandle<()>,
    host: Arc<SessionDataPlane>,
    controller: Arc<SessionDataPlane>,
    host_secure: Arc<Mutex<SecureSession>>,
    controller_secure: Arc<Mutex<SecureSession>>,
    host_agreement: Option<NegotiatedLanData>,
    controller_agreement: Option<NegotiatedLanData>,
    _host_client: Arc<QuicClient>,
    _controller_client: Arc<QuicClient>,
    client_config: QuicClientConfig,
}

impl Drop for Pair {
    fn drop(&mut self) {
        self.host.close();
        self.controller.close();
        self.relay.close();
        self.task.abort();
    }
}

fn caps(role: DeviceRole, channels: Option<u16>) -> DeviceCapabilities {
    DeviceCapabilities {
        platform: Platform::Windows,
        role,
        codecs: vec![Codec::H264],
        h264_profiles: vec![H264Profile::Main],
        width: 1280,
        height: 720,
        lan_data: channels.map(|channels| LanDataCapabilities {
            envelope_version: LAN_DATA_ENVELOPE_VERSION,
            channels,
            allow_relay_loss_continuation: true,
        }),
    }
}

fn secure_pair() -> (SecureSession, SecureSession) {
    // Public synthetic fixture identities, never production credentials.
    let host = DeviceIdentity::from_secret_key([1; 16], &[1; 32]);
    let controller = DeviceIdentity::from_secret_key([2; 16], &[2; 32]);
    let host_key = host.verify_key();
    let controller_key = controller.verify_key();
    let (mut initiator, first) = NoiseInitiator::start(controller, host_key).unwrap();
    let (mut responder, second) = NoiseResponder::accept(&first, host, controller_key).unwrap();
    let third = initiator.receive(&second).unwrap();
    responder.receive(&third).unwrap();
    (
        responder
            .finish()
            .unwrap()
            .into_secure_session(SecureRole::Responder),
        initiator
            .finish()
            .unwrap()
            .into_secure_session(SecureRole::Initiator),
    )
}

impl Pair {
    async fn new(channels: Option<u16>) -> Self {
        Self::with_config(channels, LanDataConfig::default(), None).await
    }

    async fn with_config(
        channels: Option<u16>,
        plane_config: LanDataConfig,
        flow_limits: Option<QuicFlowControlLimits>,
    ) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let der = certificate.cert.der().to_vec();
        let server_config = ServerConfig::with_single_cert(
            vec![CertificateDer::from(der.clone())],
            PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into()),
        )
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(der)).unwrap();
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
        ));
        let relay = Arc::new(
            RelayServer::bind(
                "127.0.0.1:0".parse().unwrap(),
                server_config,
                RelayConfig::default(),
            )
            .await
            .unwrap(),
        );
        let running = relay.clone();
        let task = tokio::spawn(async move { running.run().await.unwrap() });
        let mut config = QuicClientConfig::with_client_config(
            relay.local_addr().unwrap(),
            "localhost",
            client_config,
        );
        config.flow_control_limits = flow_limits;
        let host_client = Arc::new(QuicClient::connect(config.clone()).await.unwrap());
        host_client
            .join(RelayJoin::host_with_participant(
                SessionId::from_bytes([72; 16]),
                [19; 32],
                [1; 16],
            ))
            .await
            .unwrap();
        let controller_client = Arc::new(QuicClient::connect(config.clone()).await.unwrap());
        controller_client
            .join(RelayJoin::controller_with_participant(
                SessionId::from_bytes([72; 16]),
                [19; 32],
                [2; 16],
            ))
            .await
            .unwrap();
        // Establish real relay generations before the business plane owns the
        // lanes. Production establishes these while carrying Noise/capabilities.
        host_client
            .send_control(b"bootstrap host".to_vec())
            .await
            .unwrap();
        controller_client.next_control().await.unwrap();
        controller_client
            .send_control(b"bootstrap controller".to_vec())
            .await
            .unwrap();
        host_client.next_control().await.unwrap();
        assert_ne!(
            host_client.peer_generation(),
            controller_client.peer_generation()
        );
        let (host_secure, controller_secure) = secure_pair();
        let binding = host_secure.video_path_binding();
        let host_secure = Arc::new(Mutex::new(host_secure));
        let controller_secure = Arc::new(Mutex::new(controller_secure));
        let host_caps = caps(DeviceRole::Host, channels);
        let controller_caps = caps(DeviceRole::Controller, channels);
        let host_agreement = NegotiatedLanData::from_authenticated_caps(
            &host_caps,
            &controller_caps,
            binding,
            host_client.peer_generation(),
        )
        .unwrap();
        let controller_agreement = NegotiatedLanData::from_authenticated_caps(
            &controller_caps,
            &host_caps,
            binding,
            controller_client.peer_generation(),
        )
        .unwrap();
        let host_backend = Arc::new(
            QuicRelayBackend::new(host_client.clone(), host_client.peer_generation()).unwrap(),
        );
        let controller_backend = Arc::new(
            QuicRelayBackend::new(
                controller_client.clone(),
                controller_client.peer_generation(),
            )
            .unwrap(),
        );
        let host = match host_agreement {
            Some(agreement) => SessionDataPlane::relay_only_with_config(
                agreement,
                host_secure.clone(),
                host_backend,
                plane_config,
            )
            .await
            .unwrap(),
            None => SessionDataPlane::legacy(host_secure.clone(), host_backend),
        };
        let controller = match controller_agreement {
            Some(agreement) => SessionDataPlane::relay_only_with_config(
                agreement,
                controller_secure.clone(),
                controller_backend.clone(),
                plane_config,
            )
            .await
            .unwrap(),
            None => SessionDataPlane::legacy(controller_secure.clone(), controller_backend.clone()),
        };
        Self {
            relay,
            task,
            host,
            controller,
            host_secure,
            controller_secure,
            host_agreement,
            controller_agreement,
            _host_client: host_client,
            _controller_client: controller_client,
            client_config: config,
        }
    }

    async fn direct(&self) -> Direct {
        let host_endpoint =
            Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let controller_endpoint = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let binding = self.host_secure.lock().await.video_path_binding();
        let now = now();
        let candidate = DirectLanCandidate::new(
            91,
            host_endpoint.local_addr().unwrap(),
            now + 10,
            binding,
            now,
        )
        .unwrap();
        let responding = host_endpoint.clone();
        let secure = self.host_secure.clone();
        let accepting = tokio::spawn(async move {
            let raw = responding.accept().await.unwrap().unwrap();
            responding
                .accept_probe_connection_shared(raw, 91, &binding, &secure, now)
                .await
                .unwrap()
                .0
        });
        let controller_connection = controller_endpoint
            .connect_shared(&candidate, &binding, &self.controller_secure, now)
            .await
            .unwrap()
            .0;
        let host_connection = accepting.await.unwrap();
        Direct {
            _host_endpoint: host_endpoint,
            _controller_endpoint: controller_endpoint,
            candidate,
            host_connection: Some(host_connection),
            controller_connection: Some(controller_connection),
        }
    }

    async fn attach(&self, direct: &mut Direct) {
        let mut host = LanSwitchNegotiator::new(self.host_agreement.unwrap());
        let mut controller = LanSwitchNegotiator::new(self.controller_agreement.unwrap());
        let offer = host
            .offer(
                &direct.candidate,
                self.host_agreement.unwrap().channels(),
                now(),
            )
            .unwrap();
        self.host
            .send(ChannelKind::Control, encode_control(&offer).unwrap().into())
            .await
            .unwrap();
        let offer = decode_control(&receive(&self.controller, ChannelKind::Control).await).unwrap();
        let LanSwitchAction::SendReady(ready) = controller
            .receive(&offer, &direct.candidate, now())
            .unwrap()
        else {
            panic!("expected Ready");
        };
        self.controller
            .send(ChannelKind::Control, encode_control(&ready).unwrap().into())
            .await
            .unwrap();
        let ready = decode_control(&receive(&self.host, ChannelKind::Control).await).unwrap();
        let LanSwitchAction::SendCommit(commit) =
            host.receive(&ready, &direct.candidate, now()).unwrap()
        else {
            panic!("expected Commit");
        };
        self.host
            .send(
                ChannelKind::Control,
                encode_control(&commit).unwrap().into(),
            )
            .await
            .unwrap();
        let host_authority = host.commit_sent().unwrap();
        let commit =
            decode_control(&receive(&self.controller, ChannelKind::Control).await).unwrap();
        let LanSwitchAction::Attach(controller_authority) = controller
            .receive(&commit, &direct.candidate, now())
            .unwrap()
        else {
            panic!("expected authority");
        };
        let (host, controller) = tokio::join!(
            self.host
                .attach_direct(host_authority, direct.host_connection.take().unwrap()),
            self.controller.attach_direct(
                controller_authority,
                direct.controller_connection.take().unwrap()
            )
        );
        host.unwrap();
        controller.unwrap();
    }
}

struct Direct {
    _host_endpoint: Arc<DirectLanEndpoint>,
    _controller_endpoint: DirectLanEndpoint,
    candidate: DirectLanCandidate,
    host_connection: Option<DirectLanConnection>,
    controller_connection: Option<DirectLanConnection>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
async fn receive(plane: &SessionDataPlane, channel: ChannelKind) -> Bytes {
    tokio::time::timeout(BUDGET, plane.recv(channel))
        .await
        .unwrap()
        .unwrap()
}
async fn wait_direct(plane: &SessionDataPlane, channel: ChannelKind) {
    let mut health = plane.health();
    tokio::time::timeout(BUDGET, async {
        loop {
            if matches!(
                health.borrow().lanes[channel as usize - 1],
                SessionLaneHealth::Direct(_)
            ) {
                return;
            }
            health.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}
fn input(sequence: u64) -> Bytes {
    encode_input(&InputEnvelope {
        sequence,
        timestamp_us: u64::MAX,
        stream_id: u64::MAX,
        input_epoch: u64::MAX,
        event: InputEvent::MouseMove {
            x: sequence as i32,
            y: 0,
        },
    })
    .unwrap()
    .into()
}

#[tokio::test]
async fn real_relay_preserves_legacy_and_selected_lane_modes_and_input_context() {
    for channels in [None, Some(3)] {
        let pair = Pair::new(channels).await;
        let wire = input(1);
        assert_eq!(
            pair.controller
                .send(ChannelKind::Input, wire.clone())
                .await
                .unwrap(),
            LanDataRoute::Relay
        );
        let received = receive(&pair.host, ChannelKind::Input).await;
        assert_eq!(received, wire);
        assert_eq!(received.first(), Some(&0));
        assert_eq!(
            decode_session_input(&received).unwrap().input_epoch,
            u64::MAX
        );
        pair.host
            .send(
                ChannelKind::Transfer,
                Bytes::from_static(b"unselected legacy transfer"),
            )
            .await
            .unwrap();
        assert_eq!(
            receive(&pair.controller, ChannelKind::Transfer).await,
            "unselected legacy transfer"
        );
    }
}

#[tokio::test]
async fn cutover_query_cannot_overtake_a_real_relay_input_backlog_larger_than_crypto_window() {
    let pair = Pair::new(Some(LAN_DATA_KNOWN_CHANNELS)).await;
    for sequence in 1..=256 {
        pair.controller
            .send(ChannelKind::Input, input(sequence))
            .await
            .unwrap();
    }
    let mut direct = pair.direct().await;
    pair.attach(&mut direct).await;
    wait_direct(&pair.controller, ChannelKind::Control).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        pair.controller.health().borrow().lanes[1],
        SessionLaneHealth::Relay
    );
    let sending = pair.controller.clone();
    let newer = tokio::spawn(async move { sending.send(ChannelKind::Input, input(257)).await });
    assert!(!newer.is_finished());
    for sequence in 1..=256 {
        assert_eq!(
            receive(&pair.host, ChannelKind::Input).await,
            input(sequence)
        );
    }
    assert_eq!(newer.await.unwrap().unwrap(), LanDataRoute::DirectLan);
    assert_eq!(receive(&pair.host, ChannelKind::Input).await, input(257));
}

#[tokio::test]
async fn slow_file_lane_does_not_delay_input_audio_fences_or_real_relay_fallback_deduplication() {
    let pair = Pair::new(Some(LAN_DATA_KNOWN_CHANNELS)).await;
    for _ in 0..5 {
        pair.controller
            .send(ChannelKind::Transfer, Bytes::from_static(b"blocked file"))
            .await
            .unwrap();
    }
    let mut direct = pair.direct().await;
    pair.attach(&mut direct).await;
    wait_direct(&pair.controller, ChannelKind::Input).await;
    assert_eq!(
        pair.controller.health().borrow().lanes[5],
        SessionLaneHealth::Relay
    );
    assert_eq!(
        pair.controller
            .send(ChannelKind::AudioDatagram, Bytes::from_static(b"audio"))
            .await
            .unwrap(),
        LanDataRoute::DirectLan
    );
    assert_eq!(
        receive(&pair.host, ChannelKind::AudioDatagram).await,
        "audio"
    );
    for sequence in 1..=32 {
        assert_eq!(
            pair.controller
                .send(ChannelKind::Input, input(sequence))
                .await
                .unwrap(),
            LanDataRoute::DirectLan
        );
    }
    assert_eq!(
        pair.controller
            .send(ChannelKind::Input, input(33))
            .await
            .unwrap(),
        LanDataRoute::Relay
    );
    for sequence in 1..=33 {
        assert_eq!(
            receive(&pair.host, ChannelKind::Input).await,
            input(sequence)
        );
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            pair.host.recv(ChannelKind::Input)
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn authenticated_lan_keeps_all_business_lanes_when_its_owned_relay_closes() {
    let pair = Pair::new(Some(LAN_DATA_KNOWN_CHANNELS)).await;
    let mut direct = pair.direct().await;
    pair.attach(&mut direct).await;
    for number in 1..=8 {
        let channel = ChannelKind::try_from(number).unwrap();
        wait_direct(&pair.host, channel).await;
        wait_direct(&pair.controller, channel).await;
    }
    pair.relay.close();
    let mut health = pair.host.health();
    tokio::time::timeout(BUDGET, async {
        while health.borrow().relay_connected {
            health.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(health.borrow().can_continue_without_relay);
    for number in 1..=8 {
        let channel = ChannelKind::try_from(number).unwrap();
        let body = Bytes::from(vec![0, number, 0xff]);
        assert_eq!(
            pair.host.send(channel, body.clone()).await.unwrap(),
            LanDataRoute::DirectLan
        );
        assert_eq!(receive(&pair.controller, channel).await, body);
    }
}

fn file_chunk(sequence: usize) -> Bytes {
    let mut body = vec![0xa6; MAX_PACKET_PLAINTEXT_BYTES - 20];
    body[..8].copy_from_slice(&(sequence as u64).to_be_bytes());
    body.into()
}

#[tokio::test]
async fn a_full_size_transfer_fallback_timeout_interrupts_only_transfer_and_keeps_relay_input_live()
{
    let pair = Pair::with_config(
        Some(1 << (ChannelKind::Transfer as u8 - 1)),
        LanDataConfig {
            direct_timeout: Duration::from_millis(100),
            operation_timeout: Duration::from_millis(500),
        },
        Some(QuicFlowControlLimits::new(131_072, 131_072, 4_096).unwrap()),
    )
    .await;
    let mut direct = pair.direct().await;
    pair.attach(&mut direct).await;
    wait_direct(&pair.controller, ChannelKind::Transfer).await;
    let mut completed = 0;
    let mut fallback_seen = false;
    loop {
        assert!(
            completed < 512,
            "unread full-size file frames must exhaust stream credit"
        );
        match pair
            .controller
            .send(ChannelKind::Transfer, file_chunk(completed))
            .await
        {
            Ok(route) => {
                fallback_seen |= route == LanDataRoute::Relay;
                completed += 1;
            }
            Err(error) => {
                assert_eq!(error, SessionDataError::Interrupted(ChannelKind::Transfer));
                break;
            }
        }
    }
    assert!(
        fallback_seen,
        "exercise real LAN timeout and ciphertext relay fallback"
    );
    assert_eq!(
        pair.controller.health().borrow().lanes[ChannelKind::Transfer as usize - 1],
        SessionLaneHealth::Interrupted
    );
    for channel in [ChannelKind::Input, ChannelKind::Control] {
        let body = Bytes::from(vec![0, channel as u8, 0xff]);
        assert_eq!(
            pair.controller.send(channel, body.clone()).await.unwrap(),
            LanDataRoute::Relay
        );
        assert_eq!(receive(&pair.host, channel).await, body);
    }
    assert!(!pair._controller_client.is_closed());
    assert!(!pair._host_client.is_closed());
    // The last caller timed out, but its owned ciphertext must still finish
    // byte-for-byte when the real receiver releases stream credit.
    for sequence in 0..=completed {
        assert_eq!(
            receive(&pair.host, ChannelKind::Transfer).await,
            file_chunk(sequence)
        );
    }
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            pair.host.recv(ChannelKind::Transfer)
        )
        .await
        .is_err()
    );
    assert!(!pair._controller_client.is_closed());
}

#[tokio::test]
async fn a_replacement_peer_retires_a_lan_session_after_its_old_peer_lost_only_relay() {
    let pair = Pair::new(Some(LAN_DATA_KNOWN_CHANNELS)).await;
    let mut direct = pair.direct().await;
    pair.attach(&mut direct).await;
    for channel in [ChannelKind::Input, ChannelKind::Control] {
        wait_direct(&pair.host, channel).await;
        wait_direct(&pair.controller, channel).await;
    }
    let old_generation = pair._host_client.peer_generation();
    pair._controller_client
        .close(b"owned controller relay loss");
    let mut health = pair.host.health();
    tokio::time::timeout(BUDGET, async {
        while !health.borrow().peer_disconnected {
            health.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(!health.borrow().closed);
    assert!(health.borrow().can_continue_without_relay);
    assert!(!pair._host_client.is_closed());
    assert_eq!(
        pair.controller
            .send(ChannelKind::Input, input(1))
            .await
            .unwrap(),
        LanDataRoute::DirectLan
    );
    assert_eq!(receive(&pair.host, ChannelKind::Input).await, input(1));

    let replacement = QuicClient::connect(pair.client_config.clone())
        .await
        .unwrap();
    replacement
        .join(RelayJoin::controller_with_participant(
            SessionId::from_bytes([72; 16]),
            [19; 32],
            [3; 16],
        ))
        .await
        .unwrap();
    // A real replacement Noise entry payload must remain pending rather than
    // entering the old Noise session's decryptor.
    let controller = DeviceIdentity::from_secret_key([3; 16], &[3; 32]);
    let host_key = DeviceIdentity::from_secret_key([1; 16], &[1; 32]).verify_key();
    let (_, first_noise) = NoiseInitiator::start(controller, host_key).unwrap();
    replacement.send_control(first_noise.clone()).await.unwrap();
    tokio::time::timeout(BUDGET, async {
        while !health.borrow().closed {
            health.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(health.borrow().peer_replaced);
    assert!(!pair._host_client.is_closed());
    tokio::time::timeout(BUDGET, pair.host.close_and_wait())
        .await
        .unwrap();
    assert_eq!(pair.host.active_tasks(), 0);
    let (new_generation, preserved_noise) =
        tokio::time::timeout(BUDGET, pair._host_client.next_control_with_generation())
            .await
            .unwrap()
            .unwrap();
    assert!(new_generation > old_generation);
    assert_eq!(preserved_noise, first_noise);
    replacement.close(b"owned replacement test complete");
}

#[tokio::test]
async fn close_and_wait_drains_fence_children_while_crypto_is_locked_and_does_not_close_shared_relay()
 {
    let pair = Pair::new(Some(LAN_DATA_KNOWN_CHANNELS)).await;
    for sequence in 1..=33 {
        pair.controller
            .send(ChannelKind::Input, input(sequence))
            .await
            .unwrap();
    }
    let mut direct = pair.direct().await;
    pair.attach(&mut direct).await;
    let guard = pair.host_secure.lock().await;
    tokio::task::yield_now().await;
    assert!(pair.host.active_tasks() > 0);
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(pair.host.close_and_wait(), pair.controller.close_and_wait());
    })
    .await
    .unwrap();
    assert_eq!(pair.host.active_tasks(), 0);
    assert_eq!(pair.controller.active_tasks(), 0);
    assert!(!pair._host_client.is_closed());
    assert!(!pair._controller_client.is_closed());
    let weak = Arc::downgrade(&pair.host);
    drop(guard);
    drop(pair);
    assert!(
        weak.upgrade().is_none(),
        "owned tasks must not retain a strong plane cycle"
    );
}

#[tokio::test]
async fn a_concurrent_legacy_attach_waiting_for_crypto_cannot_register_after_close() {
    let pair = Pair::new(None).await;
    let mut direct = pair.direct().await;
    let guard = pair.host_secure.lock().await;
    let attaching = pair.host.clone();
    let incoming = direct.host_connection.take().unwrap();
    let task = tokio::spawn(async move { attaching.attach_legacy_video(incoming).await });
    tokio::task::yield_now().await;
    tokio::time::timeout(Duration::from_secs(1), pair.host.close_and_wait())
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap(),
        Err(desklink_transport::SessionDataError::Closed)
    ));
    assert_eq!(pair.host.active_tasks(), 0);
    drop(guard);
}

#[tokio::test]
#[ignore = "explicit 17-second real-QUIC silent LAN keepalive regression"]
async fn silent_authenticated_lan_survives_its_fifteen_second_idle_budget_without_relay() {
    let pair = Pair::new(Some(LAN_DATA_KNOWN_CHANNELS)).await;
    let mut direct = pair.direct().await;
    pair.attach(&mut direct).await;
    for number in 1..=8 {
        let channel = ChannelKind::try_from(number).unwrap();
        wait_direct(&pair.host, channel).await;
        wait_direct(&pair.controller, channel).await;
    }
    pair.relay.close();
    let mut health = pair.host.health();
    tokio::time::timeout(BUDGET, async {
        while health.borrow().relay_connected {
            health.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_secs(17)).await;
    assert!(pair.host.health().borrow().can_continue_without_relay);
    assert!(pair.controller.health().borrow().can_continue_without_relay);
    assert_eq!(
        pair.controller
            .send(ChannelKind::Input, input(1))
            .await
            .unwrap(),
        LanDataRoute::DirectLan
    );
    assert_eq!(receive(&pair.host, ChannelKind::Input).await, input(1));
    let control = encode_control(&ControlMessage::RequestKeyframe {
        stream_id: u64::MAX,
    })
    .unwrap();
    assert_eq!(
        pair.host
            .send(ChannelKind::Control, control.clone().into())
            .await
            .unwrap(),
        LanDataRoute::DirectLan
    );
    assert_eq!(
        receive(&pair.controller, ChannelKind::Control)
            .await
            .as_ref(),
        control
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(pair.host.close_and_wait(), pair.controller.close_and_wait());
    })
    .await
    .unwrap();
    assert_eq!(pair.host.active_tasks(), 0);
    assert_eq!(pair.controller.active_tasks(), 0);
}
