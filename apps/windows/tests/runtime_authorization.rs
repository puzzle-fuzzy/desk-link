#[cfg(windows)]
mod windows {
    use std::{
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use apps_windows::{
        runtime::{ControllerAuthorizer, HostRuntime, HostRuntimeError},
        trusted::{
            LocalControllerApproval, WindowsControllerAuthorizer, WindowsPairingAuthorizer,
            WindowsTrustedControllerStore,
        },
        window::{PairingApprovalGate, PendingController},
    };
    use desklink_crypto::{
        DeviceIdentity, NoiseInitiator, NoiseResponder, PairingInvite, PeerIdentity, SecureLane,
        SecureRole, SecureSession, SessionId,
    };
    use desklink_protocol::{
        ControlMessage, NoiseHandshake, NoiseHandshakeStep, PROTOCOL_VERSION, decode_control,
        decode_noise_handshake, encode_noise_handshake,
    };
    use desklink_relay::{RelayConfig, RelayServer};
    use desklink_transport::{
        JoinRejectCode, QuicClient, QuicClientConfig, RelayJoin, TransportError,
    };
    use quinn::ServerConfig;
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
            RelayServer::bind(
                "127.0.0.1:0".parse().unwrap(),
                server_config,
                RelayConfig::default(),
            )
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

    fn config(relay: &TestRelay) -> QuicClientConfig {
        QuicClientConfig::with_client_config(
            relay.address,
            "localhost",
            relay.client_config.clone(),
        )
    }

    async fn controller_handshake(
        client: &QuicClient,
        identity: DeviceIdentity,
        expected_host: ed25519_dalek::VerifyingKey,
    ) -> SecureSession {
        let (mut initiator, hello) = NoiseInitiator::start(identity, expected_host).unwrap();
        client
            .send_control(
                encode_noise_handshake(&NoiseHandshake {
                    protocol_version: PROTOCOL_VERSION,
                    step: NoiseHandshakeStep::InitiatorHello,
                    payload: hello,
                })
                .unwrap(),
            )
            .await
            .unwrap();
        let response = decode_noise_handshake(&client.next_control().await.unwrap()).unwrap();
        assert_eq!(response.step, NoiseHandshakeStep::ResponderHello);
        let finish = initiator.receive(&response.payload).unwrap();
        client
            .send_control(
                encode_noise_handshake(&NoiseHandshake {
                    protocol_version: PROTOCOL_VERSION,
                    step: NoiseHandshakeStep::InitiatorFinish,
                    payload: finish,
                })
                .unwrap(),
            )
            .await
            .unwrap();
        initiator
            .finish()
            .unwrap()
            .into_secure_session(SecureRole::Initiator)
    }

    async fn authorization_error(
        host_identity: DeviceIdentity,
        controller_identity: DeviceIdentity,
        session_id: SessionId,
        authentication: [u8; 32],
        authorizer: Arc<dyn ControllerAuthorizer>,
    ) -> HostRuntimeError {
        let relay = spawn_test_relay().await;
        let host = QuicClient::connect(config(&relay)).await.unwrap();
        let controller = QuicClient::connect(config(&relay)).await.unwrap();
        host.join(RelayJoin::host(session_id, authentication))
            .await
            .unwrap();
        controller
            .join(RelayJoin::controller(session_id, authentication))
            .await
            .unwrap();
        let expected_host = host_identity.verify_key();
        let runtime = HostRuntime::with_authorizer(host, 1, host_identity, authorizer).unwrap();
        let host_task = tokio::spawn(async move { runtime.run().await });

        let mut secure =
            controller_handshake(&controller, controller_identity, expected_host).await;
        let denial = controller.next_control().await.unwrap();
        let denial = secure.open(SecureLane::Control, &denial).unwrap();
        assert!(matches!(
            decode_control(&denial).unwrap(),
            ControlMessage::AccessDenied { .. }
        ));
        drop(controller);
        tokio::time::timeout(Duration::from_secs(7), host_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
    }

    fn authenticated_peer(device_id: [u8; 16], secret: [u8; 32]) -> PeerIdentity {
        let controller = DeviceIdentity::from_secret_key(device_id, &secret);
        let host = DeviceIdentity::from_secret_key([90; 16], &[91; 32]);
        let host_verify_key = host.verify_key();
        let (mut initiator, message_1) =
            NoiseInitiator::start(controller, host_verify_key).unwrap();
        let (mut responder, message_2) = NoiseResponder::accept_pairing(&message_1, host).unwrap();
        let message_3 = initiator.receive(&message_2).unwrap();
        responder.receive(&message_3).unwrap();
        responder.finish().unwrap().peer_identity()
    }

    fn trust_peer(store: &WindowsTrustedControllerStore, peer: PeerIdentity, now_unix_s: u64) {
        let host = DeviceIdentity::from_secret_key([92; 16], &[93; 32]);
        let mut invite = PairingInvite::new(&host, now_unix_s, 60).unwrap();
        let mut gate = PairingApprovalGate::new();
        let pending = gate.begin(&mut invite, peer, now_unix_s).unwrap();
        let approved = gate.approve(pending.identity(), now_unix_s).unwrap();
        store.trust(approved).unwrap();
    }

    struct FixedApproval(bool);

    impl LocalControllerApproval for FixedApproval {
        fn approve(&self, _pending: PendingController) -> bool {
            self.0
        }
    }

    fn temporary_store(name: &str) -> (std::path::PathBuf, WindowsTrustedControllerStore) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "desklink-runtime-auth-{name}-{}-{unique}",
            std::process::id()
        ));
        let store = WindowsTrustedControllerStore::new(directory.join("controllers.bin"));
        (directory, store)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rejected_pairing_stops_after_authentication_and_before_capture() {
        let (directory, store) = temporary_store("rejected");
        let host = DeviceIdentity::from_secret_key([1; 16], &[2; 32]);
        let controller = DeviceIdentity::from_secret_key([3; 16], &[4; 32]);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let invite = PairingInvite::new(&host, now, 60).unwrap();
        let session_id = invite.session_id();
        let authentication = *invite.relay_authentication();
        let authorizer =
            WindowsPairingAuthorizer::new(store.clone(), invite, Box::new(FixedApproval(false)));

        assert_eq!(
            authorization_error(
                host,
                controller,
                session_id,
                authentication,
                Arc::new(authorizer)
            )
            .await,
            HostRuntimeError::PairingRejected
        );
        assert!(store.list().unwrap().is_empty());
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn expired_pairing_invite_fails_closed_before_local_approval() {
        let (directory, store) = temporary_store("expired");
        let host = DeviceIdentity::from_secret_key([10; 16], &[11; 32]);
        let controller = DeviceIdentity::from_secret_key([12; 16], &[13; 32]);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let invite = PairingInvite::new(&host, now.saturating_sub(2), 1).unwrap();
        let session_id = invite.session_id();
        let authentication = *invite.relay_authentication();
        let authorizer =
            WindowsPairingAuthorizer::new(store.clone(), invite, Box::new(FixedApproval(true)));

        assert_eq!(
            authorization_error(
                host,
                controller,
                session_id,
                authentication,
                Arc::new(authorizer),
            )
            .await,
            HostRuntimeError::PairingExpired
        );
        assert!(store.list().unwrap().is_empty());
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn trusted_device_id_with_replaced_key_is_blocked_before_capture() {
        let (directory, store) = temporary_store("key-replacement");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        trust_peer(&store, authenticated_peer([20; 16], [21; 32]), now);
        let host = DeviceIdentity::from_secret_key([22; 16], &[23; 32]);
        let replacement = DeviceIdentity::from_secret_key([20; 16], &[24; 32]);

        assert_eq!(
            authorization_error(
                host,
                replacement,
                SessionId::from_bytes([25; 16]),
                [26; 32],
                Arc::new(WindowsControllerAuthorizer::new(store.clone()))
            )
            .await,
            HostRuntimeError::ControllerKeyChanged
        );
        assert_eq!(store.list().unwrap().len(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn relay_rejects_a_second_controller_for_the_same_host_session() {
        let relay = spawn_test_relay().await;
        let host = QuicClient::connect(config(&relay)).await.unwrap();
        let first = QuicClient::connect(config(&relay)).await.unwrap();
        let second = QuicClient::connect(config(&relay)).await.unwrap();
        let session = SessionId::from_bytes([30; 16]);
        host.join(RelayJoin::host(session, [31; 32])).await.unwrap();
        first
            .join(RelayJoin::controller(session, [31; 32]))
            .await
            .unwrap();

        assert_eq!(
            second.join(RelayJoin::controller(session, [31; 32])).await,
            Err(TransportError::JoinRejected(
                JoinRejectCode::SessionOccupied
            ))
        );
    }
}
