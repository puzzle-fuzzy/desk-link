use super::{initial_file_task_capabilities, start_file_task_service_for_host};
use apps_windows::transfer_task::{FileTaskOptions, FileTaskService};
use desklink_crypto::{
    DeviceIdentity, NoiseInitiator, NoiseResponder, SecureLane, SecureRole, SecureSession,
};
use desklink_protocol::{
    Codec, ControlMessage, DeviceCapabilities, DeviceRole, FileTaskMessage,
    FileTransferCapabilities, H264Profile, InputEnvelope, InputEvent, Platform, TransferMessage,
    TransferResult,
    codec::{
        decode_control, decode_input, decode_transfer, encode_control, encode_input,
        encode_transfer,
    },
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn authenticated_host(
    platform: Platform,
    role: DeviceRole,
) -> (DeviceCapabilities, SecureSession, SecureSession) {
    let controller = DeviceIdentity::from_secret_key([41; 16], &[42; 32]);
    let host = DeviceIdentity::from_secret_key([43; 16], &[44; 32]);
    let controller_key = controller.verify_key();
    let host_key = host.verify_key();
    let (mut initiator, first) = NoiseInitiator::start(controller, host_key).unwrap();
    let (mut responder, second) = NoiseResponder::accept(&first, host, controller_key).unwrap();
    responder
        .receive(&initiator.receive(&second).unwrap())
        .unwrap();
    let mut controller = initiator
        .finish()
        .unwrap()
        .into_secure_session(SecureRole::Initiator);
    let mut host = responder
        .finish()
        .unwrap()
        .into_secure_session(SecureRole::Responder);
    let capabilities = ControlMessage::Capabilities(DeviceCapabilities {
        platform,
        role,
        codecs: vec![Codec::H264],
        h264_profiles: vec![H264Profile::Main],
        width: 1920,
        height: 1080,
        lan_data: None,
    });
    let wire = host
        .seal(SecureLane::Control, &encode_control(&capabilities).unwrap())
        .unwrap();
    let decoded = decode_control(&controller.open(SecureLane::Control, &wire).unwrap()).unwrap();
    let ControlMessage::Capabilities(capabilities) = decoded else {
        panic!("expected authenticated capabilities")
    };
    (capabilities, controller, host)
}

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!(
            "desklink-file-platform-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("receive")).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn start(&self) -> Result<FileTaskService, TransferResult> {
        FileTaskService::spawn(FileTaskOptions {
            peer_key: "51".repeat(48),
            cache_root: self.0.join("cache"),
            receive_parent: self.0.join("receive"),
            authorize: Arc::new(|_, _, _| false),
            select: None,
            authorize_source_resume: None,
        })
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        assert_eq!(self.0.parent(), Some(temp.as_path()));
        assert!(
            self.0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("desklink-file-platform-")
        );
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn assert_initial_state(service: &Option<FileTaskService>, expected: &str) {
    let signal = serde_json::to_value(initial_file_task_capabilities(
        "fixture-peer".into(),
        service,
    ))
    .unwrap();
    assert_eq!(signal["state"], expected);
    for feature in ["largeFiles", "directories", "blockResume"] {
        assert_eq!(signal[feature], false);
    }
}

#[tokio::test]
async fn authenticated_windows_host_starts_real_file_actor_and_capability_pump() {
    let fixture = Fixture::new();
    let (capabilities, controller, mut host) =
        authenticated_host(Platform::Windows, DeviceRole::Host);
    let service = start_file_task_service_for_host(&capabilities, || fixture.start());
    assert_initial_state(&service, "pending");
    let FileTaskService {
        handle,
        outgoing,
        mut progress,
        controls,
    } = service.expect("Windows actor starts");
    let controller = Arc::new(Mutex::new(controller));
    let (sent, mut received) = tokio::sync::mpsc::channel(1);
    let pump = apps_windows::transfer_task_native::spawn_pump(outgoing, controls, move |message| {
        let controller = controller.clone();
        let sent = sent.clone();
        async move {
            let plaintext = encode_transfer(&TransferMessage::FileTask(message)).unwrap();
            let wire = controller
                .lock()
                .unwrap()
                .seal(SecureLane::Transfer, &plaintext)
                .unwrap();
            sent.send(wire).await.map_err(|_| TransferResult::IoFailed)
        }
    });
    let wire = tokio::time::timeout(Duration::from_secs(3), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        decode_transfer(&host.open(SecureLane::Transfer, &wire).unwrap()).unwrap(),
        TransferMessage::FileTask(FileTaskMessage::Capabilities(
            FileTransferCapabilities::WINDOWS
        )),
    );
    drop(handle);
    drop(pump);
    tokio::time::timeout(Duration::from_secs(3), async {
        while progress.changed().await.is_ok() {}
    })
    .await
    .expect("actor releases fixture and disk worker permit");
}

#[test]
fn unsupported_authenticated_hosts_skip_file_actor_and_preserve_base_noise_lanes() {
    for platform in [Platform::MacOS, Platform::IOS] {
        let (capabilities, mut controller, mut host) =
            authenticated_host(platform, DeviceRole::Host);
        let calls = AtomicU64::new(0);
        let service = start_file_task_service_for_host(&capabilities, || {
            calls.fetch_add(1, Ordering::Relaxed);
            Err(TransferResult::IoFailed)
        });
        assert!(service.is_none());
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "no actor means no unsolicited FileTask capability packet"
        );
        assert_initial_state(&service, "unsupported");

        let control = ControlMessage::SetAudioEnabled { enabled: false };
        let wire = controller
            .seal(SecureLane::Control, &encode_control(&control).unwrap())
            .unwrap();
        assert_eq!(
            decode_control(&host.open(SecureLane::Control, &wire).unwrap()).unwrap(),
            control
        );
        let now_us = || {
            u64::try_from(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_micros(),
            )
            .unwrap()
        };
        let input = InputEnvelope {
            sequence: 1,
            timestamp_us: now_us(),
            stream_id: 7,
            input_epoch: 9,
            event: InputEvent::MouseMove { x: 1, y: 2 },
        };
        let wire = controller
            .seal(SecureLane::Input, &encode_input(&input).unwrap())
            .unwrap();
        assert_eq!(
            decode_input(&host.open(SecureLane::Input, &wire).unwrap(), now_us()).unwrap(),
            input
        );
        let clipboard = TransferMessage::ClipboardSet {
            request_id: 1,
            text: "synthetic clipboard fixture".into(),
        };
        let wire = controller
            .seal(SecureLane::Transfer, &encode_transfer(&clipboard).unwrap())
            .unwrap();
        assert_eq!(
            decode_transfer(&host.open(SecureLane::Transfer, &wire).unwrap()).unwrap(),
            clipboard
        );
    }
}

#[test]
fn windows_controller_role_cannot_start_a_host_file_service() {
    let (capabilities, _, _) = authenticated_host(Platform::Windows, DeviceRole::Controller);
    let service = start_file_task_service_for_host(&capabilities, || {
        panic!("host role required before actor startup")
    });
    assert!(service.is_none());
    assert_initial_state(&service, "unsupported");
}
