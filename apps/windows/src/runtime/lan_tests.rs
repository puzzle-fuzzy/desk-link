//! Actual Host adapter / ControllerRuntime loopback. Synthetic encoded media
//! and a recording input backend exercise transport without capture, decoder
//! presentation, clipboard access, file dialogs or native input injection.
#![cfg(windows)]
use super::*;
use crate::{
    audio::CapturedAudioFrame,
    input::{InputBackend, InputInjectionError},
};
use blake2::{Blake2s256, Digest};
use desklink_crypto::SessionId;
use desklink_ffi::{
    ControllerEvent, ControllerInputSender, ControllerRuntime, ControllerTransferSender,
};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{SessionDataHealth, SessionLaneHealth};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::{path::PathBuf, sync::atomic::AtomicUsize};

const STREAM: u64 = 0xfedc_ba98_7654_3210;
const LIMIT: Duration = Duration::from_secs(20);

struct Task(tokio::task::JoinHandle<()>);
impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct Fixture {
    relay: Arc<RelayServer>,
    _task: Task,
    host: Arc<SessionDataPlane>,
    secure: SharedSecureSession,
    controller: Option<ControllerRuntime>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.host.close();
        self.relay.close();
    }
}

struct Expected(VerifyingKey);
impl ControllerAuthorizer for Expected {
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

impl Fixture {
    async fn new() -> Self {
        Self::with_channels(desklink_protocol::LAN_DATA_KNOWN_CHANNELS).await
    }

    async fn with_channels(channels: u16) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let der = certificate.cert.der().to_vec();
        let server_tls = quinn::ServerConfig::with_single_cert(
            vec![CertificateDer::from(der.clone())],
            PrivateKeyDer::Pkcs8(certificate.key_pair.serialize_der().into()),
        )
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(der)).unwrap();
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let client_tls = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
        ));
        let relay = Arc::new(
            RelayServer::bind(
                "127.0.0.1:0".parse().unwrap(),
                server_tls,
                RelayConfig::default(),
            )
            .await
            .unwrap(),
        );
        let running = relay.clone();
        let task = Task(tokio::spawn(async move {
            running.run().await.unwrap();
        }));
        let config = QuicClientConfig::with_client_config(
            relay.local_addr().unwrap(),
            "localhost",
            client_tls,
        );
        let host_identity = DeviceIdentity::generate(&mut OsRng);
        let peer_identity = DeviceIdentity::generate(&mut OsRng);
        let host_key = host_identity.verify_key();
        let peer_key = peer_identity.verify_key();
        let host = Arc::new(QuicClient::connect(config.clone()).await.unwrap());
        let peer = QuicClient::connect(config).await.unwrap();
        let session = SessionId::from_bytes([0x24; 16]);
        host.join(RelayJoin::host_with_participant(
            session,
            [0x41; 32],
            host_identity.device_id,
        ))
        .await
        .unwrap();
        peer.join(RelayJoin::controller_with_participant(
            session,
            [0x41; 32],
            peer_identity.device_id,
        ))
        .await
        .unwrap();
        let host_bootstrap = async {
            let (secure, generation) =
                perform_noise_handshake(&host, host_identity, &Expected(peer_key))
                    .await
                    .unwrap();
            let secure = Arc::new(AsyncMutex::new(secure));
            let peer_caps = receive_controller_capabilities(
                &host,
                &secure,
                generation,
                Arc::new(AtomicBool::new(false)),
                STREAM,
            )
            .await
            .unwrap();
            let local_caps = send_host_bootstrap_capabilities(
                &host,
                &secure,
                generation,
                64,
                64,
                H264Profile::Main,
                Some(desklink_protocol::LanDataCapabilities {
                    channels,
                    ..lan_session::capabilities()
                }),
            )
            .await
            .unwrap();
            let agreement = NegotiatedLanData::from_authenticated_caps(
                &local_caps,
                &peer_caps,
                secure.lock().await.video_path_binding(),
                generation,
            )
            .unwrap()
            .expect("real Windows peers must opt in");
            let data = SessionDataPlane::relay_only(
                agreement,
                secure.clone(),
                Arc::new(QuicRelayBackend::new(host.clone(), generation).unwrap()),
            )
            .await
            .unwrap();
            (data, secure)
        };
        let ((data, secure), controller) = tokio::time::timeout(LIMIT, async {
            tokio::join!(
                host_bootstrap,
                ControllerRuntime::connect_for_platform(
                    peer,
                    peer_identity,
                    host_key,
                    Platform::Windows
                )
            )
        })
        .await
        .unwrap();
        Self {
            relay,
            _task: task,
            host: data,
            secure,
            controller: Some(controller.unwrap()),
        }
    }
}

