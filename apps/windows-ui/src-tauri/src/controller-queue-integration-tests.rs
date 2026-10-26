//! Native manager/store behavior: only synthetic files in a unique fixture.
use std::{
    collections::VecDeque,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use apps_windows::{
    transfer::{OutgoingFile, prepare_outgoing_file},
    transfer_recovery::WindowsFileQueueRecoveryStore,
};
use tokio::{
    sync::watch,
    time::{Duration, sleep, timeout},
};

use super::{
    ControllerManager,
    queue_worker::{QueuePersistence, QueuePersistenceUpdate},
};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    parent: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = parent.join(format!(
            "desklink-native-queue-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self { root, parent }
    }
    fn file(&self, name: &str, id: u8) -> OutgoingFile {
        let path = self.root.join(name);
        fs::write(&path, b"synthetic queue source remains unchanged").unwrap();
        let mut file = prepare_outgoing_file(path).unwrap();
        file.transfer_id = [id; 16];
        file
    }
    fn manager(&self) -> (ControllerManager, WindowsFileQueueRecoveryStore) {
        let store = WindowsFileQueueRecoveryStore::new(self.root.join("queue.bin"));
        (
            ControllerManager {
                file_queue_recovery_store: Some(store.clone()),
                ..Default::default()
            },
            store,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(root) = fs::canonicalize(&self.root)
            && root.is_absolute()
            && root.parent() == Some(self.parent.as_path())
            && root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("desklink-native-queue-"))
        {
            let _ = fs::remove_dir_all(root);
        }
    }
}

async fn update(
    receiver: &mut watch::Receiver<Option<QueuePersistenceUpdate>>,
    revision: u64,
) -> QueuePersistenceUpdate {
    timeout(Duration::from_secs(5), async {
        loop {
            if let Some(value) = receiver.borrow_and_update().clone()
                && value.revision == revision
            {
                return value;
            }
            receiver.changed().await.unwrap();
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn native_dpapi_queue_retains_pending_admission_across_clear_until_actor_admission_completes()
{
    let fixture = Fixture::new();
    let (manager, store) = fixture.manager();
    let source = fixture.file("pending-source.bin", 7);
    let host = [8; 16];
    let epoch = manager.file_queue_persistence_epoch.load(Ordering::Acquire);
    *manager.file_task_queue_admission.lock().unwrap() = Some((1, host, source.clone()));
    let worker = manager.file_queue_worker().unwrap();
    let mut updates = worker.updates();
    worker
        .submit(manager.clone(), host, VecDeque::new(), true, 101)
        .unwrap();
    assert_eq!(update(&mut updates, 101).await.result, Ok(()));
    let stored = store.load().unwrap().unwrap();
    assert_eq!(stored.host_device_id(), host);
    assert_eq!(stored.files().len(), 1);
    assert_eq!(
        stored.files()[0].outgoing_file().transfer_id,
        source.transfer_id
    );
    assert_eq!(
        manager.file_queue_protection_status(true),
        ("protected", None)
    );
    let protected = fs::read(fixture.root.join("queue.bin")).unwrap();
    assert!(
        !protected
            .windows(source.name.len())
            .any(|window| window == source.name.as_bytes())
    );
    worker
        .submit(manager.clone(), host, VecDeque::new(), false, 102)
        .unwrap();
    assert_eq!(update(&mut updates, 102).await.result, Ok(()));
    let stored = store.load().unwrap().unwrap();
    assert_eq!(
        stored.files().len(),
        1,
        "clearing waiting items lost the source being handed off"
    );
    assert_eq!(
        stored.files()[0].outgoing_file().transfer_id,
        source.transfer_id
    );
    manager.file_task_queue_admission.lock().unwrap().take();
    worker
        .submit(manager.clone(), host, VecDeque::new(), false, 103)
        .unwrap();
    assert_eq!(update(&mut updates, 103).await.result, Ok(()));
    assert!(store.load().unwrap().is_none());
    assert!(manager.file_queue_recovery_summary().is_none());
    assert_eq!(
        manager.file_queue_persistence_epoch.load(Ordering::Acquire),
        epoch,
        "normal empty queue incorrectly changed explicit-discard epoch"
    );
    assert_eq!(
        fs::read(source.path).unwrap(),
        b"synthetic queue source remains unchanged"
    );
}

#[tokio::test]
async fn explicit_discard_epoch_prevents_a_real_queued_manager_job_from_resurrecting_dpapi_record()
{
    let fixture = Fixture::new();
    let (manager, store) = fixture.manager();
    let source = fixture.file("discard-source.bin", 9);
    let host = [10; 16];
    let files = VecDeque::from([source.clone()]);
    manager.remember_file_queue(host, &files, true).unwrap();
    let revision = manager.file_queue_recovery_summary().unwrap().revision;
    let old_epoch = manager.file_queue_persistence_epoch.load(Ordering::Acquire);
    assert_eq!(store.load().unwrap().unwrap().files().len(), 1);
    let (ready, entered) = tokio::sync::oneshot::channel();
    let mut ready = Some(ready);
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let worker = QueuePersistence::spawn_with_before_persist(move || {
        ready.take().unwrap().send(()).unwrap();
        released.recv_timeout(Duration::from_secs(5)).unwrap();
    })
    .unwrap();
    let mut updates = worker.updates();
    worker
        .submit(manager.clone(), host, files, true, 201)
        .unwrap();
    timeout(Duration::from_secs(2), entered)
        .await
        .unwrap()
        .unwrap();
    timeout(Duration::from_millis(100), sleep(Duration::from_millis(10)))
        .await
        .unwrap();
    manager.discard_file_queue_recovery(revision).unwrap();
    let current_epoch = manager.file_queue_persistence_epoch.load(Ordering::Acquire);
    assert_ne!(current_epoch, old_epoch);
    assert!(store.load().unwrap().is_none());
    release.send(()).unwrap();
    let rejected = update(&mut updates, 201).await;
    assert_eq!((rejected.epoch, rejected.host_id), (old_epoch, host));
    assert!(rejected.result.is_err());
    assert_ne!(
        rejected.epoch, current_epoch,
        "stale update could be treated as current protection"
    );
    assert!(
        store.load().unwrap().is_none(),
        "old queued job resurrected the discarded disk record"
    );
    assert!(manager.file_queue_recovery_summary().is_none());
    assert_eq!(manager.file_queue_protection_status(false), ("empty", None));
    assert_eq!(
        fs::read(source.path).unwrap(),
        b"synthetic queue source remains unchanged"
    );
}

#[tokio::test]
async fn native_queue_worker_cannot_switch_authenticated_targets_or_clear_another_targets_record() {
    let fixture = Fixture::new();
    let (manager, store) = fixture.manager();
    let first = fixture.file("host-a-source.bin", 11);
    let second = fixture.file("host-b-source.bin", 12);
    let host_a = [13; 16];
    let host_b = [14; 16];
    let worker = manager.file_queue_worker().unwrap();
    let mut updates = worker.updates();
    worker
        .submit(
            manager.clone(),
            host_a,
            VecDeque::from([first.clone()]),
            true,
            301,
        )
        .unwrap();
    assert_eq!(update(&mut updates, 301).await.result, Ok(()));
    let protected_before = fs::read(fixture.root.join("queue.bin")).unwrap();
    worker
        .submit(manager.clone(), host_b, VecDeque::new(), false, 302)
        .unwrap();
    let denied = update(&mut updates, 302).await;
    assert_eq!(denied.host_id, host_b);
    assert!(denied.result.is_err());
    assert_eq!(
        fs::read(fixture.root.join("queue.bin")).unwrap(),
        protected_before
    );
    let saved = store.load().unwrap().unwrap();
    assert_eq!(saved.host_device_id(), host_a);
    assert_eq!(
        saved.files()[0].outgoing_file().transfer_id,
        first.transfer_id
    );
    assert!(manager.restored_file_queue_for(host_b).is_empty());
    let revision = manager.file_queue_recovery_summary().unwrap().revision;
    manager.discard_file_queue_recovery(revision).unwrap();
    worker
        .submit(
            manager.clone(),
            host_b,
            VecDeque::from([second.clone()]),
            false,
            303,
        )
        .unwrap();
    let accepted = update(&mut updates, 303).await;
    assert_eq!(
        accepted.epoch,
        manager.file_queue_persistence_epoch.load(Ordering::Acquire)
    );
    assert_eq!(accepted.result, Ok(()));
    let saved = store.load().unwrap().unwrap();
    assert_eq!(saved.host_device_id(), host_b);
    assert_eq!(saved.files().len(), 1);
    assert_eq!(
        saved.files()[0].outgoing_file().transfer_id,
        second.transfer_id
    );
    assert!(manager.restored_file_queue_for(host_a).is_empty());
    assert_eq!(manager.restored_file_queue_for(host_b).len(), 1);
}

#[tokio::test]
async fn actual_dpapi_store_failure_stays_memory_only_and_same_snapshot_can_be_retried() {
    let fixture = Fixture::new();
    let blocked = fixture.root.join("not-a-directory");
    fs::write(&blocked, b"owned failure fixture").unwrap();
    let mut manager = ControllerManager {
        file_queue_recovery_store: Some(WindowsFileQueueRecoveryStore::new(
            blocked.join("queue.bin"),
        )),
        ..Default::default()
    };
    let source = fixture.file("retry-store-source.bin", 15);
    let host = [16; 16];
    let worker = manager.file_queue_worker().unwrap();
    let mut updates = worker.updates();
    worker
        .submit(
            manager.clone(),
            host,
            VecDeque::from([source.clone()]),
            true,
            401,
        )
        .unwrap();
    assert!(
        update(&mut updates, 401).await.result.is_err(),
        "worker reported successful protection after the real store failed"
    );
    assert_eq!(manager.file_queue_protection_status(true).0, "memoryOnly");
    assert!(manager.file_queue_recovery_error().is_some());
    assert_eq!(manager.restored_file_queue_for(host).len(), 1);
    let store = WindowsFileQueueRecoveryStore::new(fixture.root.join("queue.bin"));
    manager.file_queue_recovery_store = Some(store.clone());
    worker
        .submit(
            manager.clone(),
            host,
            VecDeque::from([source.clone()]),
            true,
            402,
        )
        .unwrap();
    assert_eq!(update(&mut updates, 402).await.result, Ok(()));
    assert_eq!(
        manager.file_queue_protection_status(true),
        ("protected", None)
    );
    let saved = store.load().unwrap().unwrap();
    assert_eq!(saved.host_device_id(), host);
    assert_eq!(saved.files().len(), 1);
    assert_eq!(
        saved.files()[0].outgoing_file().transfer_id,
        source.transfer_id
    );
    assert_eq!(
        fs::read(source.path).unwrap(),
        b"synthetic queue source remains unchanged"
    );
}
