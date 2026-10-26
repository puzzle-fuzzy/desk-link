//! Local QUIC resource-bound fixtures. Flow-control waits prove unavailable
//! credit, not that the relay closed a peer for sending a forbidden packet.
//! Declared payload budgets are not measured process/container RSS limits.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use desklink_crypto::SessionId;
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{
    ChannelKind, MAX_RELIABLE_MESSAGE_BYTES, QuicClient, QuicClientConfig, RelayJoin,
};
use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const TEST_TIMEOUT: Duration = Duration::from_secs(3);
const CREDIT_WAIT: Duration = Duration::from_millis(100);

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

impl TestRelay {
    async fn new() -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let certificate_der = certificate.cert.der().to_vec();
        let server_config = ServerConfig::with_single_cert(
            vec![CertificateDer::from(certificate_der.clone())],
            PrivateKeyDer::Pkcs8(certificate.key_pair.serialize_der().into()),
        )
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(certificate_der)).unwrap();
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
        let mut client_config = quinn::ClientConfig::new(Arc::new(crypto));
        // A stopped application reader must exert real receive credit, and
        // the source write must not merely disappear into an 8MiB send queue.
        let mut transport = quinn::TransportConfig::default();
        transport
            .stream_receive_window(65_549_u32.into())
            .receive_window(131_072_u32.into())
            .send_window(131_072);
        client_config.transport_config(Arc::new(transport));
        let server = Arc::new(
            RelayServer::bind(
                "127.0.0.1:0".parse().unwrap(),
                server_config,
                RelayConfig::default(),
            )
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

    async fn connect_raw(&self) -> RawPeer {
        let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(self.client_config.clone());
        let connecting = endpoint
            .connect(self.server.local_addr().unwrap(), "localhost")
            .unwrap();
        let connection = tokio::time::timeout(TEST_TIMEOUT, connecting)
            .await
            .unwrap()
            .unwrap();
        RawPeer {
            endpoint,
            connection,
        }
    }

    async fn join_raw(&self, join: RelayJoin) -> RawPeer {
        let peer = self.connect_raw().await;
        tokio::time::timeout(TEST_TIMEOUT, async {
            let (mut send, mut receive) = peer.connection.open_bi().await.unwrap();
            let envelope = join.encode();
            send.write_all(&(envelope.len() as u32).to_be_bytes())
                .await
                .unwrap();
            send.write_all(&envelope).await.unwrap();
            send.finish().unwrap();
            let mut acknowledgment = [0; 1];
            receive.read_exact(&mut acknowledgment).await.unwrap();
            assert_eq!(acknowledgment, [0]);
        })
        .await
        .unwrap();
        peer
    }

    fn config(&self) -> QuicClientConfig {
        QuicClientConfig::with_client_config(
            self.server.local_addr().unwrap(),
            "localhost".to_owned(),
            self.client_config.clone(),
        )
    }
}

struct RawPeer {
    endpoint: Endpoint,
    connection: quinn::Connection,
}

impl Drop for RawPeer {
    fn drop(&mut self) {
        self.connection
            .close(0_u32.into(), b"resource fixture finished");
        self.endpoint
            .close(0_u32.into(), b"resource fixture finished");
    }
}

fn host(id: u8) -> RelayJoin {
    RelayJoin::host_with_participant(SessionId::from_bytes([id; 16]), [id; 32], [1; 16])
}

fn controller(id: u8) -> RelayJoin {
    RelayJoin::controller_with_participant(SessionId::from_bytes([id; 16]), [id; 32], [2; 16])
}

async fn start_transfer(source: &quinn::Connection) -> quinn::SendStream {
    let (mut send, _receive) = source.open_bi().await.unwrap();
    send.write_all(&[ChannelKind::Transfer as u8])
        .await
        .unwrap();
    send.write_all(&0_u64.to_be_bytes()).await.unwrap();
    send
}

async fn write_frame(send: &mut quinn::SendStream, payload: &[u8]) {
    send.write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .unwrap();
    send.write_all(payload).await.unwrap();
}