fn all_direct(health: SessionDataHealth) -> bool {
    health
        .lanes
        .iter()
        .all(|lane| matches!(lane, SessionLaneHealth::Direct(_)))
}

fn all_negotiated_direct(health: SessionDataHealth, channels: u16) -> bool {
    health.lanes.iter().enumerate().all(|(index, lane)| {
        channels & (1_u16 << index) == 0 || matches!(lane, SessionLaneHealth::Direct(_))
    })
}

#[derive(Clone)]
struct Recording(Arc<Mutex<Vec<InputEvent>>>);
impl InputBackend for Recording {
    fn send(&mut self, event: &InputEvent, _: VirtualDesktop) -> Result<(), InputInjectionError> {
        self.0.lock().unwrap().push(event.clone());
        Ok(())
    }
}
fn key(pressed: bool) -> InputEvent {
    InputEvent::Key {
        code: desklink_protocol::KeyCode::Character('a'),
        pressed,
        modifiers: desklink_protocol::Modifiers::default(),
    }
}

struct LanDriver {
    _controller: Task,
    _control: Task,
    _capture_receiver: std::sync::mpsc::Receiver<CaptureWorkerCommand>,
    _topology: watch::Sender<u64>,
    stopping: Arc<AtomicBool>,
    peer_health: watch::Receiver<Option<SessionDataHealth>>,
}

