use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use desklink_crypto::SessionId;
use desklink_protocol::DeviceRole;
use desklink_transport::{
    DEAD_TIMEOUT, JOIN_ENVELOPE_BYTES, JOIN_ENVELOPE_V2_BYTES, JoinRejectCode, MAX_DATAGRAM_BYTES,
    MAX_RELIABLE_MESSAGE_BYTES, QuicClient, QuicClientConfig, RelayJoin, TransportError,
    TransportEvent, decode_relay_join,
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

async fn spawn_stalled_control_relay() -> MockRelay {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_der = certificate.cert.der().to_vec();
    let key_der = certificate.key_pair.serialize_der();
    let mut server_config = ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der.clone())],
        PrivateKeyDer::Pkcs8(key_der.into()),
    )
    .unwrap();
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(quinn::VarInt::from_u32(1));
    server_config.transport_config(Arc::new(transport));

    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(CertificateDer::from(certificate_der.clone()))
        .unwrap();
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
            return;
        }
        let mut join_bytes = vec![0; JOIN_ENVELOPE_BYTES];
        if join_recv.read_exact(&mut join_bytes).await.is_err()
            || decode_relay_join(&join_bytes).is_err()
        {
            return;
        }
        let _ = join_send.write_all(&[0]).await;
        let _ = join_send.finish();

        let mut held_control_streams = Vec::new();
        loop {
            tokio::select! {
                accepted = connection.accept_bi() => {
                    let Ok((_send, mut receive)) = accepted else { break; };
                    let mut channel = [0; 1];
                    if receive.read_exact(&mut channel).await.is_err() {
                        continue;
                    }
                    if channel[0] == 1 {
                        held_control_streams.push(receive);
                        continue;
                    }
                    if channel[0] != 2 {
                        continue;
                    }
                    let connection = connection.clone();
                    tokio::spawn(async move {
                        let mut length = [0; 4];
                        receive.read_exact(&mut length).await.unwrap();
                        let length = u32::from_be_bytes(length) as usize;
                        let mut message = vec![0; length];
                        receive.read_exact(&mut message).await.unwrap();
                        let Ok((mut send, _receive)) = connection.open_bi().await else {
                            return;
                        };
                        if send.write_all(&[2]).await.is_err()
                            || send.write_all(&(length as u32).to_be_bytes()).await.is_err()
                            || send.write_all(&message).await.is_err()
                        {
                            return;
                        }
                        let _ = send.finish();
                    });
                }
                _ = connection.closed() => break,
            }
        }
        drop(held_control_streams);
    });

    MockRelay {
        address,
        client_config,
        task,
    }
}

async fn spawn_malformed_peer_relay() -> MockRelay {
    spawn_peer_relay_with_reliable_prefix(vec![1, 0, 0, 0, 1]).await
}

async fn spawn_peer_relay_with_reliable_prefix(prefix: Vec<u8>) -> MockRelay {
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
            return;
        }
        let mut join_bytes = vec![0; JOIN_ENVELOPE_BYTES];
        if join_recv.read_exact(&mut join_bytes).await.is_err()
            || decode_relay_join(&join_bytes).is_err()
        {
            return;
        }
        let _ = join_send.write_all(&[0]).await;
        let _ = join_send.finish();

        let Ok((mut send, _receive)) = connection.open_bi().await else {
            return;
        };
        let _ = send.write_all(&prefix).await;
        let _ = send.finish();
        tokio::time::sleep(Duration::from_secs(1)).await;
    });

    MockRelay {
        address,
        client_config,
        task,
    }
}

