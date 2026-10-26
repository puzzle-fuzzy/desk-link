#[path = "host_runtime/input_admission.rs"]
mod input_admission;

#[path = "host_runtime/media_generation.rs"]
mod media_generation;

use std::{sync::Arc, time::Duration};

use desklink_crypto::{
    DeviceIdentity, NoiseInitiator, SecureLane, SecureRole, SecureSession, SessionId,
};
use desklink_ffi::{
    ControllerEvent, ControllerRuntime, HostCommand, HostError, HostEvent, HostIdentity,
    HostRuntime, HostState,
};
use desklink_protocol::{
    Codec, ControlMessage, DeviceCapabilities, DeviceRole, H264Profile, InputContext,
    InputEnvelope, InputEvent, KeyCode, Modifiers, MouseButton, NoiseHandshake, NoiseHandshakeStep,
    PROTOCOL_VERSION, Platform, VideoConfig, decode_video_config, encode_control, encode_input,
    encode_noise_handshake,
};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{QuicClient, QuicClientConfig, RelayJoin};
use quinn::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::{Mutex, oneshot};

struct TestRelay {
    address: std::net::SocketAddr,
    client_config: quinn::ClientConfig,
    task: tokio::task::JoinHandle<()>,
    server: Arc<RelayServer>,
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.server.close();
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
    input_context: Mutex<Option<InputContext>>,
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
            input_context: Mutex::new(None),
        }
    }

    async fn start_host(&self) -> HostRuntime {
        let host = self.start_host_only().await;
        let controller_client = self.join_controller().await;

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

    async fn start_host_only(&self) -> HostRuntime {
        let config = || {
            QuicClientConfig::with_client_config(
                self.relay_addr,
                "localhost",
                self.relay.client_config.clone(),
            )
        };
        let host_identity = self.host_identity.with_secret_key_bytes(|secret_key| {
            HostIdentity::from_secret_key(self.host_identity.device_id, secret_key)
        });
        HostRuntime::start_from_config(
            config(),
            host_identity,
            self.session_id,
            self.relay_authentication,
        )
        .unwrap()
    }

    async fn join_controller(&self) -> QuicClient {
        let config = || {
            QuicClientConfig::with_client_config(
                self.relay_addr,
                "localhost",
                self.relay.client_config.clone(),
            )
        };
        let join = RelayJoin::controller_with_participant(
            self.session_id,
            self.relay_authentication,
            [2; 16],
        );
        let controller_client =
            tokio::time::timeout(Duration::from_secs(3), QuicClient::connect(config()))
                .await
                .expect("raw controller QUIC connect timed out")
                .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if controller_client.join(join.clone()).await.is_ok() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("raw controller relay join timed out");
        controller_client
    }

    async fn wait_for_new_host_registration(&self, previous_host_generation: u64) {
        let table = self.relay.server.session_table();
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let snapshot = self.relay.server.capacity_snapshot();
                if !table.has_connection(self.session_id, previous_host_generation)
                    && snapshot.attached_participants == 1
                    && snapshot.accepted_connections == 1
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(
            result.is_ok(),
            "retired host registration was not replaced: previous_still_attached={}, snapshot={:?}",
            table.has_connection(self.session_id, previous_host_generation),
            self.relay.server.capacity_snapshot()
        );
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
        // Approval only begins capability negotiation. Capture can submit once
        // the worker grants the actual Connected producer, recorded like every
        // other fixture event so later assertions retain the full history.
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match self.next_event(host).await {
                    HostEvent::Connected { media_generation } => {
                        assert_ne!(media_generation, 0);
                        return;
                    }
                    HostEvent::State(HostState::NegotiatingCapabilities) => {}
                    event => panic!("unexpected event before capture grant: {event:?}"),
                }
            }
        })
        .await
        .expect("host did not grant capture after approval");
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
        if let ControllerEvent::VideoConfig(config) = &config {
            *self.input_context.lock().await = Some(InputContext {
                stream_id: config.stream_id,
                input_epoch: config.input_epoch,
            });
        }
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
            .send_input_with_context(
                self.input_context
                    .lock()
                    .await
                    .expect("received video context"),
                InputEvent::MouseWheel {
                    delta_x: -120,
                    delta_y: 240,
                },
            )
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
        for _ in 0..8 {
            if matches!(self.next_event(host).await, HostEvent::ReleaseAll) {
                return true;
            }
        }
        false
    }
}

