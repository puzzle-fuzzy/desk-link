use std::{sync::Arc, time::Duration};

use desklink_crypto::{DeviceIdentity, SessionId};
use desklink_ffi::{
    ControllerEvent, ControllerRuntime, HostCommand, HostEvent, HostIdentity, HostRuntime,
};
use desklink_protocol::InputEvent;
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{QuicClient, QuicClientConfig, RelayJoin};
use quinn::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::{Mutex, oneshot};

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

struct HostTestFixture {
    relay_addr: std::net::SocketAddr,
    host_identity: DeviceIdentity,
    controller_identity: DeviceIdentity,
    host_events: Mutex<Vec<HostEvent>>,
    controller_events: Mutex<Vec<ControllerEvent>>,
    relay: TestRelay,
    session_id: SessionId,
    relay_authentication: [u8; 32],
    controller: Mutex<Option<ControllerRuntime>>,
    controller_ready: Mutex<Option<oneshot::Receiver<ControllerRuntime>>>,
}

impl HostTestFixture {
    async fn new() -> Self {
        let relay = spawn_test_relay().await;
        Self {
            relay_addr: relay.address,
            host_identity: DeviceIdentity::from_secret_key([31; 16], &[32; 32]),
            controller_identity: DeviceIdentity::from_secret_key([33; 16], &[34; 32]),
            host_events: Mutex::new(Vec::new()),
            controller_events: Mutex::new(Vec::new()),
            relay,
            session_id: SessionId::from_bytes([35; 16]),
            relay_authentication: [36; 32],
            controller: Mutex::new(None),
            controller_ready: Mutex::new(None),
        }
    }

