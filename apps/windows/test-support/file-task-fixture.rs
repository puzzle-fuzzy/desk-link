//! Actor integration over the actual protocol codec and authenticated packet
//! encryption. This bridge is local and in memory; it is not a QUIC/public-network
//! or two-device performance acceptance test.
#![allow(dead_code)] // Shared by the focused tests and the explicit heavy example.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use desklink_crypto::{
    DeviceIdentity, NoiseInitiator, NoiseResponder, SecureLane, SecureRole, SecureSession,
};
use desklink_protocol::{
    FileTaskMessage, TransferId, TransferMessage, TransferResult, decode_transfer, encode_transfer,
};
use tokio::{
    sync::watch,
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};

use crate::{
    transfer_checkpoint::FILE_BLOCK_BYTES,
    transfer_task::{FileTaskHandle, FileTaskOptions, FileTaskProgress, FileTaskService, id_text},
};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

/// Each fixture spins real QUIC/Noise actor runtimes; running them in
/// parallel starves their timers on constrained machines and fails the
/// suite spuriously. Holding this lock for the fixture's lifetime
/// serializes exactly these tests, with no effect on other modules.
pub(crate) static ACTOR_SERIALIZER: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) struct Fixture(
    pub(super) PathBuf,
    PathBuf,
    bool,
    Arc<AtomicUsize>,
    pub(super) Option<std::sync::MutexGuard<'static, ()>>,
);
impl Fixture {
    pub(super) fn new() -> Self {
        Self::new_in(&std::env::temp_dir())
    }
    pub(super) fn new_in(base: &Path) -> Self {
        let serializer = ACTOR_SERIALIZER.lock().ok();
        let base = fs::canonicalize(base).unwrap();
        assert!(base.is_absolute());
        let root = base.join(format!(
            "desklink-file-actor-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        if std::env::var_os("DESKLINK_FILE_TEST_TRACE").is_some() {
            eprintln!("file-fixture {}", root.display());
        }
        for name in ["receive-a", "receive-b"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        Self(root, base, false, Arc::new(AtomicUsize::new(0)), serializer)
    }
    pub(super) fn preserve(&mut self) {
        self.2 = true;
    }
    pub(super) fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    pub(super) fn options(&self, a: bool, approve: bool) -> FileTaskOptions {
        let peer_identity = if a {
            DeviceIdentity::from_secret_key([0xb1; 16], &[0xb2; 32])
        } else {
            DeviceIdentity::from_secret_key([0xa1; 16], &[0xa2; 32])
        };
        let source_root = self.0.clone();
        let source_consents = self.3.clone();
        FileTaskOptions {
            peer_key: peer_identity
                .verify_key()
                .to_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            cache_root: self.path(if a { "cache-a" } else { "cache-b" }),
            receive_parent: self.path(if a { "receive-a" } else { "receive-b" }),
            authorize: Arc::new(move |_, _, token| approve && !token.is_cancelled()),
            select: None,
            // Explicit synthetic source-operator consent, scoped to this
            // generated fixture. No real user's files are offered by tests.
            authorize_source_resume: Some(Arc::new(move |path, directory, token| {
                source_consents.fetch_add(1, Ordering::AcqRel);
                !token.is_cancelled()
                    && path.starts_with(&source_root)
                    && path.exists()
                    && path.is_dir() == directory
            })),
        }
    }
    pub(super) fn source_resume_consents(&self) -> usize {
        self.3.load(Ordering::Acquire)
    }
    pub(super) fn received(&self, a: bool, task: TransferId, relative: &str) -> PathBuf {
        self.path(if a { "receive-a" } else { "receive-b" })
            .join(format!("DeskLink-{}", id_text(task)))
            .join(relative)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if !self.2
            && let Ok(target) = fs::canonicalize(&self.0)
        {
            // Delete only this generated direct child of the explicitly chosen
            // fixture parent. Never remove the parent or a redirected path.
            if target.is_absolute()
                && target.parent() == Some(self.1.as_path())
                && target
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("desklink-file-actor-"))
            {
                let _ = fs::remove_dir_all(target);
            }
        }
    }
}

#[derive(Default, Debug)]
pub(super) struct BridgeStats {
    pub(super) encrypted_messages: u64,
    pub(super) offers: u64,
    pub(super) transferred_bytes: u64,
    pub(super) blocks: Vec<(TransferId, u64)>,
    pub(super) resumes: Vec<(TransferId, u64)>,
    pub(super) acknowledgements: u64,
    pub(super) maximum_ack_offset: u64,
}

/// Faults run at the synthetic authenticated endpoint, before sealing. They
/// model peer/storage failures and are not a bypass of packet authentication.
pub(super) type BridgeHook = Arc<dyn Fn(&mut FileTaskMessage) + Send + Sync>;

fn sessions() -> (SecureSession, SecureSession) {
    // Fixed synthetic fixture identities. No user identity, invite, or remote
    // session is read, created, or stored by these tests.
    let a = DeviceIdentity::from_secret_key([0xa1; 16], &[0xa2; 32]);
    let b = DeviceIdentity::from_secret_key([0xb1; 16], &[0xb2; 32]);
    let (mut initiator, first) = NoiseInitiator::start(a, b.verify_key()).unwrap();
    let (mut responder, second) = NoiseResponder::accept_pairing(&first, b).unwrap();
    let third = initiator.receive(&second).unwrap();
    responder.receive(&third).unwrap();
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

async fn forward(
    mut message: FileTaskMessage,
    sender: &mut SecureSession,
    receiver: &mut SecureSession,
    target: &FileTaskHandle,
    stats: &Arc<Mutex<BridgeStats>>,
    slow_ack: Duration,
    hook: Option<&BridgeHook>,
) {
    if let Some(hook) = hook {
        hook(&mut message);
    }
    let plaintext = encode_transfer(&TransferMessage::FileTask(message)).unwrap();
    let encrypted = sender.seal(SecureLane::Transfer, &plaintext).unwrap();
    assert_ne!(plaintext, encrypted);
    let decrypted = receiver.open(SecureLane::Transfer, &encrypted).unwrap();
    let TransferMessage::FileTask(message) = decode_transfer(&decrypted).unwrap() else {
        panic!("wrong transfer value")
    };
    if std::env::var_os("DESKLINK_FILE_TEST_TRACE").is_some() {
        match &message {
            FileTaskMessage::Block { offset, bytes, .. } => {
                eprintln!("file-bridge Block offset={offset} bytes={}", bytes.len())
            }
            _ => eprintln!("file-bridge {message:?}"),
        }
    }
    if matches!(message, FileTaskMessage::BlockAck { .. }) && !slow_ack.is_zero() {
        sleep(slow_ack).await;
    }
    {
        let mut stats = stats.lock().unwrap();
        stats.encrypted_messages += 1;
        match &message {
            FileTaskMessage::Offer { .. } => stats.offers += 1,
            FileTaskMessage::Block {
                task_id,
                offset,
                bytes,
                ..
            } => {
                stats.blocks.push((*task_id, *offset));
                stats.transferred_bytes += bytes.len() as u64;
            }
            FileTaskMessage::EntryDecision {
                task_id,
                checkpoint,
                result: TransferResult::Completed,
                ..
            } => stats.resumes.push((*task_id, checkpoint.offset)),
            FileTaskMessage::BlockAck { checkpoint, .. } => {
                stats.acknowledgements += 1;
                stats.maximum_ack_offset = stats.maximum_ack_offset.max(checkpoint.offset);
            }
            _ => {}
        }
    }
    let began = Instant::now();
    loop {
        match target.ingest(message.clone()) {
            Ok(()) => return,
            Err(TransferResult::Busy) if began.elapsed() < Duration::from_secs(5) => {
                sleep(Duration::from_millis(2)).await
            }
            Err(error) => panic!("actor ingress failed: {error:?}"),
        }
    }
}

pub(super) struct Peers {
    pub(super) a: FileTaskHandle,
    pub(super) b: FileTaskHandle,
    pub(super) a_progress: watch::Receiver<Option<FileTaskProgress>>,
    pub(super) b_progress: watch::Receiver<Option<FileTaskProgress>>,
    pub(super) stats: Arc<Mutex<BridgeStats>>,
    pump: JoinHandle<()>,
}

impl Peers {
    pub(super) async fn new(fixture: &Fixture, approve_b: bool, slow_ack: Duration) -> Self {
        Self::spawn(fixture, approve_b, slow_ack, None).await
    }

    pub(super) async fn with_hook(
        fixture: &Fixture,
        approve_b: bool,
        slow_ack: Duration,
        hook: BridgeHook,
    ) -> Self {
        Self::spawn(fixture, approve_b, slow_ack, Some(hook)).await
    }

    async fn spawn(
        fixture: &Fixture,
        approve_b: bool,
        slow_ack: Duration,
        hook: Option<BridgeHook>,
    ) -> Self {
        let mut a = FileTaskService::spawn(fixture.options(true, true)).unwrap();
        let mut b = FileTaskService::spawn(fixture.options(false, approve_b)).unwrap();
        let (mut a_crypto, mut b_crypto) = sessions();
        let stats = Arc::new(Mutex::new(BridgeStats::default()));
        // Admission is ordered behind the exchanged capability messages.
        let a_capability = timeout(Duration::from_secs(5), a.outgoing.recv())
            .await
            .unwrap()
            .unwrap();
        let b_capability = timeout(Duration::from_secs(5), b.outgoing.recv())
            .await
            .unwrap()
            .unwrap();
        forward(
            a_capability,
            &mut a_crypto,
            &mut b_crypto,
            &b.handle,
            &stats,
            Duration::ZERO,
            hook.as_ref(),
        )
        .await;
        forward(
            b_capability,
            &mut b_crypto,
            &mut a_crypto,
            &a.handle,
            &stats,
            Duration::ZERO,
            hook.as_ref(),
        )
        .await;
        let (a_handle, b_handle) = (a.handle.clone(), b.handle.clone());
        let pump_stats = stats.clone();
        let pump = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    changed = a.controls.changed() => {
                        if changed.is_err() { break; }
                        let message = *a.controls.borrow_and_update();
                        if let Some((task_id, action)) = message { forward(FileTaskMessage::Action { task_id, action }, &mut a_crypto, &mut b_crypto, &b_handle, &pump_stats, Duration::ZERO, hook.as_ref()).await; }
                    }
                    changed = b.controls.changed() => {
                        if changed.is_err() { break; }
                        let message = *b.controls.borrow_and_update();
                        if let Some((task_id, action)) = message { forward(FileTaskMessage::Action { task_id, action }, &mut b_crypto, &mut a_crypto, &a_handle, &pump_stats, Duration::ZERO, hook.as_ref()).await; }
                    }
                    message = a.outgoing.recv() => {
                        let Some(message) = message else { break; };
                        forward(message, &mut a_crypto, &mut b_crypto, &b_handle, &pump_stats, slow_ack, hook.as_ref()).await;
                    }
                    message = b.outgoing.recv() => {
                        let Some(message) = message else { break; };
                        forward(message, &mut b_crypto, &mut a_crypto, &a_handle, &pump_stats, slow_ack, hook.as_ref()).await;
                    }
                }
            }
        });
        Self {
            a: a.handle,
            b: b.handle,
            a_progress: a.progress,
            b_progress: b.progress,
            stats,
            pump,
        }
    }

    pub(super) async fn close(self) {
        self.pump.abort();
        let _ = self.pump.await;
        drop(self.a);
        drop(self.b);
        // Workers observe channel closure/stop without waiting for a transport
        // timeout. Permit/part cleanup happens on those disk threads.
        sleep(Duration::from_millis(150)).await;
    }
}

