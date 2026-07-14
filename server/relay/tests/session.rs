use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use desklink_crypto::SessionId;
use desklink_relay::{RelayConfig, RelayError, RelayServer, RelaySessionTable};
use desklink_transport::{
    MAX_DATAGRAM_BYTES, MAX_RELIABLE_MESSAGE_BYTES, QuicClient, QuicClientConfig,
    RELAY_CONNECTION_LIMIT_CLOSE_CODE, RelayJoin, TransportError, TransportEvent,
};
use quinn::{Endpoint, ServerConfig};
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
    spawn_test_relay_with_config(RelayConfig::default()).await
}

async fn spawn_test_relay_with_config(config: RelayConfig) -> TestRelay {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_der = certificate.cert.der().to_vec();
    let key_der = certificate.key_pair.serialize_der();
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
        RelayServer::bind("127.0.0.1:0".parse().unwrap(), server_config, config)
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

fn server_config_for_bind_test() -> ServerConfig {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_der = certificate.cert.der().to_vec();
    let key_der = certificate.key_pair.serialize_der();
    ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der)],
        PrivateKeyDer::Pkcs8(key_der.into()),
    )
    .unwrap()
}

fn config(relay: &TestRelay) -> QuicClientConfig {
    QuicClientConfig::with_client_config(
        relay.address,
        "localhost".to_owned(),
        relay.client_config.clone(),
    )
}

fn session(value: u8) -> SessionId {
    SessionId::from_bytes([value; 16])
}

fn connection(value: u64) -> u64 {
    value
}

fn host(session_id: SessionId, auth: [u8; 32]) -> RelayJoin {
    RelayJoin::host(session_id, auth)
}

fn controller(session_id: SessionId, auth: [u8; 32]) -> RelayJoin {
    RelayJoin::controller(session_id, auth)
}

async fn next_event(client: &QuicClient) -> TransportEvent {
    tokio::time::timeout(Duration::from_secs(2), client.next_event())
        .await
        .expect("event timeout")
        .expect("event")
}

async fn raw_join(relay: &TestRelay, join: RelayJoin) -> (Endpoint, quinn::Connection) {
    let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(relay.client_config.clone());
    let connection = endpoint
        .connect(relay.address, "localhost")
        .unwrap()
        .await
        .unwrap();
    let (mut send, mut receive) = connection.open_bi().await.unwrap();
    let envelope = join.encode();
    send.write_all(&(envelope.len() as u32).to_be_bytes())
        .await
        .unwrap();
    send.write_all(&envelope).await.unwrap();
    send.finish().unwrap();
    let mut response = [0; 1];
    receive.read_exact(&mut response).await.unwrap();
    assert_eq!(response, [0]);
    (endpoint, connection)
}

#[tokio::test]
async fn relay_matches_host_and_controller_and_forwards_opaque_bytes() {
    let relay = spawn_test_relay().await;
    let host_client = QuicClient::connect(config(&relay)).await.unwrap();
    let controller_client = QuicClient::connect(config(&relay)).await.unwrap();
    let session_id = session(8);
    host_client.join(host(session_id, [4; 32])).await.unwrap();
    controller_client
        .join(controller(session_id, [4; 32]))
        .await
        .unwrap();

    let video = vec![0, 1, 2, 255, 0];
    host_client
        .send_video_datagram(video.clone())
        .await
        .unwrap();
    assert_eq!(
        next_event(&controller_client).await,
        TransportEvent::VideoDatagram(video)
    );

    let control = vec![255, 0, 254, 1];
    controller_client
        .send_control(control.clone())
        .await
        .unwrap();
    assert_eq!(
        next_event(&host_client).await,
        TransportEvent::Control(control)
    );

    let input = vec![9, 8, 7, 6];
    controller_client.send_input(input.clone()).await.unwrap();
    assert_eq!(next_event(&host_client).await, TransportEvent::Input(input));

    let config_bytes = vec![5, 4, 3, 2];
    host_client
        .send_video_config(config_bytes.clone())
        .await
        .unwrap();
    assert_eq!(
        next_event(&controller_client).await,
        TransportEvent::VideoConfig(config_bytes)
    );
}

