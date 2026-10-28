//! Actor integration through real protocol codec and authenticated packet encryption.
use crate::{
    transfer_checkpoint::{FILE_BLOCK_BYTES, SourceIndex},
    transfer_task::FileTaskService,
};
use desklink_protocol::{
    FileTaskAction, FileTaskMessage, FileTransferCapabilities, TransferResult,
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
use tokio::time::{sleep, timeout};
#[path = "../test-support/file-task-fixture.rs"]
mod fixture;
use fixture::*;

async fn retry_fixture_task(
    handle: &crate::transfer_task::FileTaskHandle,
    task: desklink_protocol::TransferId,
    item: Option<u64>,
) {
    timeout(Duration::from_secs(3), async {
        loop {
            match handle.retry(task, item) {
                Ok(()) => break,
                Err(TransferResult::Busy) => sleep(Duration::from_millis(2)).await,
                Err(error) => panic!("fixture retry rejected: {error:?}"),
            }
        }
    })
    .await
    .unwrap();
}

fn fixture_task_cache(
    fixture: &Fixture,
    a: bool,
    task: desklink_protocol::TransferId,
) -> std::path::PathBuf {
    let options = fixture.options(a, true);
    options
        .cache_root
        .join(options.peer_key)
        .join(crate::transfer_task::id_text(task))
}

#[cfg(windows)]
#[tokio::test]
async fn scan_sharing_failure_persists_partial_then_restart_retry_preserves_success_identity_and_bytes()
 {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let folder = fixture.path("scan-file-partial");
    fs::create_dir(&folder).unwrap();
    let blocked = folder.join("blocked.bin");
    let recovery_bytes = sample(2);
    fs::write(&blocked, &recovery_bytes).unwrap();
    fs::write(folder.join("good.bin"), b"successful sibling").unwrap();
    let owner = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&blocked)
        .unwrap();
    let task = [67; 16];
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    start(&peers.a, &folder, task).await;
    let partial = terminal(&mut peers.a_progress, task).await;
    assert_eq!(partial.state, "partial", "{partial:?}");
    assert_eq!((partial.completed_items, partial.failed_items), (1, 1));
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    let good_id = partial
        .items
        .iter()
        .find(|item| item.path == "good.bin")
        .unwrap()
        .id
        .clone();
    let failed_id = partial
        .items
        .iter()
        .find(|item| item.path == "blocked.bin")
        .unwrap()
        .id
        .parse()
        .unwrap();
    let manifest =
        fs::read_to_string(fixture_task_cache(&fixture, true, task).join("manifest.jsonl"))
            .unwrap();
    let records = manifest
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 2);
    assert!(records[0].get("source_path").is_some());
    assert_eq!(records[1]["scan_failure"], true);
    // Skipped scan failures do not reserve any destination identity or ledger.
    assert!(
        !fixture_task_cache(&fixture, false, task)
            .join(format!("{failed_id}.received.json"))
            .exists()
    );
    assert!(!fixture.received(false, task, "blocked.bin").exists());
    assert_eq!(
        peers.stats.lock().unwrap().transferred_bytes,
        b"successful sibling".len() as u64
    );
    peers.close().await;
    drop(owner);
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    retry_fixture_task(&peers.a, task, Some(failed_id)).await;
    wait_for_offer(&peers, 0).await;
    let complete = terminal(&mut peers.a_progress, task).await;
    assert_eq!(complete.state, "completed", "{complete:?}");
    assert_eq!((complete.completed_items, complete.failed_items), (2, 0));
    assert_eq!(
        complete
            .items
            .iter()
            .find(|item| item.path == "good.bin")
            .unwrap()
            .id,
        good_id
    );
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    assert_eq!(
        peers.stats.lock().unwrap().transferred_bytes,
        recovery_bytes.len() as u64
    );
    assert_eq!(
        fs::read(fixture.received(false, task, "blocked.bin")).unwrap(),
        recovery_bytes
    );
    assert_eq!(fs::read(blocked).unwrap(), recovery_bytes);
    peers.close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn scan_denied_subtree_recovers_after_receiver_restart_without_resending_its_sibling() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
    let fixture = Fixture::new();
    let folder = fixture.path("scan-subtree-partial");
    let blocked = folder.join("blocked");
    fs::create_dir_all(blocked.join("empty")).unwrap();
    fs::write(blocked.join("child.bin"), b"recovered subtree content").unwrap();
    fs::write(folder.join("good.bin"), b"retained sibling").unwrap();
    let owner = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
        .open(&blocked)
        .unwrap();
    let task = [68; 16];
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    start(&peers.a, &folder, task).await;
    let partial = terminal(&mut peers.a_progress, task).await;
    assert_eq!(partial.state, "partial", "{partial:?}");
    assert_eq!((partial.completed_items, partial.failed_items), (1, 1));
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    let good_id = partial
        .items
        .iter()
        .find(|item| item.path == "good.bin")
        .unwrap()
        .id
        .clone();
    peers.close().await;
    drop(owner);
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    retry_fixture_task(&peers.b, task, None).await;
    wait_for_offer(&peers, 0).await;
    let complete = terminal(&mut peers.a_progress, task).await;
    assert_eq!(complete.state, "completed", "{complete:?}");
    assert_eq!(
        complete
            .items
            .iter()
            .find(|item| item.path == "good.bin")
            .unwrap()
            .id,
        good_id
    );
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    assert_eq!(fixture.source_resume_consents(), 1);
    assert_eq!(
        peers.stats.lock().unwrap().transferred_bytes,
        b"recovered subtree content".len() as u64
    );
    assert!(fixture.received(false, task, "blocked/empty").is_dir());
    assert_eq!(
        fs::read(fixture.received(false, task, "blocked/child.bin")).unwrap(),
        b"recovered subtree content"
    );
    peers.close().await;
}

