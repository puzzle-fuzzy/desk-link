//! Joint Windows media/input evidence. No desktop capture, native input,
//! clipboard, files, or decoder presentation is started here. The positive
//! path really encodes synthetic BGRA with Microsoft's software MFT, publishes
//! its actual version with geometry, and crosses relay QUIC/Noise in both
//! directions before reaching the production input guard and a recording sink.
#![cfg(windows)]

use super::*;
use crate::{
    encoder::EncoderBackendPreference,
    input::{InputBackend, InputInjectionError},
};
use desklink_crypto::{NoiseInitiator, SessionId};
use desklink_ffi::{ControllerEvent, ControllerRuntime};
use desklink_protocol::{KeyCode, Modifiers};
use desklink_relay::{RelayConfig, RelayServer};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

const STREAM_ID: u64 = 0xfedc_ba98_7654_3210;
const SIDE: u32 = 64;
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

type RecordedInputs = Arc<Mutex<Vec<(InputEvent, VirtualDesktop)>>>;

struct RecordingBackend(RecordedInputs);

impl InputBackend for RecordingBackend {
    fn send(
        &mut self,
        event: &InputEvent,
        desktop: VirtualDesktop,
    ) -> Result<(), InputInjectionError> {
        self.0.lock().unwrap().push((event.clone(), desktop));
        Ok(())
    }
}

fn key(pressed: bool) -> InputEvent {
    InputEvent::Key {
        code: KeyCode::Character('a'),
        pressed,
        modifiers: Modifiers::default(),
    }
}

fn topology() -> DisplayTopologySnapshot {
    DisplayTopologySnapshot::new(
        vec![
            DisplayDescriptor {
                id: 7,
                name: "synthetic left".into(),
                rect: DesktopRect::new(-(SIDE as i32), 0, SIDE, SIDE),
                primary: false,
            },
            DisplayDescriptor {
                id: 19,
                name: "synthetic primary".into(),
                rect: DesktopRect::new(0, 0, SIDE, SIDE),
                primary: true,
            },
        ],
        7,
        DesktopRect::new(-(SIDE as i32), 0, SIDE * 2, SIDE),
    )
    .unwrap()
}

struct Publication {
    prepared: PreparedVideo,
    encoded: EncodedFrame,
}

struct HostFixture<'a> {
    epochs: &'a InputEpochAllocator,
    selected: Mutex<SelectedDesktop>,
    input: Mutex<InputInjector<RecordingBackend>>,
    suspended: AtomicBool,
    updates: watch::Sender<u64>,
    recorded: RecordedInputs,
    encoder: H264Encoder,
    pipeline: HostVideoPipeline,
    policy: HostInboundPolicy,
    samples: u64,
}

impl<'a> HostFixture<'a> {
    fn new(epochs: &'a InputEpochAllocator) -> Self {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let (updates, _) = watch::channel(0);
        let fixture = Self {
            epochs,
            selected: Mutex::new(SelectedDesktop::unconfigured(STREAM_ID).unwrap()),
            input: Mutex::new(InputInjector::with_backend(
                VirtualDesktop::single(DesktopRect::new(0, 0, 1, 1)),
                RecordingBackend(recorded.clone()),
            )),
            suspended: AtomicBool::new(false),
            updates,
            recorded,
            encoder: H264Encoder::new_with_settings_and_backend(
                SIDE,
                SIDE,
                H264EncoderSettings::default(),
                EncoderBackendPreference::SoftwareOnly,
            )
            .expect("Microsoft software MFT for synthetic frames"),
            pipeline: HostVideoPipeline::new(STREAM_ID),
            policy: HostInboundPolicy::new(STREAM_ID),
            samples: 0,
        };
        assert_eq!(
            fixture.encoder.backend_info().unwrap().kind,
            "microsoft_software_mft"
        );
        assert!(
            fixture
                .coordinator()
                .publish(topology(), fixture.encoder.config_version())
                .unwrap()
        );
        fixture
    }

