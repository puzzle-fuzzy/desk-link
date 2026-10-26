//! Real loopback relay/QUIC framing regression. These opaque bytes deliberately
//! exercise the writer below Noise/business codecs; no Host input is injected.
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use bytes::Bytes;
use desklink_crypto::{SecureLane, SecureSession, SessionId};
use desklink_protocol::{DeviceRole, DirectLanCandidate};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{
    ChannelKind, DirectLanEndpoint, LanDataConfig, LanRelayBackend, LanSwitchAction,
    LanSwitchNegotiator, MAX_RELIABLE_MESSAGE_BYTES, NegotiatedLanData, QuicClient,
    QuicClientConfig, QuicFlowControlLimits, QuicRelayBackend, RelayJoin, SessionDataPlane,
    SessionLaneHealth,
};
use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::{
    sync::{Mutex, mpsc, watch},
    task::JoinSet,
};

const WAIT: Duration = Duration::from_secs(5);

struct RawSinkFixture {
    relay: Arc<RelayServer>,
    relay_task: tokio::task::JoinHandle<()>,
    source: Arc<QuicClient>,
    _sink_endpoint: Endpoint,
    sink: quinn::Connection,
    sink_task: tokio::task::JoinHandle<()>,
    transfer_seen: watch::Receiver<bool>,
    drain: watch::Sender<bool>,
    frames: mpsc::Receiver<Vec<u8>>,
    echo_crypto: Arc<Mutex<Option<SecureSession>>>,
    source_generation: Arc<AtomicU64>,
}

impl Drop for RawSinkFixture {
    fn drop(&mut self) {
        self.source.close(b"owned writer fixture done");
        self.sink.close(0_u32.into(), b"owned writer fixture done");
        self.sink_task.abort();
        self.relay.close();
        self.relay_task.abort();
    }
}

impl RawSinkFixture {
    async fn new() -> Self {
        Self::with_windows(2_048, 2_048).await
    }