pub(super) async fn terminal(
    progress: &mut watch::Receiver<Option<FileTaskProgress>>,
    task: TransferId,
) -> FileTaskProgress {
    timeout(Duration::from_secs(15), async {
        loop {
            if let Some(snapshot) = progress.borrow().clone()
                && snapshot.task_id == id_text(task)
                && matches!(
                    snapshot.state,
                    "completed" | "partial" | "failed" | "cancelled"
                )
            {
                if std::env::var_os("DESKLINK_FILE_TEST_TRACE").is_some() {
                    eprintln!("file-terminal {snapshot:?}");
                }
                return snapshot;
            }
            progress
                .changed()
                .await
                .expect("actor stopped without terminal progress");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("actor timed out: {:?}", progress.borrow().as_ref()))
}

pub(super) async fn start(handle: &FileTaskHandle, path: &Path, task: TransferId) {
    timeout(Duration::from_secs(3), async {
        loop {
            match handle.start(path.to_owned(), task) {
                Ok(()) => break,
                Err(TransferResult::Busy) => sleep(Duration::from_millis(5)).await,
                Err(error) => panic!("start failed: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
}

pub(super) fn sample(blocks: usize) -> Vec<u8> {
    (0..FILE_BLOCK_BYTES as usize * blocks + 17)
        .map(|index| (index % 251) as u8)
        .collect()
}