    fn coordinator(&self) -> CaptureDesktopCoordinator<'_, RecordingBackend> {
        CaptureDesktopCoordinator {
            selected: &self.selected,
            input: &self.input,
            epochs: self.epochs,
            input_suspended: &self.suspended,
            updates: &self.updates,
        }
    }

    fn reconfigure(&mut self, topology: DisplayTopologySnapshot, settings: H264EncoderSettings) {
        let rect = topology.active_display().rect;
        let previous_version = self.encoder.config_version();
        self.encoder
            .reconfigure_for_source(rect.width, rect.height, settings)
            .unwrap();
        assert!(self.encoder.config_version() > previous_version);
        assert!(
            self.coordinator()
                .publish(topology, self.encoder.config_version())
                .unwrap()
        );
    }

    fn publication(&mut self) -> Publication {
        let (context, rect) = {
            let selected = self.selected.lock().unwrap();
            (selected.input_context().unwrap(), selected.desktop.rect)
        };
        self.samples += 1;
        let mut pixels = vec![0_u8; (rect.width * rect.height * 4) as usize];
        for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&[
                (index as u64 + self.samples) as u8,
                (index / SIDE as usize) as u8,
                0x50,
                0xff,
            ]);
        }
        let encoded = self
            .encoder
            .encode(
                CapturedFrame {
                    width: rect.width,
                    height: rect.height,
                    timestamp_us: now_micros(),
                    pixels,
                },
                true,
            )
            .expect("software MFT IDR from synthetic BGRA");
        assert!(encoded.keyframe);
        assert!(!encoded.access_unit.is_empty());
        assert!(
            encoded
                .sequence_header
                .as_ref()
                .is_some_and(|s| !s.is_empty())
        );
        assert_eq!(encoded.config_version, self.encoder.config_version());
        let (width, height) = self.encoder.dimensions();
        let PrepareVideo::Ready(prepared) = self
            .pipeline
            .prepare(encoded.clone(), width, height, context.input_epoch)
            .unwrap()
        else {
            panic!("software IDR must publish its configuration");
        };
        assert!(prepared.video_config.is_some());
        Publication { prepared, encoded }
    }

    fn inject(&mut self, plaintext: &[u8]) -> bool {
        inject_desktop_input(
            &mut self.policy,
            &self.input,
            plaintext,
            now_micros(),
            &self.selected,
            &self.suspended,
        )
        .unwrap()
    }

    fn recorded(&self) -> Vec<(InputEvent, VirtualDesktop)> {
        self.recorded.lock().unwrap().clone()
    }
}

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

struct Loopback {
    relay: Arc<RelayServer>,
    task: tokio::task::JoinHandle<()>,
    config: QuicClientConfig,
    host_identity: DeviceIdentity,
    controller_identity: DeviceIdentity,
}

impl Loopback {
    async fn new() -> Self {
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
        let config = QuicClientConfig::with_client_config(
            relay.local_addr().unwrap(),
            "localhost",
            client_tls,
        );
        let running = relay.clone();
        let task = tokio::spawn(async move { running.run().await.unwrap() });
        Self {
            relay,
            task,
            config,
            host_identity: DeviceIdentity::generate(&mut OsRng),
            controller_identity: DeviceIdentity::generate(&mut OsRng),
        }
    }