async fn assert_prejoin_datagram_is_closed(payload: Vec<u8>, partial_join: bool) {
    assert_eq!(RelayConfig::default().join_timeout, Duration::from_secs(8));
    let relay = TestRelay::new().await;
    let peer = relay.connect_raw().await;
    let _incomplete_join = if partial_join {
        let (mut send, receive) = peer.connection.open_bi().await.unwrap();
        send.write_all(&[0]).await.unwrap();
        Some((send, receive))
    } else {
        None
    };
    assert_eq!(relay.server.capacity_snapshot().accepted_connections, 1);
    peer.connection.send_datagram(payload.into()).unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(1), peer.connection.closed())
        .await
        .expect("pre-join datagram must be rejected before the eight-second join deadline");
    assert!(matches!(
        reason,
        quinn::ConnectionError::ApplicationClosed(_)
    ));
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let capacity = relay.server.capacity_snapshot();
            if capacity.accepted_connections == 0
                && capacity.attached_participants == 0
                && capacity.active_sessions == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pre-join rejection must reclaim admission without leaving membership");
}

#[tokio::test]
async fn empty_datagram_before_any_join_is_closed_and_reclaims_admission() {
    assert_prejoin_datagram_is_closed(Vec::new(), false).await;
}

#[tokio::test]
async fn nonempty_datagram_before_any_join_is_closed_and_reclaims_admission() {
    assert_prejoin_datagram_is_closed(vec![0xa5], false).await;
}

#[tokio::test]
async fn empty_datagram_during_partial_join_is_closed_and_reclaims_admission() {
    assert_prejoin_datagram_is_closed(Vec::new(), true).await;
}

#[tokio::test]
async fn nonempty_datagram_during_partial_join_is_closed_and_reclaims_admission() {
    assert_prejoin_datagram_is_closed(vec![0xa5], true).await;
}

#[tokio::test]
async fn raw_unjoined_client_cannot_obtain_uni_or_more_than_seventeen_bidi_credits() {
    let relay = TestRelay::new().await;
    let budget = relay.server.resource_budget();
    assert_eq!(budget.max_incoming_uni_streams, 0);
    assert_eq!(budget.max_incoming_bidi_streams, 17);
    assert_eq!(budget.stream_receive_window_bytes, 65_549);
    assert_eq!(budget.connection_receive_window_bytes, 131_072);
    assert_eq!(budget.send_window_bytes, 131_072);
    assert_eq!(budget.datagram_receive_buffer_bytes, 32_768);
    assert_eq!(budget.datagram_send_buffer_bytes, 65_536);
    assert!(budget.modeled_total_buffer_bytes <= budget.declared_buffer_budget_bytes);
    let peer = relay.connect_raw().await;

    assert!(
        tokio::time::timeout(CREDIT_WAIT, peer.connection.open_uni())
            .await
            .is_err()
    );
    assert!(
        peer.connection.close_reason().is_none(),
        "credit wait is not a protocol close"
    );
    let mut retained = Vec::new();
    for _ in 0..budget.max_incoming_bidi_streams {
        let (mut send, receive) = tokio::time::timeout(TEST_TIMEOUT, peer.connection.open_bi())
            .await
            .unwrap()
            .unwrap();
        // One incomplete join length prevents the relay from retiring the
        // first stream. Retain every stream so no credit can be recycled.
        send.write_all(&[0]).await.unwrap();
        retained.push((send, receive));
    }
    assert!(
        tokio::time::timeout(CREDIT_WAIT, peer.connection.open_bi())
            .await
            .is_err()
    );
    assert_eq!(retained.len(), 17);
    assert!(peer.connection.close_reason().is_none());
    assert_eq!(relay.server.capacity_snapshot().attached_participants, 0);
}

