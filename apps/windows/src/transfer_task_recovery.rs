//! Convert file task progress to bounded native recovery records and back.
//! Wire/UI decimal strings are converted only at these explicit boundaries.

use std::{collections::VecDeque, path::PathBuf};

use desklink_protocol::{TransferEntryKind, TransferId, TransferResult};

use crate::{
    transfer_task::{FileTaskItem, FileTaskProgress, id_text},
    transfer_task_catalog::{
        RECOVERY_ITEM_WINDOW, RecoveryDirection, RecoveryItem, RecoveryState, RecoverySummary,
    },
};

pub fn restore_progress(summary: &RecoverySummary) -> FileTaskProgress {
    FileTaskProgress {
        task_id: id_text(summary.task_id),
        source_admitted: summary.direction == RecoveryDirection::Upload,
        revision: 0,
        direction: summary.direction.as_str(),
        name: summary.name.clone(),
        state: summary.state.as_str(),
        scan_complete: summary.scan_complete,
        scanned_items: summary.scanned_items,
        total_bytes: summary.total_bytes,
        sent_bytes: summary.sent_bytes,
        acknowledged_bytes: summary.acknowledged_bytes,
        completed_items: summary.completed_items,
        failed_items: summary.failed_items,
        active_item: summary.active_item.as_ref().map(restore_item),
        items: summary
            .items
            .iter()
            .rev()
            .take(RECOVERY_ITEM_WINDOW)
            .rev()
            .map(restore_item)
            .collect::<VecDeque<_>>(),
        message: summary.message.clone(),
    }
}

pub fn summarize_progress(
    progress: &FileTaskProgress,
    task_id: TransferId,
    source_path: Option<PathBuf>,
    directory: bool,
) -> Result<RecoverySummary, TransferResult> {
    if progress.task_id != id_text(task_id) || progress.items.len() > RECOVERY_ITEM_WINDOW {
        return Err(TransferResult::InvalidData);
    }
    let summary = RecoverySummary {
        task_id,
        direction: RecoveryDirection::parse(progress.direction)?,
        source_path,
        name: progress.name.clone(),
        directory,
        state: RecoveryState::parse(progress.state)?,
        scan_complete: progress.scan_complete,
        scanned_items: progress.scanned_items,
        total_bytes: progress.total_bytes,
        sent_bytes: progress.sent_bytes,
        acknowledged_bytes: progress.acknowledged_bytes,
        completed_items: progress.completed_items,
        failed_items: progress.failed_items,
        active_item: progress
            .active_item
            .as_ref()
            .map(summarize_item)
            .transpose()?,
        items: progress
            .items
            .iter()
            .map(summarize_item)
            .collect::<Result<Vec<_>, _>>()?,
        message: progress.message.clone(),
    };
    summary.validate()?;
    Ok(summary)
}

fn restore_item(item: &RecoveryItem) -> FileTaskItem {
    FileTaskItem {
        id: item.item_id.to_string(),
        path: item.path.clone(),
        state: item.state.as_str(),
        kind: match item.kind {
            TransferEntryKind::File => "file",
            TransferEntryKind::Directory => "directory",
        },
        total_bytes: item.total_bytes,
        sent_bytes: item.sent_bytes,
        acknowledged_bytes: item.acknowledged_bytes,
        message: item.message.clone(),
    }
}

