#[cfg(windows)]
mod windows {
    use std::{
        sync::Arc,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    use apps_windows::{
        runtime::HostRuntime,
        trusted::{
            LocalControllerApproval, WindowsControllerAuthorizer, WindowsPairingAuthorizer,
            WindowsTrustedControllerStore,
        },
        window::PendingController,
    };
    use desklink_crypto::{DeviceIdentity, PairingInvite};
    use desklink_ffi::{ControllerEvent, ControllerRuntime};
    use desklink_protocol::{FrameFlags, Platform, VideoConfig};
    use desklink_relay::{RelayConfig, RelayServer};
    use desklink_transport::{QuicClient, QuicClientConfig, RelayJoin};
    use desklink_video::EncodedFrame;
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

    struct AcceptLocalPairing;

    impl LocalControllerApproval for AcceptLocalPairing {
        fn approve(&self, _pending: PendingController) -> bool {
            true
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires access to the interactive Windows desktop"]
    async fn local_relay_windows_pairing_persists_trust_and_connection_for_reconnect() {
        let relay = spawn_test_relay().await;
        let config = || {
            QuicClientConfig::with_client_config(
                relay.address,
                "localhost",
                relay.client_config.clone(),
            )
        };
        let host = QuicClient::connect(config()).await.unwrap();
        let controller = QuicClient::connect(config()).await.unwrap();
        let host_identity = DeviceIdentity::from_secret_key([61; 16], &[62; 32]);
        let host_verify_key = host_identity.verify_key();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let invite = PairingInvite::new(&host_identity, now, 60).unwrap();
        let session_id = invite.session_id();
        let authentication = *invite.relay_authentication();
        host.join(RelayJoin::host(session_id, authentication))
            .await
            .unwrap();
        controller
            .join(RelayJoin::controller(session_id, authentication))
            .await
            .unwrap();

        let controller_identity = DeviceIdentity::from_secret_key([63; 16], &[64; 32]);
        let controller_verify_key = controller_identity.verify_key();
        let directory = std::env::temp_dir().join(format!(
            "desklink-runtime-trust-{}-{now}",
            std::process::id()
        ));
        let trusted = WindowsTrustedControllerStore::new(directory.join("controllers.bin"));
        let authorizer =
            WindowsPairingAuthorizer::new(trusted.clone(), invite, Box::new(AcceptLocalPairing));
        let runtime =
            HostRuntime::with_authorizer(host, 1, host_identity, Arc::new(authorizer)).unwrap();
        let runtime_task = tokio::spawn(async move { runtime.run().await });
        let mut controller = ControllerRuntime::connect_for_platform(
            controller,
            controller_identity,
            host_verify_key,
            Platform::Windows,
        )
        .await
        .unwrap();

        let mut video_config: Option<VideoConfig> = None;
        let mut frame: Option<EncodedFrame> = None;
        let mut cursor = None;
        while video_config.is_none() || frame.is_none() || cursor.is_none() {
            match tokio::time::timeout(Duration::from_secs(5), controller.next_event())
                .await
                .unwrap()
                .unwrap()
            {
                ControllerEvent::VideoConfig(config) => video_config = Some(config),
                ControllerEvent::H264AccessUnit(access_unit) => frame = Some(access_unit),
                ControllerEvent::Cursor(update) => cursor = Some(update),
                ControllerEvent::Control(_)
                | ControllerEvent::Audio(_)
                | ControllerEvent::Transfer(_)
                | ControllerEvent::Closed { .. } => {}
            }
        }
        let video_config = video_config.unwrap();
        assert_eq!(video_config.stream_id, 1);
        assert!(!video_config.sequence_header.is_empty());
        let frame = frame.unwrap();
        assert_eq!(frame.stream_id, 1);
        assert_eq!(frame.config_version, video_config.config_version);
        assert_ne!(frame.flags.0 & FrameFlags::KEYFRAME.0, 0);
        assert!(!frame.data.is_empty());
        let cursor = cursor.unwrap();
        assert_eq!(cursor.stream_id, 1);
        assert!((0..=1_000_000).contains(&cursor.x_millionths));
        assert!((0..=1_000_000).contains(&cursor.y_millionths));
        assert_eq!(controller.metrics().completed_frames, 1);
        let records = trusted.list().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].device_id(), [63; 16]);
        assert_eq!(records[0].verify_key(), controller_verify_key);
        controller.request_keyframe().await.unwrap();

        runtime_task.abort();
        let _ = runtime_task.await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(controller);

        let reconnect_relay = spawn_test_relay().await;
        let reconnect_config = || {
            QuicClientConfig::with_client_config(
                reconnect_relay.address,
                "localhost",
                reconnect_relay.client_config.clone(),
            )
        };
        let reconnect_host = QuicClient::connect(reconnect_config()).await.unwrap();
        let reconnect_controller = QuicClient::connect(reconnect_config()).await.unwrap();
        let reconnect_session = session_id;
        let reconnect_authentication = authentication;
        reconnect_host
            .join(RelayJoin::host(reconnect_session, reconnect_authentication))
            .await
            .unwrap();
        reconnect_controller
            .join(RelayJoin::controller(
                reconnect_session,
                reconnect_authentication,
            ))
            .await
            .unwrap();

        let reconnect_runtime = HostRuntime::with_authorizer(
            reconnect_host,
            2,
            DeviceIdentity::from_secret_key([61; 16], &[62; 32]),
            Arc::new(WindowsControllerAuthorizer::new(trusted.clone())),
        )
        .unwrap();
        let reconnect_task = tokio::spawn(async move { reconnect_runtime.run().await });
        let mut reconnect_controller = ControllerRuntime::connect_for_platform(
            reconnect_controller,
            DeviceIdentity::from_secret_key([63; 16], &[64; 32]),
            host_verify_key,
            Platform::Windows,
        )
        .await
        .unwrap();
        loop {
            if matches!(
                tokio::time::timeout(Duration::from_secs(5), reconnect_controller.next_event())
                    .await
                    .unwrap()
                    .unwrap(),
                ControllerEvent::VideoConfig(_)
            ) {
                break;
            }
        }
        assert_eq!(trusted.list().unwrap().len(), 1);
        reconnect_task.abort();
        let _ = reconnect_task.await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(reconnect_controller);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "hardware soak; run through scripts/verify-windows-resilience.py"]
    async fn local_relay_hardware_soak_keeps_secure_media_and_cursor_alive() {
        let soak_seconds = std::env::var("DESKLINK_SOAK_SECONDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30)
            .clamp(10, 300);
        let relay = spawn_test_relay().await;
        let config = || {
            QuicClientConfig::with_client_config(
                relay.address,
                "localhost",
                relay.client_config.clone(),
            )
        };
        let host = QuicClient::connect(config()).await.unwrap();
        let controller = QuicClient::connect(config()).await.unwrap();
        let host_identity = DeviceIdentity::from_secret_key([91; 16], &[92; 32]);
        let host_verify_key = host_identity.verify_key();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let invite = PairingInvite::new(&host_identity, now, 600).unwrap();
        let session_id = invite.session_id();
        let authentication = *invite.relay_authentication();
        host.join(RelayJoin::host(session_id, authentication))
            .await
            .unwrap();
        controller
            .join(RelayJoin::controller(session_id, authentication))
            .await
            .unwrap();

        let controller_identity = DeviceIdentity::from_secret_key([93; 16], &[94; 32]);
        let directory = std::env::temp_dir().join(format!(
            "desklink-runtime-soak-{}-{now}",
            std::process::id()
        ));
        let trusted = WindowsTrustedControllerStore::new(directory.join("controllers.bin"));
        let authorizer =
            WindowsPairingAuthorizer::new(trusted, invite, Box::new(AcceptLocalPairing));
        let runtime =
            HostRuntime::with_authorizer(host, 1, host_identity, Arc::new(authorizer)).unwrap();
        let runtime_task = tokio::spawn(async move { runtime.run().await });
        let mut controller =
            ControllerRuntime::connect(controller, controller_identity, host_verify_key)
                .await
                .unwrap();

        let started = Instant::now();
        let deadline = started + Duration::from_secs(soak_seconds);
        let mut video_configs = 0_u64;
        let mut frames = 0_u64;
        let mut cursors = 0_u64;
        let mut last_frame_id = 0_u64;
        let mut last_cursor_sequence = 0_u64;
        let mut next_keyframe_request = started + Duration::from_secs(5);
        while Instant::now() < deadline {
            let event = tokio::time::timeout(Duration::from_secs(3), controller.next_event())
                .await
                .expect("soak event timeout")
                .expect("soak controller closed");
            match event {
                ControllerEvent::VideoConfig(config) => {
                    assert_eq!(config.stream_id, 1);
                    assert!(!config.sequence_header.is_empty());
                    video_configs += 1;
                }
                ControllerEvent::H264AccessUnit(frame) => {
                    assert_eq!(frame.stream_id, 1);
                    assert!(frame.frame_id > last_frame_id);
                    assert!(!frame.data.is_empty());
                    last_frame_id = frame.frame_id;
                    frames += 1;
                }
                ControllerEvent::Cursor(cursor) => {
                    assert_eq!(cursor.stream_id, 1);
                    assert!(cursor.sequence > last_cursor_sequence);
                    assert!((0..=1_000_000).contains(&cursor.x_millionths));
                    assert!((0..=1_000_000).contains(&cursor.y_millionths));
                    last_cursor_sequence = cursor.sequence;
                    cursors += 1;
                }
                ControllerEvent::Control(_) => {}
                ControllerEvent::Audio(_) => {}
                ControllerEvent::Transfer(_) => {}
                ControllerEvent::Closed { reason } => {
                    panic!("secure soak session closed early: {reason}")
                }
            }
            if Instant::now() >= next_keyframe_request {
                controller.request_keyframe().await.unwrap();
                next_keyframe_request += Duration::from_secs(5);
            }
        }
        assert!(video_configs >= 1);
        assert!(
            frames >= 2,
            "expected multiple H.264 frames, observed {frames}"
        );
        assert!(
            cursors >= soak_seconds.saturating_mul(10),
            "cursor lane stalled: observed {cursors} updates in {soak_seconds}s"
        );
        assert!(controller.metrics().completed_frames >= frames);

        runtime_task.abort();
        let _ = runtime_task.await;
        drop(controller);
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::remove_dir_all(directory).unwrap();
    }
}