    async fn connect(&self, tag: u8) -> ConnectedPair {
        let session_id = SessionId::from_bytes([tag; 16]);
        let authentication = [0x73; 32];
        let host = QuicClient::connect(self.config.clone()).await.unwrap();
        host.join(RelayJoin::host_with_participant(
            session_id,
            authentication,
            self.host_identity.device_id,
        ))
        .await
        .unwrap();
        let peer = QuicClient::connect(self.config.clone()).await.unwrap();
        peer.join(RelayJoin::controller_with_participant(
            session_id,
            authentication,
            self.controller_identity.device_id,
        ))
        .await
        .unwrap();
        let host_identity = self.host_identity.with_secret_key_bytes(|secret| {
            DeviceIdentity::from_secret_key(self.host_identity.device_id, secret)
        });
        let controller_identity = self.controller_identity.with_secret_key_bytes(|secret| {
            DeviceIdentity::from_secret_key(self.controller_identity.device_id, secret)
        });
        let controller_key = self.controller_identity.verify_key();
        let host_key = self.host_identity.verify_key();
        let host_negotiation = async {
            let (secure, generation) =
                perform_noise_handshake(&host, host_identity, &ExpectedController(controller_key))
                    .await
                    .unwrap();
            let secure = Arc::new(AsyncMutex::new(secure));
            receive_controller_capabilities(
                &host,
                &secure,
                generation,
                Arc::new(AtomicBool::new(false)),
                STREAM_ID,
            )
            .await
            .unwrap();
            let topology = topology();
            send_host_capabilities(
                &host,
                &secure,
                generation,
                SIDE,
                SIDE,
                H264Profile::Main,
                &remote_displays(topology.displays()).unwrap(),
                topology.active_display_id(),
            )
            .await
            .unwrap();
            (secure, generation)
        };
        let controller_negotiation = ControllerRuntime::connect_for_platform(
            peer,
            controller_identity,
            host_key,
            Platform::Windows,
        );
        let ((secure, generation), controller) = tokio::time::timeout(TEST_TIMEOUT, async {
            tokio::join!(host_negotiation, controller_negotiation)
        })
        .await
        .expect("local authenticated negotiation");
        ConnectedPair {
            host,
            secure,
            generation,
            controller: controller.unwrap(),
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.relay.close();
        self.task.abort();
    }
}

struct ConnectedPair {
    host: QuicClient,
    secure: SharedSecureSession,
    generation: u64,
    controller: ControllerRuntime,
}

impl ConnectedPair {
    async fn publish(&mut self, publication: Publication) -> VideoConfig {
        let ciphertext = seal(
            &self.secure,
            SecureLane::VideoConfig,
            publication.prepared.video_config.as_ref().unwrap(),
        )
        .await
        .unwrap();
        self.host
            .send_video_config_for_generation(self.generation, ciphertext)
            .await
            .unwrap();
        let configuration = tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                match self.controller.next_event().await.unwrap() {
                    ControllerEvent::VideoConfig(config) => break config,
                    ControllerEvent::Control(_) => {}
                    event => panic!("unexpected event before configuration: {event:?}"),
                }
            }
        })
        .await
        .expect("production Controller consumes pipeline configuration");
        assert_eq!(
            configuration.config_version,
            publication.encoded.config_version
        );
        assert_eq!(
            configuration.sequence_header,
            *publication.encoded.sequence_header.as_ref().unwrap()
        );
        for packet in publication.prepared.datagrams {
            let ciphertext = seal(&self.secure, SecureLane::VideoReliable, &packet)
                .await
                .unwrap();
            self.host
                .send_video_reliable_for_generation(self.generation, ciphertext)
                .await
                .unwrap();
        }
        let frame = tokio::time::timeout(TEST_TIMEOUT, async {
            loop {
                match self.controller.next_event().await.unwrap() {
                    ControllerEvent::H264AccessUnit(frame) => break frame,
                    ControllerEvent::Control(_) => {}
                    event => panic!("unexpected event before encoded frame: {event:?}"),
                }
            }
        })
        .await
        .expect("production Controller assembles real software encoded IDR");
        assert_eq!(frame.stream_id, configuration.stream_id);
        assert_eq!(frame.config_version, configuration.config_version);
        assert_eq!(
            (frame.width, frame.height),
            (configuration.width, configuration.height)
        );
        assert_eq!(frame.data, publication.encoded.access_unit);
        configuration
    }

    async fn receive_input(&self) -> (Vec<u8>, Vec<u8>) {
        let ciphertext = tokio::time::timeout(
            TEST_TIMEOUT,
            self.host.next_input_for_generation(self.generation),
        )
        .await
        .expect("input crosses local relay")
        .unwrap();
        let plaintext = open(&self.secure, SecureLane::Input, &ciphertext)
            .await
            .expect("production host Noise opens controller input");
        (ciphertext, plaintext)
    }

    async fn send_and_inject(
        &self,
        host: &mut HostFixture<'_>,
        context: InputContext,
        event: InputEvent,
    ) -> bool {
        self.controller
            .input_sender()
            .send_input_with_context(context, event.clone())
            .await
            .unwrap();
        let (_, plaintext) = self.receive_input().await;
        let decoded = decode_session_input(&plaintext).unwrap();
        assert_eq!(decoded.context(), context);
        assert_eq!(decoded.event, event);
        host.inject(&plaintext)
    }
}

impl Drop for ConnectedPair {
    fn drop(&mut self) {
        self.host.close(b"joint synthetic host finished");
    }
}

