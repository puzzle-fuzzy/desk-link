//! Real loopback QUIC relay and Noise packet encryption. Synthetic identities
//! and independent fixture files only; no desktop capture or OS input injection.
use apps_windows::transfer_task::{FileTaskHandle, FileTaskProgress, FileTaskService};
use apps_windows::transfer_task_native::{FileTaskPump, spawn_pump};
use desklink_crypto::{
    DeviceIdentity, NoiseInitiator, NoiseResponder, SecureLane, SecureRole, SecureSession,
    SessionId,
};
use desklink_protocol::{
    FileTaskMessage, TransferMessage, TransferResult, decode_transfer, encode_transfer,
};
use desklink_relay::{RelayConfig, RelayServer};
use desklink_transport::{QuicClient, QuicClientConfig, RelayJoin};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::{
    fs,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};

pub use apps_windows::{transfer_checkpoint, transfer_task};
#[path = "../test-support/file-task-fixture.rs"]
mod fixture;
use fixture::{Fixture, start, terminal};

struct Abort(JoinHandle<()>);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct NetworkEndpoint {
    handle: FileTaskHandle,
    progress: watch::Receiver<Option<FileTaskProgress>>,
    _sender: FileTaskPump,
    _receiver: Abort,
}
fn endpoint(
    client: Arc<QuicClient>,
    secure: Arc<Mutex<SecureSession>>,
    service: FileTaskService,
    caps: Arc<AtomicUsize>,
    slow_disk: bool,
) -> NetworkEndpoint {
    let handle = service.handle;
    let sender_client = client.clone();
    let sender_secure = secure.clone();
    let sender = spawn_pump(service.outgoing, service.controls, move |message| {
        let client = sender_client.clone();
        let secure = sender_secure.clone();
        async move {
            let plain = encode_transfer(&TransferMessage::FileTask(message))
                .map_err(|_| TransferResult::InvalidData)?;
            let encrypted = secure
                .lock()
                .await
                .seal(SecureLane::Transfer, &plain)
                .map_err(|_| TransferResult::InvalidData)?;
            client
                .send_transfer(encrypted.to_vec())
                .await
                .map_err(|_| TransferResult::IoFailed)
        }
    });
    let receiver_handle = handle.clone();
    let receiver = tokio::spawn(async move {
        loop {
            let encrypted = client.next_transfer().await.unwrap();
            let plain = secure
                .lock()
                .await
                .open(SecureLane::Transfer, &encrypted)
                .unwrap();
            let TransferMessage::FileTask(message) = decode_transfer(&plain).unwrap() else {
                panic!("unexpected transfer type")
            };
            if matches!(&message, FileTaskMessage::Capabilities(_)) {
                caps.fetch_add(1, Ordering::AcqRel);
            }
            // Delay only the dedicated file consumer. Media/input consumers
            // below continue through their actual independent QUIC lanes.
            if slow_disk && matches!(&message, FileTaskMessage::Block { .. }) {
                sleep(Duration::from_millis(15)).await;
            }
            let began = std::time::Instant::now();
            loop {
                match receiver_handle.ingest(message.clone()) {
                    Ok(()) => break,
                    Err(TransferResult::Busy) if began.elapsed() < Duration::from_secs(2) => {
                        sleep(Duration::from_millis(2)).await
                    }
                    other => panic!("file ingress: {other:?}"),
                }
            }
        }
    });
    NetworkEndpoint {
        handle,
        progress: service.progress,
        _sender: sender,
        _receiver: Abort(receiver),
    }
}

async fn local_relay() -> (Arc<RelayServer>, quinn::ClientConfig, Abort) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let der = cert.cert.der().to_vec();
    let server = quinn::ServerConfig::with_single_cert(
        vec![CertificateDer::from(der.clone())],
        PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into()),
    )
    .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(der)).unwrap();
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
    ));
    let relay = Arc::new(
        RelayServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            server,
            RelayConfig::default(),
        )
        .await
        .unwrap(),
    );
    let runner = relay.clone();
    let task = Abort(tokio::spawn(async move {
        runner.run().await.unwrap();
    }));
    (relay, client, task)
}

