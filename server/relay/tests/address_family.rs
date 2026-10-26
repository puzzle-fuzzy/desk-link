//! Real loopback relay coverage for both address families. Payloads remain
//! opaque transport fixtures; no protocol input prefix or OS input injection.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use desklink_crypto::SessionId;
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{
    QuicClient, QuicClientConfig, RelayDirectoryLookup, RelayDirectoryRegistration, RelayJoin,
    TransportEvent,
};
use quinn::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const TEST_TIMEOUT: Duration = Duration::from_secs(3);

struct TestRelay {
    server: Arc<RelayServer>,
    client_config: quinn::ClientConfig,
    task: tokio::task::JoinHandle<()>,
}

impl TestRelay {
    async fn new(address: SocketAddr) -> Self {
        assert!(address.ip().is_loopback());
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
        let client_config = quinn::ClientConfig::new(Arc::new(crypto));
        let server = Arc::new(
            RelayServer::bind(address, server_config, RelayConfig::default())
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

    async fn connect(&self) -> QuicClient {
        let address = self.server.local_addr().unwrap();
        let client = tokio::time::timeout(
            TEST_TIMEOUT,
            QuicClient::connect(QuicClientConfig::with_client_config(
                address,
                "localhost",
                self.client_config.clone(),
            )),
        )
        .await
        .expect("loopback connection timed out")
        .expect("relay address family must be supported by the client bind");
        assert_eq!(client.remote_address(), address);
        client
    }
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.server.close();
        self.task.abort();
    }
}

async fn next(client: &QuicClient) -> TransportEvent {
    tokio::time::timeout(TEST_TIMEOUT, client.next_event())
        .await
        .expect("opaque loopback payload timed out")
        .unwrap()
}

async fn directory_and_media_round_trip(address: SocketAddr) {
    let relay = TestRelay::new(address).await;
    let session = SessionId::from_bytes([6; 16]);
    let invitation = b"synthetic address-family invitation".to_vec();
    let access_code = *b"AB2DEF3G";
    let device_id = 123_456_789_012;
    let host = relay.connect().await;
    host.join(
        RelayJoin::host_with_participant(session, [7; 32], [1; 16])
            .with_directory_registration(
                RelayDirectoryRegistration::new(device_id, access_code, invitation.clone(), 60)
                    .unwrap(),
            )
            .unwrap(),
    )
    .await
    .unwrap();

    let directory = relay.connect().await;
    assert_eq!(
        directory
            .lookup_directory(RelayDirectoryLookup::new(device_id, access_code).unwrap())
            .await
            .unwrap(),
        invitation,
    );
    directory.close(b"address-family directory query complete");

    let controller = relay.connect().await;
    controller
        .join(RelayJoin::controller_with_participant(
            session, [7; 32], [2; 16],
        ))
        .await
        .unwrap();

    let config = vec![0, 11, 0, 12];
    host.send_video_config(config.clone()).await.unwrap();
    assert_eq!(next(&controller).await, TransportEvent::VideoConfig(config));
    let video = vec![0, 21, 0, 22];
    host.send_video_datagram(video.clone()).await.unwrap();
    assert_eq!(
        next(&controller).await,
        TransportEvent::VideoDatagram(video)
    );
    let audio = vec![0, 31, 0, 32];
    host.send_audio_datagram(audio.clone()).await.unwrap();
    assert_eq!(
        next(&controller).await,
        TransportEvent::AudioDatagram(audio)
    );
    let input = vec![0, 41, 0, 42];
    controller.send_input(input.clone()).await.unwrap();
    assert_eq!(next(&host).await, TransportEvent::Input(input));

    host.close(b"address-family fixture complete");
    controller.close(b"address-family fixture complete");
    tokio::time::timeout(TEST_TIMEOUT, async {
        while relay.server.capacity_snapshot().accepted_connections != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("address-family connections did not release admission");
}

#[tokio::test]
async fn ipv4_relay_directory_input_and_media_round_trip() {
    directory_and_media_round_trip("127.0.0.1:0".parse().unwrap()).await;
}

#[tokio::test]
async fn ipv6_relay_directory_input_and_media_round_trip() {
    directory_and_media_round_trip("[::1]:0".parse().unwrap()).await;
}
