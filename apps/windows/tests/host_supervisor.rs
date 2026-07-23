#[cfg(windows)]
mod windows {
    use std::{
        net::SocketAddr,
        sync::{Arc, Mutex, OnceLock},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use apps_windows::runtime::{
        ControllerAuthorization, ControllerAuthorizer, HostLifecycleEvent, HostRuntimeError,
        HostSupervisor, HostSupervisorError,
    };
    use desklink_crypto::{DeviceIdentity, PairingInvite, PeerIdentity, SessionId};
    use desklink_ffi::{ControllerEvent, ControllerRuntime};
    use desklink_relay::{RelayConfig, RelayServer};
    use desklink_session::ReconnectPolicy;
    use desklink_transport::{
        JoinRejectCode, QuicClient, QuicClientConfig, RelayDirectoryLookup,
        RelayDirectoryRegistration, RelayJoin, TransportError,
    };
    use ed25519_dalek::VerifyingKey;
    use quinn::ServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    struct TestTls {
        certificate_der: Vec<u8>,
        key_der: Vec<u8>,
        client_config: quinn::ClientConfig,
    }

    impl TestTls {
        fn new() -> Self {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let certificate =
                rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
            let certificate_der = certificate.cert.der().to_vec();
            let key_der = certificate.key_pair.serialize_der();
            let mut roots = rustls::RootCertStore::empty();
            roots
                .add(CertificateDer::from(certificate_der.clone()))
                .unwrap();
            let client_tls = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let client_crypto =
                quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap();
            Self {
                certificate_der,
                key_der,
                client_config: quinn::ClientConfig::new(Arc::new(client_crypto)),
            }
        }

        fn server_config(&self) -> ServerConfig {
            ServerConfig::with_single_cert(
                vec![CertificateDer::from(self.certificate_der.clone())],
                PrivateKeyDer::Pkcs8(self.key_der.clone().into()),
            )
            .unwrap()
        }

        fn client_config(&self, address: SocketAddr) -> QuicClientConfig {
            QuicClientConfig::with_client_config(address, "localhost", self.client_config.clone())
                .try_with_timeouts(Duration::from_millis(50), Duration::from_millis(500))
                .unwrap()
        }
    }

    struct TestRelay {
        address: SocketAddr,
        relay: Arc<RelayServer>,
        task: tokio::task::JoinHandle<()>,
    }

    impl TestRelay {
        async fn spawn(address: SocketAddr, tls: &TestTls) -> Self {
            let mut attempts = 0;
            let relay = loop {
                attempts += 1;
                match RelayServer::bind(
                    address,
                    tls.server_config(),
                    RelayConfig {
                        keep_alive: Duration::from_millis(50),
                        dead_timeout: Duration::from_millis(500),
                        sweep_interval: Duration::from_millis(50),
                        ..RelayConfig::default()
                    },
                )
                .await
                {
                    Ok(relay) => break Arc::new(relay),
                    Err(error) if address.port() != 0 && attempts < 100 => {
                        let _ = error;
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                    Err(error) => panic!("test relay bind failed: {error}"),
                }
            };
            let address = relay.local_addr().unwrap();
            let task_relay = relay.clone();
            let task = tokio::spawn(async move {
                let _ = task_relay.run().await;
            });
            Self {
                address,
                relay,
                task,
            }
        }

        async fn shutdown(self) {
            self.relay.close();
            let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
        }
    }

    struct ExpectedController(VerifyingKey);

    fn host_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

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

    fn supervisor(
        transport: QuicClientConfig,
        session_id: SessionId,
        authentication: [u8; 32],
        host: DeviceIdentity,
        controller_key: VerifyingKey,
        expires_at_unix_s: Option<u64>,
    ) -> HostSupervisor {
        HostSupervisor::new(
            transport,
            session_id,
            authentication,
            1,
            host,
            Arc::new(ExpectedController(controller_key)),
            expires_at_unix_s,
        )
        .unwrap()
    }

    async fn connect_controller(
        tls: &TestTls,
        address: SocketAddr,
        session_id: SessionId,
        authentication: [u8; 32],
        identity: DeviceIdentity,
        host_key: VerifyingKey,
    ) -> ControllerRuntime {
        let mut identity = Some(identity);
        for _ in 0..100 {
            let client = QuicClient::connect(tls.client_config(address))
                .await
                .unwrap();
            match client
                .join(RelayJoin::controller_with_participant(
                    session_id,
                    authentication,
                    [2; 16],
                ))
                .await
            {
                Ok(()) => {
                    return ControllerRuntime::connect(client, identity.take().unwrap(), host_key)
                        .await
                        .unwrap();
                }
                Err(TransportError::JoinRejected(JoinRejectCode::SessionNotFound)) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("controller join failed: {error}"),
            }
        }
        panic!("host did not recreate the relay session in time")
    }

    async fn next_video_config(controller: &mut ControllerRuntime) -> u64 {
        loop {
            match tokio::time::timeout(Duration::from_secs(10), controller.next_event())
                .await
                .unwrap()
                .unwrap()
            {
                ControllerEvent::VideoConfig(config) => return config.stream_id,
                ControllerEvent::Control(_)
                | ControllerEvent::H264AccessUnit(_)
                | ControllerEvent::Cursor(_)
                | ControllerEvent::Audio(_)
                | ControllerEvent::Transfer(_)
                | ControllerEvent::Closed { .. } => {}
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_host_becomes_available_without_waiting_for_a_controller() {
        let _serial = host_test_lock().lock().await;
        let tls = TestTls::new();
        let relay = TestRelay::spawn("127.0.0.1:0".parse().unwrap(), &tls).await;
        let (event_sender, mut event_receiver) = tokio::sync::mpsc::unbounded_channel();
        let observer = Arc::new(move |event| {
            let _ = event_sender.send(event);
        });
        let host_task = tokio::spawn(
            supervisor(
                tls.client_config(relay.address),
                SessionId::from_bytes([101; 16]),
                [102; 32],
                DeviceIdentity::from_secret_key([103; 16], &[104; 32]),
                DeviceIdentity::from_secret_key([105; 16], &[106; 32]).verify_key(),
                None,
            )
            .with_observer(observer)
            .run(),
        );

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if matches!(
                    event_receiver.recv().await,
                    Some(HostLifecycleEvent::Available { .. })
                ) {
                    break;
                }
            }
        })
        .await
        .expect("host never published its available state");
        tokio::time::sleep(Duration::from_millis(100)).await;
        while let Ok(event) = event_receiver.try_recv() {
            assert!(!matches!(event, HostLifecycleEvent::Reconnecting { .. }));
        }

        host_task.abort();
        let _ = host_task.await;
        relay.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires access to the interactive Windows desktop"]
    async fn repeated_relay_restarts_rebuild_host_runtime_with_fresh_streams() {
        let _serial = host_test_lock().lock().await;
        let tls = TestTls::new();
        let first_relay = TestRelay::spawn("127.0.0.1:0".parse().unwrap(), &tls).await;
        let address = first_relay.address;
        let session_id = SessionId::from_bytes([111; 16]);
        let authentication = [112; 32];
        let host = DeviceIdentity::from_secret_key([113; 16], &[114; 32]);
        let host_key = host.verify_key();
        let controller_secret = [116; 32];
        let controller = DeviceIdentity::from_secret_key([115; 16], &controller_secret);
        let controller_key = controller.verify_key();
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer_events = events.clone();
        let observer = Arc::new(move |event| observer_events.lock().unwrap().push(event));
        let host_task = tokio::spawn(
            supervisor(
                tls.client_config(address),
                session_id,
                authentication,
                host,
                controller_key,
                None,
            )
            .with_reconnect_policy(
                ReconnectPolicy::new(Duration::from_millis(20), Duration::from_millis(100), 20)
                    .unwrap(),
            )
            .with_observer(observer)
            .run(),
        );
        let mut first_controller = connect_controller(
            &tls,
            address,
            session_id,
            authentication,
            controller,
            host_key,
        )
        .await;
        let first_stream = next_video_config(&mut first_controller).await;
        assert_eq!(first_stream, 1);

        first_relay.shutdown().await;
        drop(first_controller);
        let second_relay = TestRelay::spawn(address, &tls).await;
        let mut second_controller = connect_controller(
            &tls,
            address,
            session_id,
            authentication,
            DeviceIdentity::from_secret_key([115; 16], &controller_secret),
            host_key,
        )
        .await;
        let second_stream = next_video_config(&mut second_controller).await;
        assert!(second_stream > first_stream);

        second_relay.shutdown().await;
        drop(second_controller);
        let third_relay = TestRelay::spawn(address, &tls).await;
        let mut third_controller = connect_controller(
            &tls,
            address,
            session_id,
            authentication,
            DeviceIdentity::from_secret_key([115; 16], &controller_secret),
            host_key,
        )
        .await;
        let third_stream = next_video_config(&mut third_controller).await;
        assert!(third_stream > second_stream);
        {
            let recorded = events.lock().unwrap();
            assert!(
                recorded.iter().any(|event| matches!(
                    event,
                    HostLifecycleEvent::Reconnecting { retry: 1, .. }
                ))
            );
            assert!(recorded.iter().any(|event| matches!(
                event,
                HostLifecycleEvent::Connected { stream_id } if *stream_id == third_stream
            )));
            assert!(
                recorded
                    .iter()
                    .filter(|event| matches!(event, HostLifecycleEvent::Reconnecting { .. }))
                    .count()
                    >= 2
            );
        }

        host_task.abort();
        let _ = host_task.await;
        drop(third_controller);
        third_relay.shutdown().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires access to the interactive Windows desktop"]
    async fn repeated_controller_disconnects_rearm_host_without_stopping() {
        let _serial = host_test_lock().lock().await;
        let tls = TestTls::new();
        let relay = TestRelay::spawn("127.0.0.1:0".parse().unwrap(), &tls).await;
        let session_id = SessionId::from_bytes([141; 16]);
        let authentication = [142; 32];
        let host = DeviceIdentity::from_secret_key([143; 16], &[144; 32]);
        let host_key = host.verify_key();
        let controller_secret = [146; 32];
        let controller_key =
            DeviceIdentity::from_secret_key([145; 16], &controller_secret).verify_key();
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer_events = events.clone();
        let observer = Arc::new(move |event| observer_events.lock().unwrap().push(event));
        let host_task = tokio::spawn(
            supervisor(
                tls.client_config(relay.address),
                session_id,
                authentication,
                host,
                controller_key,
                None,
            )
            .with_reconnect_policy(
                ReconnectPolicy::new(Duration::from_millis(20), Duration::from_millis(100), 20)
                    .unwrap(),
            )
            .with_observer(observer)
            .run(),
        );

        let mut previous_stream = 0;
        for cycle in 0..5 {
            let mut controller = connect_controller(
                &tls,
                relay.address,
                session_id,
                authentication,
                DeviceIdentity::from_secret_key([145; 16], &controller_secret),
                host_key,
            )
            .await;
            let stream = next_video_config(&mut controller).await;
            assert!(
                stream > previous_stream,
                "cycle {cycle} reused an old stream ID"
            );
            previous_stream = stream;
            drop(controller);

            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let available = events.lock().unwrap().iter().any(|event| {
                        matches!(
                            event,
                            HostLifecycleEvent::Available { stream_id } if *stream_id > stream
                        )
                    });
                    if available {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("host did not re-register after the controller disconnected");
        }

        {
            let recorded = events.lock().unwrap();
            assert_eq!(
                recorded
                    .iter()
                    .filter(|event| matches!(event, HostLifecycleEvent::Connected { .. }))
                    .count(),
                5
            );
            assert!(
                recorded
                    .iter()
                    .all(|event| !matches!(event, HostLifecycleEvent::Stopped { .. }))
            );
            assert!(
                recorded
                    .iter()
                    .all(|event| !matches!(event, HostLifecycleEvent::Reconnecting { .. })),
                "controller disconnects must not take the host relay registration offline"
            );
        }

        host_task.abort();
        let _ = host_task.await;
        relay.shutdown().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires access to the interactive Windows desktop"]
    async fn fixed_password_directory_flow_survives_disconnect_and_reconnect() {
        let _serial = host_test_lock().lock().await;
        let tls = TestTls::new();
        let relay = TestRelay::spawn("127.0.0.1:0".parse().unwrap(), &tls).await;
        let session_id = SessionId::from_bytes([151; 16]);
        let authentication = [152; 32];
        let host = DeviceIdentity::from_secret_key([153; 16], &[154; 32]);
        let host_key = host.verify_key();
        let controller_secret = [156; 32];
        let controller_key =
            DeviceIdentity::from_secret_key([155; 16], &controller_secret).verify_key();
        let invite = PairingInvite::for_persistent_connection(&host, session_id, authentication);
        let encoded_invite = invite.encode().unwrap();
        let directory_id = 959_282_312_055;
        let access_code = *b"ABCDEFGH";
        let registration = RelayDirectoryRegistration::new(
            directory_id,
            access_code,
            encoded_invite.as_bytes().to_vec(),
            0,
        )
        .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer_events = events.clone();
        let observer = Arc::new(move |event| observer_events.lock().unwrap().push(event));
        let host_task = tokio::spawn(
            supervisor(
                tls.client_config(relay.address),
                session_id,
                authentication,
                host,
                controller_key,
                None,
            )
            .with_directory_registration(registration)
            .with_reconnect_policy(
                ReconnectPolicy::new(Duration::from_millis(20), Duration::from_millis(100), 20)
                    .unwrap(),
            )
            .with_observer(observer)
            .run(),
        );

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|event| matches!(event, HostLifecycleEvent::Available { .. }))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fixed-password host never became available");

        let lookup_client = QuicClient::connect(tls.client_config(relay.address))
            .await
            .unwrap();
        let invitation = lookup_client
            .lookup_directory(RelayDirectoryLookup::new(directory_id, access_code).unwrap())
            .await
            .unwrap();
        let decoded = PairingInvite::decode(&invitation, now_unix_s()).unwrap();
        assert!(decoded.is_persistent());
        assert_eq!(decoded.session_id(), session_id);
        assert_eq!(decoded.host_verify_key(), host_key);

        let mut first = connect_controller(
            &tls,
            relay.address,
            decoded.session_id(),
            *decoded.relay_authentication(),
            DeviceIdentity::from_secret_key([155; 16], &controller_secret),
            decoded.host_verify_key(),
        )
        .await;
        let first_stream = next_video_config(&mut first).await;
        drop(first);

        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if events.lock().unwrap().iter().any(|event| {
                    matches!(
                        event,
                        HostLifecycleEvent::Available { stream_id } if *stream_id > first_stream
                    )
                }) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fixed-password host did not republish after disconnect");

        let lookup_client = QuicClient::connect(tls.client_config(relay.address))
            .await
            .unwrap();
        let invitation = lookup_client
            .lookup_directory(RelayDirectoryLookup::new(directory_id, access_code).unwrap())
            .await
            .unwrap();
        let decoded = PairingInvite::decode(&invitation, now_unix_s()).unwrap();
        let mut second = connect_controller(
            &tls,
            relay.address,
            decoded.session_id(),
            *decoded.relay_authentication(),
            DeviceIdentity::from_secret_key([155; 16], &controller_secret),
            decoded.host_verify_key(),
        )
        .await;
        assert!(next_video_config(&mut second).await > first_stream);
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .all(|event| !matches!(event, HostLifecycleEvent::Stopped { .. }))
        );

        host_task.abort();
        let _ = host_task.await;
        drop(second);
        relay.shutdown().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires access to the interactive Windows desktop"]
    async fn malformed_controller_attempt_does_not_stop_the_host_service() {
        let _serial = host_test_lock().lock().await;
        let tls = TestTls::new();
        let relay = TestRelay::spawn("127.0.0.1:0".parse().unwrap(), &tls).await;
        let session_id = SessionId::from_bytes([131; 16]);
        let authentication = [132; 32];
        let host = DeviceIdentity::from_secret_key([133; 16], &[134; 32]);
        let host_key = host.verify_key();
        let controller_secret = [136; 32];
        let controller = DeviceIdentity::from_secret_key([135; 16], &controller_secret);
        let controller_key = controller.verify_key();
        let events = Arc::new(Mutex::new(Vec::new()));
        let observer_events = events.clone();
        let observer = Arc::new(move |event| observer_events.lock().unwrap().push(event));
        let host_task = tokio::spawn(
            supervisor(
                tls.client_config(relay.address),
                session_id,
                authentication,
                host,
                controller_key,
                None,
            )
            .with_reconnect_policy(
                ReconnectPolicy::new(Duration::from_millis(20), Duration::from_millis(100), 20)
                    .unwrap(),
            )
            .with_observer(observer)
            .run(),
        );

        let malformed = loop {
            let client = QuicClient::connect(tls.client_config(relay.address))
                .await
                .unwrap();
            match client
                .join(RelayJoin::controller_with_participant(
                    session_id,
                    authentication,
                    [2; 16],
                ))
                .await
            {
                Ok(()) => break client,
                Err(TransportError::JoinRejected(JoinRejectCode::SessionNotFound)) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => panic!("malformed controller join failed: {error}"),
            }
        };
        malformed.send_control(vec![0xff]).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            events.lock().unwrap().iter().all(|event| !matches!(
                event,
                HostLifecycleEvent::Reconnecting { .. } | HostLifecycleEvent::Stopped { .. }
            )),
            "a malformed peer attempt must not change the durable host status"
        );
        drop(malformed);

        let mut valid = connect_controller(
            &tls,
            relay.address,
            session_id,
            authentication,
            DeviceIdentity::from_secret_key([135; 16], &controller_secret),
            host_key,
        )
        .await;
        assert!(next_video_config(&mut valid).await >= 1);
        {
            let recorded = events.lock().unwrap();
            assert!(
                recorded
                    .iter()
                    .all(|event| !matches!(event, HostLifecycleEvent::Reconnecting { .. }))
            );
            assert!(
                recorded
                    .iter()
                    .all(|event| !matches!(event, HostLifecycleEvent::Stopped { .. }))
            );
        }

        host_task.abort();
        let _ = host_task.await;
        drop(valid);
        relay.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authentication_mismatch_fails_without_retrying() {
        let _serial = host_test_lock().lock().await;
        let tls = TestTls::new();
        let relay = TestRelay::spawn("127.0.0.1:0".parse().unwrap(), &tls).await;
        let session_id = SessionId::from_bytes([121; 16]);
        let existing = QuicClient::connect(tls.client_config(relay.address))
            .await
            .unwrap();
        existing
            .join(RelayJoin::host_with_participant(
                session_id, [122; 32], [1; 16],
            ))
            .await
            .unwrap();
        let host = DeviceIdentity::from_secret_key([123; 16], &[124; 32]);
        let controller = DeviceIdentity::from_secret_key([125; 16], &[126; 32]);
        let result = supervisor(
            tls.client_config(relay.address),
            session_id,
            [127; 32],
            host,
            controller.verify_key(),
            None,
        )
        .run()
        .await;
        assert_eq!(
            result,
            Err(HostSupervisorError::Permanent(HostRuntimeError::Transport(
                TransportError::JoinRejected(JoinRejectCode::AuthenticationMismatch)
            )))
        );
        drop(existing);
        relay.shutdown().await;
    }

    fn now_unix_s() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn occupied_normal_session_keeps_recovering_while_pairing_honors_expiry() {
        let _serial = host_test_lock().lock().await;
        let tls = TestTls::new();
        let relay = TestRelay::spawn("127.0.0.1:0".parse().unwrap(), &tls).await;
        let session_id = SessionId::from_bytes([131; 16]);
        let authentication = [132; 32];
        let existing = QuicClient::connect(tls.client_config(relay.address))
            .await
            .unwrap();
        existing
            .join(RelayJoin::host_with_participant(
                session_id,
                authentication,
                [1; 16],
            ))
            .await
            .unwrap();
        let host = DeviceIdentity::from_secret_key([133; 16], &[134; 32]);
        let controller = DeviceIdentity::from_secret_key([135; 16], &[136; 32]);
        let result = tokio::time::timeout(
            Duration::from_millis(20),
            supervisor(
                tls.client_config(relay.address),
                session_id,
                authentication,
                host,
                controller.verify_key(),
                None,
            )
            .with_reconnect_policy(
                ReconnectPolicy::new(Duration::from_millis(1), Duration::from_millis(1), 2)
                    .unwrap(),
            )
            .run(),
        )
        .await;
        assert!(
            result.is_err(),
            "normal hosting must keep retrying after one budget window"
        );

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = supervisor(
            tls.client_config(relay.address),
            session_id,
            authentication,
            DeviceIdentity::from_secret_key([133; 16], &[134; 32]),
            DeviceIdentity::from_secret_key([135; 16], &[136; 32]).verify_key(),
            Some(now),
        )
        .run()
        .await;
        assert_eq!(result, Err(HostSupervisorError::PairingExpired));
        drop(existing);
        relay.shutdown().await;
    }
}
