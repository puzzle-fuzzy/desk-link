//! Serialize DPAPI/recovery-store IO away from the controller event loop.
//! One immutable snapshot may be active and one newest snapshot may wait.

use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, atomic::Ordering},
};

use apps_windows::{transfer::OutgoingFile, transfer_recovery::MAX_FILE_QUEUE_RECOVERY_ITEMS};
use tokio::sync::watch;

use super::ControllerManager;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct QueuePersistenceUpdate {
    pub epoch: u64,
    pub revision: u64,
    pub host_id: [u8; 16],
    pub result: Result<(), String>,
}

struct Pending<T> {
    epoch: u64,
    revision: u64,
    host_id: [u8; 16],
    payload: T,
}

struct State<T> {
    pending: Option<Pending<T>>,
    closed: bool,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    changed: Condvar,
}

/// The thread captures this state and its processor, never a controller. A job
/// owns its controller clone only until that immutable snapshot is processed.
struct LatestWorker<T> {
    shared: Arc<Shared<T>>,
    updates: watch::Receiver<Option<QueuePersistenceUpdate>>,
}

impl<T: Send + 'static> LatestWorker<T> {
    fn spawn(
        mut process: impl FnMut(T) -> Result<(), String> + Send + 'static,
    ) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                pending: None,
                closed: false,
            }),
            changed: Condvar::new(),
        });
        let worker = shared.clone();
        let (published, updates) = watch::channel(None);
        std::thread::Builder::new()
            .name("desklink-file-queue-save".to_owned())
            .spawn(move || {
                loop {
                    let job = {
                        let mut state = worker
                            .state
                            .lock()
                            .unwrap_or_else(|poison| poison.into_inner());
                        while state.pending.is_none() && !state.closed {
                            state = worker
                                .changed
                                .wait(state)
                                .unwrap_or_else(|poison| poison.into_inner());
                        }
                        match state.pending.take() {
                            Some(job) => job,
                            None => return,
                        }
                    };
                    let Pending {
                        epoch,
                        revision,
                        host_id,
                        payload,
                    } = job;
                    let result = process(payload);
                    published.send_replace(Some(QueuePersistenceUpdate {
                        epoch,
                        revision,
                        host_id,
                        result,
                    }));
                }
            })
            .map_err(|_| "DeskLink 无法启动等待队列保存线程。".to_owned())?;
        Ok(Self { shared, updates })
    }

    fn submit(
        &self,
        epoch: u64,
        revision: u64,
        host_id: [u8; 16],
        payload: T,
    ) -> Result<(), String> {
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| "DeskLink 等待队列保存状态不可用。".to_owned())?;
        if state.closed {
            return Err("DeskLink 等待队列保存线程已关闭。".to_owned());
        }
        // The caller owns generation ordering. Preserve its revision and target
        // exactly; completion of an older active job never rewrites this slot.
        state.pending = Some(Pending {
            epoch,
            revision,
            host_id,
            payload,
        });
        drop(state);
        self.shared.changed.notify_one();
        Ok(())
    }

    fn updates(&self) -> watch::Receiver<Option<QueuePersistenceUpdate>> {
        self.updates.clone()
    }
}

impl<T> Drop for LatestWorker<T> {
    fn drop(&mut self) {
        // Do not wait for a blocked OS write from the controller thread. The
        // worker completes it, consumes the one final pending snapshot and exits.
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.closed = true;
        drop(state);
        self.shared.changed.notify_one();
    }
}

struct QueueSnapshot {
    epoch: u64,
    manager: ControllerManager,
    host_id: [u8; 16],
    files: VecDeque<OutgoingFile>,
    paused: bool,
}

/// Keep this handle behind the manager's existing Arc when the manager is
/// cloned. No permanently captured manager/controller keeps the worker alive.
pub(super) struct QueuePersistence {
    worker: LatestWorker<QueueSnapshot>,
}

impl QueuePersistence {
    pub fn spawn() -> Result<Self, String> {
        Self::spawn_with(|| {})
    }