async fn connect_lan(
    f: &mut Fixture,
) -> (
    ControllerInputSender,
    ControllerTransferSender,
    mpsc::Receiver<ControllerEvent>,
    LanDriver,
) {
    let mut controller = f.controller.take().unwrap();
    let input_sender = controller.input_sender();
    let transfer_sender = controller.transfer_sender();
    let (events_tx, events) = mpsc::channel(64);
    let (peer_health_tx, mut peer_health) = watch::channel(controller.session_data_health());
    let (failure_tx, failure) = watch::channel(None::<String>);
    let stopping = Arc::new(AtomicBool::new(false));
    let stopping_driver = stopping.clone();
    let feedback_count = Arc::new(AtomicUsize::new(0));
    let sent_feedback = feedback_count.clone();
    let _controller = Task(tokio::spawn(async move {
        let mut include_transfer = true;
        loop {
            // Keep authenticated Control traffic active throughout probe
            // and switch, without ever cancelling next_event mid-action.
            for _ in 0..4 {
                if let Err(error) = controller.report_video_network_feedback(1, 0, 0, 0).await {
                    peer_health_tx.send_replace(controller.session_data_health());
                    if !stopping_driver.load(Ordering::Acquire) {
                        eprintln!("Host LAN fixture feedback failed: {error}");
                        failure_tx.send_replace(Some(error.to_string()));
                    }
                    return;
                }
                sent_feedback.fetch_add(1, Ordering::Release);
            }
            let event = match controller.next_event_with_transfer(include_transfer).await {
                Ok(event) => event,
                Err(desklink_ffi::ControllerError::SessionData(
                    SessionDataError::NoPath(ChannelKind::Transfer)
                    | SessionDataError::Interrupted(ChannelKind::Transfer),
                )) => {
                    include_transfer = false;
                    peer_health_tx.send_replace(controller.session_data_health());
                    continue;
                }
                Err(error) => {
                    peer_health_tx.send_replace(controller.session_data_health());
                    if !stopping_driver.load(Ordering::Acquire) {
                        eprintln!("Host LAN fixture Controller driver failed: {error}");
                        failure_tx.send_replace(Some(error.to_string()));
                    }
                    return;
                }
            };
            peer_health_tx.send_replace(controller.session_data_health());
            if events_tx.send(event).await.is_err() {
                return;
            }
        }
    }));
    let binding = f.secure.lock().await.video_path_binding();
    let session = Arc::new(
        DirectLanSession::bind(
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:9".parse().unwrap(),
            91,
            binding,
            now_unix_s(),
        )
        .unwrap(),
    );
    let candidate = session.candidate().clone();
    let host = f.host.clone();
    let secure = f.secure.clone();
    let (capture_receiver_keepalive, capture_commands) = {
        let (send, receive) = std::sync::mpsc::sync_channel(CAPTURE_COMMAND_QUEUE_CAPACITY);
        (receive, send)
    };
    let (topology_keepalive, topology_updates) = watch::channel(0);
    let (audio_enabled_updates, _audio_receiver) = watch::channel(true);
    let (input_state_updates, _input_states) = watch::channel(None);
    let native_input = Arc::new(Mutex::new(InputInjector::new(VirtualDesktop::single(
        DesktopRect::new(0, 0, 1, 1),
    ))));
    let control_context = HostInboundContext {
        input_state_updates,
        capture_commands,
        selected_desktop: Arc::new(Mutex::new(SelectedDesktop::unconfigured(STREAM).unwrap())),
        input: native_input,
        input_suspended: Arc::new(AtomicBool::new(false)),
        topology_updates,
        audio_enabled: Arc::new(AtomicBool::new(true)),
        audio_enabled_updates,
        audio_queue: Arc::new(Mutex::new(AudioFrameQueue::new(2))),
        audio_notify: Arc::new(Notify::new()),
        direct_session: Some(session),
        direct_connection: Arc::new(AsyncMutex::new(None)),
        media_observer: None,
    };
    let stopping_control = stopping.clone();
    let _switch = Task(tokio::spawn(async move {
        if let Err(error) = receive_host_control(
            host,
            secure,
            STREAM,
            Arc::new(AtomicBool::new(false)),
            control_context,
        )
        .await
            && !stopping_control.load(Ordering::Acquire)
        {
            panic!("production Host control adapter failed: {error}");
        }
    }));
    send_control_data(
        &f.host,
        &ControlMessage::VideoPathCandidateOffer { candidate },
    )
    .await
    .unwrap();
    let channels = f.host.agreement().unwrap().channels();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if let Some(error) = failure.borrow().as_ref() {
                panic!("Controller failed during LAN activation: {error}");
            }
            send_control_data(&f.host, &ControlMessage::InputState { reason: None })
                .await
                .unwrap();
            if all_negotiated_direct(*f.host.health().borrow(), channels)
                && peer_health
                    .borrow()
                    .is_some_and(|health| all_negotiated_direct(health, channels))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = peer_health.borrow_and_update();
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "LAN activation timed out: host={:?}, controller={:?}, driver={:?}",
            *f.host.health().borrow(),
            *peer_health.borrow(),
            *failure.borrow()
        )
    });
    assert!(
        feedback_count.load(Ordering::Acquire) >= 8,
        "activation ran under sustained authenticated Control feedback"
    );
    (
        input_sender,
        transfer_sender,
        events,
        LanDriver {
            _controller,
            _control: _switch,
            _capture_receiver: capture_receiver_keepalive,
            _topology: topology_keepalive,
            stopping,
            peer_health,
        },
    )
}

async fn remove_owned_relay(f: &Fixture) {
    f.relay.close();
    let mut health = f.host.health();
    while health.borrow().relay_connected {
        health.changed().await.unwrap();
    }
    assert!(health.borrow().can_continue_without_relay);
}