struct RawController {
    client: QuicClient,
    secure: SecureSession,
    input_sequence: u64,
    input_context: Option<InputContext>,
    host_generation: u64,
}

#[derive(Clone, Copy, Debug)]
enum ForbiddenControllerLane {
    VideoConfig,
    VideoDatagram,
    CursorDatagram,
}

impl RawController {
    async fn pair(fixture: &HostTestFixture) -> Self {
        let client = fixture.join_controller().await;
        let identity = fixture
            .controller_identity
            .with_secret_key_bytes(|secret_key| {
                DeviceIdentity::from_secret_key(fixture.controller_identity.device_id, secret_key)
            });
        let (mut initiator, hello) =
            NoiseInitiator::start(identity, fixture.host_identity.verify_key()).unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            client.send_control(
                encode_noise_handshake(&NoiseHandshake {
                    protocol_version: PROTOCOL_VERSION,
                    step: NoiseHandshakeStep::InitiatorHello,
                    payload: hello,
                })
                .unwrap(),
            ),
        )
        .await
        .expect("Noise initiator hello send timed out")
        .unwrap();
        let (host_generation, response_bytes) = tokio::time::timeout(
            Duration::from_secs(3),
            client.next_control_with_generation(),
        )
        .await
        .expect("Noise responder hello receive timed out")
        .unwrap();
        let response = desklink_protocol::decode_noise_handshake(&response_bytes).unwrap();
        assert_eq!(response.step, NoiseHandshakeStep::ResponderHello);
        let finish = initiator.receive(&response.payload).unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            client.send_control(
                encode_noise_handshake(&NoiseHandshake {
                    protocol_version: PROTOCOL_VERSION,
                    step: NoiseHandshakeStep::InitiatorFinish,
                    payload: finish,
                })
                .unwrap(),
            ),
        )
        .await
        .expect("Noise initiator finish send timed out")
        .unwrap();
        Self {
            client,
            secure: initiator
                .finish()
                .unwrap()
                .into_secure_session(SecureRole::Initiator),
            input_sequence: 0,
            input_context: None,
            host_generation,
        }
    }

    async fn negotiate(&mut self) {
        tokio::time::timeout(Duration::from_secs(3), async {
            self.send_control(ControlMessage::Hello {
                platform: Platform::MacOS,
                role: DeviceRole::Controller,
            })
            .await;
            self.send_control(ControlMessage::Capabilities(DeviceCapabilities {
                platform: Platform::MacOS,
                role: DeviceRole::Controller,
                codecs: vec![Codec::H264],
                h264_profiles: vec![H264Profile::Main],
                width: 1920,
                height: 1080,
            }))
            .await;
        })
        .await
        .expect("raw capability negotiation sends timed out");
    }

    async fn send_input(&mut self) {
        self.send_input_for_context(
            self.input_context
                .expect("input helper requires a received VideoConfig"),
            InputEvent::MouseWheel {
                delta_x: 1,
                delta_y: -1,
            },
        )
        .await;
    }

    async fn send_input_for_context(&mut self, context: InputContext, event: InputEvent) {
        self.send_input_for_context_at(context, event, now_micros())
            .await;
    }

    async fn send_input_for_context_at(
        &mut self,
        context: InputContext,
        event: InputEvent,
        timestamp_us: u64,
    ) {
        self.input_sequence += 1;
        self.send_input_envelope(InputEnvelope {
            sequence: self.input_sequence,
            timestamp_us,
            stream_id: context.stream_id,
            input_epoch: context.input_epoch,
            event,
        })
        .await;
    }

    async fn send_input_envelope(&mut self, input: InputEnvelope) {
        let plaintext = encode_input(&input).unwrap();
        let ciphertext = self.secure.seal(SecureLane::Input, &plaintext).unwrap();
        tokio::time::timeout(Duration::from_secs(3), self.client.send_input(ciphertext))
            .await
            .expect("raw input send timed out")
            .unwrap();
    }

    async fn receive_video_config(&mut self) -> VideoConfig {
        let ciphertext =
            tokio::time::timeout(Duration::from_secs(3), self.client.next_video_config())
                .await
                .unwrap()
                .unwrap();
        let plaintext = self
            .secure
            .open(SecureLane::VideoConfig, &ciphertext)
            .unwrap();
        let config = decode_video_config(&plaintext).unwrap();
        let context = InputContext {
            stream_id: config.stream_id,
            input_epoch: config.input_epoch,
        };
        assert!(
            context.is_valid(),
            "host must publish a nonzero input context"
        );
        self.input_context = Some(context);
        config
    }

    async fn send_forbidden_lane(&mut self, lane: ForbiddenControllerLane) {
        let plaintext = b"controller media is forbidden";
        match lane {
            ForbiddenControllerLane::VideoConfig => {
                self.client
                    .send_video_config(
                        self.secure
                            .seal(SecureLane::VideoConfig, plaintext)
                            .unwrap(),
                    )
                    .await
            }
            ForbiddenControllerLane::VideoDatagram => {
                self.client
                    .send_video_datagram(
                        self.secure
                            .seal(SecureLane::VideoDatagram, plaintext)
                            .unwrap(),
                    )
                    .await
            }
            ForbiddenControllerLane::CursorDatagram => {
                self.client
                    .send_cursor_datagram(
                        self.secure
                            .seal(SecureLane::CursorDatagram, plaintext)
                            .unwrap(),
                    )
                    .await
            }
        }
        .unwrap();
    }

    async fn send_control(&mut self, message: ControlMessage) {
        let plaintext = encode_control(&message).unwrap();
        let ciphertext = self.secure.seal(SecureLane::Control, &plaintext).unwrap();
        tokio::time::timeout(Duration::from_secs(3), self.client.send_control(ciphertext))
            .await
            .expect("raw control/negotiation send timed out")
            .unwrap();
    }
}

