use desklink_protocol::*;

fn round_trip(message: FileTaskMessage) {
    let message = TransferMessage::FileTask(message);
    let encoded = encode_transfer(&message).unwrap();
    assert_eq!(decode_transfer(&encoded).unwrap(), message);
    for length in 0..encoded.len() {
        assert!(decode_transfer(&encoded[..length]).is_err());
    }
    let mut appended = encoded;
    appended.push(0);
    assert!(decode_transfer(&appended).is_err());
}

#[test]
fn file_tasks_preserve_large_offsets_and_require_exact_consumption() {
    let task_id = [14; 16];
    let size = 4 * 1024 * 1024 * 1024 + 123_u64;
    round_trip(FileTaskMessage::Capabilities(
        FileTransferCapabilities::WINDOWS,
    ));
    round_trip(FileTaskMessage::Offer {
        task_id,
        request_id: None,
        name: "中文目录".into(),
        directory: true,
    });
    round_trip(FileTaskMessage::Entry {
        task_id,
        entry: ManifestEntry {
            item_id: 1,
            relative_path: "中文目录/大文件.bin".into(),
            kind: TransferEntryKind::File,
            size,
            content_root: Some([7; 32]),
        },
    });
    round_trip(FileTaskMessage::EntryDecision {
        task_id,
        item_id: 1,
        result: TransferResult::Completed,
        checkpoint: ResumeCheckpoint {
            offset: size,
            proof: [9; 32],
        },
    });
    round_trip(FileTaskMessage::Block {
        task_id,
        item_id: 1,
        offset: (size / MAX_TRANSFER_CHUNK_BYTES as u64) * MAX_TRANSFER_CHUNK_BYTES as u64,
        bytes: vec![1; 123],
        digest: [1; 32],
    });
    round_trip(FileTaskMessage::ManifestComplete {
        task_id,
        entries: MAX_FILE_TASK_ENTRIES,
        total_bytes: MAX_FILE_TASK_BYTES,
    });
    round_trip(FileTaskMessage::TaskResult {
        task_id,
        completed: 9,
        failed: 2,
    });
}

#[test]
fn hostile_manifests_budgets_and_integer_overflow_are_rejected() {
    for path in [
        "/absolute",
        "../escape",
        "a/../b",
        "C:/file",
        "x\\y",
        "a//b",
        "con",
        "a/nul.txt",
        "x:ads",
        "a/.",
        "x. ",
        "",
    ] {
        assert!(!is_valid_transfer_relative_path(path), "{path}");
    }
    let mut budget = FileTransferCapabilities::WINDOWS;
    budget.max_file_bytes = u64::MAX;
    assert!(
        encode_transfer(&TransferMessage::FileTask(FileTaskMessage::Capabilities(
            budget
        )))
        .is_err()
    );
    let message = FileTaskMessage::TaskResult {
        task_id: [1; 16],
        completed: u64::MAX,
        failed: 1,
    };
    assert!(encode_transfer(&TransferMessage::FileTask(message)).is_err());
    let message = FileTaskMessage::Entry {
        task_id: [1; 16],
        entry: ManifestEntry {
            item_id: 1,
            relative_path: "empty".into(),
            kind: TransferEntryKind::Directory,
            size: 1,
            content_root: None,
        },
    };
    assert!(encode_transfer(&TransferMessage::FileTask(message)).is_err());
    let message = FileTaskMessage::Block {
        task_id: [1; 16],
        item_id: 1,
        offset: MAX_LARGE_FILE_BYTES,
        bytes: vec![0],
        digest: [0; 32],
    };
    assert!(encode_transfer(&TransferMessage::FileTask(message)).is_err());
}

#[test]
fn negotiation_takes_minimum_resources_without_inventing_support() {
    let mut peer = FileTransferCapabilities::WINDOWS;
    peer.directories = false;
    peer.max_entries = 1;
    peer.max_file_bytes = 8 * 1024 * 1024 * 1024;
    let agreed = peer.intersect(FileTransferCapabilities::WINDOWS).unwrap();
    assert!(!agreed.directories);
    assert_eq!(agreed.max_entries, 1);
    assert_eq!(agreed.max_file_bytes, peer.max_file_bytes);
    // The old file messages do not silently inherit the negotiated ceiling.
    assert!(
        encode_transfer(&TransferMessage::FileOffer {
            transfer_id: [1; 16],
            request_id: None,
            name: "big.bin".into(),
            size: MAX_TRANSFER_FILE_BYTES + 1
        })
        .is_err()
    );
}

#[test]
fn manifest_work_heartbeat_allows_bounded_real_passes_without_expanding_item_budget() {
    let limit = MAX_FILE_TASK_ENTRIES * MAX_FILE_TASK_SCAN_WORK_PASSES;
    let activity = |completed, item_id, total| FileTaskMessage::Activity {
        task_id: [14; 16],
        item_id,
        phase: FileTaskPhase::Scanning,
        completed,
        total,
    };
    round_trip(activity(limit, None, 0));
    for malformed in [
        activity(limit + 1, None, 0),
        activity(u64::MAX, None, 0),
        activity(1, Some(1), 0),
        activity(1, None, 1),
    ] {
        assert!(!malformed.is_valid());
        let value = TransferMessage::FileTask(malformed);
        assert!(encode_transfer(&value).is_err());
        assert!(decode_transfer(&postcard::to_allocvec(&value).unwrap()).is_err());
    }
    let mut capabilities = FileTransferCapabilities::WINDOWS;
    capabilities.max_entries = MAX_FILE_TASK_ENTRIES + 1;
    assert!(!capabilities.is_valid());
}

#[test]
fn proof_index_activity_has_an_independent_bounded_record_budget_and_appended_wire_tag() {
    assert_eq!(
        postcard::to_allocvec(&FileTaskPhase::Scanning).unwrap(),
        [0]
    );
    assert_eq!(postcard::to_allocvec(&FileTaskPhase::Hashing).unwrap(), [1]);
    assert_eq!(
        postcard::to_allocvec(&FileTaskPhase::Verifying).unwrap(),
        [2]
    );
    assert_eq!(
        postcard::to_allocvec(&FileTaskPhase::Indexing).unwrap(),
        [3]
    );
    let activity = |completed, total, item_id| FileTaskMessage::Activity {
        task_id: [14; 16],
        item_id,
        phase: FileTaskPhase::Indexing,
        completed,
        total,
    };
    round_trip(activity(
        MAX_FILE_TASK_INDEX_RECORDS,
        MAX_FILE_TASK_INDEX_RECORDS,
        Some(1),
    ));
    for bad in [
        activity(1, MAX_FILE_TASK_INDEX_RECORDS + 1, Some(1)),
        activity(2, 1, Some(1)),
        activity(1, 1, None),
        activity(1, 1, Some(0)),
    ] {
        assert!(!bad.is_valid());
        let message = TransferMessage::FileTask(bad);
        assert!(encode_transfer(&message).is_err());
        assert!(decode_transfer(&postcard::to_allocvec(&message).unwrap()).is_err());
    }
}