fn context(configuration: &VideoConfig) -> InputContext {
    InputContext {
        stream_id: configuration.stream_id,
        input_epoch: configuration.input_epoch,
    }
}

#[tokio::test]
async fn joint_software_video_config_geometry_and_input_share_the_published_epoch() {
    let epochs = InputEpochAllocator::from_seed(0xf000_0000_0000_0001).unwrap();
    let mut host = HostFixture::new(&epochs);
    let network = Loopback::new().await;
    let mut pair = network.connect(0x91).await;
    let initial = pair.publish(host.publication()).await;
    let initial_context = context(&initial);
    assert_eq!(initial_context.stream_id, STREAM_ID);
    assert_eq!(
        host.selected.lock().unwrap().input_context(),
        Some(initial_context)
    );
    assert!(
        pair.send_and_inject(&mut host, initial_context, key(true))
            .await
    );
    assert_eq!(host.recorded()[0].1.rect.left, -(SIDE as i32));

    // Reusing the encoder version that established this mapping cannot create
    // a geometry boundary. The failed publication retains the old held key.
    let moved = topology().select(19).unwrap().unwrap();
    assert!(matches!(
        host.coordinator()
            .publish(moved.clone(), host.encoder.config_version()),
        Err(HostRuntimeError::InconsistentVideoConfig)
    ));
    assert_eq!(host.recorded().len(), 1);

    // A real codec rebuild advances the encoder version, while retaining the
    // same topology/context and the key held by that context.
    let codec_settings = H264EncoderSettings {
        bitrate: H264EncoderSettings::default().bitrate / 2,
        ..H264EncoderSettings::default()
    };
    host.reconfigure(topology(), codec_settings);
    assert_eq!(
        host.recorded().len(),
        1,
        "codec-only change must retain held input"
    );
    let codec = pair.publish(host.publication()).await;
    assert!(codec.config_version > initial.config_version);
    assert_eq!(context(&codec), initial_context);
    assert!(
        pair.send_and_inject(
            &mut host,
            context(&codec),
            InputEvent::MouseMove {
                x: 500_000,
                y: 500_000
            },
        )
        .await
    );

    // Equal source dimensions and SPS still require a new encoder version and
    // mapping epoch. Rebuild the real encoder, never fabricate a frame version.
    assert_eq!(host.recorded().len(), 2);
    host.reconfigure(moved, codec_settings);
    let boundary_inputs = host.recorded();
    assert_eq!(boundary_inputs.len(), 3);
    assert_eq!(boundary_inputs[2].0, key(false));
    assert_eq!(boundary_inputs[2].1.rect.left, -(SIDE as i32));
    let geometry = pair.publish(host.publication()).await;
    assert_eq!(
        (geometry.width, geometry.height),
        (codec.width, codec.height)
    );
    assert_eq!(geometry.sequence_header, codec.sequence_header);
    assert!(geometry.config_version > codec.config_version);
    assert_ne!(context(&geometry), initial_context);
    assert_eq!(context(&geometry).stream_id, initial_context.stream_id);

    assert!(
        pair.send_and_inject(&mut host, context(&geometry), key(true))
            .await
    );
    let after_current_press = host.recorded();
    assert_eq!(after_current_press.last().unwrap().1.rect.left, 0);
    assert!(
        !pair
            .send_and_inject(&mut host, initial_context, key(false))
            .await
    );
    assert!(
        !pair
            .send_and_inject(
                &mut host,
                initial_context,
                InputEvent::MouseMove {
                    x: 1_000_000,
                    y: 1_000_000
                },
            )
            .await
    );
    assert_eq!(host.recorded(), after_current_press);
    // This release is produced by the actual pressed-state tracker: rejecting
    // the old release must leave the new epoch's press held until cleanup.
    assert_eq!(
        host.input.lock().unwrap().release_all().unwrap(),
        vec![key(false)]
    );
    assert_eq!(host.recorded().last().unwrap().1.rect.left, 0);
}