fn summarize_item(item: &FileTaskItem) -> Result<RecoveryItem, TransferResult> {
    let item_id = item
        .id
        .parse::<u64>()
        .map_err(|_| TransferResult::InvalidData)?;
    if item.id != item_id.to_string() {
        return Err(TransferResult::InvalidData);
    }
    Ok(RecoveryItem {
        item_id,
        path: item.path.clone(),
        kind: match item.kind {
            "file" => TransferEntryKind::File,
            "directory" => TransferEntryKind::Directory,
            _ => return Err(TransferResult::InvalidData),
        },
        state: RecoveryState::parse(item.state)?,
        total_bytes: item.total_bytes,
        sent_bytes: item.sent_bytes,
        acknowledged_bytes: item.acknowledged_bytes,
        message: item.message.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(direction: RecoveryDirection) -> RecoverySummary {
        RecoverySummary {
            task_id: [7; 16],
            direction,
            source_path: (direction == RecoveryDirection::Upload)
                .then(|| std::env::temp_dir().join("中文目录")),
            name: "中文目录".into(),
            directory: true,
            state: RecoveryState::Paused,
            scan_complete: true,
            scanned_items: 100,
            total_bytes: 4_294_967_300,
            sent_bytes: 4_294_967_298,
            acknowledged_bytes: 4_294_967_296,
            completed_items: 2,
            failed_items: 64,
            active_item: None,
            items: (1..=64)
                .map(|item_id| RecoveryItem {
                    item_id,
                    path: format!("资料/失败-{item_id}.txt"),
                    kind: TransferEntryKind::File,
                    state: RecoveryState::Failed,
                    total_bytes: 100,
                    sent_bytes: 80,
                    acknowledged_bytes: 40,
                    message: Some("失败项可以重试".into()),
                })
                .collect(),
            message: Some("已暂停，重试沿用原任务".into()),
        }
    }

    #[test]
    fn source_and_receiver_round_trip_native_u64_and_full_failed_item_window() {
        for direction in [RecoveryDirection::Upload, RecoveryDirection::Download] {
            let summary = summary(direction);
            let progress = restore_progress(&summary);
            assert_eq!(progress.revision, 0);
            assert_eq!(progress.direction, direction.as_str());
            assert_eq!(progress.acknowledged_bytes, 4_294_967_296);
            assert_eq!(progress.items.len(), 64);
            assert_eq!(progress.items.front().unwrap().id, "1");
            assert_eq!(progress.items.back().unwrap().id, "64");
            let recovered = summarize_progress(
                &progress,
                summary.task_id,
                summary.source_path.clone(),
                true,
            )
            .unwrap();
            assert_eq!(recovered, summary);
            let json = serde_json::to_value(progress).unwrap();
            assert_eq!(json["acknowledgedBytes"], "4294967296");
            assert_eq!(json["items"][63]["id"], "64");
        }
    }

    #[test]
    fn summary_rejects_mismatched_task_invalid_item_ids_and_unknown_states() {
        let summary = summary(RecoveryDirection::Upload);
        let mut progress = restore_progress(&summary);
        assert!(matches!(
            summarize_progress(&progress, [8; 16], summary.source_path.clone(), true),
            Err(TransferResult::InvalidData)
        ));
        for bad_id in ["0", "01", "-1", "18446744073709551616", "not-a-number"] {
            progress.items.front_mut().unwrap().id = bad_id.into();
            assert!(matches!(
                summarize_progress(
                    &progress,
                    summary.task_id,
                    summary.source_path.clone(),
                    true
                ),
                Err(TransferResult::InvalidData)
            ));
        }
        progress.items.front_mut().unwrap().id = "1".into();
        progress.state = "invented-state";
        assert!(matches!(
            summarize_progress(
                &progress,
                summary.task_id,
                summary.source_path.clone(),
                true
            ),
            Err(TransferResult::InvalidData)
        ));
        progress.state = "paused";
        progress.items.front_mut().unwrap().kind = "symbolic-link";
        assert!(matches!(
            summarize_progress(
                &progress,
                summary.task_id,
                summary.source_path.clone(),
                true
            ),
            Err(TransferResult::InvalidData)
        ));
    }

    #[test]
    fn extra_items_are_rejected_and_restore_cannot_expand_a_public_summary_window() {
        let mut summary = summary(RecoveryDirection::Upload);
        summary.items.push(RecoveryItem {
            item_id: 65,
            ..summary.items[0].clone()
        });
        let mut progress = restore_progress(&summary);
        assert_eq!(progress.items.len(), 64);
        progress.items.push_back(progress.items[0].clone());
        assert!(matches!(
            summarize_progress(&progress, summary.task_id, summary.source_path, true),
            Err(TransferResult::InvalidData)
        ));
    }
}
