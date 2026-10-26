//! Local QUIC deadlines for unauthenticated joins and incomplete reliable
//! frames. Healthy authenticated lanes may stay idle between complete frames.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use desklink_crypto::SessionId;
use desklink_relay::{RelayConfig, RelayError, RelayServer};
use desklink_transport::{ChannelKind, QuicClient, QuicClientConfig, RelayJoin, TransportEvent};
use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const TEST_TIMEOUT: Duration = Duration::from_secs(3);
const JOIN_TIMEOUT: Duration = Duration::from_millis(300);
const PARTIAL_FRAME_TIMEOUT: Duration = Duration::from_millis(100);

struct TestRelay {
    server: Arc<RelayServer>,
    client_config: quinn::ClientConfig,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.server.close();
        self.task.abort();
    }
}

fn certificate_configs() -> (ServerConfig, quinn::ClientConfig) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_der = certificate.cert.der().to_vec();
    let server_config = ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der.clone())],
        PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into()),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(certificate_der)).unwrap();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    (server_config, quinn::ClientConfig::new(Arc::new(crypto)))
}

fn deadline_config() -> RelayConfig {
    RelayConfig {
        join_timeout: JOIN_TIMEOUT,
        partial_frame_timeout: PARTIAL_FRAME_TIMEOUT,
        keep_alive: Duration::from_millis(20),
        dead_timeout: Duration::from_secs(2),
        ..RelayConfig::default()
    }
}

impl TestRelay {
    async fn new() -> Self {
        Self::with_config(deadline_config()).await
    }

    async fn with_config(config: RelayConfig) -> Self {
        let (server_config, client_config) = certificate_configs();
        let server = Arc::new(
            RelayServer::bind("127.0.0.1:0".parse().unwrap(), server_config, config)
                .await
                .unwrap(),
        );
        let running = server.clone();
        let task = tokio::spawn(async move { running.run().await.unwrap() });
        Self {
            server,
            client_config,
            task,
        }
    }

    fn client_config(&self) -> QuicClientConfig {
        QuicClientConfig::with_client_config(
            self.server.local_addr().unwrap(),
            "localhost".to_owned(),
            self.client_config.clone(),
        )
    }

    async fn connect_raw(&self, reply_window: Option<u32>) -> RawPeer {
        let mut client_config = self.client_config.clone();
        if let Some(reply_window) = reply_window {
            let mut transport = quinn::TransportConfig::default();
            transport.stream_receive_window(reply_window.into());
            client_config.transport_config(Arc::new(transport));
        }
        let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(client_config);
        let connection = tokio::time::timeout(
            TEST_TIMEOUT,
            endpoint
                .connect(self.server.local_addr().unwrap(), "localhost")
                .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        RawPeer {
            _endpoint: endpoint,
            connection,
        }
    }

    async fn join_raw(&self, join: RelayJoin) -> RawPeer {
        let peer = self.connect_raw(None).await;
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (mut send, mut receive) = peer.connection.open_bi().await.unwrap();
            let envelope = join.encode();
            send.write_all(&(envelope.len() as u32).to_be_bytes())
                .await
                .unwrap();
            send.write_all(&envelope).await.unwrap();
            send.finish().unwrap();
            let mut reply = [0; 1];
            receive.read_exact(&mut reply).await.unwrap();
            assert_eq!(reply, [0]);
        })
        .await
        .unwrap();
        peer
    }

    async fn await_connections(&self, expected: usize) {
        tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                if self.server.capacity_snapshot().accepted_connections == expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "admission did not converge to {expected}: {:?}",
                self.server.capacity_snapshot()
            )
        });
    }
}

struct RawPeer {
    _endpoint: Endpoint,
    connection: quinn::Connection,
}

impl Drop for RawPeer {
    fn drop(&mut self) {
        self.connection
            .close(0_u32.into(), b"deadline fixture complete");
    }
}

fn host(number: u8) -> RelayJoin {
    RelayJoin::host_with_participant(SessionId::from_bytes([number; 16]), [number; 32], [1; 16])
}

fn controller(number: u8) -> RelayJoin {
    RelayJoin::controller_with_participant(
        SessionId::from_bytes([number; 16]),
        [number; 32],
        [2; 16],
    )
}

async fn application_closed(connection: &quinn::Connection) {
    let error = tokio::time::timeout(TEST_TIMEOUT, connection.closed())
        .await
        .expect("application deadline failed to close the peer");
    assert!(
        matches!(error, quinn::ConnectionError::ApplicationClosed(_)),
        "expected application deadline rather than transport idle timeout: {error:?}"
    );
}