fn now_micros() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

async fn wait_for_state(host: &HostRuntime, expected: HostState) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while host.state() != expected {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn terminal_events(host: &HostRuntime) -> Vec<HostEvent> {
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(3), host.next_event())
            .await
            .unwrap()
            .unwrap();
        let closed = matches!(event, HostEvent::State(HostState::Closed));
        if !matches!(event, HostEvent::State(_) | HostEvent::Connected { .. }) || closed {
            events.push(event);
        }
        if closed {
            return events;
        }
    }
}

async fn next_connected_state(host: &HostRuntime) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(
                host.next_event().await.unwrap(),
                HostEvent::Connected { .. }
            ) {
                return;
            }
        }
    })
    .await
    .unwrap();
}

async fn start_approved_raw(fixture: &HostTestFixture) -> (HostRuntime, RawController) {
    let host = fixture.start_host_only().await;
    let mut controller = RawController::pair(fixture).await;
    assert!(matches!(
        fixture.next_event(&host).await,
        HostEvent::ApprovalRequested { .. }
    ));
    fixture.approve(&host).await;
    controller.negotiate().await;
    next_connected_state(&host).await;
    (host, controller)
}

async fn send_command_eventually(host: &HostRuntime, command: HostCommand) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match host.send(command.clone()) {
                Ok(()) => return,
                Err(HostError::CommandQueueFull) => tokio::task::yield_now().await,
                result => panic!("command was rejected: {result:?}"),
            }
        }
    })
    .await
    .expect("command queue did not make progress");
}

async fn publish_raw_config(
    host: &HostRuntime,
    controller: &mut RawController,
    stream_id: u64,
    version: u32,
    width: u16,
    height: u16,
) -> VideoConfig {
    send_command_eventually(host, raw_config_command(stream_id, version, width, height)).await;
    controller.receive_video_config().await
}

fn raw_config_command(stream_id: u64, version: u32, width: u16, height: u16) -> HostCommand {
    HostCommand::SendVideoConfig {
        stream_id,
        version,
        width,
        height,
        bytes: vec![
            0, 0, 0, 1, 0x67, 0x4d, 0, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80,
        ],
    }
}

async fn fill_event_queue(host: &HostRuntime) {
    for _ in 0..HostRuntime::EVENT_CAPACITY * 2 {
        send_command_eventually(host, HostCommand::RequestKeyframe).await;
    }
}

async fn expect_input(host: &HostRuntime, expected: InputEvent) {
    let event = tokio::time::timeout(Duration::from_secs(3), host.next_event())
        .await
        .unwrap()
        .unwrap();
    let HostEvent::Input {
        media_generation,
        input,
    } = event
    else {
        panic!("expected captured input, received {event:?}");
    };
    assert_ne!(media_generation, 0);
    assert!(input.context().is_valid());
    assert_eq!(input.event, expected);
}

async fn expect_release_all(host: &HostRuntime) {
    let event = tokio::time::timeout(Duration::from_secs(3), host.next_event())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event, HostEvent::ReleaseAll);
}