#[tokio::test]
async fn maximum_opaque_reliable_frames_fit_the_window_and_replenish_receive_credit() {
    let relay = TestRelay::new().await;
    let host = relay.join_raw(host(40)).await;
    let controller = relay.join_raw(controller(40)).await;
    let frame = vec![0xa5; MAX_RELIABLE_MESSAGE_BYTES];
    assert_eq!(
        frame.len() + 9 + 4,
        relay.server.resource_budget().stream_receive_window_bytes as usize
    );
    tokio::time::timeout(TEST_TIMEOUT, async {
        let writing = async {
            let mut send = start_transfer(&controller.connection).await;
            write_frame(&mut send, &frame).await;
            write_frame(&mut send, &frame).await;
            send.finish().unwrap();
        };
        let reading = async {
            let (_send, mut receive) = host.connection.accept_bi().await.unwrap();
            let mut header = [0; 9];
            receive.read_exact(&mut header).await.unwrap();
            assert_eq!(header[0], ChannelKind::Transfer as u8);
            assert_ne!(u64::from_be_bytes(header[1..].try_into().unwrap()), 0);
            for _ in 0..2 {
                let mut length = [0; 4];
                receive.read_exact(&mut length).await.unwrap();
                assert_eq!(
                    u32::from_be_bytes(length) as usize,
                    MAX_RELIABLE_MESSAGE_BYTES
                );
                let mut forwarded = vec![0; MAX_RELIABLE_MESSAGE_BYTES];
                receive.read_exact(&mut forwarded).await.unwrap();
                assert_eq!(forwarded, frame);
            }
        };
        tokio::join!(writing, reading);
    })
    .await
    .expect("full legal frames must survive bounded receive windows");
}

#[tokio::test]
async fn stopped_reader_backpressures_its_source_without_blocking_another_session() {
    let relay = TestRelay::new().await;
    let slow_host = relay.join_raw(host(41)).await;
    let slow_controller = relay.join_raw(controller(41)).await;
    let healthy_host = QuicClient::connect(relay.config()).await.unwrap();
    healthy_host.join(host(42)).await.unwrap();
    let healthy_controller = QuicClient::connect(relay.config()).await.unwrap();
    healthy_controller.join(controller(42)).await.unwrap();

    let written = Arc::new(AtomicUsize::new(0));
    let source = slow_controller.connection.clone();
    let progress = written.clone();
    let mut flooding = tokio::spawn(async move {
        let mut send = start_transfer(&source).await;
        let frame = vec![0x5a; MAX_RELIABLE_MESSAGE_BYTES];
        for _ in 0..16 {
            write_frame(&mut send, &frame).await;
            progress.fetch_add(1, Ordering::Release);
        }
        send.finish().unwrap();
    });
    // The slow Host never accepts/reads its transfer stream. These client
    // windows and the relay budget bound the buffers well below 1MiB, so the
    // writer must eventually await credit rather than finish all 16 frames.
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut flooding)
            .await
            .is_err()
    );
    assert!(
        written.load(Ordering::Acquire) > 0,
        "flood must actually send complete frames"
    );
    assert!(written.load(Ordering::Acquire) < 16);
    assert!(slow_host.connection.close_reason().is_none());

    tokio::time::timeout(TEST_TIMEOUT, async {
        healthy_controller
            .send_input(b"independent session input".to_vec())
            .await
            .unwrap();
        assert_eq!(
            healthy_host.next_input().await.unwrap(),
            b"independent session input"
        );
    })
    .await
    .expect("another session must remain serviceable during stopped-reader backpressure");
    assert!(!flooding.is_finished());
    flooding.abort();
    let _ = flooding.await;
    slow_host
        .connection
        .close(0_u32.into(), b"slow fixture cleanup");
    slow_controller
        .connection
        .close(0_u32.into(), b"slow fixture cleanup");
    healthy_host.close(b"resource fixture cleanup");
    healthy_controller.close(b"resource fixture cleanup");
    tokio::time::timeout(TEST_TIMEOUT, async {
        while relay.server.capacity_snapshot().accepted_connections != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("connection capacity must be reclaimed after local attack cleanup");
}