#[tokio::test]
async fn host_lan_adapter_keeps_captured_input_media_and_transfer_after_relay_loss() {
    let stage = Mutex::new("bootstrap");
    tokio::time::timeout(LIMIT, async {
        let mut f = Fixture::new().await;
        *stage.lock().unwrap() = "authenticated probe and eight-lane admission fences";
        let (input_sender, transfer_sender, mut events, drive) = connect_lan(&mut f).await;
        // Both endpoints completed every reliable-lane admission fence before
        // closing only this fixture's real relay listener/connections.
        *stage.lock().unwrap() = "owned relay shutdown";
        remove_owned_relay(&f).await;

        *stage.lock().unwrap() = "captured input over LAN only";
        let epoch = InputEpochAllocator::from_seed(0xfedc_ba98_1234_5678)
            .unwrap()
            .allocate()
            .unwrap();
        let context = InputContext {
            stream_id: STREAM,
            input_epoch: epoch.get(),
        };
        let topology = DisplayTopologySnapshot::new(
            vec![DisplayDescriptor {
                id: 7,
                name: "synthetic".into(),
                rect: DesktopRect::new(0, 0, 64, 64),
                primary: true,
            }],
            7,
            DesktopRect::new(0, 0, 64, 64),
        )
        .unwrap();
        let selected = Arc::new(Mutex::new(SelectedDesktop::from_snapshot(
            topology,
            NonZeroU64::new(STREAM).unwrap(),
            epoch,
            NonZeroU32::new(1).unwrap(),
        )));
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let input = Arc::new(Mutex::new(InputInjector::with_backend(
            VirtualDesktop::single(DesktopRect::new(0, 0, 64, 64)),
            Recording(recorded.clone()),
        )));
        let suspended = Arc::new(AtomicBool::new(false));
        let (states, _) = watch::channel(None);
        let host = f.host.clone();
        let input_for_task = input.clone();
        let selected_for_task = selected.clone();
        let suspended_for_task = suspended.clone();
        let input_task = tokio::spawn(async move {
            receive_host_input(
                host,
                STREAM,
                input_for_task,
                selected_for_task,
                states,
                suspended_for_task,
            )
            .await
        });
        input_sender
            .send_input_with_context(context, key(true))
            .await
            .unwrap();
        while !recorded.lock().unwrap().contains(&key(true)) {
            tokio::task::yield_now().await;
        }
        let stale = InputContext {
            input_epoch: context.input_epoch - 1,
            ..context
        };
        input_sender
            .send_input_with_context(stale, key(false))
            .await
            .unwrap();
        // Ordered valid input after the stale release proves it was processed.
        input_sender
            .send_input_with_context(context, InputEvent::MouseMove { x: 1, y: 2 })
            .await
            .unwrap();
        while recorded.lock().unwrap().len() < 2 {
            tokio::task::yield_now().await;
        }
        assert!(!recorded.lock().unwrap().contains(&key(false)));

        *stage.lock().unwrap() = "Transfer plaintext adapters";
        let transfer = TransferMessage::ClipboardRequest { request_id: 71 };
        transfer_sender.send(transfer.clone()).await.unwrap();
        assert_eq!(
            decode_transfer(&f.host.recv(ChannelKind::Transfer).await.unwrap()).unwrap(),
            transfer
        );
        let response = TransferMessage::ClipboardResult {
            request_id: 71,
            result: TransferResult::Completed,
        };
        send_transfer_response(&f.host, response.clone())
            .await
            .unwrap();

        *stage.lock().unwrap() = "production video and Opus audio over LAN only";
        let video_queue = Arc::new(Mutex::new(LatestFrameQueue::new(2)));
        video_queue
            .lock()
            .unwrap()
            .push_latest(EncodedDesktopFrame {
                frame: EncodedFrame {
                    frame_id: 1,
                    config_version: 1,
                    keyframe: true,
                    timestamp_us: now_micros(),
                    access_unit: vec![0x65; 2500],
                    sequence_header: Some(vec![0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2]),
                },
                width: 64,
                height: 64,
                input_epoch: context.input_epoch,
            });
        let video_notify = Arc::new(Notify::new());
        video_notify.notify_one();
        let host = f.host.clone();
        let _video = Task(tokio::spawn(async move {
            let _ = send_video_loop(
                host,
                STREAM,
                video_queue,
                video_notify,
                Arc::new(AtomicBool::new(false)),
            )
            .await;
        }));
        let audio_queue = Arc::new(Mutex::new(AudioFrameQueue::new(2)));
        audio_queue
            .lock()
            .unwrap()
            .push_drop_oldest(CapturedAudioFrame {
                capture_timestamp_us: now_micros(),
                payload: vec![0; desklink_protocol::MAX_AUDIO_PAYLOAD_BYTES],
            });
        let audio_notify = Arc::new(Notify::new());
        audio_notify.notify_one();
        let (_capture, capture_state) = watch::channel(AudioCaptureState::Available);
        let (_enable, enabled) = watch::channel(true);
        let host = f.host.clone();
        let audio_queue_for_task = audio_queue.clone();
        let audio_notify_for_task = audio_notify.clone();
        let _audio = Task(tokio::spawn(async move {
            let _ = send_audio_loop(
                host,
                STREAM,
                audio_queue_for_task,
                audio_notify_for_task,
                capture_state,
                enabled,
            )
            .await;
        }));
        let mut seen = [false; 4];
        while !seen.iter().all(|seen| *seen) {
            match events.recv().await.unwrap() {
                ControllerEvent::Transfer(message) if message == response => seen[0] = true,
                ControllerEvent::VideoConfig(config) => {
                    assert_eq!(config.input_epoch, context.input_epoch);
                    seen[1] = true;
                }
                ControllerEvent::H264AccessUnit(frame) => {
                    assert_eq!(frame.data, vec![0x65; 2500]);
                    seen[2] = true;
                }
                ControllerEvent::Audio(packet) => {
                    assert_eq!(packet.codec, AudioCodec::Opus);
                    seen[3] = true;
                }
                _ => {}
            }
        }
        *stage.lock().unwrap() = "real bidirectional files and blocked disk isolation";
        real_files_with_blocked_disk(
            f.host.clone(),
            &transfer_sender,
            &input_sender,
            context,
            recorded.clone(),
            &mut events,
            audio_queue,
            audio_notify,
        )
        .await;
        assert!(all_direct(*f.host.health().borrow()));
        *stage.lock().unwrap() = "owned close and input release";
        drive.stopping.store(true, Ordering::Release);
        f.host.close();
        assert!(input_task.await.unwrap().is_err());
        assert!(selected.lock().unwrap().input_closed);
        assert!(suspended.load(Ordering::Acquire));
        assert!(
            recorded.lock().unwrap().contains(&key(false)),
            "production exit guard releases held input"
        );
        assert!(
            input_sender
                .send_input_with_context(context, key(true))
                .await
                .is_err()
        );
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "real Host LAN adapter timed out at {}",
            *stage.lock().unwrap()
        )
    });
}