#[tokio::test]
async fn second_controller_is_rejected() {
    let relay = spawn_test_relay().await;
    let first_host = QuicClient::connect(config(&relay)).await.unwrap();
    let first_controller = QuicClient::connect(config(&relay)).await.unwrap();
    let second_controller = QuicClient::connect(config(&relay)).await.unwrap();
    let session_id = session(1);
    first_host.join(host(session_id, [4; 32])).await.unwrap();
    first_controller
        .join(controller(session_id, [4; 32]))
        .await
        .unwrap();

    assert_eq!(
        second_controller
            .join(controller(session_id, [4; 32]))
            .await,
        Err(TransportError::JoinRejected(
            desklink_transport::JoinRejectCode::SessionOccupied
        ))
    );
}

#[tokio::test]
async fn relay_enforces_connection_and_session_admission_caps() {
    let connection_limited = spawn_test_relay_with_config(RelayConfig {
        max_connections: 1,
        max_sessions: 4,
        ..RelayConfig::default()
    })
    .await;
    let first = QuicClient::connect(config(&connection_limited))
        .await
        .unwrap();
    first.join(host(session(16), [4; 32])).await.unwrap();
    let second = QuicClient::connect(config(&connection_limited))
        .await
        .unwrap();
    let error = second
        .join(host(session(18), [4; 32]))
        .await
        .expect_err("connection cap should reject the second connection");
    assert_eq!(error, TransportError::ConnectionLimit);
    assert_eq!(RELAY_CONNECTION_LIMIT_CLOSE_CODE, 0x444c_0001);

    let session_limited = spawn_test_relay_with_config(RelayConfig {
        max_connections: 4,
        max_sessions: 1,
        ..RelayConfig::default()
    })
    .await;
    let first = QuicClient::connect(config(&session_limited)).await.unwrap();
    first.join(host(session(17), [4; 32])).await.unwrap();
    let second = QuicClient::connect(config(&session_limited)).await.unwrap();
    assert_eq!(
        second.join(host(session(18), [4; 32])).await,
        Err(TransportError::JoinRejected(
            desklink_transport::JoinRejectCode::SessionLimit
        ))
    );
}

#[test]
fn second_controller_is_rejected_by_the_session_table() {
    let table = RelaySessionTable::new(RelayConfig::default());
    table.attach_host(session(1), connection(1)).unwrap();
    table.attach_controller(session(1), connection(2)).unwrap();
    assert_eq!(
        table.attach_controller(session(1), connection(3)),
        Err(RelayError::SessionOccupied)
    );
}

#[test]
fn session_expiry_and_precise_detach_are_deterministic() {
    let config = RelayConfig {
        session_ttl: Duration::from_secs(10),
        ..RelayConfig::default()
    };
    let table = RelaySessionTable::new(config);
    let session_id = session(2);
    table.attach_host(session_id, connection(10)).unwrap();
    table.attach_controller(session_id, connection(11)).unwrap();
    assert!(!table.detach(session_id, connection(99)));
    assert!(table.has_connection(session_id, connection(10)));
    assert!(table.detach(session_id, connection(10)));
    assert!(table.has_connection(session_id, connection(11)));
    let expired = table.sweep(Instant::now() + Duration::from_secs(11));
    assert_eq!(expired, vec![session_id]);
    assert!(!table.has_connection(session_id, connection(11)));
}

#[test]
fn expiry_returns_exact_connections_before_immediate_reattach() {
    let table = RelaySessionTable::new(RelayConfig {
        session_ttl: Duration::from_secs(1),
        ..RelayConfig::default()
    });
    let session_id = session(12);
    table.attach_host(session_id, connection(101)).unwrap();
    table
        .attach_controller(session_id, connection(202))
        .unwrap();

    let expired = table.sweep_expired(Instant::now() + Duration::from_secs(2));
    table.attach_host(session_id, connection(303)).unwrap();

    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].session_id(), session_id);
    assert_eq!(expired[0].host_connection_id(), Some(connection(101)));
    assert_eq!(expired[0].controller_connection_id(), Some(connection(202)));
    assert!(table.has_connection(session_id, connection(303)));
}

#[test]
fn admission_caps_are_atomic_and_stable() {
    let table = RelaySessionTable::new(RelayConfig {
        max_connections: 1,
        max_sessions: 4,
        ..RelayConfig::default()
    });
    table.attach_host(session(13), connection(1)).unwrap();
    assert_eq!(
        table.attach_controller(session(13), connection(2)),
        Err(RelayError::ConnectionLimitReached)
    );

    let table = RelaySessionTable::new(RelayConfig {
        max_connections: 4,
        max_sessions: 1,
        ..RelayConfig::default()
    });
    table.attach_host(session(14), connection(3)).unwrap();
    assert_eq!(
        table.attach_host(session(15), connection(4)),
        Err(RelayError::SessionLimitReached)
    );
}

