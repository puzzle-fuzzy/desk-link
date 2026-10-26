//! Loopback-only lifecycle fixtures; opaque synthetic payloads never reach
//! an OS input injector. A stopped application reader still ACKs QUIC traffic.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use desklink_crypto::SessionId;
use desklink_relay::{RelayConfig, RelayError, RelayServer, RelayShutdownSnapshot};
use desklink_transport::{ChannelKind, MAX_RELIABLE_MESSAGE_BYTES, RelayJoin};
use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const TEST_TIMEOUT: Duration = Duration::from_secs(10);

struct Fixture {
    server: Arc<RelayServer>,
    client_config: quinn::ClientConfig,
    task: Option<tokio::task::JoinHandle<Result<(), RelayError>>>,
}

impl Fixture {
    async fn new(config: RelayConfig) -> Self {
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
        let mut transport = quinn::TransportConfig::default();
        transport
            .stream_receive_window(65_549_u32.into())
            .receive_window(131_072_u32.into())
            .send_window(131_072);
        client_config.transport_config(Arc::new(transport));
        let server = Arc::new(
            RelayServer::bind("127.0.0.1:0".parse().unwrap(), server_config, config)
                .await
                .unwrap(),
        );
        let running = server.clone();
        let task = Some(tokio::spawn(async move { running.run().await }));
        Self {
            server,
            client_config,
            task,
        }
    }