#[tokio::test]
async fn optional_transfer_loss_parks_only_that_service_until_owned_session_close() {
    let f = Fixture::new().await;
    let mut optional = Box::pin(lan_session::optional_lane(f.host.clone(), async {
        Err(HostRuntimeError::SessionData(SessionDataError::NoPath(
            ChannelKind::Transfer,
        )))
    }));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut optional)
            .await
            .is_err()
    );
    assert!(!f.host.health().borrow().closed);
    f.host.close();
    assert!(
        tokio::time::timeout(LIMIT, optional)
            .await
            .unwrap()
            .is_err()
    );
}

struct TemporaryFiles(PathBuf);
impl TemporaryFiles {
    fn new() -> Self {
        let base = std::env::temp_dir().canonicalize().unwrap();
        let root = base.join(format!(
            "desklink-lan-files-{}-{}",
            std::process::id(),
            now_micros()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        assert!(root.starts_with(&base));
        Self(root)
    }
}
impl Drop for TemporaryFiles {
    fn drop(&mut self) {
        let base = std::env::temp_dir().canonicalize().unwrap();
        assert!(self.0.starts_with(base));
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("desklink-lan-files-")
        );
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct DiskRelease(Option<std::sync::mpsc::Sender<()>>);
impl DiskRelease {
    fn release(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}
impl Drop for DiskRelease {
    fn drop(&mut self) {
        self.release();
    }
}
fn blocked_disk() -> (
    DiskRelease,
    oneshot::Receiver<()>,
    impl Fn() + Send + 'static,
) {
    let (release, wait) = std::sync::mpsc::channel();
    let (entered, waiting) = oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let first = AtomicBool::new(true);
    let hook = move || {
        if first.swap(false, Ordering::AcqRel) {
            if let Some(entered) = entered.lock().unwrap().take() {
                let _ = entered.send(());
            }
            let _ = wait.recv();
        }
    };
    (DiskRelease(Some(release)), waiting, hook)
}

async fn next_file_event(events: &mut mpsc::Receiver<ControllerEvent>) -> TransferMessage {
    loop {
        if let ControllerEvent::Transfer(message) = events.recv().await.unwrap() {
            return message;
        }
    }
}

async fn controller_file_chunks(
    sender: ControllerTransferSender,
    source: OutgoingFile,
    sent: Arc<AtomicUsize>,
) {
    let (chunks, mut pending) = mpsc::channel(2);
    let producer = tokio::task::spawn_blocking(move || {
        let mut source_reader =
            OutgoingFileReader::open(&source, 0, None, Arc::new(AtomicBool::new(false))).unwrap();
        while let Some((offset, bytes)) = source_reader.read_chunk().unwrap() {
            chunks
                .blocking_send(TransferMessage::FileChunk {
                    transfer_id: source.transfer_id,
                    offset,
                    bytes,
                })
                .unwrap();
        }
        chunks
            .blocking_send(TransferMessage::FileComplete {
                transfer_id: source.transfer_id,
                content_hash: source_reader.finish().unwrap(),
            })
            .unwrap();
    });
    while let Some(message) = pending.recv().await {
        sender.send(message).await.unwrap();
        sent.fetch_add(1, Ordering::Release);
    }
    producer.await.unwrap();
}

#[allow(clippy::too_many_arguments)]
async fn real_files_with_blocked_disk(
    host: Arc<SessionDataPlane>,
    sender: &ControllerTransferSender,
    input_sender: &ControllerInputSender,
    context: InputContext,
    recorded: Arc<Mutex<Vec<InputEvent>>>,
    events: &mut mpsc::Receiver<ControllerEvent>,
    audio_queue: Arc<Mutex<AudioFrameQueue>>,
    audio_notify: Arc<Notify>,
) {
    // Existing disk tests deliberately use all eight production worker slots.
    // The same test-only gate prevents this real LAN worker competing with them.
    let _disk_gate = crate::transfer::lock_disk_worker_tests().await;
    let temporary = TemporaryFiles::new();
    let payload: Vec<u8> = (0..(desklink_protocol::MAX_TRANSFER_CHUNK_BYTES * 7 + 23))
        .map(|i| (i % 251) as u8)
        .collect();
    let source_path = temporary.0.join("upload-source.bin");
    std::fs::write(&source_path, &payload).unwrap();
    let source = prepare_outgoing_file(source_path).unwrap();
    let upload_id = source.transfer_id;
    let offer = TransferMessage::FileOffer {
        transfer_id: upload_id,
        request_id: None,
        name: source.name.clone(),
        size: source.size,
    };
    sender.send(offer.clone()).await.unwrap();
    assert_eq!(
        decode_transfer(&host.recv(ChannelKind::Transfer).await.unwrap()).unwrap(),
        offer
    );
    let (mut release, entered, hook) = blocked_disk();
    let mut receive_file = IncomingFileWorker::create_in_for_test(
        upload_id,
        source.name.clone(),
        source.size,
        temporary.0.join("upload-stage"),
        temporary.0.join("host-downloads"),
        hook,
    )
    .await
    .unwrap();
    let decision = TransferMessage::FileDecision {
        transfer_id: upload_id,
        result: TransferResult::Completed,
        resume_offset: receive_file.resume_offset(),
        resume_prefix_hash: receive_file.resume_prefix_hash(),
    };
    send_transfer_response(&host, decision.clone())
        .await
        .unwrap();
    assert_eq!(next_file_event(events).await, decision);
    let receiver_host = host.clone();
    let receiving = tokio::spawn(async move {
        loop {
            match decode_transfer(&receiver_host.recv(ChannelKind::Transfer).await.unwrap())
                .unwrap()
            {
                TransferMessage::FileChunk {
                    transfer_id,
                    offset,
                    bytes,
                } => {
                    assert_eq!(transfer_id, upload_id);
                    receive_file.write_chunk(offset, bytes).await.unwrap();
                }
                TransferMessage::FileComplete {
                    transfer_id,
                    content_hash,
                } => {
                    assert_eq!(transfer_id, upload_id);
                    let saved = receive_file.finish(content_hash).await.unwrap();
                    // Completion ACK is sent only after the production worker
                    // has checked size/hash, flushed and published the file.
                    send_transfer_response(
                        &receiver_host,
                        TransferMessage::FileResult {
                            transfer_id,
                            result: TransferResult::Completed,
                        },
                    )
                    .await
                    .unwrap();
                    return saved;
                }
                other => panic!("unexpected upload message: {other:?}"),
            }
        }
    });
    let sent = Arc::new(AtomicUsize::new(0));
    let uploading = tokio::spawn(controller_file_chunks(sender.clone(), source, sent.clone()));
    entered.await.unwrap();
    // One chunk is in the real blocked disk write, four more occupy the
    // production Transfer ingress queue. A sixth reliable receipt must wait.
    while sent.load(Ordering::Acquire) < 5 {
        tokio::task::yield_now().await;
    }
    tokio::time::timeout(Duration::from_millis(120), async {
        let movement = InputEvent::MouseMove { x: 37, y: 41 };
        input_sender
            .send_input_with_context(context, movement.clone())
            .await
            .unwrap();
        let marker = now_micros();
        audio_queue
            .lock()
            .unwrap()
            .push_drop_oldest(CapturedAudioFrame {
                capture_timestamp_us: marker,
                payload: vec![0; desklink_protocol::MAX_AUDIO_PAYLOAD_BYTES],
            });
        audio_notify.notify_one();
        loop {
            if let ControllerEvent::Audio(audio) = events.recv().await.unwrap()
                && audio.capture_timestamp_us == marker
            {
                break;
            }
        }
        while !recorded.lock().unwrap().contains(&movement) {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            sent.load(Ordering::Acquire),
            5,
            "full file lane must remain bounded while input/audio progress"
        );
    })
    .await
    .expect("file disk pressure blocked the independent input/audio lanes");
    release.release();
    uploading.await.unwrap();
    let host_saved = receiving.await.unwrap();
    assert_eq!(std::fs::read(&host_saved).unwrap(), payload);
    let upload_ack = TransferMessage::FileResult {
        transfer_id: upload_id,
        result: TransferResult::Completed,
    };
    assert_eq!(next_file_event(events).await, upload_ack);
    let actual_hash: [u8; 32] = Blake2s256::digest(std::fs::read(&host_saved).unwrap()).into();
    let expected_hash: [u8; 32] = Blake2s256::digest(&payload).into();
    assert_eq!(actual_hash, expected_hash);

    // Reverse direction uses the production Host outgoing-file loop (including
    // its existing disk producer and one-pass OutgoingFileReader) and another
    // real IncomingFileWorker in the explicitly authorized temporary directory.
    let download_source = prepare_outgoing_file(host_saved).unwrap();
    let download_id = download_source.transfer_id;
    let download_offer = TransferMessage::FileOffer {
        transfer_id: download_id,
        request_id: Some(72),
        name: download_source.name.clone(),
        size: download_source.size,
    };
    send_transfer_response(&host, download_offer.clone())
        .await
        .unwrap();
    assert_eq!(next_file_event(events).await, download_offer);
    let mut controller_file = IncomingFileWorker::create_in_for_test(
        download_id,
        download_source.name.clone(),
        download_source.size,
        temporary.0.join("download-stage"),
        temporary.0.join("controller-downloads"),
        || {},
    )
    .await
    .unwrap();
    let ready = TransferMessage::FileDecision {
        transfer_id: download_id,
        result: TransferResult::Completed,
        resume_offset: controller_file.resume_offset(),
        resume_prefix_hash: controller_file.resume_prefix_hash(),
    };
    sender.send(ready.clone()).await.unwrap();
    assert_eq!(
        decode_transfer(&host.recv(ChannelKind::Transfer).await.unwrap()).unwrap(),
        ready
    );
    let outgoing_host = host.clone();
    let downloading = tokio::spawn(async move {
        stream_host_outgoing_file(
            outgoing_host,
            download_source,
            0,
            None,
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    });
    let saved = loop {
        match next_file_event(events).await {
            TransferMessage::FileChunk {
                transfer_id,
                offset,
                bytes,
            } => {
                assert_eq!(transfer_id, download_id);
                controller_file.write_chunk(offset, bytes).await.unwrap();
            }
            TransferMessage::FileComplete {
                transfer_id,
                content_hash,
            } => {
                assert_eq!(transfer_id, download_id);
                assert_eq!(content_hash, actual_hash);
                break controller_file.finish(content_hash).await.unwrap();
            }
            other => panic!("unexpected download message: {other:?}"),
        }
    };
    assert_eq!(std::fs::read(saved).unwrap(), payload);
    let completed = TransferMessage::FileResult {
        transfer_id: download_id,
        result: TransferResult::Completed,
    };
    sender.send(completed.clone()).await.unwrap();
    assert_eq!(
        decode_transfer(&host.recv(ChannelKind::Transfer).await.unwrap()).unwrap(),
        completed
    );
    downloading.await.unwrap();
}

#[tokio::test]
async fn cancelling_a_full_owned_input_lane_closes_clones_and_releases_host_keys() {
    tokio::time::timeout(LIMIT, async {
        let mut f = Fixture::new().await;
        let (sender, _transfer, _events, mut drive) = connect_lan(&mut f).await;
        remove_owned_relay(&f).await;
        let epoch = InputEpochAllocator::from_seed(0xfedc_ba98_2345_6789).unwrap().allocate().unwrap();
        let topology = DisplayTopologySnapshot::new(vec![DisplayDescriptor {
            id: 7, name: "input cancellation geometry".into(),
            rect: DesktopRect::new(0, 0, 64, 64), primary: true,
        }], 7, DesktopRect::new(0, 0, 64, 64)).unwrap();
        let selected = Arc::new(Mutex::new(SelectedDesktop::from_snapshot(
            topology, NonZeroU64::new(STREAM).unwrap(), epoch, NonZeroU32::new(1).unwrap(),
        )));
        let captured = selected.lock().unwrap().input_context().unwrap();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let input = Arc::new(Mutex::new(InputInjector::with_backend(
            VirtualDesktop::single(DesktopRect::new(0, 0, 64, 64)), Recording(recorded.clone()),
        )));
        let suspended = Arc::new(AtomicBool::new(false));
        let (states, _) = watch::channel(None);
        // Retain this exact production future and its exit guard. We poll it
        // until keyDown is injected, then intentionally stop consuming Input.
        let mut receiving = Box::pin(receive_host_input(
            f.host.clone(), STREAM, input, selected.clone(), states, suspended.clone(),
        ));
        sender.send_input_with_context(captured, key(true)).await.unwrap();
        tokio::select! {
            result = &mut receiving => panic!("Host input unexpectedly stopped before keyDown: {result:?}"),
            _ = async {
                while !recorded.lock().unwrap().contains(&key(true)) {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
        assert_eq!(*recorded.lock().unwrap(), vec![key(true)]);
        // Each completed send has a real admission receipt. With Host business
        // consumption paused, all 32 entries now occupy the actual Input queue.
        for coordinate in 0..32 {
            sender.send_input_with_context(captured, InputEvent::MouseMove {
                x: coordinate, y: coordinate,
            }).await.unwrap();
        }
        assert_eq!(*recorded.lock().unwrap(), vec![key(true)]);
        assert!(all_direct(*f.host.health().borrow()));
        let clone = sender.clone();
        let cancelled = sender.clone();
        drive.stopping.store(true, Ordering::Release);
        // This is deliberately shorter than the LAN receipt deadline. Timeout
        // drops the polled owned send itself, invoking PendingPlaneInput; it is
        // a cancellation witness, not a performance measurement or a manual
        // close of either endpoint.
        assert!(tokio::time::timeout(Duration::from_millis(20), async move {
            cancelled.send_input_with_context(captured, key(false)).await
        }).await.is_err());
        loop {
            if drive.peer_health.borrow().is_some_and(|health| health.closed) { break; }
            drive.peer_health.changed().await.expect("Controller must publish cancellation closure");
        }
        let mut host_health = f.host.health();
        while host_health.borrow().direct_path.is_some() { host_health.changed().await.unwrap(); }
        assert!(!host_health.borrow().relay_connected);
        assert!(clone.send_input_with_context(captured, key(false)).await.is_err());
        assert!(sender.send_input_with_context(captured, key(true)).await.is_err());
        // Resuming the retained production receiver observes lost authority
        // before draining queued motion, returns, and drops HostInputExitGuard.
        assert!(receiving.await.is_err());
        assert!(selected.lock().unwrap().input_closed);
        assert!(suspended.load(Ordering::Acquire));
        assert_eq!(*recorded.lock().unwrap(), vec![key(true), key(false)],
            "only the real exit guard may release the held key; queued motion and the cancelled release never inject");
    }).await.expect("owned Input cancellation did not close and release the original LAN session");
}
