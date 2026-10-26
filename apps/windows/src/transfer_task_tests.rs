//! Actor integration through real protocol codec and authenticated packet encryption.
use crate::{
    transfer_checkpoint::{FILE_BLOCK_BYTES, SourceIndex},
    transfer_task::FileTaskService,
};
use desklink_protocol::{
    FileTaskAction, FileTaskMessage, FileTransferCapabilities, ResumeCheckpoint, TransferResult,
};
use std::{
    fs,
    fs::OpenOptions,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::time::{Instant, sleep, timeout};
#[path = "../test-support/file-task-fixture.rs"]
mod fixture;
use fixture::*;

#[tokio::test]
async fn encrypted_actors_transfer_real_file_and_directory_in_both_directions() {
    let fixture = Fixture::new();
    let bytes = sample(3);
    let source = fixture.path("你好.bin");
    fs::write(&source, &bytes).unwrap();
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    let upload = [21; 16];
    start(&peers.a, &source, upload).await;
    assert_eq!(
        terminal(&mut peers.a_progress, upload).await.state,
        "completed"
    );
    assert_eq!(
        terminal(&mut peers.b_progress, upload).await.state,
        "completed"
    );
    assert_eq!(
        fs::read(fixture.received(false, upload, "你好.bin")).unwrap(),
        bytes
    );
    assert_eq!(fs::read(&source).unwrap(), bytes);

    let folder = fixture.path("原始目录");
    fs::create_dir_all(folder.join("中文/空目录")).unwrap();
    fs::write(folder.join("中文/文件.txt"), "目录内容").unwrap();
    fs::write(folder.join("empty.bin"), []).unwrap();
    let download = [22; 16];
    start(&peers.b, &folder, download).await;
    let result = terminal(&mut peers.b_progress, download).await;
    assert_eq!(result.state, "completed", "{result:?}");
    assert_eq!(result.failed_items, 0);
    assert_eq!(
        terminal(&mut peers.a_progress, download).await.state,
        "completed"
    );
    assert_eq!(
        fs::read(fixture.received(true, download, "中文/文件.txt")).unwrap(),
        "目录内容".as_bytes()
    );
    assert!(fixture.received(true, download, "中文/空目录").is_dir());
    assert_eq!(
        fs::metadata(fixture.received(true, download, "empty.bin"))
            .unwrap()
            .len(),
        0
    );
    assert!(folder.join("中文/文件.txt").is_file());
    assert!(peers.stats.lock().unwrap().encrypted_messages > 20);
    peers.close().await;
}

#[tokio::test]
async fn encrypted_actor_receiver_rejection_writes_no_destination() {
    let fixture = Fixture::new();
    let source = fixture.path("rejected.bin");
    fs::write(&source, sample(1)).unwrap();
    let mut peers = Peers::new(&fixture, false, Duration::ZERO).await;
    let task = [23; 16];
    start(&peers.a, &source, task).await;
    let result = terminal(&mut peers.a_progress, task).await;
    assert_eq!(result.state, "failed");
    assert_eq!(result.message.as_deref(), Some("接收端已拒绝此项。"));
    assert_eq!(fs::read_dir(fixture.path("receive-b")).unwrap().count(), 0);
    assert_eq!(peers.stats.lock().unwrap().blocks.len(), 0);
    peers.close().await;
}

#[tokio::test]
async fn encrypted_actor_disconnect_and_recreation_resume_from_durable_ack() {
    let fixture = Fixture::new();
    let source = fixture.path("restart.bin");
    let bytes = sample(40);
    fs::write(&source, &bytes).unwrap();
    let peers = Peers::new(&fixture, true, Duration::from_millis(20)).await;
    let task = [24; 16];
    start(&peers.a, &source, task).await;
    timeout(Duration::from_secs(10), async {
        loop {
            if peers.stats.lock().unwrap().acknowledgements >= 2 {
                break;
            }
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    peers.close().await;
    assert!(!fixture.received(false, task, "restart.bin").exists());
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    peers.a.retry(task, None).unwrap();
    let result = terminal(&mut peers.a_progress, task).await;
    assert_eq!(result.state, "completed", "{result:?}");
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    let stats = peers.stats.lock().unwrap();
    let resumed = stats.resumes.iter().find(|(id, _)| *id == task).unwrap().1;
    assert!(resumed >= u64::from(FILE_BLOCK_BYTES) * 2, "{stats:?}");
    assert!(
        stats
            .blocks
            .iter()
            .filter(|(id, _)| *id == task)
            .all(|(_, offset)| *offset >= resumed)
    );
    drop(stats);
    assert_eq!(
        fs::read(fixture.received(false, task, "restart.bin")).unwrap(),
        bytes
    );
    peers.close().await;
}

#[tokio::test]
async fn encrypted_actor_cancel_during_backpressure_keeps_async_timers_responsive() {
    let fixture = Fixture::new();
    let source = fixture.path("cancel.bin");
    fs::write(&source, sample(40)).unwrap();
    let mut peers = Peers::new(&fixture, true, Duration::from_millis(40)).await;
    let task = [25; 16];
    start(&peers.a, &source, task).await;
    timeout(Duration::from_secs(10), async {
        while peers.stats.lock().unwrap().acknowledgements == 0 {
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    peers.a.action(task, FileTaskAction::Cancel).unwrap();
    let timer = timeout(Duration::from_millis(100), sleep(Duration::from_millis(10))).await;
    assert!(timer.is_ok());
    assert_eq!(
        terminal(&mut peers.a_progress, task).await.state,
        "cancelled"
    );
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "cancelled"
    );
    assert!(!fixture.received(false, task, "cancel.bin").exists());
    assert!(source.exists());
    peers.close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn cancelling_a_full_outgoing_queue_releases_the_source_pin_within_500_ms() {
    let fixture = Fixture::new();
    let source = fixture.path("full-outgoing-queue.bin");
    let bytes = sample(5);
    fs::write(&source, &bytes).unwrap();
    let mut proofs = SourceIndex::build(
        &source,
        &fixture.path("proofs.index"),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let empty_proof = proofs.resume_proof(0).unwrap();
    let first = proofs.resume_proof(u64::from(FILE_BLOCK_BYTES)).unwrap();
    let second = proofs
        .resume_proof(u64::from(FILE_BLOCK_BYTES) * 2)
        .unwrap();
    drop(proofs);
    let mut service = FileTaskService::spawn(fixture.options(true, true)).unwrap();
    timeout(Duration::from_secs(5), service.outgoing.recv())
        .await
        .unwrap()
        .unwrap();
    service
        .handle
        .ingest(FileTaskMessage::Capabilities(
            FileTransferCapabilities::WINDOWS,
        ))
        .unwrap();
    let task = [26; 16];
    start(&service.handle, &source, task).await;
    assert!(matches!(
        timeout(Duration::from_secs(5), service.outgoing.recv())
            .await
            .unwrap()
            .unwrap(),
        FileTaskMessage::Offer { .. }
    ));
    service
        .handle
        .ingest(FileTaskMessage::Decision {
            task_id: task,
            result: TransferResult::Completed,
            budget: FileTransferCapabilities::WINDOWS,
        })
        .unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(5), service.outgoing.recv())
            .await
            .unwrap()
            .unwrap(),
        FileTaskMessage::Entry { .. }
    ));
    service
        .handle
        .ingest(FileTaskMessage::EntryDecision {
            task_id: task,
            item_id: 1,
            result: TransferResult::Completed,
            checkpoint: ResumeCheckpoint {
                offset: 0,
                proof: empty_proof,
            },
        })
        .unwrap();
    timeout(Duration::from_secs(5), async {
        while service.outgoing.len() < 1 {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    service
        .handle
        .ingest(FileTaskMessage::BlockAck {
            task_id: task,
            item_id: 1,
            checkpoint: ResumeCheckpoint {
                offset: u64::from(FILE_BLOCK_BYTES),
                proof: first,
            },
        })
        .unwrap();
    timeout(Duration::from_secs(5), async {
        while service.outgoing.len() < 2 {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    service
        .handle
        .ingest(FileTaskMessage::BlockAck {
            task_id: task,
            item_id: 1,
            checkpoint: ResumeCheckpoint {
                offset: u64::from(FILE_BLOCK_BYTES) * 2,
                proof: second,
            },
        })
        .unwrap();
    sleep(Duration::from_millis(5)).await;
    assert!(
        OpenOptions::new().write(true).open(&source).is_err(),
        "fixture did not hold a stable source pin"
    );
    assert_eq!(
        service.outgoing.len(),
        2,
        "fixture did not saturate the outgoing channel"
    );
    service.handle.action(task, FileTaskAction::Cancel).unwrap();
    let began = Instant::now();
    let released = timeout(Duration::from_millis(500), async {
        loop {
            if let Ok(file) = OpenOptions::new().write(true).open(&source) {
                return file;
            }
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    assert!(
        released.is_ok(),
        "cancel retained source pin behind a saturated wire queue for {:?}",
        began.elapsed()
    );
    drop(released);
    assert_eq!(
        terminal(&mut service.progress, task).await.state,
        "cancelled"
    );
    drop(service);
    sleep(Duration::from_millis(150)).await;
    assert_eq!(fs::read(&source).unwrap(), bytes);
}

#[tokio::test]
async fn encrypted_directory_conflict_is_partial_and_retry_sends_only_failed_file_bytes() {
    let fixture = Fixture::new();
    let folder = fixture.path("partial-source");
    fs::create_dir(&folder).unwrap();
    let failed_bytes = sample(1);
    let good_bytes = b"independently completed item";
    fs::write(folder.join("blocked.bin"), &failed_bytes).unwrap();
    fs::write(folder.join("good.bin"), good_bytes).unwrap();
    let task = [27; 16];
    let conflict = fixture.received(false, task, "blocked.bin");
    let hook_path = conflict.clone();
    let injected = Arc::new(AtomicBool::new(false));
    let hook_injected = injected.clone();
    let hook: BridgeHook = Arc::new(move |message| {
        if let FileTaskMessage::Entry { entry, .. } = message
            && entry.relative_path == "blocked.bin"
            && !hook_injected.swap(true, Ordering::AcqRel)
        {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&hook_path)
                .unwrap();
            std::io::Write::write_all(&mut file, b"test-owned preexisting conflict").unwrap();
            file.sync_all().unwrap();
        }
    });
    let mut peers = Peers::with_hook(&fixture, true, Duration::ZERO, hook).await;
    start(&peers.a, &folder, task).await;
    let first = terminal(&mut peers.a_progress, task).await;
    assert_eq!(first.state, "partial", "{first:?}");
    assert_eq!((first.completed_items, first.failed_items), (1, 1));
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    assert!(injected.load(Ordering::Acquire));
    assert_eq!(
        fs::read(&conflict).unwrap(),
        b"test-owned preexisting conflict"
    );
    assert_eq!(
        fs::read(fixture.received(false, task, "good.bin")).unwrap(),
        good_bytes
    );
    let failed_item = first
        .items
        .iter()
        .find(|item| item.path == "blocked.bin")
        .unwrap()
        .id
        .parse::<u64>()
        .unwrap();
    let before_retry = peers.stats.lock().unwrap().transferred_bytes;
    let offers_before_retry = peers.stats.lock().unwrap().offers;
    assert_eq!(before_retry, good_bytes.len() as u64);
    // Remove only the explicitly generated conflict, after the failed create
    // proved it was preserved. The source is never modified.
    fs::remove_file(&conflict).unwrap();
    timeout(Duration::from_secs(3), async {
        loop {
            match peers.a.retry(task, Some(failed_item)) {
                Ok(()) => break,
                Err(TransferResult::Busy) => sleep(Duration::from_millis(2)).await,
                Err(error) => panic!("retry failed: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
    wait_for_offer(&peers, offers_before_retry).await;
    let retried = terminal(&mut peers.a_progress, task).await;
    assert_eq!(retried.state, "completed", "{retried:?}");
    assert_eq!((retried.completed_items, retried.failed_items), (2, 0));
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    assert_eq!(
        peers.stats.lock().unwrap().transferred_bytes - before_retry,
        failed_bytes.len() as u64
    );
    assert_eq!(fs::read(&conflict).unwrap(), failed_bytes);
    assert_eq!(fs::read(folder.join("blocked.bin")).unwrap(), failed_bytes);
    assert_eq!(fs::read(folder.join("good.bin")).unwrap(), good_bytes);
    peers.close().await;
}

async fn deliver(handle: &crate::transfer_task::FileTaskHandle, message: FileTaskMessage) {
    let bytes =
        desklink_protocol::encode_transfer(&desklink_protocol::TransferMessage::FileTask(message))
            .unwrap();
    let desklink_protocol::TransferMessage::FileTask(message) =
        desklink_protocol::decode_transfer(&bytes).unwrap()
    else {
        unreachable!()
    };
    timeout(Duration::from_secs(5), async {
        loop {
            match handle.ingest(message.clone()) {
                Ok(()) => break,
                Err(TransferResult::Busy) => sleep(Duration::from_millis(2)).await,
                Err(error) => panic!("fixture ingress failed: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn receiver_final_disk_verification_refuses_a_wrong_whole_content_root_and_reports_partial() {
    let fixture = Fixture::new();
    let source = fixture.path("original.bin");
    let bytes = b"real original fixture bytes";
    fs::write(&source, bytes).unwrap();
    let index = SourceIndex::build(
        &source,
        &fixture.path("original.index"),
        &AtomicBool::new(false),
        |_| {},
    )
    .unwrap();
    let expected_root = index.content_root();
    drop(index);
    let mut receiver = FileTaskService::spawn(fixture.options(false, true)).unwrap();
    timeout(Duration::from_secs(5), receiver.outgoing.recv())
        .await
        .unwrap()
        .unwrap();
    deliver(
        &receiver.handle,
        FileTaskMessage::Capabilities(FileTransferCapabilities::WINDOWS),
    )
    .await;
    let task = [28; 16];
    deliver(
        &receiver.handle,
        FileTaskMessage::Offer {
            task_id: task,
            request_id: None,
            name: "corrupt.bin".into(),
            directory: false,
        },
    )
    .await;
    assert!(matches!(
        timeout(Duration::from_secs(5), receiver.outgoing.recv())
            .await
            .unwrap()
            .unwrap(),
        FileTaskMessage::Decision {
            result: TransferResult::Completed,
            ..
        }
    ));
    deliver(
        &receiver.handle,
        FileTaskMessage::Entry {
            task_id: task,
            entry: desklink_protocol::ManifestEntry {
                item_id: 1,
                relative_path: "corrupt.bin".into(),
                kind: desklink_protocol::TransferEntryKind::File,
                size: bytes.len() as u64,
                content_root: Some(expected_root),
            },
        },
    )
    .await;
    assert!(matches!(
        timeout(Duration::from_secs(5), receiver.outgoing.recv())
            .await
            .unwrap()
            .unwrap(),
        FileTaskMessage::EntryDecision {
            result: TransferResult::Completed,
            ..
        }
    ));
    let corrupt = vec![0xff; bytes.len()];
    deliver(
        &receiver.handle,
        FileTaskMessage::Block {
            task_id: task,
            item_id: 1,
            offset: 0,
            digest: crate::transfer_checkpoint::block_digest(&corrupt),
            bytes: corrupt,
        },
    )
    .await;
    let ack = timeout(Duration::from_secs(5), receiver.outgoing.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(ack, FileTaskMessage::BlockAck { checkpoint, .. } if checkpoint.offset == bytes.len() as u64 && checkpoint.proof != expected_root)
    );
    deliver(
        &receiver.handle,
        FileTaskMessage::EntryComplete {
            task_id: task,
            item_id: 1,
            content_root: expected_root,
        },
    )
    .await;
    assert!(matches!(
        timeout(Duration::from_secs(5), receiver.outgoing.recv())
            .await
            .unwrap()
            .unwrap(),
        FileTaskMessage::EntryResult {
            result: TransferResult::InvalidData,
            ..
        }
    ));
    assert!(
        !fixture.received(false, task, "corrupt.bin").exists(),
        "unverified bytes were published"
    );
    deliver(
        &receiver.handle,
        FileTaskMessage::ManifestComplete {
            task_id: task,
            entries: 1,
            total_bytes: bytes.len() as u64,
        },
    )
    .await;
    assert!(matches!(
        timeout(Duration::from_secs(5), receiver.outgoing.recv())
            .await
            .unwrap()
            .unwrap(),
        FileTaskMessage::TaskResult {
            completed: 0,
            failed: 1,
            ..
        }
    ));
    let progress = terminal(&mut receiver.progress, task).await;
    assert_eq!(progress.state, "partial");
    assert_eq!((progress.completed_items, progress.failed_items), (0, 1));
    assert_eq!(fs::read(&source).unwrap(), bytes);
    drop(receiver);
    sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn encrypted_restart_refuses_a_changed_source_even_with_restored_length_and_mtime() {
    let fixture = Fixture::new();
    let source = fixture.path("changed-source.bin");
    let bytes = sample(40);
    fs::write(&source, &bytes).unwrap();
    let modified = fs::metadata(&source).unwrap().modified().unwrap();
    let peers = Peers::new(&fixture, true, Duration::from_millis(20)).await;
    let task = [29; 16];
    start(&peers.a, &source, task).await;
    timeout(Duration::from_secs(10), async {
        while peers.stats.lock().unwrap().acknowledgements < 2 {
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    peers.close().await;
    let mut changed = bytes.clone();
    changed[0] ^= 0xff;
    {
        let mut file = OpenOptions::new().write(true).open(&source).unwrap();
        std::io::Write::write_all(&mut file, &changed).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        file.sync_all().unwrap();
    }
    assert_eq!(fs::metadata(&source).unwrap().len(), bytes.len() as u64);
    assert_eq!(fs::metadata(&source).unwrap().modified().unwrap(), modified);
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    peers.a.retry(task, None).unwrap();
    let result = terminal(&mut peers.a_progress, task).await;
    assert_eq!(result.state, "partial", "{result:?}");
    assert_eq!((result.completed_items, result.failed_items), (0, 1));
    assert!(
        result
            .items
            .iter()
            .any(|item| item.message.as_deref() == Some("源文件已变化，请重新选择后发送。"))
    );
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    assert_eq!(peers.stats.lock().unwrap().transferred_bytes, 0);
    assert!(!fixture.received(false, task, "changed-source.bin").exists());
    assert_eq!(fs::read(&source).unwrap(), changed);
    peers.close().await;
}

async fn next_revision(
    progress: &mut tokio::sync::watch::Receiver<Option<crate::transfer_task::FileTaskProgress>>,
    revision: u64,
) {
    timeout(Duration::from_secs(5), async {
        while progress
            .borrow()
            .as_ref()
            .is_some_and(|snapshot| snapshot.revision <= revision)
        {
            progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
}

async fn wait_for_offer(peers: &Peers, previous: u64) {
    timeout(Duration::from_secs(5), async {
        while peers.stats.lock().unwrap().offers <= previous {
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn encrypted_receiver_can_request_retry_after_both_actors_are_recreated() {
    let fixture = Fixture::new();
    let source = fixture.path("receiver-retry.bin");
    let bytes = sample(40);
    fs::write(&source, &bytes).unwrap();
    let peers = Peers::new(&fixture, true, Duration::from_millis(20)).await;
    let task = [30; 16];
    start(&peers.a, &source, task).await;
    timeout(Duration::from_secs(10), async {
        while peers.stats.lock().unwrap().acknowledgements < 2 {
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    peers.close().await;
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    let before = peers
        .b_progress
        .borrow()
        .as_ref()
        .map(|snapshot| snapshot.revision)
        .unwrap_or(0);
    peers.b.retry(task, None).unwrap();
    next_revision(&mut peers.b_progress, before).await;
    let result = terminal(&mut peers.b_progress, task).await;
    assert_eq!(result.state, "completed", "{result:?}");
    assert_eq!(result.direction, "download");
    assert_eq!(
        terminal(&mut peers.a_progress, task).await.state,
        "completed"
    );
    let stats = peers.stats.lock().unwrap();
    assert!(
        stats
            .resumes
            .iter()
            .any(|(id, offset)| *id == task && *offset >= u64::from(FILE_BLOCK_BYTES) * 2),
        "{stats:?}"
    );
    assert!(stats.transferred_bytes < bytes.len() as u64);
    drop(stats);
    assert_eq!(
        fs::read(fixture.received(false, task, "receiver-retry.bin")).unwrap(),
        bytes
    );
    assert_eq!(
        fixture.source_resume_consents(),
        1,
        "receiver retry bypassed explicit source-operator consent or prompted twice"
    );
    peers.close().await;
}

#[tokio::test]
async fn paused_actors_restore_confirmed_progress_and_resume_after_recreation() {
    let fixture = Fixture::new();
    let source = fixture.path("paused-recovery.bin");
    let bytes = sample(40);
    fs::write(&source, &bytes).unwrap();
    let peers = Peers::new(&fixture, true, Duration::from_millis(20)).await;
    let task = [31; 16];
    start(&peers.a, &source, task).await;
    timeout(Duration::from_secs(10), async {
        while peers.stats.lock().unwrap().acknowledgements < 2 {
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    peers.a.action(task, FileTaskAction::Pause).unwrap();
    timeout(Duration::from_secs(5), async {
        loop {
            if peers
                .a_progress
                .borrow()
                .as_ref()
                .is_some_and(|snapshot| snapshot.state == "paused")
                && peers
                    .b_progress
                    .borrow()
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.state == "paused")
            {
                break;
            }
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let confirmed = peers
        .a_progress
        .borrow()
        .as_ref()
        .unwrap()
        .acknowledged_bytes;
    assert!(confirmed >= u64::from(FILE_BLOCK_BYTES));
    peers.close().await;
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    let restored = peers
        .a_progress
        .borrow()
        .clone()
        .expect("paused task was not restored");
    assert_eq!(restored.task_id, crate::transfer_task::id_text(task));
    assert_eq!(restored.state, "paused");
    assert_eq!(restored.acknowledged_bytes, confirmed);
    assert_eq!(peers.b_progress.borrow().as_ref().unwrap().state, "paused");
    peers.a.action(task, FileTaskAction::Resume).unwrap();
    next_revision(&mut peers.a_progress, restored.revision).await;
    let result = terminal(&mut peers.a_progress, task).await;
    assert_eq!(result.state, "completed", "{result:?}");
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    assert_eq!(
        fs::read(fixture.received(false, task, "paused-recovery.bin")).unwrap(),
        bytes
    );
    peers.close().await;
}

#[tokio::test]
async fn cancelled_active_part_retries_from_zero_with_a_fresh_staging_file() {
    let fixture = Fixture::new();
    let source = fixture.path("cancelled-retry.bin");
    let bytes = sample(40);
    fs::write(&source, &bytes).unwrap();
    let mut peers = Peers::new(&fixture, true, Duration::from_millis(10)).await;
    let task = [32; 16];
    start(&peers.a, &source, task).await;
    timeout(Duration::from_secs(10), async {
        while peers.stats.lock().unwrap().acknowledgements < 2 {
            sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    peers.a.action(task, FileTaskAction::Cancel).unwrap();
    let cancelled = terminal(&mut peers.a_progress, task).await;
    assert_eq!(cancelled.state, "cancelled");
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "cancelled"
    );
    assert!(
        !fixture
            .received(false, task, "cancelled-retry.bin")
            .exists()
    );
    let prior_resumes = peers.stats.lock().unwrap().resumes.len();
    let offers_before_retry = peers.stats.lock().unwrap().offers;
    timeout(Duration::from_secs(3), async {
        loop {
            match peers.a.retry(task, None) {
                Ok(()) => break,
                Err(TransferResult::Busy) => sleep(Duration::from_millis(2)).await,
                Err(error) => panic!("cancelled task retry failed: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
    wait_for_offer(&peers, offers_before_retry).await;
    let retried = terminal(&mut peers.a_progress, task).await;
    assert_eq!(retried.state, "completed", "{retried:?}");
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    assert_eq!(
        peers.stats.lock().unwrap().resumes[prior_resumes..]
            .iter()
            .find(|(id, _)| *id == task)
            .unwrap()
            .1,
        0
    );
    assert_eq!(
        fs::read(fixture.received(false, task, "cancelled-retry.bin")).unwrap(),
        bytes
    );
    assert_eq!(fs::read(&source).unwrap(), bytes);
    peers.close().await;
}

#[tokio::test]
async fn full_outgoing_selection_cancel_preserves_request_binding_until_retry_can_send_it() {
    let fixture = Fixture::new();
    let mut service = FileTaskService::spawn(fixture.options(true, true)).unwrap();
    // Keep the initial Capabilities packet queued, then add the local selector
    // request. These are the real two admitted outgoing messages.
    service.handle.request_remote(101, false).unwrap();
    timeout(Duration::from_secs(3), async {
        while service.outgoing.len() < 2 {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(service.handle.cancel_remote(), Err(TransferResult::Busy));
    let first = service.outgoing.recv().await.unwrap();
    assert_eq!(
        service.handle.cancel_remote(),
        Ok(true),
        "full queue lost the exact request before cancellation was admitted"
    );
    let mut messages = vec![first];
    for _ in 0..2 {
        messages.push(
            timeout(Duration::from_secs(3), service.outgoing.recv())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    assert!(messages.iter().any(|message| matches!(
        message,
        FileTaskMessage::SelectionRequest {
            request_id: 101,
            ..
        }
    )));
    assert!(messages.iter().any(|message| matches!(
        message,
        FileTaskMessage::SelectionCancel { request_id: 101 }
    )));
    assert_eq!(service.handle.cancel_remote(), Ok(false));
    drop(service);
    sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn cancellation_after_matching_offer_admission_cancels_that_task_instead_of_losing_selector_binding()
 {
    let fixture = Fixture::new();
    let mut service = FileTaskService::spawn(fixture.options(true, true)).unwrap();
    timeout(Duration::from_secs(3), service.outgoing.recv())
        .await
        .unwrap()
        .unwrap();
    deliver(
        &service.handle,
        FileTaskMessage::Capabilities(FileTransferCapabilities::WINDOWS),
    )
    .await;
    service.handle.request_remote(102, false).unwrap();
    assert!(matches!(
        timeout(Duration::from_secs(3), service.outgoing.recv())
            .await
            .unwrap()
            .unwrap(),
        FileTaskMessage::SelectionRequest {
            request_id: 102,
            ..
        }
    ));
    let task = [33; 16];
    deliver(
        &service.handle,
        FileTaskMessage::Offer {
            task_id: task,
            request_id: Some(102),
            name: "accepted-selector.bin".into(),
            directory: false,
        },
    )
    .await;
    assert_eq!(service.handle.cancel_remote(), Ok(true));
    let control = *service.controls.borrow();
    assert_eq!(
        control,
        Some((task, FileTaskAction::Cancel)),
        "accepted selector cancellation was sent to a different task"
    );
    let progress = terminal(&mut service.progress, task).await;
    assert_eq!(progress.state, "cancelled", "{progress:?}");
    assert_eq!(progress.task_id, crate::transfer_task::id_text(task));
    assert!(
        !fixture
            .received(true, task, "accepted-selector.bin")
            .exists()
    );
    drop(service);
    sleep(Duration::from_millis(100)).await;
}

#[tokio::test]
async fn forged_task_result_cannot_turn_a_known_partial_directory_into_success() {
    let fixture = Fixture::new();
    let folder = fixture.path("forged-result-source");
    fs::create_dir(&folder).unwrap();
    fs::write(folder.join("blocked.bin"), b"blocked source").unwrap();
    fs::write(folder.join("good.bin"), b"good source").unwrap();
    let task = [34; 16];
    let conflict = fixture.received(false, task, "blocked.bin");
    let hook_path = conflict.clone();
    let forged = Arc::new(AtomicBool::new(false));
    let hook_forged = forged.clone();
    let hook: BridgeHook = Arc::new(move |message| match message {
        FileTaskMessage::Entry { entry, .. } if entry.relative_path == "blocked.bin" => {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&hook_path)
                .unwrap();
            std::io::Write::write_all(&mut file, b"owned conflict survives").unwrap();
        }
        FileTaskMessage::TaskResult {
            task_id,
            completed,
            failed,
        } if *task_id == task => {
            assert_eq!((*completed, *failed), (1, 1));
            *completed = 2;
            *failed = 0;
            hook_forged.store(true, Ordering::Release);
        }
        _ => {}
    });
    let mut peers = Peers::with_hook(&fixture, true, Duration::ZERO, hook).await;
    start(&peers.a, &folder, task).await;
    let result = terminal(&mut peers.a_progress, task).await;
    assert!(forged.load(Ordering::Acquire));
    assert_eq!(result.state, "failed", "{result:?}");
    assert_eq!((result.completed_items, result.failed_items), (1, 1));
    assert_eq!(
        result.message.as_deref(),
        Some("内容或断点校验失败，请重试。")
    );
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    assert_eq!(fs::read(conflict).unwrap(), b"owned conflict survives");
    assert_eq!(
        fs::read(fixture.received(false, task, "good.bin")).unwrap(),
        b"good source"
    );
    peers.close().await;
}