async fn noise_over_quic(
    a: &QuicClient,
    b: &QuicClient,
) -> (Arc<Mutex<SecureSession>>, Arc<Mutex<SecureSession>>) {
    let identity_a = DeviceIdentity::from_secret_key([0xa1; 16], &[0xa2; 32]);
    let identity_b = DeviceIdentity::from_secret_key([0xb1; 16], &[0xb2; 32]);
    let (mut initiator, first) =
        NoiseInitiator::start(identity_a, identity_b.verify_key()).unwrap();
    a.send_control(first).await.unwrap();
    let received = b.next_control().await.unwrap();
    let (mut responder, second) = NoiseResponder::accept_pairing(&received, identity_b).unwrap();
    b.send_control(second).await.unwrap();
    let received = a.next_control().await.unwrap();
    let third = initiator.receive(&received).unwrap();
    a.send_control(third).await.unwrap();
    responder.receive(&b.next_control().await.unwrap()).unwrap();
    (
        Arc::new(Mutex::new(
            initiator
                .finish()
                .unwrap()
                .into_secure_session(SecureRole::Initiator),
        )),
        Arc::new(Mutex::new(
            responder
                .finish()
                .unwrap()
                .into_secure_session(SecureRole::Responder),
        )),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn loopback_quic_noise_transfers_directory_both_ways_while_other_lanes_continue() {
    timeout(Duration::from_secs(30), async {
        let fixture = Fixture::new();
        let source = fixture.path("中文-source");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("空目录")).unwrap();
        let bytes = fixture::sample(24);
        fs::write(source.join("数据.bin"), &bytes).unwrap();
        let (relay, client_config, _relay_task) = local_relay().await;
        let config = || {
            QuicClientConfig::with_client_config(
                relay.local_addr().unwrap(),
                "localhost",
                client_config.clone(),
            )
        };
        let a = Arc::new(QuicClient::connect(config()).await.unwrap());
        let b = Arc::new(QuicClient::connect(config()).await.unwrap());
        let session = SessionId::from_bytes([0x14; 16]);
        let authentication = [0x30; 32];
        a.join(RelayJoin::host_with_participant(
            session,
            authentication,
            [1; 16],
        ))
        .await
        .unwrap();
        b.join(RelayJoin::controller_with_participant(
            session,
            authentication,
            [2; 16],
        ))
        .await
        .unwrap();
        let (secure_a, secure_b) = noise_over_quic(&a, &b).await;
        let caps = Arc::new(AtomicUsize::new(0));
        let mut endpoint_a = endpoint(
            a.clone(),
            secure_a.clone(),
            FileTaskService::spawn(fixture.options(true, true)).unwrap(),
            caps.clone(),
            false,
        );
        let mut endpoint_b = endpoint(
            b.clone(),
            secure_b.clone(),
            FileTaskService::spawn(fixture.options(false, true)).unwrap(),
            caps.clone(),
            true,
        );
        while caps.load(Ordering::Acquire) < 2 {
            sleep(Duration::from_millis(5)).await;
        }
        let task = [0x31; 16];
        start(&endpoint_a.handle, &source, task).await;
        timeout(Duration::from_secs(5), async {
            loop {
                if endpoint_a
                    .progress
                    .borrow()
                    .as_ref()
                    .is_some_and(|p| p.acknowledged_bytes > 0)
                {
                    break;
                }
                endpoint_a.progress.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        // These are synthetic payloads consumed here; they never enter an OS
        // input handler or decoder. Assert actual lane progress during file IO.
        for i in 1..=12_u8 {
            let packet = vec![i; 32];
            let control = secure_a
                .lock()
                .await
                .seal(SecureLane::Control, &packet)
                .unwrap();
            let input = secure_a
                .lock()
                .await
                .seal(SecureLane::Input, &packet)
                .unwrap();
            let video = secure_a
                .lock()
                .await
                .seal(SecureLane::VideoDatagram, &packet)
                .unwrap();
            let audio = secure_a
                .lock()
                .await
                .seal(SecureLane::AudioDatagram, &packet)
                .unwrap();
            a.send_control(control.to_vec()).await.unwrap();
            a.send_input(input.to_vec()).await.unwrap();
            a.send_video_datagram(video.to_vec()).await.unwrap();
            a.send_audio_datagram(audio.to_vec()).await.unwrap();
            timeout(Duration::from_secs(1), async {
                assert_eq!(
                    secure_b
                        .lock()
                        .await
                        .open(SecureLane::Control, &b.next_control().await.unwrap())
                        .unwrap(),
                    packet
                );
                assert_eq!(
                    secure_b
                        .lock()
                        .await
                        .open(SecureLane::Input, &b.next_input().await.unwrap())
                        .unwrap(),
                    packet
                );
                assert_eq!(
                    secure_b
                        .lock()
                        .await
                        .open(
                            SecureLane::VideoDatagram,
                            &b.next_video_datagram().await.unwrap()
                        )
                        .unwrap(),
                    packet
                );
                assert_eq!(
                    secure_b
                        .lock()
                        .await
                        .open(
                            SecureLane::AudioDatagram,
                            &b.next_audio_datagram().await.unwrap()
                        )
                        .unwrap(),
                    packet
                );
            })
            .await
            .unwrap();
        }
        assert_eq!(
            terminal(&mut endpoint_a.progress, task).await.state,
            "completed"
        );
        assert_eq!(
            terminal(&mut endpoint_b.progress, task).await.state,
            "completed"
        );
        assert_eq!(
            fs::read(fixture.received(false, task, "数据.bin")).unwrap(),
            bytes
        );
        assert!(fixture.received(false, task, "空目录").is_dir());
        let reverse = [0x32; 16];
        start(&endpoint_b.handle, &source, reverse).await;
        assert_eq!(
            terminal(&mut endpoint_b.progress, reverse).await.state,
            "completed"
        );
        assert_eq!(
            terminal(&mut endpoint_a.progress, reverse).await.state,
            "completed"
        );
        assert_eq!(
            fs::read(fixture.received(true, reverse, "数据.bin")).unwrap(),
            bytes
        );
        drop(endpoint_a);
        drop(endpoint_b);
        sleep(Duration::from_millis(100)).await;
        relay.close();
    })
    .await
    .unwrap();
}