    #[cfg(test)]
    pub(super) fn spawn_with_before_persist(
        before: impl FnMut() + Send + 'static,
    ) -> Result<Self, String> {
        Self::spawn_with(before)
    }

    fn spawn_with(mut before: impl FnMut() + Send + 'static) -> Result<Self, String> {
        let worker = LatestWorker::spawn(move |snapshot: QueueSnapshot| {
            before();
            snapshot.manager.remember_file_queue_if_epoch(
                snapshot.epoch,
                snapshot.host_id,
                &snapshot.files,
                snapshot.paused,
            )?;
            if let Some(error) = snapshot.manager.file_queue_recovery_error() {
                return Err(error);
            }
            let (state, message) = snapshot
                .manager
                .file_queue_protection_status(!snapshot.files.is_empty());
            if state == "memoryOnly" {
                Err(message.unwrap_or_else(|| "等待队列尚未加密保存。".to_owned()))
            } else {
                Ok(())
            }
        })?;
        Ok(Self { worker })
    }

    pub fn submit(
        &self,
        manager: ControllerManager,
        host_id: [u8; 16],
        files: VecDeque<OutgoingFile>,
        paused: bool,
        revision: u64,
    ) -> Result<(), String> {
        if files.len() > MAX_FILE_QUEUE_RECOVERY_ITEMS {
            return Err("等待队列及交接中的文件总数不能超过 20 个。".to_owned());
        }
        if host_id == [0; 16] {
            return Err("等待队列缺少已认证的目标设备。".to_owned());
        }
        let epoch = manager.file_queue_persistence_epoch.load(Ordering::Acquire);
        self.worker.submit(
            epoch,
            revision,
            host_id,
            QueueSnapshot {
                epoch,
                manager,
                host_id,
                files,
                paused,
            },
        )
    }

