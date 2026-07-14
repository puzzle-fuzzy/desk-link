use std::{sync::Arc, time::Duration};

use desklink_crypto::SessionId;
use desklink_protocol::DeviceRole;
use desklink_transport::{
    JOIN_ENVELOPE_BYTES, JoinRejectCode, MAX_DATAGRAM_BYTES, MAX_RELIABLE_MESSAGE_BYTES,
    QuicClient, QuicClientConfig, RelayJoin, TransportError, TransportEvent,
};
use quinn::{Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

struct MockRelay {
    address: std::net::SocketAddr,
    client_config: quinn::ClientConfig,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn spawn_mock_relay(reject_join: bool) -> MockRelay {
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
    let endpoint = Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let address = endpoint.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let Some(connecting) = endpoint.accept().await else {
            return;
        };
        let Ok(connection) = connecting.await else {
            return;
        };
        let Ok((mut join_send, mut join_recv)) = connection.accept_bi().await else {
            return;
        };
        let mut length = [0; 4];
        if join_recv.read_exact(&mut length).await.is_err()
            || u32::from_be_bytes(length) as usize != JOIN_ENVELOPE_BYTES
        {
            let _ = join_send
                .write_all(&[JoinRejectCode::Malformed as u8])
                .await;
            let _ = join_send.finish();
            return;
        }
        let mut join = vec![0; JOIN_ENVELOPE_BYTES];
        if join_recv.read_exact(&mut join).await.is_err() {
            return;
        }
        if reject_join {
            let _ = join_send
                .write_all(&[JoinRejectCode::Malformed as u8])
                .await;
        } else {
            let _ = join_send.write_all(&[0]).await;
        }
        let _ = join_send.finish();
        if reject_join {
            tokio::time::sleep(Duration::from_millis(20)).await;
            return;
        }

        loop {
            tokio::select! {
                accepted = connection.accept_bi() => {
                    let Ok((_send, recv)) = accepted else { return; };
                    let connection = connection.clone();
                    tokio::spawn(async move {
                        let mut recv = recv;
                        let mut channel = [0; 1];
                        if recv.read_exact(&mut channel).await.is_err() {
                            return;
                        }
                        loop {
                            let mut length = [0; 4];
                            if recv.read_exact(&mut length).await.is_err() {
                                return;
                            }
                            let length = u32::from_be_bytes(length) as usize;
                            if length > MAX_RELIABLE_MESSAGE_BYTES {
                                return;
                            }
                            let mut message = vec![0; length];
                            if recv.read_exact(&mut message).await.is_err() {
                                return;
                            }
                            let Ok((mut send, _recv)) = connection.open_bi().await else {
                                return;
                            };
                            if send.write_all(&channel).await.is_err()
                                || send.write_all(&(length as u32).to_be_bytes()).await.is_err()
                                || send.write_all(&message).await.is_err()
                            {
                                return;
                            }
                            let _ = send.finish();
                        }
                    });
                }
                datagram = connection.read_datagram() => {
                    let Ok(datagram) = datagram else { return; };
                    let _ = connection.send_datagram(datagram);
                }
            }
        }
    });

    MockRelay {
        address,
        client_config,
        task,
    }
}

fn config(relay: &MockRelay) -> QuicClientConfig {
    QuicClientConfig::with_client_config(
        relay.address,
        "localhost".to_owned(),
        relay.client_config.clone(),
    )
}

fn join(role: DeviceRole) -> RelayJoin {
    RelayJoin::new(SessionId::from_bytes([8; 16]), role, [4; 32])
}

async fn next_n(client: &QuicClient, count: usize) -> Vec<TransportEvent> {
    let mut events = Vec::with_capacity(count);
    for _ in 0..count {
        events.push(
            tokio::time::timeout(Duration::from_secs(2), client.next_event())
                .await
                .expect("event timeout")
                .expect("event"),
        );
    }
    events
}

#[tokio::test]
async fn localhost_client_keeps_reliable_channels_separate_and_forwards_datagrams() {
    let relay = spawn_mock_relay(false).await;
    let client = QuicClient::connect(config(&relay)).await.unwrap();
    client.join(join(DeviceRole::Host)).await.unwrap();

    client.send_control(vec![0, 1, 2, 255]).await.unwrap();
    client.send_input(vec![3, 4, 5, 254]).await.unwrap();
    client.send_video_config(vec![6, 7, 8, 253]).await.unwrap();
    client
        .send_video_datagram(vec![9, 10, 11, 252])
        .await
        .unwrap();
    client
        .send_cursor_datagram(vec![12, 13, 14, 251])
        .await
        .unwrap();

    let events = next_n(&client, 5).await;
    assert!(events.contains(&TransportEvent::Control(vec![0, 1, 2, 255])));
    assert!(events.contains(&TransportEvent::Input(vec![3, 4, 5, 254])));
    assert!(events.contains(&TransportEvent::VideoConfig(vec![6, 7, 8, 253])));
    assert!(events.contains(&TransportEvent::VideoDatagram(vec![9, 10, 11, 252])));
    assert!(events.contains(&TransportEvent::CursorDatagram(vec![12, 13, 14, 251])));
}

#[tokio::test]
async fn client_rejects_oversized_reliable_and_datagram_messages() {
    let relay = spawn_mock_relay(false).await;
    let client = QuicClient::connect(config(&relay)).await.unwrap();
    client.join(join(DeviceRole::Host)).await.unwrap();

    assert!(matches!(
        client
            .send_control(vec![0; MAX_RELIABLE_MESSAGE_BYTES + 1])
            .await,
        Err(TransportError::MessageTooLarge { maximum, .. }) if maximum == MAX_RELIABLE_MESSAGE_BYTES
    ));
    assert!(matches!(
        client
            .send_video_datagram(vec![0; MAX_DATAGRAM_BYTES + 1])
            .await,
        Err(TransportError::MessageTooLarge { maximum, .. }) if maximum == MAX_DATAGRAM_BYTES
    ));
}

#[tokio::test]
async fn malformed_join_ack_is_reported_without_panicking() {
    let relay = spawn_mock_relay(true).await;
    let client = QuicClient::connect(config(&relay)).await.unwrap();

    assert_eq!(
        client.join(join(DeviceRole::Host)).await,
        Err(TransportError::JoinRejected(JoinRejectCode::Malformed))
    );
}