async fn next_event(client: &QuicClient) -> TransportEvent {
    tokio::time::timeout(TEST_TIMEOUT, client.next_event())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn silent_keepalive_connection_expires_and_admission_is_reusable() {
    let relay = TestRelay::new().await;
    let silent = relay.connect_raw(None).await;
    // The protocol driver answers server keepalives even though the
    // application never opens a stream. Transport idle expiry is not enough.
    tokio::time::timeout(TEST_TIMEOUT, async {
        while silent.connection.stats().frame_rx.ping == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(silent.connection.close_reason().is_none());
    application_closed(&silent.connection).await;
    relay.await_connections(0).await;
    let replacement = relay.join_raw(host(10)).await;
    assert!(replacement.connection.close_reason().is_none());
    assert_eq!(relay.server.capacity_snapshot().attached_participants, 1);
}

#[tokio::test]
async fn incomplete_join_length_and_body_release_admission() {
    let relay = TestRelay::new().await;
    for prefix in [vec![0], vec![0, 0, 0, 128, 1]] {
        let peer = relay.connect_raw(None).await;
        let (mut send, _receive) = peer.connection.open_bi().await.unwrap();
        send.write_all(&prefix).await.unwrap();
        application_closed(&peer.connection).await;
        relay.await_connections(0).await;
        assert_eq!(relay.server.capacity_snapshot().attached_participants, 0);
    }
}

#[tokio::test]
async fn zero_join_reply_window_cannot_pin_an_attached_participant() {
    let relay = TestRelay::new().await;
    let peer = relay.connect_raw(Some(0)).await;
    let (mut send, _receive) = peer.connection.open_bi().await.unwrap();
    let envelope = host(11).encode();
    send.write_all(&(envelope.len() as u32).to_be_bytes())
        .await
        .unwrap();
    send.write_all(&envelope).await.unwrap();
    send.finish().unwrap();
    tokio::time::timeout(TEST_TIMEOUT, async {
        while relay.server.capacity_snapshot().attached_participants != 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("join never reached its blocked reply write");
    application_closed(&peer.connection).await;
    relay.await_connections(0).await;
    assert_eq!(relay.server.capacity_snapshot().attached_participants, 0);
}

#[tokio::test]
async fn partial_route_header_length_and_body_close_only_the_bad_source() {
    let relay = TestRelay::new().await;
    let healthy_host = QuicClient::connect(relay.client_config()).await.unwrap();
    let healthy_controller = QuicClient::connect(relay.client_config()).await.unwrap();
    healthy_host.join(host(12)).await.unwrap();
    healthy_controller.join(controller(12)).await.unwrap();

    let header = [ChannelKind::Input as u8, 0, 0, 0, 0, 0, 0, 0, 0];
    let mut partial_length = header.to_vec();
    partial_length.push(0);
    let mut partial_body = header.to_vec();
    partial_body.extend_from_slice(&2_u32.to_be_bytes());
    partial_body.push(42);
    for (number, prefix) in [vec![ChannelKind::Input as u8], partial_length, partial_body]
        .into_iter()
        .enumerate()
    {
        let bad = relay.join_raw(host(20 + number as u8)).await;
        let (mut send, _receive) = bad.connection.open_bi().await.unwrap();
        send.write_all(&prefix).await.unwrap();
        application_closed(&bad.connection).await;
        relay.await_connections(2).await;
        healthy_controller
            .send_input(vec![number as u8])
            .await
            .unwrap();
        assert_eq!(
            next_event(&healthy_host).await,
            TransportEvent::Input(vec![number as u8])
        );
    }
}

#[tokio::test]
async fn same_reliable_lane_survives_idle_before_and_between_complete_frames() {
    let relay = TestRelay::new().await;
    let destination = QuicClient::connect(relay.client_config()).await.unwrap();
    destination.join(host(30)).await.unwrap();
    let source = relay.join_raw(controller(30)).await;
    let (mut send, _receive) = source.connection.open_bi().await.unwrap();
    send.write_all(&[ChannelKind::Input as u8, 0, 0, 0, 0, 0, 0, 0, 0])
        .await
        .unwrap();
    for payload in [b"first".as_slice(), b"second".as_slice()] {
        tokio::time::sleep(PARTIAL_FRAME_TIMEOUT * 3).await;
        assert!(source.connection.close_reason().is_none());
        send.write_all(&(payload.len() as u32).to_be_bytes())
            .await
            .unwrap();
        send.write_all(payload).await.unwrap();
        assert_eq!(
            next_event(&destination).await,
            TransportEvent::Input(payload.to_vec())
        );
    }
    send.finish().unwrap();
    // A clean EOF ends this lane without closing its authenticated connection.
    tokio::time::sleep(PARTIAL_FRAME_TIMEOUT * 2).await;
    assert!(source.connection.close_reason().is_none());
}

#[tokio::test]
async fn normally_fragmented_join_and_frame_complete_within_their_budgets() {
    let relay = TestRelay::with_config(RelayConfig {
        join_timeout: Duration::from_secs(1),
        partial_frame_timeout: Duration::from_millis(400),
        ..deadline_config()
    })
    .await;
    let destination = QuicClient::connect(relay.client_config()).await.unwrap();
    destination.join(host(31)).await.unwrap();
    let source = relay.connect_raw(None).await;
    let (mut join_send, mut join_receive) = source.connection.open_bi().await.unwrap();
    let envelope = controller(31).encode();
    let length = (envelope.len() as u32).to_be_bytes();
    join_send.write_all(&length[..2]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    join_send.write_all(&length[2..]).await.unwrap();
    join_send.write_all(&envelope[..16]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    join_send.write_all(&envelope[16..]).await.unwrap();
    join_send.finish().unwrap();
    let mut reply = [0; 1];
    tokio::time::timeout(TEST_TIMEOUT, join_receive.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply, [0]);

    let (mut send, _receive) = source.connection.open_bi().await.unwrap();
    send.write_all(&[ChannelKind::Input as u8]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    send.write_all(&0_u64.to_be_bytes()).await.unwrap();
    send.write_all(&[0]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    send.write_all(&[0, 0, 3, 1]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    send.write_all(&[2, 3]).await.unwrap();
    assert_eq!(
        next_event(&destination).await,
        TransportEvent::Input(vec![1, 2, 3])
    );
    assert!(source.connection.close_reason().is_none());
}

#[tokio::test]
async fn zero_application_deadlines_are_rejected_at_bind() {
    for (join_timeout, partial_frame_timeout) in [
        (Duration::ZERO, PARTIAL_FRAME_TIMEOUT),
        (JOIN_TIMEOUT, Duration::ZERO),
    ] {
        let (server_config, _) = certificate_configs();
        let result = RelayServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            server_config,
            RelayConfig {
                join_timeout,
                partial_frame_timeout,
                ..deadline_config()
            },
        )
        .await;
        assert!(matches!(result, Err(RelayError::InvalidConfig(_))));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual 512-connection local cleanup measurement; runs the default 8s join budget"]
async fn silent_512_connection_admission_cleanup_probe() {
    const CONNECTIONS: usize = 512;
    let config = RelayConfig {
        max_connections: CONNECTIONS,
        max_sessions: CONNECTIONS,
        buffer_budget_bytes: 2 * 1024 * 1024 * 1024,
        keep_alive: Duration::from_millis(100),
        ..RelayConfig::default()
    };
    let join_timeout = config.join_timeout;
    let relay = TestRelay::with_config(config).await;
    let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(relay.client_config.clone());
    let probe_started = Instant::now();
    let mut connecting = tokio::task::JoinSet::new();
    let mut peers = Vec::with_capacity(CONNECTIONS);
    // Respect the production endpoint's 64 pending-handshake cap instead of
    // treating its intentional refusal of an unbounded burst as a test failure.
    for _ in 0..CONNECTIONS / 16 {
        for _ in 0..16 {
            let started = Instant::now();
            let connection = endpoint
                .connect(relay.server.local_addr().unwrap(), "localhost")
                .unwrap();
            connecting.spawn(async move {
                let connection = tokio::time::timeout(Duration::from_secs(20), connection)
                    .await
                    .unwrap()
                    .unwrap();
                (connection, started)
            });
        }
        while let Some(peer) = connecting.join_next().await {
            peers.push(peer.unwrap());
        }
    }
    let peak = relay.server.capacity_snapshot();
    assert_eq!(peak.accepted_connections, CONNECTIONS);
    assert_eq!(peak.attached_participants, 0);
    let connect_phase = probe_started.elapsed();
    println!(
        "DL2-053 local cleanup probe peak_connections={} joined=0 join_budget_ms={} \
         connect_phase_ms={:.3}",
        peak.accepted_connections,
        join_timeout.as_millis(),
        connect_phase.as_secs_f64() * 1_000.0,
    );
    let mut closing = tokio::task::JoinSet::new();
    for (peer, started) in peers {
        closing.spawn(async move {
            let error = tokio::time::timeout(Duration::from_secs(20), peer.closed())
                .await
                .unwrap();
            assert!(matches!(
                error,
                quinn::ConnectionError::ApplicationClosed(_)
            ));
            (started.elapsed(), peer.stats().frame_rx.ping)
        });
    }
    let mut observed = Vec::with_capacity(CONNECTIONS);
    let mut keepalive_peers = 0;
    while let Some(result) = closing.join_next().await {
        let (elapsed, pings) = result.unwrap();
        observed.push(elapsed);
        keepalive_peers += usize::from(pings > 0);
    }
    relay.await_connections(0).await;
    observed.sort_unstable();
    let percentile = |percent: usize| {
        observed[(observed.len() * percent).div_ceil(100) - 1].as_secs_f64() * 1_000.0
    };
    println!(
        "DL2-053 local cleanup probe samples={} keepalive_peers={} \
         client_connect_start_to_application_close_p95_ms={:.3} p99_ms={:.3} max_ms={:.3} \
         total_until_admission_zero_ms={:.3} accepted_connections=0 active_workers={}",
        observed.len(),
        keepalive_peers,
        percentile(95),
        percentile(99),
        observed.last().unwrap().as_secs_f64() * 1_000.0,
        probe_started.elapsed().as_secs_f64() * 1_000.0,
        relay.server.media_snapshot().active_workers,
    );
    assert_eq!(keepalive_peers, CONNECTIONS);
    assert_eq!(relay.server.media_snapshot().active_workers, 0);
}