#[tokio::test]
async fn relay_rejects_invalid_timeout_and_admission_configuration() {
    assert!(matches!(
        RelayServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            server_config_for_bind_test(),
            RelayConfig {
                keep_alive: Duration::from_secs(15),
                dead_timeout: Duration::from_secs(15),
                ..RelayConfig::default()
            }
        )
        .await,
        Err(RelayError::InvalidConfig(_))
    ));
    assert!(matches!(
        RelayServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            server_config_for_bind_test(),
            RelayConfig {
                max_connections: 0,
                ..RelayConfig::default()
            }
        )
        .await,
        Err(RelayError::InvalidConfig(_))
    ));
}

#[tokio::test]
async fn session_and_authentication_mismatches_have_stable_errors() {
    let relay = spawn_test_relay().await;
    let controller_without_host = QuicClient::connect(config(&relay)).await.unwrap();
    assert_eq!(
        controller_without_host
            .join(controller(session(3), [4; 32]))
            .await,
        Err(TransportError::JoinRejected(
            desklink_transport::JoinRejectCode::SessionNotFound
        ))
    );

    let host_client = QuicClient::connect(config(&relay)).await.unwrap();
    host_client.join(host(session(4), [4; 32])).await.unwrap();
    let wrong_auth = QuicClient::connect(config(&relay)).await.unwrap();
    assert_eq!(
        wrong_auth.join(controller(session(4), [5; 32])).await,
        Err(TransportError::JoinRejected(
            desklink_transport::JoinRejectCode::AuthenticationMismatch
        ))
    );
}

#[tokio::test]
async fn malformed_and_oversized_network_inputs_close_cleanly() {
    let relay = spawn_test_relay().await;
    let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(relay.client_config.clone());
    let connection = endpoint
        .connect(relay.address, "localhost")
        .unwrap()
        .await
        .unwrap();

    let (mut join_send, mut join_recv) = connection.open_bi().await.unwrap();
    join_send.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    join_send.finish().unwrap();
    let mut response = [0; 1];
    let _ = join_recv.read_exact(&mut response).await;
    assert_ne!(response[0], 0);
}

#[tokio::test]
async fn relay_rejects_oversized_reliable_and_datagram_inputs_at_the_boundary() {
    let relay = spawn_test_relay().await;
    let client = QuicClient::connect(config(&relay)).await.unwrap();
    client.join(host(session(9), [4; 32])).await.unwrap();

    assert!(matches!(
        client
            .send_control(vec![0; MAX_RELIABLE_MESSAGE_BYTES + 1])
            .await,
        Err(TransportError::MessageTooLarge { maximum, .. }) if maximum == MAX_RELIABLE_MESSAGE_BYTES
    ));
    assert!(matches!(
        client
            .send_cursor_datagram(vec![0; MAX_DATAGRAM_BYTES + 1])
            .await,
        Err(TransportError::MessageTooLarge { maximum, .. }) if maximum == MAX_DATAGRAM_BYTES
    ));
}

#[tokio::test]
async fn malformed_reliable_stream_is_closed_without_allocating_an_oversized_message() {
    let relay = spawn_test_relay().await;
    let (_host_endpoint, host_connection) = raw_join(&relay, host(session(10), [4; 32])).await;
    let controller_client = QuicClient::connect(config(&relay)).await.unwrap();
    controller_client
        .join(controller(session(10), [4; 32]))
        .await
        .unwrap();

    let (mut send, _receive) = host_connection.open_bi().await.unwrap();
    send.write_all(&[1]).await.unwrap();
    send.write_all(&((MAX_RELIABLE_MESSAGE_BYTES as u32) + 1).to_be_bytes())
        .await
        .unwrap();
    send.finish().unwrap();
    tokio::time::timeout(Duration::from_secs(2), host_connection.closed())
        .await
        .expect("relay did not close malformed stream");
}

#[tokio::test]
async fn malformed_datagram_channel_is_closed_without_payload_inspection() {
    let relay = spawn_test_relay().await;
    let (_host_endpoint, host_connection) = raw_join(&relay, host(session(11), [4; 32])).await;
    let controller_client = QuicClient::connect(config(&relay)).await.unwrap();
    controller_client
        .join(controller(session(11), [4; 32]))
        .await
        .unwrap();

    host_connection
        .send_datagram(bytes::Bytes::from_static(&[99, 0]))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), host_connection.closed())
        .await
        .expect("relay did not close malformed datagram");
}
