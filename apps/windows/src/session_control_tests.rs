use super::*;
use desklink_crypto::SessionId;
use desklink_ffi::{ControllerError, ControllerRuntime};
use desklink_relay::{RelayConfig, RelayServer};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

fn local_tls() -> (quinn::ServerConfig, quinn::ClientConfig) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let der = certificate.cert.der().to_vec();
    let server = quinn::ServerConfig::with_single_cert(
        vec![CertificateDer::from(der.clone())],
        PrivateKeyDer::Pkcs8(certificate.key_pair.serialize_der().into()),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(der)).unwrap();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client = quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap();
    (server, quinn::ClientConfig::new(Arc::new(client)))
}

struct ExpectedController(VerifyingKey);

impl ControllerAuthorizer for ExpectedController {
    fn pinned_verify_key(&self) -> Option<VerifyingKey> {
        Some(self.0)
    }

    fn authorize(&self, identity: PeerIdentity) -> Result<ControllerAuthorization, String> {
        Ok(if identity.verify_key() == self.0 {
            ControllerAuthorization::Authorized
        } else {
            ControllerAuthorization::Unknown
        })
    }
}

/// Exercise real relay framing, Noise authentication, production Controller
/// negotiation/event handling, and the production terminal notification helper.
/// The Host side advertises synthetic dimensions; no capture, audio, clipboard,
/// files, native windows, or actual input injection is started by this test.
#[tokio::test]
async fn synthetic_peer_observes_terminal_end_then_same_host_accepts_a_new_connection() {
    let (server_tls, client_tls) = local_tls();
    let relay = Arc::new(
        RelayServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            server_tls,
            RelayConfig::default(),
        )
        .await
        .unwrap(),
    );
    let address = relay.local_addr().unwrap();
    let relay_worker = relay.clone();
    let relay_task = tokio::spawn(async move {
        relay_worker.run().await.unwrap();
    });
    let config = QuicClientConfig::with_client_config(address, "localhost", client_tls);
    let host_identity = DeviceIdentity::generate(&mut OsRng);
    let host_id = host_identity.device_id;
    let host_key = host_identity.verify_key();
    let host_secret = host_identity.with_secret_key_bytes(|secret| Zeroizing::new(*secret));
    let controller_identity = DeviceIdentity::generate(&mut OsRng);
    let controller_id = controller_identity.device_id;
    let controller_key = controller_identity.verify_key();
    let controller_secret =
        controller_identity.with_secret_key_bytes(|secret| Zeroizing::new(*secret));
    let session_id = SessionId::from_bytes([0x48; 16]);
    let authentication = [0x64; 32];
    let host = QuicClient::connect(config.clone()).await.unwrap();
    host.join(RelayJoin::host_with_participant(
        session_id,
        authentication,
        host_id,
    ))
    .await
    .unwrap();
    let control = HostSessionControl::default();
    let host_control = control.clone();
    let (ready_tx, mut ready_rx) = mpsc::channel(2);
    let host_task = tokio::spawn(async move {
        for stream_id in 1..=3 {
            let identity = DeviceIdentity::from_secret_key(host_id, &host_secret);
            let (secure, peer_generation) =
                perform_noise_handshake(&host, identity, &ExpectedController(controller_key))
                    .await
                    .unwrap();
            let secure = Arc::new(AsyncMutex::new(secure));
            receive_controller_capabilities(
                &host,
                &secure,
                peer_generation,
                Arc::new(AtomicBool::new(false)),
                stream_id,
            )
            .await
            .unwrap();
            send_host_capabilities(
                &host,
                &secure,
                peer_generation,
                16,
                16,
                H264Profile::Main,
                &[RemoteDisplay {
                    id: 1,
                    width: 16,
                    height: 16,
                    primary: true,
                }],
                1,
            )
            .await
            .unwrap();
            let active = host_control.activate(stream_id);
            ready_tx.send(stream_id).await.unwrap();
            let request = host_control.next_end(stream_id).await;
            drop(active);
            let notified = notify_local_control_ended(&host, &secure, peer_generation).await;
            // The production supervisor retains this relay client and resets
            // its reliable channels before admitting the next stream.
            if notified {
                host.reset_reliable_channels().await;
            }
            request.complete(notified);
            assert_eq!(notified, stream_id != 3);
        }
    });

    for expected_stream in 1..=2 {
        let peer = QuicClient::connect(config.clone()).await.unwrap();
        peer.join(RelayJoin::controller_with_participant(
            session_id,
            authentication,
            controller_id,
        ))
        .await
        .unwrap();
        let identity = DeviceIdentity::from_secret_key(controller_id, &controller_secret);
        let mut controller = tokio::time::timeout(
            Duration::from_secs(5),
            ControllerRuntime::connect_for_platform(peer, identity, host_key, Platform::MacOS),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(ready_rx.recv().await, Some(expected_stream));
        let completion = control.request_end().unwrap();
        let failure = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match controller.next_event().await {
                    Ok(_) => {}
                    Err(error) => break error,
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            failure,
            ControllerError::AccessDenied(AccessDenialReason::HostUnavailable)
        ));
        drop(controller);
        assert!(
            tokio::time::timeout(Duration::from_secs(3), completion)
                .await
                .unwrap()
                .unwrap()
        );
    }
    // A peer that never consumes the terminal state is not reported as a
    // successful end. The native command uses this false acknowledgement to
    // persist a local pause instead of admitting an automatic reconnection.
    let peer = QuicClient::connect(config.clone()).await.unwrap();
    peer.join(RelayJoin::controller_with_participant(
        session_id,
        authentication,
        controller_id,
    ))
    .await
    .unwrap();
    let identity = DeviceIdentity::from_secret_key(controller_id, &controller_secret);
    let unresponsive =
        ControllerRuntime::connect_for_platform(peer, identity, host_key, Platform::MacOS)
            .await
            .unwrap();
    assert_eq!(ready_rx.recv().await, Some(3));
    let completion = control.request_end().unwrap();
    assert!(
        !tokio::time::timeout(Duration::from_secs(3), completion)
            .await
            .unwrap()
            .unwrap()
    );
    assert!(control.request_end().is_err());
    drop(unresponsive);
    host_task.await.unwrap();
    relay.close();
    relay_task.await.unwrap();
}

#[test]
fn acknowledged_end_rearms_persistent_host_but_unconfirmed_end_does_not() {
    assert!(host_error_rearms_on_connected_relay(
        &HostRuntimeError::LocalControlEnded
    ));
    assert!(host_error_is_retryable_for_session(
        &HostRuntimeError::LocalControlEnded,
        true
    ));
    assert!(!host_error_is_retryable_for_session(
        &HostRuntimeError::LocalControlEnded,
        false
    ));
    assert!(!host_error_rearms_on_connected_relay(
        &HostRuntimeError::LocalControlEndUnconfirmed
    ));
    assert!(!host_error_is_retryable_for_session(
        &HostRuntimeError::LocalControlEndUnconfirmed,
        true
    ));
}