    async fn raw(&self, join: Option<RelayJoin>) -> RawPeer {
        let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint.set_default_client_config(self.client_config.clone());
        let connection = tokio::time::timeout(
            TEST_TIMEOUT,
            endpoint
                .connect(self.server.local_addr().unwrap(), "localhost")
                .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        if let Some(join) = join {
            let (mut send, mut receive) = connection.open_bi().await.unwrap();
            let envelope = join.encode();
            send.write_all(&(envelope.len() as u32).to_be_bytes())
                .await
                .unwrap();
            send.write_all(&envelope).await.unwrap();
            send.finish().unwrap();
            let mut reply = [0; 1];
            receive.read_exact(&mut reply).await.unwrap();
            assert_eq!(reply, [0]);
        }
        RawPeer {
            _endpoint: endpoint,
            connection,
        }
    }

    async fn finish(&mut self) -> Result<(), RelayError> {
        self.server.close();
        tokio::time::timeout(TEST_TIMEOUT, self.task.take().unwrap())
            .await
            .expect("relay run exceeded its total cleanup budget")
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.close();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

struct RawPeer {
    _endpoint: Endpoint,
    connection: quinn::Connection,
}

impl Drop for RawPeer {
    fn drop(&mut self) {
        self.connection
            .close(0_u32.into(), b"shutdown fixture done");
    }
}

fn host(id: u8) -> RelayJoin {
    RelayJoin::host_with_participant(SessionId::from_bytes([id; 16]), [id; 32], [1; 16])
}

fn controller(id: u8) -> RelayJoin {
    RelayJoin::controller_with_participant(SessionId::from_bytes([id; 16]), [id; 32], [2; 16])
}

async fn wait_until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("lifecycle counters did not converge");
}

fn blocked_transfer(
    source: &RawPeer,
) -> (
    tokio::task::JoinSet<()>,
    tokio::task::AbortHandle,
    Arc<AtomicUsize>,
) {
    let source = source.connection.clone();
    let progress = Arc::new(AtomicUsize::new(0));
    let sent = progress.clone();
    let mut tasks = tokio::task::JoinSet::new();
    let handle = tasks.spawn(async move {
        let (mut send, _receive) = source.open_bi().await.unwrap();
        if send
            .write_all(&[ChannelKind::Transfer as u8, 0, 0, 0, 0, 0, 0, 0, 0])
            .await
            .is_err()
        {
            return;
        }
        let body = vec![42; MAX_RELIABLE_MESSAGE_BYTES];
        for _ in 0..100 {
            if send
                .write_all(&(body.len() as u32).to_be_bytes())
                .await
                .is_err()
                || send.write_all(&body).await.is_err()
            {
                return;
            }
            sent.fetch_add(1, Ordering::Release);
        }
    });
    (tasks, handle, progress)
}

async fn input_reaches(source: &RawPeer, target: &RawPeer) {
    tokio::time::timeout(TEST_TIMEOUT, async {
        let (mut send, _receive) = source.connection.open_bi().await.unwrap();
        send.write_all(&[ChannelKind::Input as u8, 0, 0, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        send.write_all(&3_u32.to_be_bytes()).await.unwrap();
        send.write_all(&[1, 2, 3]).await.unwrap();
        let (_send, mut receive) = target.connection.accept_bi().await.unwrap();
        let mut header = [0; 13];
        receive.read_exact(&mut header).await.unwrap();
        assert_eq!(header[0], ChannelKind::Input as u8);
        assert_eq!(&header[9..], &3_u32.to_be_bytes());
        let mut body = [0; 3];
        receive.read_exact(&mut body).await.unwrap();
        assert_eq!(body, [1, 2, 3]);
        send.finish().unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn closing_only_a_backpressured_source_releases_its_reliable_children() {
    let mut fixture = Fixture::new(RelayConfig::default()).await;
    let source = fixture.raw(Some(host(1))).await;
    let sink = fixture.raw(Some(controller(1))).await;
    let healthy_host = fixture.raw(Some(host(2))).await;
    let healthy_controller = fixture.raw(Some(controller(2))).await;
    let mut transfers: Vec<_> = (0..2).map(|_| blocked_transfer(&source)).collect();
    wait_until(|| {
        transfers
            .iter()
            .all(|(_, _, progress)| progress.load(Ordering::Acquire) > 0)
            && sink.connection.stats().frame_rx.stream > 0
            && fixture.server.shutdown_snapshot().reliable_workers == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        transfers.iter().all(|(_, handle, _)| !handle.is_finished()),
        "stopped sink did not exert actual reliable backpressure"
    );
    source
        .connection
        .close(0_u32.into(), b"source alone closed");
    wait_until(|| {
        let snapshot = fixture.server.shutdown_snapshot();
        snapshot.accepted_connections == 3
            && snapshot.reliable_workers == 0
            && snapshot.media_workers == 9
    })
    .await;
    assert!(sink.connection.close_reason().is_none());
    input_reaches(&healthy_controller, &healthy_host).await;
    for (tasks, _, _) in &mut transfers {
        tasks.shutdown().await;
    }
    fixture.finish().await.unwrap();
    assert_eq!(
        fixture.server.shutdown_snapshot(),
        RelayShutdownSnapshot::default()
    );
}

#[tokio::test]
async fn repeated_close_drains_mixed_peers_and_refuses_new_admission() {
    let mut fixture = Fixture::new(RelayConfig {
        max_connections: 3,
        ..RelayConfig::default()
    })
    .await;
    let source = fixture.raw(Some(host(3))).await;
    let sink = fixture.raw(Some(controller(3))).await;
    let unjoined = fixture.raw(None).await;
    let (mut flood, handle, progress) = blocked_transfer(&source);
    wait_until(|| progress.load(Ordering::Acquire) > 0).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!handle.is_finished());
    source
        .connection
        .send_datagram(bytes::Bytes::from_static(&[4, 0, 0, 0, 0, 0, 0, 0, 0, 42]))
        .unwrap();
    wait_until(|| fixture.server.media_snapshot().video.enqueued > 0).await;

    let mut rejection_endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    rejection_endpoint.set_default_client_config(fixture.client_config.clone());
    // Keep an over-cap rejection handler inside its TLS handshake. Forward
    // client Initial packets but blackhole the server's handshake response.
    let proxy = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = proxy.local_addr().unwrap();
    let server_address = fixture.server.local_addr().unwrap();
    let mut proxy_tasks = tokio::task::JoinSet::new();
    proxy_tasks.spawn(async move {
        let mut packet = vec![0; 65_536];
        while let Ok((length, source)) = proxy.recv_from(&mut packet).await {
            if source != server_address {
                let _ = proxy.send_to(&packet[..length], server_address).await;
            }
        }
    });
    let pending_rejection = rejection_endpoint
        .connect(proxy_address, "localhost")
        .unwrap();
    wait_until(|| fixture.server.shutdown_snapshot().rejection_tasks > 0).await;
    assert_eq!(fixture.server.capacity_snapshot().accepted_connections, 3);

    fixture.server.close();
    fixture.server.close();
    for peer in [&source, &sink, &unjoined] {
        let reason = tokio::time::timeout(TEST_TIMEOUT, peer.connection.closed())
            .await
            .unwrap();
        let quinn::ConnectionError::ApplicationClosed(close) = reason else {
            panic!("peer waited for transport failure rather than server close: {reason:?}");
        };
        assert_eq!(close.error_code, 0_u32.into());
        assert_eq!(close.reason.as_ref(), b"relay shutdown");
    }
    fixture.finish().await.unwrap();
    assert_eq!(
        fixture.server.shutdown_snapshot(),
        RelayShutdownSnapshot::default()
    );
    let connecting = rejection_endpoint
        .connect(fixture.server.local_addr().unwrap(), "localhost")
        .unwrap();
    assert!(
        !matches!(
            tokio::time::timeout(Duration::from_millis(150), connecting).await,
            Ok(Ok(_))
        ),
        "closed relay admitted a new connection"
    );
    assert_eq!(fixture.server.capacity_snapshot().accepted_connections, 0);
    drop(pending_rejection);
    proxy_tasks.shutdown().await;
    flood.shutdown().await;
}

#[tokio::test]
async fn an_expired_cleanup_budget_is_reported_as_timeout_not_graceful() {
    let mut fixture = Fixture::new(RelayConfig {
        shutdown_timeout: Duration::from_nanos(1),
        ..RelayConfig::default()
    })
    .await;
    let _unjoined = fixture.raw(None).await;
    assert_eq!(fixture.finish().await, Err(RelayError::ShutdownTimeout));
    wait_until(|| fixture.server.shutdown_snapshot() == RelayShutdownSnapshot::default()).await;
}

#[tokio::test]
async fn closing_before_run_starts_is_idempotent() {
    let mut fixture = Fixture::new(RelayConfig::default()).await;
    fixture.server.close();
    fixture.server.close();
    fixture.finish().await.unwrap();
    assert_eq!(
        fixture.server.shutdown_snapshot(),
        RelayShutdownSnapshot::default()
    );
}
