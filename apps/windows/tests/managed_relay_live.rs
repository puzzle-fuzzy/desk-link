#[cfg(windows)]
mod windows {
    use std::{sync::Arc, time::Duration};

    use apps_windows::runtime::{
        ControllerAuthorization, ControllerAuthorizer, HostLifecycleEvent, HostSupervisor,
    };
    use desklink_crypto::{DeviceIdentity, PairingCode, PairingInvite, PeerIdentity, SessionId};
    use desklink_ffi::{ControllerEvent, ControllerRuntime};
    use desklink_protocol::{InputEvent, Platform};
    use desklink_transport::{
        QuicClient, QuicClientConfig, RelayDirectoryLookup, RelayDirectoryRegistration, RelayJoin,
    };
    use ed25519_dalek::VerifyingKey;
    use rand_core::{OsRng, RngCore};

    const RELAY_ADDRESS: &str = "101.35.246.159:4433";
    const RELAY_SERVER_NAME: &str = "turn.p2p.yxswy.com";

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "live managed relay probe; run explicitly before publishing a Windows installer"]
    async fn managed_relay_completes_directory_noise_video_and_reconnect() {
        let relay_address = RELAY_ADDRESS.parse().unwrap();
        let transport = QuicClientConfig::new(relay_address, RELAY_SERVER_NAME.to_owned()).unwrap();
        let mut rng = OsRng;
        let mut session_bytes = [0_u8; 16];
        let mut authentication = [0_u8; 32];
        rng.fill_bytes(&mut session_bytes);
        rng.fill_bytes(&mut authentication);
        let session_id = SessionId::from_bytes(session_bytes);
        let host = DeviceIdentity::generate(&mut rng);
        let controller = DeviceIdentity::generate(&mut rng);
        let controller_device_id = controller.device_id;
        let controller_secret = controller.with_secret_key_bytes(|secret| *secret);
        let controller_key = controller.verify_key();
        let invite = PairingInvite::for_persistent_connection(&host, session_id, authentication);
        let encoded = invite.encode().unwrap();
        let access_code = PairingCode::generate(&mut rng);
        let directory_id = 100_000_000_000 + rng.next_u64() % 900_000_000_000;
        let registration = RelayDirectoryRegistration::new(
            directory_id,
            *access_code.as_bytes(),
            encoded.as_bytes().to_vec(),
            0,
        )
        .unwrap();
        let (events, mut lifecycle) = tokio::sync::mpsc::unbounded_channel();
        let observer = Arc::new(move |event| {
            let _ = events.send(event);
        });
        let host_task = tokio::spawn(
            HostSupervisor::new(
                transport.clone(),
                session_id,
                authentication,
                1,
                host,
                Arc::new(ExpectedController(controller_key)),
                None,
            )
            .unwrap()
            .with_directory_registration(registration)
            .with_observer(observer)
            .run(),
        );

        wait_for_available(&mut lifecycle, 0).await;
        let first_invite = lookup_invite(&transport, directory_id, *access_code.as_bytes()).await;
        let mut first = connect_controller(
            &transport,
            &first_invite,
            controller_device_id,
            controller_secret,
        )
        .await;
        let first_stream = wait_for_video(&mut first).await;
        let (pointer_x, pointer_y) = wait_for_cursor(&mut first).await;
        first
            .send_input(InputEvent::MouseMove {
                x: pointer_x,
                y: pointer_y,
            })
            .await
            .expect("send live pointer input");
        assert_session_survives_input(&mut first).await;
        drop(first);

        let next_stream = wait_for_available(&mut lifecycle, first_stream).await;
        let second_invite = lookup_invite(&transport, directory_id, *access_code.as_bytes()).await;
        let mut second = connect_controller(
            &transport,
            &second_invite,
            controller_device_id,
            controller_secret,
        )
        .await;
        let second_stream = wait_for_video(&mut second).await;
        assert!(next_stream > first_stream);
        assert!(second_stream > first_stream);

        drop(second);
        host_task.abort();
        let _ = host_task.await;
    }

    async fn lookup_invite(
        transport: &QuicClientConfig,
        directory_id: u64,
        access_code: [u8; 8],
    ) -> PairingInvite {
        let client = QuicClient::connect(transport.clone()).await.unwrap();
        let invitation = client
            .lookup_directory(RelayDirectoryLookup::new(directory_id, access_code).unwrap())
            .await
            .unwrap();
        PairingInvite::decode(&invitation, now_unix_s()).unwrap()
    }

    async fn connect_controller(
        transport: &QuicClientConfig,
        invite: &PairingInvite,
        device_id: [u8; 16],
        secret: [u8; 32],
    ) -> ControllerRuntime {
        let client = QuicClient::connect(transport.clone()).await.unwrap();
        client
            .join(RelayJoin::controller_with_participant(
                invite.session_id(),
                *invite.relay_authentication(),
                device_id,
            ))
            .await
            .unwrap();
        ControllerRuntime::connect_for_platform(
            client,
            DeviceIdentity::from_secret_key(device_id, &secret),
            invite.host_verify_key(),
            Platform::Windows,
        )
        .await
        .unwrap()
    }

    async fn wait_for_available(
        events: &mut tokio::sync::mpsc::UnboundedReceiver<HostLifecycleEvent>,
        after_stream: u64,
    ) -> u64 {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match events.recv().await {
                    Some(HostLifecycleEvent::Available { stream_id })
                        if stream_id > after_stream =>
                    {
                        return stream_id;
                    }
                    Some(HostLifecycleEvent::Stopped { reason }) => {
                        panic!("managed relay host stopped: {reason}")
                    }
                    Some(_) => {}
                    None => panic!("managed relay host lifecycle channel closed"),
                }
            }
        })
        .await
        .expect("managed relay host never became available")
    }

    async fn wait_for_video(controller: &mut ControllerRuntime) -> u64 {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                match controller.next_event().await.unwrap() {
                    ControllerEvent::H264AccessUnit(frame) => return frame.stream_id,
                    ControllerEvent::Closed { reason } => {
                        panic!("managed relay controller closed before video: {reason}")
                    }
                    ControllerEvent::Control(_)
                    | ControllerEvent::VideoConfig(_)
                    | ControllerEvent::Cursor(_)
                    | ControllerEvent::Audio(_)
                    | ControllerEvent::Transfer(_) => {}
                }
            }
        })
        .await
        .expect("managed relay did not deliver an H.264 frame")
    }

    async fn wait_for_cursor(controller: &mut ControllerRuntime) -> (i32, i32) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match controller.next_event().await.unwrap() {
                    ControllerEvent::Cursor(cursor) => {
                        return (cursor.x_millionths, cursor.y_millionths);
                    }
                    ControllerEvent::Closed { reason } => {
                        panic!("managed relay controller closed before cursor input: {reason}")
                    }
                    ControllerEvent::Control(_)
                    | ControllerEvent::VideoConfig(_)
                    | ControllerEvent::H264AccessUnit(_)
                    | ControllerEvent::Audio(_)
                    | ControllerEvent::Transfer(_) => {}
                }
            }
        })
        .await
        .expect("managed relay did not deliver cursor state")
    }

    async fn assert_session_survives_input(controller: &mut ControllerRuntime) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(750);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return;
            }
            match tokio::time::timeout(remaining, controller.next_event()).await {
                Ok(Ok(ControllerEvent::Closed { reason })) => {
                    panic!("managed relay controller closed after pointer input: {reason}")
                }
                Ok(Err(error)) => panic!("pointer input ended the secure session: {error}"),
                Ok(Ok(_)) => {}
                Err(_) => return,
            }
        }
    }

    fn now_unix_s() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }
}