    pub fn updates(&self) -> watch::Receiver<Option<QueuePersistenceUpdate>> {
        self.worker.updates()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::{Duration, sleep, timeout};

    struct Payload {
        value: u64,
        retired: Arc<AtomicUsize>,
    }
    impl Drop for Payload {
        fn drop(&mut self) {
            self.retired.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[tokio::test]
    async fn blocked_store_keeps_timers_live_and_retains_only_active_and_latest_snapshots() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let worker_seen = seen.clone();
        let (ready, entered) = tokio::sync::oneshot::channel();
        let mut ready = Some(ready);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let worker = LatestWorker::spawn(move |payload: Payload| {
            if payload.value == 1 {
                ready.take().unwrap().send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            worker_seen.lock().unwrap().push(payload.value);
            Ok(())
        })
        .unwrap();
        let retired = Arc::new(AtomicUsize::new(0));
        let mut updates = worker.updates();
        worker
            .submit(
                41,
                101,
                [7; 16],
                Payload {
                    value: 1,
                    retired: retired.clone(),
                },
            )
            .unwrap();
        timeout(Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        worker
            .submit(
                41,
                102,
                [7; 16],
                Payload {
                    value: 2,
                    retired: retired.clone(),
                },
            )
            .unwrap();
        worker
            .submit(
                41,
                103,
                [7; 16],
                Payload {
                    value: 3,
                    retired: retired.clone(),
                },
            )
            .unwrap();
        assert_eq!(
            retired.load(Ordering::Acquire),
            1,
            "superseded pending snapshot was retained"
        );
        timeout(Duration::from_millis(100), sleep(Duration::from_millis(10)))
            .await
            .unwrap();
        assert!(
            updates.borrow().is_none(),
            "snapshot was reported protected before the store returned"
        );
        release.send(()).unwrap();
        timeout(Duration::from_secs(2), async {
            loop {
                updates.changed().await.unwrap();
                if updates
                    .borrow()
                    .as_ref()
                    .is_some_and(|update| update.revision == 103)
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        let latest = updates.borrow().clone().unwrap();
        assert_eq!(latest.host_id, [7; 16]);
        assert_eq!(latest.epoch, 41);
        assert_eq!(latest.result, Ok(()));
        assert_eq!(*seen.lock().unwrap(), [1, 3]);
        assert_eq!(retired.load(Ordering::Acquire), 3);
        drop(worker);
        // `changed` can have observed the active job's version while borrow()
        // already saw the newest job. Drain any unseen version before proving
        // the sender/thread closed; neither value implies a new persistence IO.
        timeout(Duration::from_secs(2), async {
            while updates.changed().await.is_ok() {}
        })
        .await
        .unwrap();
        assert_eq!(updates.borrow().as_ref().unwrap().revision, 103);
    }

    #[tokio::test]
    async fn dropping_handle_finishes_active_store_and_last_pending_snapshot_before_exit() {
        let (ready, entered) = tokio::sync::oneshot::channel();
        let mut ready = Some(ready);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let processed = Arc::new(Mutex::new(Vec::new()));
        let worker_processed = processed.clone();
        let worker = LatestWorker::spawn(move |value: u64| {
            if value == 1 {
                ready.take().unwrap().send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            worker_processed.lock().unwrap().push(value);
            Ok(())
        })
        .unwrap();
        let mut updates = worker.updates();
        worker.submit(51, 1, [8; 16], 1).unwrap();
        timeout(Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        worker.submit(51, 2, [8; 16], 2).unwrap();
        let began = std::time::Instant::now();
        drop(worker);
        assert!(
            began.elapsed() < Duration::from_millis(100),
            "handle Drop waited for disk IO"
        );
        release.send(()).unwrap();
        timeout(Duration::from_secs(2), async {
            while updates.changed().await.is_ok() {}
        })
        .await
        .unwrap();
        assert_eq!(*processed.lock().unwrap(), [1, 2]);
        let last = updates.borrow().clone().unwrap();
        assert_eq!(
            (last.revision, last.host_id, last.result),
            (2, [8; 16], Ok(()))
        );
    }

    #[tokio::test]
    async fn failed_store_keeps_exact_revision_and_target_and_does_not_stop_later_jobs() {
        let worker = LatestWorker::spawn(|value: bool| {
            if value {
                Ok(())
            } else {
                Err("synthetic store failure".to_owned())
            }
        })
        .unwrap();
        let mut updates = worker.updates();
        worker.submit(61, 21, [9; 16], false).unwrap();
        timeout(Duration::from_secs(2), updates.changed())
            .await
            .unwrap()
            .unwrap();
        let failed = updates.borrow().clone().unwrap();
        assert_eq!(failed.revision, 21);
        assert_eq!(failed.epoch, 61);
        assert_eq!(failed.host_id, [9; 16]);
        assert_eq!(failed.result, Err("synthetic store failure".to_owned()));
        worker.submit(62, 22, [10; 16], true).unwrap();
        timeout(Duration::from_secs(2), updates.changed())
            .await
            .unwrap()
            .unwrap();
        let latest = updates.borrow().clone().unwrap();
        assert_eq!(
            (latest.epoch, latest.revision, latest.host_id, latest.result),
            (62, 22, [10; 16], Ok(()))
        );
    }

    #[tokio::test]
    async fn job_owned_controller_clone_drops_without_a_permanent_worker_cycle() {
        struct Owner {
            worker: LatestWorker<Arc<Owner>>,
        }
        let (ready, entered) = tokio::sync::oneshot::channel();
        let mut ready = Some(ready);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let worker = LatestWorker::spawn(move |owner: Arc<Owner>| {
            ready.take().unwrap().send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
            drop(owner);
            Ok(())
        })
        .unwrap();
        let owner = Arc::new(Owner { worker });
        let weak_owner = Arc::downgrade(&owner);
        let mut updates = owner.worker.updates();
        owner.worker.submit(1, 1, [11; 16], owner.clone()).unwrap();
        timeout(Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        drop(owner);
        assert!(
            weak_owner.upgrade().is_some(),
            "active immutable job lost its owner before persistence completed"
        );
        release.send(()).unwrap();
        timeout(Duration::from_secs(2), async {
            while updates.changed().await.is_ok() {}
        })
        .await
        .unwrap();
        assert!(
            weak_owner.upgrade().is_none(),
            "worker retained its owner after the final job completed"
        );
    }
}