async fn spawn_video_flood_relay() -> MockRelay {
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
            return;
        }
        let mut join_bytes = vec![0; JOIN_ENVELOPE_BYTES];
        if join_recv.read_exact(&mut join_bytes).await.is_err()
            || decode_relay_join(&join_bytes).is_err()
        {
            return;
        }
        let _ = join_send.write_all(&[0]).await;
        let _ = join_send.finish();

        let Ok((_send, mut receive)) = connection.accept_bi().await else {
            return;
        };
        let mut channel = [0; 1];
        if receive.read_exact(&mut channel).await.is_err() || channel[0] != 2 {
            return;
        }
        if receive.read_exact(&mut length).await.is_err() {
            return;
        }
        let length = u32::from_be_bytes(length) as usize;
        let mut message = vec![0; length];
        if receive.read_exact(&mut message).await.is_err() {
            return;
        }
        for _ in 0..256 {
            let _ = connection.send_datagram(Bytes::from_static(&[4, 0xaa]));
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;

        let Ok((mut send, _receive)) = connection.open_bi().await else {
            return;
        };
        let _ = send.write_all(&channel).await;
        let _ = send.write_all(&(length as u32).to_be_bytes()).await;
        let _ = send.write_all(&message).await;
        let _ = send.finish();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    MockRelay {
        address,
        client_config,
        task,
    }
}

async fn spawn_input_flood_with_control_relay() -> MockRelay {
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
            return;
        }
        let mut join_bytes = vec![0; JOIN_ENVELOPE_BYTES];
        if join_recv.read_exact(&mut join_bytes).await.is_err()
            || decode_relay_join(&join_bytes).is_err()
        {
            return;
        }
        let _ = join_send.write_all(&[0]).await;
        let _ = join_send.finish();

        let Ok((mut input_send, _receive)) = connection.open_bi().await else {
            return;
        };
        let input = [0xaa];
        let _ = input_send.write_all(&[2]).await;
        for sequence in 0..256u16 {
            let _ = input_send.write_all(&1u32.to_be_bytes()).await;
            let _ = input_send.write_all(&[sequence as u8]).await;
        }
        let _ = input_send.finish();

        let Ok((mut control_send, _receive)) = connection.open_bi().await else {
            return;
        };
        let _ = control_send.write_all(&[1]).await;
        let _ = control_send.write_all(&1u32.to_be_bytes()).await;
        let _ = control_send.write_all(&input).await;
        let _ = control_send.finish();
        tokio::time::sleep(Duration::from_millis(100)).await;
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

#[test]
fn relay_join_v2_round_trips_participant_identity_and_keeps_v1_compatible() {
    let session_id = SessionId::from_bytes([8; 16]);
    let legacy = RelayJoin::host(session_id, [4; 32]);
    let legacy_bytes = legacy.encode();
    assert_eq!(legacy_bytes.len(), JOIN_ENVELOPE_BYTES);
    assert_eq!(decode_relay_join(&legacy_bytes).unwrap(), legacy);

    let resumable = RelayJoin::controller_with_participant(session_id, [4; 32], [9; 16]);
    let resumable_bytes = resumable.encode();
    assert_eq!(resumable_bytes.len(), JOIN_ENVELOPE_V2_BYTES);
    assert_eq!(decode_relay_join(&resumable_bytes).unwrap(), resumable);
}

#[tokio::test]
async fn explicit_lan_mode_accepts_a_self_signed_local_relay() {
    let relay = spawn_mock_relay(false).await;
    let config = QuicClientConfig::new_lan(relay.address, "desklink-lan").unwrap();
    let client = QuicClient::connect(config).await.unwrap();
    client.join(join(DeviceRole::Host)).await.unwrap();
    client.send_control(vec![1, 2, 3]).await.unwrap();
    assert_eq!(
        client.next_event().await.unwrap(),
        TransportEvent::Control(vec![1, 2, 3])
    );
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
async fn stalled_control_channel_does_not_block_input_channel() {
    let relay = spawn_stalled_control_relay().await;
    let client = Arc::new(QuicClient::connect(config(&relay)).await.unwrap());
    client
        .join(RelayJoin::new(
            SessionId::from_bytes([7; 16]),
            DeviceRole::Controller,
            [3; 32],
        ))
        .await
        .unwrap();

    let mut stalled_control = tokio::spawn({
        let client = Arc::clone(&client);
        async move {
            client
                .send_control(vec![0; MAX_RELIABLE_MESSAGE_BYTES])
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut stalled_control)
            .await
            .is_err(),
        "control send should remain blocked by the stalled peer"
    );

    let input = tokio::time::timeout(Duration::from_millis(250), client.send_input(vec![1, 2, 3]))
        .await
        .expect("input channel made no progress while control was stalled");
    input.unwrap();
    stalled_control.abort();
    relay.task.abort();
}

#[tokio::test]
async fn client_rejects_invalid_timeout_overrides_before_connecting() {
    let relay = spawn_mock_relay(false).await;
    let invalid_zero = config(&relay).with_timeouts(Duration::ZERO, DEAD_TIMEOUT);
    assert!(matches!(
        QuicClient::connect(invalid_zero).await,
        Err(TransportError::InvalidConfig(_))
    ));

    let invalid_order = config(&relay).with_timeouts(DEAD_TIMEOUT, DEAD_TIMEOUT);
    assert!(matches!(
        QuicClient::connect(invalid_order).await,
        Err(TransportError::InvalidConfig(_))
    ));
    relay.task.abort();
}

#[tokio::test]
async fn malformed_peer_stream_emits_closed_without_panicking() {
    let relay = spawn_malformed_peer_relay().await;
    let client = QuicClient::connect(config(&relay)).await.unwrap();
    client.join(join(DeviceRole::Host)).await.unwrap();

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), client.next_event())
            .await
            .unwrap(),
        Ok(TransportEvent::Closed {
            reason: "malformed reliable message".to_owned()
        })
    );
}