#[tokio::test]
async fn scan_entry_budget_keeps_valid_prefix_and_retry_resolves_marker_at_exact_budget() {
    let fixture = Fixture::new();
    let folder = fixture.path("scan-budget-partial");
    fs::create_dir(&folder).unwrap();
    let names = ["a.bin", "b.bin", "c.bin", "d.bin"];
    for name in names {
        fs::write(folder.join(name), b"four").unwrap();
    }
    let hook: BridgeHook = Arc::new(|message| {
        if let FileTaskMessage::Capabilities(capabilities) = message {
            capabilities.max_entries = 3;
        }
    });
    let task = [69; 16];
    let mut peers = Peers::with_hook(&fixture, true, Duration::ZERO, hook).await;
    start(&peers.a, &folder, task).await;
    let partial = terminal(&mut peers.a_progress, task).await;
    assert_eq!(partial.state, "partial", "{partial:?}");
    assert_eq!((partial.completed_items, partial.failed_items), (2, 1));
    assert_eq!(partial.scanned_items, 3);
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    let retained = partial
        .items
        .iter()
        .filter(|item| item.state == "completed")
        .map(|item| (item.path.clone(), item.id.clone()))
        .collect::<Vec<_>>();
    let missing = names
        .iter()
        .filter(|name| !retained.iter().any(|(path, _)| path == *name))
        .collect::<Vec<_>>();
    assert_eq!(missing.len(), 2);
    fs::remove_file(folder.join(missing[0])).unwrap();
    let before = peers.stats.lock().unwrap().transferred_bytes;
    let offers = peers.stats.lock().unwrap().offers;
    retry_fixture_task(&peers.a, task, None).await;
    wait_for_offer(&peers, offers).await;
    let complete = terminal(&mut peers.a_progress, task).await;
    assert_eq!(complete.state, "completed", "{complete:?}");
    assert_eq!((complete.completed_items, complete.failed_items), (3, 0));
    for (path, id) in retained {
        assert_eq!(
            complete
                .items
                .iter()
                .find(|item| item.path == path)
                .unwrap()
                .id,
            id
        );
    }
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    assert_eq!(peers.stats.lock().unwrap().transferred_bytes - before, 4);
    assert_eq!(
        fs::read(fixture.received(false, task, missing[1])).unwrap(),
        b"four"
    );
    peers.close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn tampered_scan_failure_cannot_retry_a_successful_sibling_and_become_completed() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let folder = fixture.path("scan-scope-tamper");
    fs::create_dir(&folder).unwrap();
    let blocked = folder.join("blocked.bin");
    fs::write(&blocked, b"still needs explicit recovery").unwrap();
    fs::write(folder.join("good.bin"), b"already saved").unwrap();
    let owner = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&blocked)
        .unwrap();
    let task = [70; 16];
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    start(&peers.a, &folder, task).await;
    let partial = terminal(&mut peers.a_progress, task).await;
    assert_eq!(partial.state, "partial");
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    peers.close().await;
    drop(owner);
    let manifest_path = fixture_task_cache(&fixture, true, task).join("manifest.jsonl");
    let mut records = fs::read_to_string(&manifest_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    let failure = records
        .iter_mut()
        .find(|record| record.get("scan_failure").is_some())
        .unwrap();
    failure["failure"]["retry_relative_path"] = "good.bin".into();
    let encoded = records
        .iter()
        .map(|record| serde_json::to_string(record).unwrap() + "\n")
        .collect::<String>();
    fs::write(&manifest_path, encoded.as_bytes()).unwrap();
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    retry_fixture_task(&peers.a, task, None).await;
    wait_for_offer(&peers, 0).await;
    let failed = terminal(&mut peers.a_progress, task).await;
    assert_eq!(failed.state, "failed", "{failed:?}");
    assert_eq!(
        failed.message.as_deref(),
        Some("内容或断点校验失败，请重试。")
    );
    assert_eq!(peers.stats.lock().unwrap().transferred_bytes, 0);
    assert_eq!(fs::read(&manifest_path).unwrap(), encoded.as_bytes());
    assert_eq!(
        fs::read(fixture.received(false, task, "good.bin")).unwrap(),
        b"already saved"
    );
    assert!(!fixture.received(false, task, "blocked.bin").exists());
    peers.close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn scan_junction_is_partial_and_repair_retry_never_reads_or_publishes_its_target() {
    let fixture = Fixture::new();
    let folder = fixture.path("scan-junction-source");
    let outside = fixture.path("outside-authorized-source");
    fs::create_dir(&folder).unwrap();
    fs::create_dir(&outside).unwrap();
    fs::write(folder.join("good.bin"), b"safe sibling").unwrap();
    fs::write(
        outside.join("private.bin"),
        b"outside canary remains unchanged",
    )
    .unwrap();
    let link = folder.join("blocked");
    let created = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(created.status.success(), "{created:?}");
    let task = [71; 16];
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    start(&peers.a, &folder, task).await;
    let partial = terminal(&mut peers.a_progress, task).await;
    assert_eq!(partial.state, "partial", "{partial:?}");
    assert_eq!((partial.completed_items, partial.failed_items), (1, 1));
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    assert!(
        !fixture
            .received(false, task, "blocked/private.bin")
            .exists()
    );
    assert_eq!(
        peers.stats.lock().unwrap().transferred_bytes,
        b"safe sibling".len() as u64
    );
    let failed_id = partial
        .items
        .iter()
        .find(|item| item.path == "blocked")
        .unwrap()
        .id
        .parse()
        .unwrap();
    peers.close().await;
    // Remove only our direct generated junction, then repair it as a real dir.
    fs::remove_dir(&link).unwrap();
    fs::create_dir(&link).unwrap();
    fs::write(link.join("repaired.bin"), b"new explicitly retried subtree").unwrap();
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    retry_fixture_task(&peers.a, task, Some(failed_id)).await;
    wait_for_offer(&peers, 0).await;
    let complete = terminal(&mut peers.a_progress, task).await;
    assert_eq!(complete.state, "completed", "{complete:?}");
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    assert_eq!(
        peers.stats.lock().unwrap().transferred_bytes,
        b"new explicitly retried subtree".len() as u64
    );
    assert_eq!(
        fs::read(fixture.received(false, task, "blocked/repaired.bin")).unwrap(),
        b"new explicitly retried subtree"
    );
    assert!(
        !fixture
            .received(false, task, "blocked/private.bin")
            .exists()
    );
    assert_eq!(
        fs::read(outside.join("private.bin")).unwrap(),
        b"outside canary remains unchanged"
    );
    peers.close().await;
}

#[tokio::test]
async fn replay_rejects_changed_directory_kind_including_an_empty_selected_root() {
    let fixture = Fixture::new();
    let folder = fixture.path("kind-replay-source");
    fs::create_dir_all(folder.join("child")).unwrap();
    let task = [72; 16];
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    start(&peers.a, &folder, task).await;
    assert_eq!(
        terminal(&mut peers.a_progress, task).await.state,
        "completed"
    );
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    peers.close().await;
    fs::remove_dir(folder.join("child")).unwrap();
    fs::write(folder.join("child"), b"changed kind must not be adopted").unwrap();
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    retry_fixture_task(&peers.a, task, None).await;
    wait_for_offer(&peers, 0).await;
    let partial = terminal(&mut peers.a_progress, task).await;
    assert_eq!(partial.state, "partial", "{partial:?}");
    assert_eq!((partial.completed_items, partial.failed_items), (0, 1));
    assert_eq!(
        partial.items.front().unwrap().message.as_deref(),
        Some("源文件已变化，请重新选择后发送。")
    );
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    assert!(fixture.received(false, task, "child").is_dir());
    assert_eq!(peers.stats.lock().unwrap().transferred_bytes, 0);
    peers.close().await;

    let empty = fixture.path("empty-selected-root");
    fs::create_dir(&empty).unwrap();
    let task = [73; 16];
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    start(&peers.a, &empty, task).await;
    assert_eq!(
        terminal(&mut peers.a_progress, task).await.state,
        "completed"
    );
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    peers.close().await;
    fs::remove_dir(&empty).unwrap();
    fs::write(&empty, b"replacement root file").unwrap();
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    retry_fixture_task(&peers.a, task, None).await;
    timeout(Duration::from_secs(5), async {
        loop {
            if peers
                .a_progress
                .borrow()
                .as_ref()
                .is_some_and(|p| p.state == "failed")
            {
                break;
            }
            peers.a_progress.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(
        peers
            .a_progress
            .borrow()
            .as_ref()
            .unwrap()
            .message
            .as_deref(),
        Some("源文件已变化，请重新选择后发送。")
    );
    assert_eq!(peers.stats.lock().unwrap().offers, 0);
    assert_eq!(peers.stats.lock().unwrap().transferred_bytes, 0);
    assert_eq!(fs::read(empty).unwrap(), b"replacement root file");
    peers.close().await;
}

#[tokio::test]
async fn enumeration_retry_prunes_replacement_directory_children_and_keeps_original_item_identity()
{
    let fixture = Fixture::new();
    let folder = fixture.path("kind-dedupe-source");
    fs::create_dir(&folder).unwrap();
    let names = ["a.bin", "b.bin", "c.bin"];
    for name in names {
        fs::write(folder.join(name), b"four").unwrap();
    }
    let hook: BridgeHook = Arc::new(|message| {
        if let FileTaskMessage::Capabilities(capabilities) = message {
            capabilities.max_entries = 2;
        }
    });
    let task = [74; 16];
    let mut peers = Peers::with_hook(&fixture, true, Duration::ZERO, hook.clone()).await;
    start(&peers.a, &folder, task).await;
    let partial = terminal(&mut peers.a_progress, task).await;
    assert_eq!(partial.state, "partial", "{partial:?}");
    assert_eq!((partial.completed_items, partial.failed_items), (1, 1));
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    let original = partial
        .items
        .iter()
        .find(|item| item.state == "completed")
        .unwrap();
    let original_path = original.path.clone();
    let original_id = original.id.clone();
    peers.close().await;
    let missing = names
        .iter()
        .filter(|name| **name != original_path)
        .collect::<Vec<_>>();
    fs::remove_file(folder.join(missing[0])).unwrap();
    fs::remove_file(folder.join(&original_path)).unwrap();
    fs::create_dir(folder.join(&original_path)).unwrap();
    fs::write(
        folder.join(&original_path).join("must-not-adopt.bin"),
        b"replacement directory child",
    )
    .unwrap();
    let mut peers = Peers::with_hook(&fixture, true, Duration::ZERO, hook).await;
    retry_fixture_task(&peers.a, task, None).await;
    wait_for_offer(&peers, 0).await;
    let partial = terminal(&mut peers.a_progress, task).await;
    assert_eq!(partial.state, "partial", "{partial:?}");
    let original = partial
        .items
        .iter()
        .find(|item| item.path == original_path)
        .unwrap();
    assert_eq!(original.id, original_id);
    assert_eq!(original.state, "failed");
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    assert_eq!(
        fs::read(fixture.received(false, task, &original_path)).unwrap(),
        b"four"
    );
    let manifest =
        fs::read_to_string(fixture_task_cache(&fixture, true, task).join("manifest.jsonl"))
            .unwrap();
    assert!(!manifest.contains("must-not-adopt.bin"));
    assert_eq!(peers.stats.lock().unwrap().transferred_bytes, 4);
    assert_eq!(
        fs::read(folder.join(&original_path).join("must-not-adopt.bin")).unwrap(),
        b"replacement directory child"
    );
    peers.close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn completed_publication_has_a_verified_index_before_ack_and_retry_opens_without_payload_reread()
 {
    let fixture = Fixture::new();
    let source = fixture.path("published-cache-source.bin");
    let bytes = sample(3);
    fs::write(&source, &bytes).unwrap();
    let task = [75; 16];
    let mut peers = Peers::new(&fixture, true, Duration::ZERO).await;
    start(&peers.a, &source, task).await;
    assert_eq!(
        terminal(&mut peers.a_progress, task).await.state,
        "completed"
    );
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    let target = fixture.received(false, task, "published-cache-source.bin");
    let index_path = fixture_task_cache(&fixture, false, task).join("1.published.index");
    assert!(
        index_path.is_file(),
        "NTFS fixture must have exported the verified proof records before completed acknowledgement"
    );
    let mut checked_payload = 0;
    let index = SourceIndex::open_from_file_with_control(
        fs::File::open(&target).unwrap(),
        &index_path,
        &AtomicBool::new(false),
        |checked| checked_payload += checked,
    )
    .unwrap();
    assert_eq!(index.size(), bytes.len() as u64);
    assert_eq!(
        checked_payload, 0,
        "first retry index opening reread completed file payload"
    );
    drop(index);
    assert_eq!(fs::read(&target).unwrap(), bytes);
    let offers = peers.stats.lock().unwrap().offers;
    let before = peers.stats.lock().unwrap().transferred_bytes;
    retry_fixture_task(&peers.a, task, None).await;
    wait_for_offer(&peers, offers).await;
    assert_eq!(
        terminal(&mut peers.a_progress, task).await.state,
        "completed"
    );
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    assert_eq!(peers.stats.lock().unwrap().transferred_bytes, before);
    assert_eq!(fs::read(target).unwrap(), bytes);
    peers.close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn receiver_empty_directory_is_pinned_during_entry_complete_and_cannot_be_replaced() {
    let fixture = Fixture::new();
    let folder = fixture.path("guarded-receive-source");
    fs::create_dir_all(folder.join("empty")).unwrap();
    let task = [76; 16];
    let destination = fixture.received(false, task, "empty");
    let attempted = Arc::new(AtomicBool::new(false));
    let observed = attempted.clone();
    let hook: BridgeHook = Arc::new(move |message| {
        if matches!(message, FileTaskMessage::EntryComplete { task_id, item_id: 1, content_root } if *task_id == task && *content_root == [0; 32])
        {
            assert!(destination.is_dir());
            assert!(
                fs::remove_dir(&destination).is_err(),
                "destination leaf was not pinned while awaiting completion"
            );
            assert!(fs::write(&destination, b"replacement must never appear").is_err());
            observed.store(true, Ordering::Release);
        }
    });
    let mut peers = Peers::with_hook(&fixture, true, Duration::ZERO, hook).await;
    start(&peers.a, &folder, task).await;
    assert_eq!(
        terminal(&mut peers.a_progress, task).await.state,
        "completed"
    );
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    assert!(attempted.load(Ordering::Acquire));
    assert!(fixture.received(false, task, "empty").is_dir());
    assert_eq!(
        fs::read_dir(fixture.received(false, task, "empty"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(peers.stats.lock().unwrap().transferred_bytes, 0);
    peers.close().await;
}

#[cfg(windows)]
#[tokio::test]
async fn overlapping_failed_file_and_scan_limit_do_not_starve_a_recovered_sibling_at_the_entry_cap()
{
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let folder = fixture.path("overlapping-scan-failures");
    fs::create_dir(&folder).unwrap();
    for index in 0..5 {
        fs::write(folder.join(format!("file-{index}.bin")), b"four").unwrap();
    }
    // Use this unchanged directory's observed order to guarantee that a
    // specific failed entry precedes the later discovery-limit marker.
    let observed = fs::read_dir(&folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    let blocked = observed[1].clone();
    let blocked_name = blocked.file_name().unwrap().to_str().unwrap().to_owned();
    let owner = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&blocked)
        .unwrap();
    let cap_hook: BridgeHook = Arc::new(|message| {
        if let FileTaskMessage::Capabilities(capabilities) = message {
            capabilities.max_entries = 4;
        }
    });
    let task = [77; 16];
    let mut peers = Peers::with_hook(&fixture, true, Duration::ZERO, cap_hook.clone()).await;
    start(&peers.a, &folder, task).await;
    let partial = terminal(&mut peers.a_progress, task).await;
    assert_eq!(partial.state, "partial", "{partial:?}");
    assert_eq!((partial.completed_items, partial.failed_items), (2, 2));
    assert!(
        partial
            .items
            .iter()
            .any(|item| item.path == blocked_name && item.state == "failed")
    );
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    let retained = partial
        .items
        .iter()
        .filter(|item| item.state == "completed")
        .map(|item| (item.path.clone(), item.id.clone()))
        .collect::<Vec<_>>();
    peers.close().await;
    let omitted = observed
        .iter()
        .filter(|path| {
            *path != &blocked
                && !retained
                    .iter()
                    .any(|(name, _)| path.file_name().unwrap() == name.as_str())
        })
        .collect::<Vec<_>>();
    assert_eq!(omitted.len(), 2);
    fs::remove_file(omitted[0]).unwrap();
    let mut peers = Peers::with_hook(&fixture, true, Duration::ZERO, cap_hook.clone()).await;
    retry_fixture_task(&peers.a, task, None).await;
    wait_for_offer(&peers, 0).await;
    let partial = terminal(&mut peers.a_progress, task).await;
    assert_eq!(partial.state, "partial", "{partial:?}");
    assert_eq!(
        (partial.completed_items, partial.failed_items),
        (3, 1),
        "duplicate failure scopes consumed the valid sibling's slot"
    );
    assert_eq!(partial.scanned_items, 4);
    assert_eq!(terminal(&mut peers.b_progress, task).await.state, "partial");
    for (path, id) in retained {
        assert_eq!(
            partial
                .items
                .iter()
                .find(|item| item.path == path)
                .unwrap()
                .id,
            id
        );
    }
    assert_eq!(peers.stats.lock().unwrap().transferred_bytes, 4);
    let failure = partial
        .items
        .iter()
        .find(|item| item.state == "failed")
        .unwrap();
    assert_eq!(failure.path, blocked_name);
    let failed_id = failure.id.parse().unwrap();
    peers.close().await;
    drop(owner);
    let mut peers = Peers::with_hook(&fixture, true, Duration::ZERO, cap_hook).await;
    retry_fixture_task(&peers.a, task, Some(failed_id)).await;
    wait_for_offer(&peers, 0).await;
    let complete = terminal(&mut peers.a_progress, task).await;
    assert_eq!(complete.state, "completed", "{complete:?}");
    assert_eq!((complete.completed_items, complete.failed_items), (4, 0));
    assert_eq!(
        terminal(&mut peers.b_progress, task).await.state,
        "completed"
    );
    assert_eq!(peers.stats.lock().unwrap().transferred_bytes, 4);
    assert_eq!(fs::read(blocked).unwrap(), b"four");
    peers.close().await;
}

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
    {
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
    }
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
    timeout(Duration::from_secs(5), async {
        loop {
            match service.outgoing.recv().await.unwrap() {
                FileTaskMessage::Entry { task_id, entry } => {
                    assert_eq!((task_id, entry.item_id), (task, 1));
                    break;
                }
                FileTaskMessage::Activity {
                    task_id,
                    phase: desklink_protocol::FileTaskPhase::Scanning,
                    item_id: None,
                    completed,
                    total: 0,
                } => {
                    assert_eq!(task_id, task);
                    assert!(
                        completed > 0
                            && completed
                                <= desklink_protocol::MAX_FILE_TASK_SCAN_WORK_PASSES
                                    * FileTransferCapabilities::WINDOWS.max_entries
                    );
                }
                message => panic!("unexpected source preparation response: {message:?}"),
            }
        }
    })
    .await
    .unwrap();
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
        while service.outgoing.is_empty() {
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
    {
        let stats = peers.stats.lock().unwrap();
        assert!(
            stats
                .resumes
                .iter()
                .any(|(id, offset)| *id == task && *offset >= u64::from(FILE_BLOCK_BYTES) * 2),
            "{stats:?}"
        );
        assert!(stats.transferred_bytes < bytes.len() as u64);
    }
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