fn fresh_noise_sessions(network: &Loopback) -> (SecureSession, SecureSession) {
    let initiator_identity = network.controller_identity.with_secret_key_bytes(|secret| {
        DeviceIdentity::from_secret_key(network.controller_identity.device_id, secret)
    });
    let responder_identity = network.host_identity.with_secret_key_bytes(|secret| {
        DeviceIdentity::from_secret_key(network.host_identity.device_id, secret)
    });
    let (mut initiator, first) =
        NoiseInitiator::start(initiator_identity, network.host_identity.verify_key()).unwrap();
    let (mut responder, second) = NoiseResponder::accept(
        &first,
        responder_identity,
        network.controller_identity.verify_key(),
    )
    .unwrap();
    let finish = initiator.receive(&second).unwrap();
    responder.receive(&finish).unwrap();
    (
        initiator
            .finish()
            .unwrap()
            .into_secure_session(SecureRole::Initiator),
        responder
            .finish()
            .unwrap()
            .into_secure_session(SecureRole::Responder),
    )
}

#[tokio::test]
async fn joint_new_noise_same_stream_rejects_old_plaintext_and_old_releases() {
    let epochs = InputEpochAllocator::from_seed(0xe000_0000_0000_0001).unwrap();
    let network = Loopback::new().await;
    let mut first_host = HostFixture::new(&epochs);
    let mut first = network.connect(0x92).await;
    let first_config = first.publish(first_host.publication()).await;
    let old_context = context(&first_config);
    first
        .controller
        .send_input_with_context(old_context, key(true))
        .await
        .unwrap();
    let (old_ciphertext, old_plaintext) = first.receive_input().await;
    assert!(first_host.inject(&old_plaintext));
    let first_binding = first.secure.lock().await.video_path_binding();
    first_host.input.lock().unwrap().release_all().unwrap();
    drop(first);

    // Same identities and full-width stream ID, fresh transport/Noise and a
    // process allocator shared with the previous host attempt. Both contexts
    // come exclusively from pipeline configs consumed by ControllerRuntime.
    let mut host = HostFixture::new(&epochs);
    let mut pair = network.connect(0x93).await;
    let config = pair.publish(host.publication()).await;
    let current = context(&config);
    assert_eq!(current.stream_id, old_context.stream_id);
    assert_ne!(current.input_epoch, old_context.input_epoch);
    assert_ne!(pair.secure.lock().await.video_path_binding(), first_binding);
    assert!(
        pair.secure
            .lock()
            .await
            .open(SecureLane::Input, &old_ciphertext)
            .is_err()
    );

    // The production sender can carry an event admitted before reconnection.
    // It is valid fresh-Noise ciphertext, but the mapping guard must reject the
    // old context before consuming its wire-order sequence.
    assert!(
        !pair
            .send_and_inject(&mut host, old_context, key(true))
            .await
    );
    assert_eq!(host.policy.last_input_sequence, 0);
    assert!(host.recorded().is_empty());
    assert!(pair.send_and_inject(&mut host, current, key(true)).await);
    let current_inputs = host.recorded();
    let current_sequence = host.policy.last_input_sequence;

    // Additional packet-level replay uses the exact previously decrypted bytes,
    // with no timestamp/sequence/context edits, in another real fresh Noise
    // session. This checks plaintext rebinding separately from the production
    // sender's new serialization above; it deliberately is not a QUIC claim.
    let (mut reencryptor, mut receiver) = fresh_noise_sessions(&network);
    let rebound_ciphertext = reencryptor.seal(SecureLane::Input, &old_plaintext).unwrap();
    let rebound_plaintext = receiver
        .open(SecureLane::Input, &rebound_ciphertext)
        .unwrap();
    assert_eq!(rebound_plaintext, old_plaintext);
    assert!(!host.inject(&rebound_plaintext));
    assert_eq!(host.policy.last_input_sequence, current_sequence);
    assert_eq!(host.recorded(), current_inputs);
    assert!(
        !pair
            .send_and_inject(&mut host, old_context, key(false))
            .await
    );
    assert_eq!(host.recorded(), current_inputs);
    assert_eq!(
        host.input.lock().unwrap().release_all().unwrap(),
        vec![key(false)]
    );

    // Rejected stale packets do not close or poison the active encrypted lane.
    assert!(pair.send_and_inject(&mut host, current, key(true)).await);
    assert!(pair.send_and_inject(&mut host, current, key(false)).await);
    assert!(host.input.lock().unwrap().release_all().unwrap().is_empty());
}