async fn expect_no_input_until_quiet(host: &HostRuntime) {
    loop {
        match tokio::time::timeout(Duration::from_millis(100), host.next_event()).await {
            Ok(Ok(event)) => assert!(
                !matches!(event, HostEvent::Input { .. }),
                "unexpected injection: {event:?}"
            ),
            Err(_) => return,
            Ok(Err(error)) => panic!("host unexpectedly stopped: {error:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn approval_emits_connected_state_for_capture_start() {
    let fixture = HostTestFixture::new().await;
    let host = fixture.start_host().await;
    assert!(matches!(
        fixture.next_event(&host).await,
        HostEvent::ApprovalRequested { .. }
    ));
    fixture.approve(&host).await;
    next_connected_state(&host).await;
    host.destroy();
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
    for _ in 0..8 {
        match fixture.next_event(&host).await {
            HostEvent::Input {
                media_generation,
                input:
                    InputEnvelope {
                        event:
                            InputEvent::MouseWheel {
                                delta_x: -120,
                                delta_y: 240,
                            },
                        stream_id: 9,
                        input_epoch,
                        ..
                    },
            } if media_generation != 0 && input_epoch != 0 => saw_input = true,
            HostEvent::KeyframeRequested => saw_keyframe = true,
            HostEvent::State(_) | HostEvent::Connected { .. } => continue,
            event => panic!("unexpected host event: {event:?}"),
        }
        if saw_input && saw_keyframe {
            break;
        }
    }
    assert!(saw_input && saw_keyframe);

    fixture.reject(&host).await;
    assert!(fixture.received_release_all(&host).await);
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_cancels_a_pending_noise_handshake() {
    let fixture = HostTestFixture::new().await;
    let host = fixture.start_host_only().await;
    let _controller = fixture.join_controller().await;

    host.send(HostCommand::Stop).unwrap();
    let events = terminal_events(&host).await;
    assert!(matches!(
        events.as_slice(),
        [HostEvent::ReleaseAll, HostEvent::State(HostState::Closed)]
    ));
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reject_cancels_pending_capability_negotiation() {
    let fixture = HostTestFixture::new().await;
    let host = fixture.start_host_only().await;
    let _controller = RawController::pair(&fixture).await;
    assert!(matches!(
        fixture.next_event(&host).await,
        HostEvent::ApprovalRequested { .. }
    ));
    fixture.approve(&host).await;
    wait_for_state(&host, HostState::NegotiatingCapabilities).await;

    host.send(HostCommand::Reject).unwrap();
    let events = terminal_events(&host).await;
    assert!(matches!(
        events.as_slice(),
        [HostEvent::ReleaseAll, HostEvent::State(HostState::Closed)]
    ));
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_command_queue_does_not_publish_a_false_terminal_phase() {
    let fixture = HostTestFixture::new().await;
    let host = fixture.start_host_only().await;
    let _controller = RawController::pair(&fixture).await;
    assert!(matches!(
        fixture.next_event(&host).await,
        HostEvent::ApprovalRequested { .. }
    ));
    fixture.approve(&host).await;
    wait_for_state(&host, HostState::NegotiatingCapabilities).await;

    let mut accepted = 0;
    loop {
        match host.send(HostCommand::ReleaseAll) {
            Ok(()) => accepted += 1,
            Err(HostError::CommandQueueFull) => break,
            result => panic!("unexpected command result: {result:?}"),
        }
    }
    assert!(accepted > 0);
    assert!(matches!(
        host.send(HostCommand::Reject),
        Err(HostError::CommandQueueFull)
    ));
    assert_eq!(host.state(), HostState::NegotiatingCapabilities);
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn destroy_completes_when_input_and_keyframe_events_saturate_the_queue() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
    fill_event_queue(&host).await;
    // A same-geometry config is an ordered command-processing barrier that
    // preserves the input epoch and proves the preceding events were produced.
    publish_raw_config(&host, &mut controller, 9, 2, 640, 360).await;
    controller.send_input().await;
    wait_for_state(&host, HostState::Closed).await;

    tokio::time::timeout(
        Duration::from_secs(1),
        tokio::task::spawn_blocking(move || host.destroy()),
    )
    .await
    .expect("destroy must not wait for event-channel capacity")
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_input_context_follows_published_stream_and_geometry_not_encoder_version() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    let first = publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
    let original = controller.input_context.unwrap();
    assert_eq!(first.input_epoch, original.input_epoch);
    let held = InputEvent::Key {
        code: KeyCode::Character('c'),
        pressed: true,
        modifiers: Modifiers::CONTROL,
    };
    controller
        .send_input_for_context(original, held.clone())
        .await;
    expect_input(&host, held).await;

    let encoder_reconfiguration = publish_raw_config(&host, &mut controller, 9, 2, 640, 360).await;
    assert_eq!(encoder_reconfiguration.input_epoch, first.input_epoch);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), host.next_event())
            .await
            .is_err(),
        "same-geometry encoder reconfiguration must not release pressed input"
    );

    let resized = publish_raw_config(&host, &mut controller, 9, 3, 800, 600).await;
    assert!(resized.input_epoch > first.input_epoch);
    expect_release_all(&host).await;
    // Old-context releases are discarded before timestamp validation; they
    // cannot close the new session just because their queue delay was large.
    controller
        .send_input_for_context_at(
            original,
            InputEvent::Key {
                code: KeyCode::Character('c'),
                pressed: false,
                modifiers: Modifiers::CONTROL,
            },
            1,
        )
        .await;
    for old_event in [
        InputEvent::Key {
            code: KeyCode::Character('c'),
            pressed: false,
            modifiers: Modifiers::CONTROL,
        },
        InputEvent::Key {
            code: KeyCode::Character('x'),
            pressed: true,
            modifiers: Modifiers::default(),
        },
        InputEvent::MouseMove {
            x: 900_000,
            y: 900_000,
        },
    ] {
        controller.send_input_for_context(original, old_event).await;
    }
    let current = controller.input_context.unwrap();
    let current_event = InputEvent::MouseWheel {
        delta_x: -5,
        delta_y: 7,
    };
    controller
        .send_input_for_context(current, current_event.clone())
        .await;
    expect_input(&host, current_event).await;
    expect_no_input_until_quiet(&host).await;

    let new_stream = publish_raw_config(&host, &mut controller, 10, 1, 800, 600).await;
    assert!(new_stream.input_epoch > resized.input_epoch);
    expect_release_all(&host).await;
    controller
        .send_input_for_context(
            current,
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            },
        )
        .await;
    let current_event = InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: true,
    };
    controller
        .send_input_for_context(controller.input_context.unwrap(), current_event.clone())
        .await;
    expect_input(&host, current_event).await;
    expect_no_input_until_quiet(&host).await;
    host.destroy();
}

async fn expect_config_rejection_before_publication(
    host: HostRuntime,
    controller: &mut RawController,
    command: HostCommand,
) {
    send_command_eventually(&host, command).await;
    wait_for_state(&host, HostState::Closed).await;
    assert!(
        !matches!(
            tokio::time::timeout(
                Duration::from_millis(100),
                controller.client.next_video_config()
            )
            .await,
            Ok(Ok(_))
        ),
        "invalid decoder configuration was published before rejection"
    );
    let events = terminal_events(&host).await;
    assert!(matches!(
        events.as_slice(),
        [
            HostEvent::ReleaseAll,
            HostEvent::Error(HostError::Protocol(_)),
            HostEvent::State(HostState::Closed)
        ]
    ));
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_rejects_same_geometry_configuration_version_rollback_before_publication() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    publish_raw_config(&host, &mut controller, 9, 2, 640, 360).await;
    expect_config_rejection_before_publication(
        host,
        &mut controller,
        raw_config_command(9, 1, 640, 360),
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_rejects_same_version_sps_profile_changes_before_publication() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    publish_raw_config(&host, &mut controller, 9, 2, 640, 360).await;
    let mut changed = raw_config_command(9, 2, 640, 360);
    if let HostCommand::SendVideoConfig { bytes, .. } = &mut changed {
        bytes[5] = 0x64;
    }
    expect_config_rejection_before_publication(host, &mut controller, changed).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn identical_configuration_retry_is_not_republished_and_quality_version_can_advance() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    let first = publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
    send_command_eventually(&host, raw_config_command(9, 1, 640, 360)).await;
    let quality = publish_raw_config(&host, &mut controller, 9, 2, 640, 360).await;
    assert_eq!(
        quality.config_version, 2,
        "duplicate config must not remain queued for the peer"
    );
    assert_eq!(quality.input_epoch, first.input_epoch);
    let new_stream = publish_raw_config(&host, &mut controller, 10, 1, 640, 360).await;
    assert_eq!(new_stream.config_version, 1);
    assert_ne!(new_stream.input_epoch, quality.input_epoch);
    expect_release_all(&host).await;
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_old_access_unit_version_is_never_relabelled_as_the_new_configuration() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    publish_raw_config(&host, &mut controller, 9, 2, 640, 360).await;
    send_command_eventually(
        &host,
        HostCommand::SendVideoAccessUnit {
            stream_id: 9,
            frame_id: 1,
            config_version: 1,
            bytes: vec![0, 0, 0, 1, 0x65, 1],
        },
    )
    .await;
    wait_for_state(&host, HostState::Closed).await;
    assert!(!matches!(
        tokio::time::timeout(
            Duration::from_millis(100),
            controller.client.next_video_datagram()
        )
        .await,
        Ok(Ok(_))
    ));
    let events = terminal_events(&host).await;
    assert!(matches!(
        events.as_slice(),
        [
            HostEvent::ReleaseAll,
            HostEvent::Error(HostError::InvalidState),
            HostEvent::State(HostState::Closed)
        ]
    ));
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_new_noise_session_never_reuses_the_same_stream_and_geometry_context() {
    let fixture = HostTestFixture::new().await;
    let (host, mut first_controller) = start_approved_raw(&fixture).await;
    let original = publish_raw_config(&host, &mut first_controller, 9, 1, 640, 360).await;
    let retired = first_controller.input_context.unwrap();
    let retired_host_generation = first_controller.host_generation;
    first_controller
        .client
        .close(b"test reconnect with the same capture stream");
    drop(first_controller);
    wait_for_state(&host, HostState::Connecting).await;
    fixture
        .wait_for_new_host_registration(retired_host_generation)
        .await;

    // The authenticated controller identity remains approved, but a new Noise
    // session with reused capture metadata still needs a fresh input context.
    let mut successor = RawController::pair(&fixture).await;
    assert_ne!(successor.host_generation, retired_host_generation);
    successor.negotiate().await;
    next_connected_state(&host).await;
    let current = publish_raw_config(&host, &mut successor, 9, 1, 640, 360).await;
    assert_eq!(
        (current.stream_id, current.width, current.height),
        (original.stream_id, original.width, original.height)
    );
    assert!(current.input_epoch > original.input_epoch);
    let pressed = InputEvent::Key {
        code: KeyCode::Character('c'),
        pressed: true,
        modifiers: Modifiers::default(),
    };
    successor
        .send_input_for_context(successor.input_context.unwrap(), pressed.clone())
        .await;
    expect_input(&host, pressed).await;
    successor
        .send_input_for_context(
            retired,
            InputEvent::Key {
                code: KeyCode::Character('c'),
                pressed: false,
                modifiers: Modifiers::default(),
            },
        )
        .await;
    expect_no_input_until_quiet(&host).await;
    assert_eq!(host.state(), HostState::Connected);
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_calibrates_positive_and_negative_ten_minute_input_clock_offsets() {
    for offset in [-600_000_000_i64, 600_000_000_i64] {
        let fixture = HostTestFixture::new().await;
        let (host, mut controller) = start_approved_raw(&fixture).await;
        publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
        let context = controller.input_context.unwrap();
        let input = InputEvent::MouseMove { x: 10, y: 20 };
        controller
            .send_input_for_context_at(
                context,
                input.clone(),
                now_micros().saturating_add_signed(offset),
            )
            .await;
        expect_input(&host, input).await;
        assert_eq!(host.state(), HostState::Connected, "offset={offset}");
        host.destroy();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn input_clock_jumps_mid_session_preserve_inputs_and_do_not_resurrect_a_replay() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
    let context = controller.input_context.unwrap();
    for (index, offset) in [0_i64, 600_000_000, -600_000_000, 0]
        .into_iter()
        .enumerate()
    {
        let input = InputEvent::MouseMove {
            x: index as i32 + 1,
            y: 20,
        };
        controller
            .send_input_for_context_at(
                context,
                input.clone(),
                now_micros().saturating_add_signed(offset),
            )
            .await;
        expect_input(&host, input).await;
    }
    // A fresh encrypted packet carrying an old application sequence is still
    // a replay; an extreme timestamp cannot recalibrate it back into admission.
    controller
        .send_input_envelope(InputEnvelope {
            sequence: 1,
            timestamp_us: u64::MAX,
            stream_id: context.stream_id,
            input_epoch: context.input_epoch,
            event: InputEvent::Key {
                code: KeyCode::Enter,
                pressed: true,
                modifiers: Modifiers::default(),
            },
        })
        .await;
    expect_no_input_until_quiet(&host).await;
    let input = InputEvent::MouseWheel {
        delta_x: 1,
        delta_y: 2,
    };
    controller
        .send_input_for_context(context, input.clone())
        .await;
    expect_input(&host, input).await;
    assert_eq!(host.state(), HostState::Connected);
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retired_target_high_sequence_cannot_poison_successor_clock_calibration() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
    let old = controller.input_context.unwrap();
    let initial = InputEvent::MouseMove { x: 1, y: 2 };
    controller
        .send_input_for_context_at(
            old,
            initial.clone(),
            now_micros().saturating_add(600_000_000),
        )
        .await;
    expect_input(&host, initial).await;
    let resized = publish_raw_config(&host, &mut controller, 9, 2, 800, 600).await;
    expect_release_all(&host).await;
    controller
        .send_input_envelope(InputEnvelope {
            sequence: u64::MAX,
            timestamp_us: u64::MAX,
            stream_id: old.stream_id,
            input_epoch: old.input_epoch,
            event: InputEvent::Key {
                code: KeyCode::Enter,
                pressed: false,
                modifiers: Modifiers::default(),
            },
        })
        .await;
    let current = controller.input_context.unwrap();
    let input = InputEvent::MouseMove { x: 10, y: 20 };
    controller
        .send_input_for_context_at(
            current,
            input.clone(),
            now_micros().saturating_sub(600_000_000),
        )
        .await;
    expect_input(&host, input).await;
    let quality_only = publish_raw_config(&host, &mut controller, 9, 3, 800, 600).await;
    assert_eq!(quality_only.input_epoch, resized.input_epoch);
    let input = InputEvent::MouseMove { x: 30, y: 40 };
    controller
        .send_input_for_context_at(
            current,
            input.clone(),
            now_micros().saturating_sub(600_000_000),
        )
        .await;
    expect_input(&host, input).await;
    assert_eq!(host.state(), HostState::Connected);
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resize_cannot_publish_a_new_epoch_with_an_old_frame_configuration_version() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
    host.send(HostCommand::SendVideoConfig {
        stream_id: 9,
        version: 1,
        width: 800,
        height: 600,
        bytes: vec![0, 0, 0, 1, 0x67, 0x4d],
    })
    .unwrap();
    wait_for_state(&host, HostState::Closed).await;
    let events = terminal_events(&host).await;
    assert!(matches!(events.first(), Some(HostEvent::ReleaseAll)));
    assert!(events.iter().any(|event| matches!(event, HostEvent::Error(HostError::Protocol(message)) if message.contains("configuration version"))));
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_rejects_input_until_it_has_published_a_video_context() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    assert!(controller.input_context.is_none());
    // Deliberately guessed context: this is not a helper's current-context fallback.
    let ungranted = InputContext {
        stream_id: 9001,
        input_epoch: 9002,
    };
    for event in [
        InputEvent::MouseMove { x: 1, y: 2 },
        InputEvent::Key {
            code: KeyCode::Enter,
            pressed: true,
            modifiers: Modifiers::default(),
        },
        InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        },
    ] {
        controller.send_input_for_context(ungranted, event).await;
    }
    expect_no_input_until_quiet(&host).await;
    assert_eq!(host.state(), HostState::Connected);
    publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
    let event = InputEvent::MouseMove { x: 10, y: 20 };
    controller
        .send_input_for_context(controller.input_context.unwrap(), event.clone())
        .await;
    expect_input(&host, event).await;
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_event_queue_terminates_before_losing_an_epoch_release_barrier() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
    fill_event_queue(&host).await;
    publish_raw_config(&host, &mut controller, 9, 2, 640, 360).await;
    send_command_eventually(
        &host,
        HostCommand::SendVideoConfig {
            stream_id: 9,
            version: 3,
            width: 800,
            height: 600,
            bytes: vec![0, 0, 0, 1, 0x67, 0x4d, 0, 31],
        },
    )
    .await;
    wait_for_state(&host, HostState::Closed).await;
    let events = terminal_events(&host).await;
    assert!(matches!(
        &events[events.len() - 3..],
        [
            HostEvent::ReleaseAll,
            HostEvent::Error(HostError::CommandQueueFull),
            HostEvent::State(HostState::Closed)
        ]
    ));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, HostEvent::Input { .. }))
    );
    host.destroy();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_with_a_full_event_queue_keeps_release_and_closed_bounded() {
    let fixture = HostTestFixture::new().await;
    let (host, mut controller) = start_approved_raw(&fixture).await;
    publish_raw_config(&host, &mut controller, 9, 1, 640, 360).await;
    fill_event_queue(&host).await;
    publish_raw_config(&host, &mut controller, 9, 2, 640, 360).await;
    host.stop();
    wait_for_state(&host, HostState::Closed).await;
    let events = terminal_events(&host).await;
    assert!(matches!(
        &events[events.len() - 2..],
        [HostEvent::ReleaseAll, HostEvent::State(HostState::Closed)]
    ));
    tokio::time::timeout(
        Duration::from_secs(1),
        tokio::task::spawn_blocking(move || host.destroy()),
    )
    .await
    .expect("cancelled host must not wait for event capacity")
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_rejects_controller_originated_video_lanes() {
    for lane in [
        ForbiddenControllerLane::VideoConfig,
        ForbiddenControllerLane::VideoDatagram,
        ForbiddenControllerLane::CursorDatagram,
    ] {
        let fixture = HostTestFixture::new().await;
        let host = fixture.start_host_only().await;
        let mut controller = RawController::pair(&fixture).await;
        assert!(matches!(
            fixture.next_event(&host).await,
            HostEvent::ApprovalRequested { .. }
        ));
        fixture.approve(&host).await;
        controller.negotiate().await;
        wait_for_state(&host, HostState::Connected).await;

        controller.send_forbidden_lane(lane).await;
        let events = terminal_events(&host).await;
        assert!(
            matches!(events.as_slice(), [
            HostEvent::ReleaseAll,
            HostEvent::Error(HostError::Protocol(message)),
            HostEvent::State(HostState::Closed),
        ] if message.contains("host-only transport lane")),
            "lane: {lane:?}"
        );
        host.destroy();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_host_join_times_out_when_the_relay_does_not_acknowledge() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_der = certificate.cert.der().to_vec();
    let server_config = ServerConfig::with_single_cert(
        vec![CertificateDer::from(certificate_der.clone())],
        PrivateKeyDer::Pkcs8(certificate.key_pair.serialize_der().into()),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(certificate_der)).unwrap();
    let client_tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_tls).unwrap(),
    ));
    let endpoint = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let address = endpoint.local_addr().unwrap();
    let (joined_sender, joined_receiver) = oneshot::channel();
    let (release_sender, release_receiver) = oneshot::channel();
    let relay = tokio::spawn(async move {
        let connection = endpoint.accept().await.unwrap().await.unwrap();
        let (_send, mut receive) = connection.accept_bi().await.unwrap();
        let mut length = [0; 4];
        receive.read_exact(&mut length).await.unwrap();
        let mut envelope = vec![0; u32::from_be_bytes(length) as usize];
        receive.read_exact(&mut envelope).await.unwrap();
        desklink_transport::decode_relay_join(&envelope).unwrap();
        joined_sender.send(()).unwrap();
        // Keep QUIC alive while deliberately withholding the join response.
        // The release gate also makes a failing regression terminate cleanly.
        let _ = release_receiver.await;
        connection.close(quinn::VarInt::from_u32(0), b"test finished");
        endpoint.wait_idle().await;
    });
    let mut startup = tokio::task::spawn_blocking(move || {
        HostRuntime::start_from_config(
            QuicClientConfig::with_client_config(address, "localhost", client_config),
            HostIdentity::from_secret_key([31; 16], &[32; 32]),
            SessionId::from_bytes([35; 16]),
            [36; 32],
        )
    });
    tokio::time::timeout(Duration::from_secs(3), joined_receiver)
        .await
        .expect("host did not send its first join")
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(12), &mut startup).await;
    let _ = release_sender.send(());
    relay.await.unwrap();
    let outcome = match outcome {
        Ok(outcome) => outcome.unwrap(),
        Err(_) => {
            // spawn_blocking cannot be aborted; release and reap it before
            // failing so the test never leaves a detached host worker behind.
            let _ = startup.await;
            panic!("first host join exceeded its bounded request deadline");
        }
    };
    match outcome {
        Err(HostError::Transport(message)) => assert!(message.contains("relay join timed out")),
        Err(error) => panic!("unexpected startup failure: {error}"),
        Ok(host) => {
            host.destroy();
            panic!("host became ready without a relay join acknowledgement");
        }
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
        server: relay,
    }
}