    async fn with_windows(stream_window: u32, source_send_window: u32) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let der = certificate.cert.der().to_vec();
        let server = ServerConfig::with_single_cert(
            vec![CertificateDer::from(der.clone())],
            PrivateKeyDer::Pkcs8(certificate.key_pair.serialize_der().into()),
        )
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(der)).unwrap();
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let mut client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
        ));
        // A stopped Transfer exhausts its stream credit, while the 16 KiB
        // connection window leaves room for the actively read core lanes.
        let limits = QuicFlowControlLimits::new(2_048, 16_384, stream_window).unwrap();
        let source_limits =
            QuicFlowControlLimits::new(source_send_window, 16_384, stream_window).unwrap();
        let mut transport = quinn::TransportConfig::default();
        limits.apply_to(&mut transport);
        transport.keep_alive_interval(Some(Duration::from_millis(200)));
        client_config.transport_config(Arc::new(transport));
        let relay = Arc::new(
            RelayServer::bind_with_flow_control_limits(
                "127.0.0.1:0".parse().unwrap(),
                server,
                RelayConfig::default(),
                limits,
            )
            .await
            .unwrap(),
        );
        let running = relay.clone();
        let relay_task = tokio::spawn(async move {
            running.run().await.unwrap();
        });
        let source = Arc::new(
            QuicClient::connect(
                QuicClientConfig::with_client_config(
                    relay.local_addr().unwrap(),
                    "localhost",
                    client_config.clone(),
                )
                .with_flow_control_limits(source_limits),
            )
            .await
            .unwrap(),
        );
        let session = SessionId::from_bytes([181; 16]);
        source
            .join(RelayJoin::host_with_participant(
                session, [182; 32], [183; 16],
            ))
            .await
            .unwrap();
        let mut sink_endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        sink_endpoint.set_default_client_config(client_config);
        let sink = sink_endpoint
            .connect(relay.local_addr().unwrap(), "localhost")
            .unwrap()
            .await
            .unwrap();
        let (mut request, mut response) = sink.open_bi().await.unwrap();
        let join = RelayJoin::controller_with_participant(session, [182; 32], [184; 16]).encode();
        request
            .write_all(&(join.len() as u32).to_be_bytes())
            .await
            .unwrap();
        request.write_all(&join).await.unwrap();
        request.finish().unwrap();
        let mut ack = [255];
        response.read_exact(&mut ack).await.unwrap();
        assert_eq!(ack, [0]);
        let (drain, paused) = watch::channel(false);
        let (seen, transfer_seen) = watch::channel(false);
        let (sent, frames) = mpsc::channel(4);
        let echo_crypto: Arc<Mutex<Option<SecureSession>>> = Arc::new(Mutex::new(None));
        let source_generation = Arc::new(AtomicU64::new(0));
        let remote_crypto = echo_crypto.clone();
        let remote_generation = source_generation.clone();
        let raw = sink.clone();
        let sink_task = tokio::spawn(async move {
            let mut streams = JoinSet::new();
            loop {
                tokio::select! {
                    result = streams.join_next(), if !streams.is_empty() => { result.unwrap().unwrap(); }
                    accepted = raw.accept_bi() => {
                        let Ok((_unused, mut receive)) = accepted else { break; };
                        assert!(streams.len() < 4, "fixture only permits its three business streams");
                        let connection = raw.clone();
                        let mut paused = paused.clone();
                        let seen = seen.clone();
                        let sent = sent.clone();
                        let crypto = remote_crypto.clone();
                        let generation = remote_generation.clone();
                        streams.spawn(async move {
                            let mut route = [0; 9];
                            receive.read_exact(&mut route).await.unwrap();
                            let channel = ChannelKind::try_from(route[0]).unwrap();
                            generation.store(u64::from_be_bytes(route[1..].try_into().unwrap()), Ordering::Release);
                            assert!(matches!(channel, ChannelKind::Transfer | ChannelKind::Control | ChannelKind::Input));
                            // Keep one reply stream alive per core lane. Finishing
                            // it would look like a peer departure to QuicClient.
                            let mut reply = if channel == ChannelKind::Transfer {
                                seen.send_replace(true);
                                None
                            } else {
                                let (mut reply, _) = connection.open_bi().await.unwrap();
                                reply.write_all(&route).await.unwrap();
                                Some(reply)
                            };
                            loop {
                                if channel == ChannelKind::Transfer {
                                    while !*paused.borrow() {
                                        paused.changed().await.unwrap();
                                    }
                                }
                                let mut length = [0; 4];
                                if receive.read_exact(&mut length).await.is_err() { return; }
                                let length = u32::from_be_bytes(length) as usize;
                                assert!(length <= MAX_RELIABLE_MESSAGE_BYTES);
                                let mut body = vec![0; length];
                                receive.read_exact(&mut body).await.unwrap();
                                if let Some(reply) = &mut reply {
                                    // Once the test enables Noise, echo business
                                    // plaintext under the opposite direction key.
                                    // Reflecting ciphertext would fail AEAD.
                                    let body = {
                                        let mut secure = crypto.lock().await;
                                        if let Some(secure) = secure.as_mut() {
                                            let lane = match channel {
                                                ChannelKind::Control => SecureLane::Control,
                                                ChannelKind::Input => SecureLane::Input,
                                                _ => unreachable!(),
                                            };
                                            let plaintext = secure.open(lane, &body).unwrap();
                                            secure.seal(lane, &plaintext).unwrap()
                                        } else { body }
                                    };
                                    reply.write_all(&(body.len() as u32).to_be_bytes()).await.unwrap();
                                    reply.write_all(&body).await.unwrap();
                                } else if sent.send(body).await.is_err() { return; }
                            }
                        });
                    }
                }
            }
        });
        source
            .send_control(b"bootstrap generation".to_vec())
            .await
            .unwrap();
        assert_eq!(
            source.next_control().await.unwrap(),
            b"bootstrap generation"
        );
        assert_ne!(source.peer_generation(), 0);
        Self {
            relay,
            relay_task,
            source,
            _sink_endpoint: sink_endpoint,
            sink,
            sink_task,
            transfer_seen,
            drain,
            frames,
            echo_crypto,
            source_generation,
        }
    }

    async fn core_roundtrips(&self, backend: &QuicRelayBackend) {
        for channel in [ChannelKind::Input, ChannelKind::Control] {
            let payload = Bytes::from(vec![channel as u8; 31]);
            tokio::time::timeout(WAIT, backend.send(channel, payload.clone()))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                tokio::time::timeout(
                    WAIT,
                    self.source
                        .next_channel_for_generation(channel, backend.peer_generation())
                )
                .await
                .unwrap()
                .unwrap(),
                payload
            );
        }
    }

    async fn frame(&mut self) -> Vec<u8> {
        tokio::time::timeout(WAIT, self.frames.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn canceled_relay_waiters_preserve_a_started_frame_and_the_core_connection() {
    let mut fixture = tokio::time::timeout(WAIT, RawSinkFixture::new())
        .await
        .unwrap();
    let backend =
        QuicRelayBackend::new(fixture.source.clone(), fixture.source.peer_generation()).unwrap();
    let prime = Bytes::from(vec![0x31; 8_192]);
    tokio::time::timeout(WAIT, backend.send(ChannelKind::Transfer, prime.clone()))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(WAIT, async {
        while !*fixture.transfer_seen.borrow() {
            fixture.transfer_seen.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    // The raw sink has read only the route header. The relay is now forwarding
    // a complete prime frame into a 2 KiB window, not accepting another frame.
    let large = Bytes::from(vec![0xa5; MAX_RELIABLE_MESSAGE_BYTES]);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(200),
            backend.send(ChannelKind::Transfer, large.clone())
        )
        .await
        .is_err(),
        "the two bounded stream windows must block this complete large frame"
    );
    // This small request can enter the one-slot writer queue, but its caller
    // departs before the started large frame is released. It must be skipped.
    let canceled_small = Bytes::from_static(b"canceled queued opaque ciphertext");
    assert!(
        tokio::time::timeout(
            Duration::from_millis(50),
            backend.send(ChannelKind::Transfer, canceled_small)
        )
        .await
        .is_err()
    );
    assert!(!fixture.source.is_closed());
    assert!(fixture.sink.close_reason().is_none());
    fixture.core_roundtrips(&backend).await;
    assert_eq!(fixture.relay.capacity_snapshot().accepted_connections, 2);
    assert_eq!(fixture.source.peer_generation(), backend.peer_generation());

    fixture.drain.send_replace(true);
    assert_eq!(fixture.frame().await.as_slice(), prime.as_ref());
    assert_eq!(
        fixture.frame().await.as_slice(),
        large.as_ref(),
        "post-cancel delivery proves the worker started this frame before its caller departed, and preserved every byte"
    );
    let sentinel = Bytes::from_static(b"next uncanceled opaque ciphertext");
    tokio::time::timeout(WAIT, backend.send(ChannelKind::Transfer, sentinel.clone()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        fixture.frame().await.as_slice(),
        sentinel.as_ref(),
        "the canceled queued request must neither precede nor corrupt the next complete frame"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(75), fixture.frames.recv())
            .await
            .is_err()
    );
    fixture.core_roundtrips(&backend).await;
    assert!(!fixture.source.is_closed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn promotion_fence_timeout_preserves_its_owned_frame_and_the_relay_core() {
    // A full generation-bound DLF1 frame is 56 bytes. Constrain source-only
    // send buffering to seven bytes as well as the 48-byte receive stream:
    // receiver credit alone does not prove write_all itself is still pending.
    // Their combined 55 bytes still cannot accept the entire 56-byte frame.
    // Relay and sink retain their separate 2 KiB send windows.
    let mut fixture = tokio::time::timeout(WAIT, RawSinkFixture::with_windows(48, 7))
        .await
        .unwrap();
    let backend = Arc::new(
        QuicRelayBackend::new(fixture.source.clone(), fixture.source.peer_generation()).unwrap(),
    );
    let prime = Bytes::from(vec![0x42; 256]);
    tokio::time::timeout(WAIT, backend.send(ChannelKind::Transfer, prime.clone()))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(WAIT, async {
        while !*fixture.transfer_seen.borrow() {
            fixture.transfer_seen.changed().await.unwrap();
        }
    })
    .await
    .unwrap();

    let (source_secure, remote_secure) = super::secure_pair();
    let binding = source_secure.video_path_binding();
    let source_secure = Arc::new(Mutex::new(source_secure));
    let remote_secure = Arc::new(Mutex::new(remote_secure));
    let source_endpoint =
        Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
    let remote_endpoint = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let now = super::now();
    let candidate = DirectLanCandidate::new(
        92,
        source_endpoint.local_addr().unwrap(),
        now + 10,
        binding,
        now,
    )
    .unwrap();
    let accepting = source_endpoint.clone();
    let accepting_crypto = source_secure.clone();
    let probe = tokio::spawn(async move {
        let raw = accepting.accept().await.unwrap().unwrap();
        accepting
            .accept_probe_connection_shared(raw, 92, &binding, &accepting_crypto, now)
            .await
            .unwrap()
            .0
    });
    let remote_direct = remote_endpoint
        .connect_shared(&candidate, &binding, &remote_secure, now)
        .await
        .unwrap()
        .0;
    let source_direct = probe.await.unwrap();
    *fixture.echo_crypto.lock().await =
        Some(Arc::try_unwrap(remote_secure).ok().unwrap().into_inner());
    let selected = 1 << (ChannelKind::Transfer as u16 - 1);
    let source_caps = super::caps(DeviceRole::Host, Some(selected));
    let remote_caps = super::caps(DeviceRole::Controller, Some(selected));
    let agreement = NegotiatedLanData::from_authenticated_caps(
        &source_caps,
        &remote_caps,
        binding,
        backend.peer_generation(),
    )
    .unwrap()
    .unwrap();
    let remote_generation = fixture.source_generation.load(Ordering::Acquire);
    assert_ne!(remote_generation, 0);
    let remote_agreement = NegotiatedLanData::from_authenticated_caps(
        &remote_caps,
        &source_caps,
        binding,
        remote_generation,
    )
    .unwrap()
    .unwrap();
    let config = LanDataConfig {
        direct_timeout: Duration::from_millis(50),
        operation_timeout: Duration::from_millis(300),
    };
    let plane =
        SessionDataPlane::relay_only_with_config(agreement, source_secure, backend.clone(), config)
            .await
            .unwrap();

    // The test owns both typed negotiation state machines. Authorities still
    // require a real Noise-bound probe and exact Offer/Ready/Commit progression;
    // no private authority fields or alternative runtime contract are forged.
    let mut host_switch = LanSwitchNegotiator::new(agreement);
    let mut remote_switch = LanSwitchNegotiator::new(remote_agreement);
    let offer = host_switch.offer(&candidate, selected, now).unwrap();
    let LanSwitchAction::SendReady(ready) = remote_switch.receive(&offer, &candidate, now).unwrap()
    else {
        panic!("expected Ready");
    };
    let LanSwitchAction::SendCommit(commit) = host_switch.receive(&ready, &candidate, now).unwrap()
    else {
        panic!("expected Commit");
    };
    let commit_bytes = Bytes::from(super::encode_control(&commit).unwrap());
    assert_eq!(
        plane
            .send(ChannelKind::Control, commit_bytes.clone())
            .await
            .unwrap(),
        desklink_transport::LanDataRoute::Relay
    );
    assert_eq!(
        tokio::time::timeout(WAIT, plane.recv(ChannelKind::Control))
            .await
            .unwrap()
            .unwrap(),
        commit_bytes
    );
    let authority = host_switch.commit_sent().unwrap();
    assert!(matches!(
        remote_switch.receive(&commit, &candidate, now).unwrap(),
        LanSwitchAction::Attach(_)
    ));
    plane.attach_direct(authority, source_direct).await.unwrap();
    tokio::time::sleep(config.operation_timeout + Duration::from_millis(100)).await;
    let state = *plane.health().borrow();
    assert!(state.relay_connected && !state.closed);
    assert_eq!(
        state.lanes[ChannelKind::Transfer as usize - 1],
        SessionLaneHealth::Relay,
        "a promotion timeout leaves the existing relay route intact"
    );
    assert!(!fixture.source.is_closed());
    assert!(fixture.sink.close_reason().is_none());
    assert_eq!(fixture.source.peer_generation(), backend.peer_generation());
    for (channel, payload) in [
        (
            ChannelKind::Control,
            Bytes::from(
                super::encode_control(&desklink_protocol::ControlMessage::SetAudioEnabled {
                    enabled: true,
                })
                .unwrap(),
            ),
        ),
        (
            ChannelKind::Input,
            Bytes::from(
                super::encode_input(&desklink_protocol::InputEnvelope {
                    sequence: 1,
                    timestamp_us: 1,
                    stream_id: 1,
                    input_epoch: 1,
                    event: desklink_protocol::InputEvent::MouseWheel {
                        delta_x: 1,
                        delta_y: -1,
                    },
                })
                .unwrap(),
            ),
        ),
    ] {
        let sent = plane.send(channel, payload.clone()).await;
        assert!(
            sent.is_ok(),
            "core send {channel:?} failed: {sent:?}; source_closed={}, sink_closed={:?}, health={:?}",
            fixture.source.is_closed(),
            fixture.sink.close_reason(),
            *plane.health().borrow()
        );
        assert_eq!(sent.unwrap(), desklink_transport::LanDataRoute::Relay);
        assert_eq!(
            tokio::time::timeout(WAIT, plane.recv(channel))
                .await
                .unwrap()
                .unwrap(),
            payload
        );
    }
    fixture.drain.send_replace(true);
    assert_eq!(fixture.frame().await.as_slice(), prime.as_ref());
    let fence = fixture.frame().await;
    let fence_plaintext = fixture
        .echo_crypto
        .lock()
        .await
        .as_mut()
        .unwrap()
        .open(SecureLane::Transfer, &fence)
        .unwrap();
    assert_eq!(fence_plaintext.len(), 28);
    assert_eq!(&fence_plaintext[..4], b"DLF1");
    assert_eq!(
        u64::from_be_bytes(fence_plaintext[4..12].try_into().unwrap()),
        1
    );
    assert_eq!(fence.len() + 4, 56);
    assert!(
        fence.len() + 4 > 48 + 7,
        "the actual fence must exceed source receive credit plus send buffering"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(75), fixture.frames.recv())
            .await
            .is_err()
    );
    assert!(
        !fixture.source.is_closed(),
        "the started fence completed after its caller timed out"
    );
    assert!(!remote_direct.is_closed());
    tokio::time::timeout(WAIT, plane.close_and_wait())
        .await
        .unwrap();
    assert_eq!(plane.active_tasks(), 0);
}