#[tokio::test]
async fn video_datagram_flood_does_not_block_input_delivery() {
    let relay = spawn_video_flood_relay().await;
    let client = QuicClient::connect(config(&relay)).await.unwrap();
    client.join(join(DeviceRole::Host)).await.unwrap();
    let input = vec![7, 6, 5, 4];
    client.send_input(input.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut saw_input = false;
    for _ in 0..256 {
        let event = tokio::time::timeout(Duration::from_secs(2), client.next_event())
            .await
            .unwrap()
            .unwrap();
        if event == TransportEvent::Input(input.clone()) {
            saw_input = true;
            break;
        }
    }
    assert!(saw_input, "input was blocked behind the video flood");
}

#[tokio::test]
async fn dedicated_input_receiver_is_not_blocked_by_video_flood() {
    let relay = spawn_video_flood_relay().await;
    let client = QuicClient::connect(config(&relay)).await.unwrap();
    client.join(join(DeviceRole::Host)).await.unwrap();
    let input = vec![8, 7, 6, 5];
    client.send_input(input.clone()).await.unwrap();

    assert_eq!(
        tokio::time::timeout(Duration::from_millis(250), client.next_input())
            .await
            .unwrap()
            .unwrap(),
        input
    );
}

#[tokio::test]
async fn input_flood_does_not_starve_control_delivery() {
    let relay = spawn_input_flood_with_control_relay().await;
    let client = QuicClient::connect(config(&relay)).await.unwrap();
    client.join(join(DeviceRole::Host)).await.unwrap();

    let mut saw_control = false;
    for _ in 0..8 {
        let event = tokio::time::timeout(Duration::from_secs(2), client.next_event())
            .await
            .unwrap()
            .unwrap();
        if event == TransportEvent::Control(vec![0xaa]) {
            saw_control = true;
            break;
        }
    }
    assert!(saw_control, "control was starved by the input flood");
}

#[tokio::test]
async fn empty_reliable_stream_emits_a_closed_event() {
    let relay = spawn_peer_relay_with_reliable_prefix(Vec::new()).await;
    let client = QuicClient::connect(config(&relay)).await.unwrap();
    client.join(join(DeviceRole::Host)).await.unwrap();

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), client.next_event())
            .await
            .unwrap(),
        Ok(TransportEvent::Closed {
            reason: "empty reliable stream".to_owned()
        })
    );
}

#[tokio::test]
async fn channel_only_reliable_stream_emits_a_closed_event() {
    let relay = spawn_peer_relay_with_reliable_prefix(vec![1]).await;
    let client = QuicClient::connect(config(&relay)).await.unwrap();
    client.join(join(DeviceRole::Host)).await.unwrap();

    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), client.next_event())
            .await
            .unwrap(),
        Ok(TransportEvent::Closed {
            reason: "malformed reliable message".to_owned()
        })
    );
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