    async fn start_host(&self) -> HostRuntime {
        let config = || {
            QuicClientConfig::with_client_config(
                self.relay_addr,
                "localhost",
                self.relay.client_config.clone(),
            )
        };
        let host_client = QuicClient::connect(config()).await.unwrap();
        let controller_client = QuicClient::connect(config()).await.unwrap();
        let host_identity = self.host_identity.with_secret_key_bytes(|secret_key| {
            HostIdentity::from_secret_key(self.host_identity.device_id, secret_key)
        });
        let host = HostRuntime::start(
            host_client,
            host_identity,
            self.session_id,
            self.relay_authentication,
        )
        .unwrap();

        let join = RelayJoin::controller(self.session_id, self.relay_authentication);
        let mut joined = false;
        for _ in 0..20 {
            if controller_client.join(join.clone()).await.is_ok() {
                joined = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(joined, "controller could not join the host session");

        let controller_identity = self
            .controller_identity
            .with_secret_key_bytes(|secret_key| {
                DeviceIdentity::from_secret_key(self.controller_identity.device_id, secret_key)
            });
        let host_verify_key = self.host_identity.verify_key();
        let (sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let controller =
                ControllerRuntime::connect(controller_client, controller_identity, host_verify_key)
                    .await
                    .unwrap();
            let _ = sender.send(controller);
        });
        *self.controller_ready.lock().await = Some(receiver);
        host
    }

    async fn approve(&self, host: &HostRuntime) {
        host.send(HostCommand::Approve {
            controller_device_id: self.controller_identity.device_id,
            controller_verify_key: *self.controller_identity.verify_key().as_bytes(),
        })
        .unwrap();
    }

    async fn reject(&self, host: &HostRuntime) {
        host.send(HostCommand::Reject).unwrap();
    }

    async fn send_test_video(&self, host: &HostRuntime) {
        host.send(HostCommand::SendVideoConfig {
            stream_id: 9,
            version: 1,
            width: 1280,
            height: 720,
            bytes: vec![
                0, 0, 0, 1, 0x67, 0x64, 0, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80,
            ],
        })
        .unwrap();
        let mut controller = self.controller.lock().await;
        if controller.is_none() {
            let receiver = self.controller_ready.lock().await.take().unwrap();
            *controller = Some(receiver.await.unwrap());
        }
        let config = tokio::time::timeout(
            Duration::from_secs(3),
            controller.as_mut().unwrap().next_event(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(config, ControllerEvent::VideoConfig(_)));
        self.controller_events.lock().await.push(config);
        drop(controller);
        host.send(HostCommand::SendVideoAccessUnit {
            stream_id: 9,
            frame_id: 1,
            config_version: 1,
            bytes: vec![0, 0, 0, 1, 0x65, 0x88, 0x84],
        })
        .unwrap();
    }

    async fn next_event(&self, host: &HostRuntime) -> HostEvent {
        let event = host.next_event().await.unwrap();
        self.host_events.lock().await.push(event.clone());
        event
    }

    async fn next_event_timeout(&self, host: &HostRuntime, timeout: Duration) -> Option<HostEvent> {
        tokio::time::timeout(timeout, self.next_event(host))
            .await
            .ok()
    }

    async fn controller_received_video(&self) -> bool {
        let mut controller = self.controller.lock().await;
        if controller.is_none() {
            let receiver = self.controller_ready.lock().await.take().unwrap();
            *controller = Some(receiver.await.unwrap());
        }
        loop {
            let event = tokio::time::timeout(
                Duration::from_secs(3),
                controller.as_mut().unwrap().next_event(),
            )
            .await
            .ok()
            .and_then(Result::ok);
            let Some(event) = event else {
                return false;
            };
            self.controller_events.lock().await.push(event.clone());
            if matches!(event, ControllerEvent::H264AccessUnit(_)) {
                return true;
            }
        }
    }

    async fn controller_sends_input_and_keyframe(&self) {
        let mut controller = self.controller.lock().await;
        controller
            .as_mut()
            .unwrap()
            .send_input(InputEvent::MouseWheel {
                delta_x: -120,
                delta_y: 240,
            })
            .await
            .unwrap();
        controller
            .as_mut()
            .unwrap()
            .request_keyframe()
            .await
            .unwrap();
    }

    async fn received_release_all(&self, host: &HostRuntime) -> bool {
        for _ in 0..3 {
            if matches!(self.next_event(host).await, HostEvent::ReleaseAll) {
                return true;
            }
        }
        false
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_does_not_publish_video_before_approval() {
    let fixture = HostTestFixture::new().await;
    let host = fixture.start_host().await;
    let request = fixture.next_event(&host).await;
    assert!(matches!(request, HostEvent::ApprovalRequested { .. }));
    assert!(matches!(
        host.send(HostCommand::SendVideoConfig {
            stream_id: 9,
            version: 1,
            width: 1280,
            height: 720,
            bytes: vec![1],
        }),
        Err(desklink_ffi::HostError::InvalidState)
    ));
    assert!(
        fixture
            .next_event_timeout(&host, Duration::from_millis(50))
            .await
            .is_none()
    );
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approval_allows_video_and_reject_emits_release_all() {
    let fixture = HostTestFixture::new().await;
    let host = fixture.start_host().await;
    assert!(matches!(
        fixture.next_event(&host).await,
        HostEvent::ApprovalRequested { .. }
    ));
    fixture.approve(&host).await;
    fixture.send_test_video(&host).await;
    assert!(fixture.controller_received_video().await);

    fixture.reject(&host).await;
    assert!(fixture.received_release_all(&host).await);
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_decodes_input_and_keyframe_requests_after_approval() {
    let fixture = HostTestFixture::new().await;
    let host = fixture.start_host().await;
    assert!(matches!(
        fixture.next_event(&host).await,
        HostEvent::ApprovalRequested { .. }
    ));
    fixture.approve(&host).await;
    fixture.send_test_video(&host).await;
    assert!(fixture.controller_received_video().await);
    fixture.controller_sends_input_and_keyframe().await;

    let mut saw_input = false;
    let mut saw_keyframe = false;
    for _ in 0..2 {
        match fixture.next_event(&host).await {
            HostEvent::Input(InputEvent::MouseWheel {
                delta_x: -120,
                delta_y: 240,
            }) => saw_input = true,
            HostEvent::KeyframeRequested => saw_keyframe = true,
            event => panic!("unexpected host event: {event:?}"),
        }
    }
    assert!(saw_input && saw_keyframe);

    fixture.reject(&host).await;
    assert!(fixture.received_release_all(&host).await);
    host.destroy();
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
