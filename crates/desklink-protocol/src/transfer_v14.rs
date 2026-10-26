//! Protocol 14 file tasks. The legacy single-file messages retain their 256 MiB
//! budget; these messages require an explicit capability and receiver decision.
use serde::{Deserialize, Serialize};

use crate::{MAX_TRANSFER_CHUNK_BYTES, TransferId, TransferResult};

pub const MAX_LARGE_FILE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
pub const MAX_FILE_TASK_BYTES: u64 = 16 * MAX_LARGE_FILE_BYTES;
pub const MAX_FILE_TASK_ENTRIES: u64 = 1_000_000;
/// Actual manifest-record work: validate, copy, discover, compact at most two
/// passes of temporary failure slots, and read the final manifest. This is
/// independent from the item budget and never describes a transfer percentage.
pub const MAX_FILE_TASK_SCAN_WORK_PASSES: u64 = 6;
/// Indexing activity counts copied block-proof records, independently of data
/// bytes already verified and durably received.
pub const MAX_FILE_TASK_INDEX_RECORDS: u64 =
    MAX_LARGE_FILE_BYTES.div_ceil(MAX_TRANSFER_CHUNK_BYTES as u64);
pub const MAX_TRANSFER_RELATIVE_PATH_BYTES: usize = 4096;
pub const MAX_TRANSFER_DIRECTORY_DEPTH: usize = 64;
pub const BLOCK_ROOT_DOMAIN: &[u8] = b"DeskLink blocks v14\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileTransferCapabilities {
    pub large_files: bool,
    pub directories: bool,
    pub block_resume: bool,
    pub max_file_bytes: u64,
    pub max_task_bytes: u64,
    pub max_entries: u64,
    pub block_bytes: u32,
}

impl FileTransferCapabilities {
    pub const WINDOWS: Self = Self {
        large_files: true,
        directories: true,
        block_resume: true,
        max_file_bytes: MAX_LARGE_FILE_BYTES,
        max_task_bytes: MAX_FILE_TASK_BYTES,
        max_entries: MAX_FILE_TASK_ENTRIES,
        block_bytes: MAX_TRANSFER_CHUNK_BYTES as u32,
    };

    pub fn is_valid(self) -> bool {
        self.max_file_bytes > 0
            && self.max_file_bytes <= MAX_LARGE_FILE_BYTES
            && self.max_task_bytes >= self.max_file_bytes
            && self.max_task_bytes <= MAX_FILE_TASK_BYTES
            && self.max_entries > 0
            && self.max_entries <= MAX_FILE_TASK_ENTRIES
            && self.block_bytes == MAX_TRANSFER_CHUNK_BYTES as u32
            && (!self.directories || self.large_files)
            && (!self.large_files || self.block_resume)
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        if !self.is_valid() || !other.is_valid() {
            return None;
        }
        Some(Self {
            large_files: self.large_files && other.large_files,
            directories: self.directories && other.directories,
            block_resume: self.block_resume && other.block_resume,
            max_file_bytes: self.max_file_bytes.min(other.max_file_bytes),
            max_task_bytes: self.max_task_bytes.min(other.max_task_bytes),
            max_entries: self.max_entries.min(other.max_entries),
            block_bytes: self.block_bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TransferEntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub item_id: u64,
    pub relative_path: String,
    pub kind: TransferEntryKind,
    pub size: u64,
    /// A content identity, not a path/mtime identity. Present for files only.
    pub content_root: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResumeCheckpoint {
    pub offset: u64,
    pub proof: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FileTaskAction {
    Pause,
    Resume,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FileTaskPhase {
    Scanning,
    Hashing,
    Verifying,
    /// Actual block-proof records copied into the published-file index.
    Indexing,
}

/// Only one entry is admitted at a time. A sender must wait for EntryDecision,
/// durable BlockAck and EntryResult before advancing; manifest memory is bounded.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FileTaskMessage {
    Capabilities(FileTransferCapabilities),
    SelectionRequest {
        request_id: u64,
        directory: bool,
    },
    Offer {
        task_id: TransferId,
        request_id: Option<u64>,
        name: String,
        directory: bool,
    },
    Decision {
        task_id: TransferId,
        result: TransferResult,
        budget: FileTransferCapabilities,
    },
    Entry {
        task_id: TransferId,
        entry: ManifestEntry,
    },
    EntryDecision {
        task_id: TransferId,
        item_id: u64,
        result: TransferResult,
        checkpoint: ResumeCheckpoint,
    },
    Block {
        task_id: TransferId,
        item_id: u64,
        offset: u64,
        bytes: Vec<u8>,
        digest: [u8; 32],
    },
    /// Bytes acknowledged here have reached both the staged file and checkpoint.
    BlockAck {
        task_id: TransferId,
        item_id: u64,
        checkpoint: ResumeCheckpoint,
    },
    EntryComplete {
        task_id: TransferId,
        item_id: u64,
        content_root: [u8; 32],
    },
    EntryResult {
        task_id: TransferId,
        item_id: u64,
        result: TransferResult,
    },
    ManifestComplete {
        task_id: TransferId,
        entries: u64,
        total_bytes: u64,
    },
    TaskResult {
        task_id: TransferId,
        completed: u64,
        failed: u64,
    },
    Action {
        task_id: TransferId,
        action: FileTaskAction,
    },
    /// A source entry failed before admission; preserves its identity in the
    /// aggregate result without leaving a receiver waiting for file bytes.
    EntrySkipped {
        task_id: TransferId,
        item_id: u64,
        relative_path: String,
        size: u64,
        result: TransferResult,
    },
    /// Close an admitted entry after source IO/identity validation fails.
    EntryAbort {
        task_id: TransferId,
        item_id: u64,
        result: TransferResult,
    },
    SelectionResult {
        request_id: u64,
        result: TransferResult,
    },
    SelectionCancel {
        request_id: u64,
    },
    /// Ask the same authenticated source peer to replay its durable task. No
    /// remote path is accepted, and the receiver must authorize its new offer.
    RetryRequest {
        task_id: TransferId,
        item_id: Option<u64>,
    },
    /// Only a failed attempt to begin replay uses this response. Success is a
    /// fresh Offer followed by the ordinary entry/checkpoint handshake.
    RetryResult {
        task_id: TransferId,
        result: TransferResult,
    },
    /// Actual bounded progress keeps long source scans/final disk verification
    /// alive. Receivers additionally enforce the active task/entry/phase and
    /// strictly advancing counters before extending their inactivity deadline.
    Activity {
        task_id: TransferId,
        item_id: Option<u64>,
        phase: FileTaskPhase,
        completed: u64,
        total: u64,
    },
}

pub fn is_valid_transfer_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_TRANSFER_RELATIVE_PATH_BYTES
        && path.split('/').count() <= MAX_TRANSFER_DIRECTORY_DEPTH
        && path.split('/').all(is_valid_transfer_path_segment)
}

/// A native Windows filename component, independent of the legacy v13 file
/// offer's UTF-8 byte budget. UTF-16 length admits long Chinese names while
/// retaining the native 255-code-unit component limit.
pub fn is_valid_transfer_path_segment(name: &str) -> bool {
    if name.is_empty()
        || name.encode_utf16().count() > 255
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*'
                )
        })
    {
        return false;
    }
    let stem = name.split('.').next().unwrap_or_default();
    let stem = stem.trim_end_matches(['.', ' ']).to_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) {
        return false;
    }
    if let Some(suffix) = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        && matches!(
            suffix,
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
        )
    {
        return false;
    }
    true
}

impl FileTaskMessage {
    pub fn task_id(&self) -> Option<TransferId> {
        match self {
            Self::Capabilities(_)
            | Self::SelectionRequest { .. }
            | Self::SelectionResult { .. }
            | Self::SelectionCancel { .. } => None,
            Self::Offer { task_id, .. }
            | Self::Decision { task_id, .. }
            | Self::Entry { task_id, .. }
            | Self::EntryDecision { task_id, .. }
            | Self::Block { task_id, .. }
            | Self::BlockAck { task_id, .. }
            | Self::EntryComplete { task_id, .. }
            | Self::EntryResult { task_id, .. }
            | Self::ManifestComplete { task_id, .. }
            | Self::TaskResult { task_id, .. }
            | Self::Action { task_id, .. }
            | Self::EntrySkipped { task_id, .. }
            | Self::EntryAbort { task_id, .. }
            | Self::RetryRequest { task_id, .. }
            | Self::RetryResult { task_id, .. }
            | Self::Activity { task_id, .. } => Some(*task_id),
        }
    }

    /// Syntax and resource bounds only. The authenticated endpoint additionally
    /// checks task ownership, authorization and its current handshake phase.
    pub fn is_valid(&self) -> bool {
        if self.task_id().is_some_and(|id| id == [0; 16]) {
            return false;
        }
        let checkpoint_valid = |c: &ResumeCheckpoint| c.offset <= MAX_LARGE_FILE_BYTES;
        match self {
            Self::Capabilities(c) => c.is_valid(),
            Self::SelectionRequest { request_id, .. } => *request_id != 0,
            Self::SelectionResult { request_id, result } => {
                *request_id != 0 && *result != TransferResult::Completed
            }
            Self::SelectionCancel { request_id } => *request_id != 0,
            Self::RetryRequest { item_id, .. } => {
                item_id.is_none_or(|id| id > 0 && id <= MAX_FILE_TASK_ENTRIES)
            }
            Self::RetryResult { result, .. } => *result != TransferResult::Completed,
            Self::Activity {
                item_id,
                phase,
                completed,
                total,
                ..
            } => match phase {
                FileTaskPhase::Scanning => {
                    item_id.is_none()
                        && *completed <= MAX_FILE_TASK_ENTRIES * MAX_FILE_TASK_SCAN_WORK_PASSES
                        && *total == 0
                }
                FileTaskPhase::Indexing => {
                    item_id.is_some_and(|id| id > 0 && id <= MAX_FILE_TASK_ENTRIES)
                        && *completed <= *total
                        && *total <= MAX_FILE_TASK_INDEX_RECORDS
                }
                FileTaskPhase::Hashing | FileTaskPhase::Verifying => {
                    item_id.is_some_and(|id| id > 0 && id <= MAX_FILE_TASK_ENTRIES)
                        && *completed <= *total
                        && *total <= MAX_LARGE_FILE_BYTES
                }
            },
            Self::Offer {
                request_id, name, ..
            } => request_id.is_none_or(|id| id != 0) && is_valid_transfer_path_segment(name),
            Self::Decision { budget, .. } => budget.is_valid(),
            Self::Entry { entry, .. } => {
                entry.item_id > 0
                    && entry.item_id <= MAX_FILE_TASK_ENTRIES
                    && is_valid_transfer_relative_path(&entry.relative_path)
                    && entry.size <= MAX_LARGE_FILE_BYTES
                    && match entry.kind {
                        TransferEntryKind::File => entry.content_root.is_some(),
                        TransferEntryKind::Directory => {
                            entry.size == 0 && entry.content_root.is_none()
                        }
                    }
            }
            Self::EntryDecision {
                item_id,
                checkpoint,
                result,
                ..
            } => {
                *item_id > 0
                    && *item_id <= MAX_FILE_TASK_ENTRIES
                    && checkpoint_valid(checkpoint)
                    && (*result == TransferResult::Completed || checkpoint.offset == 0)
            }
            Self::Block {
                item_id,
                offset,
                bytes,
                ..
            } => {
                *item_id > 0
                    && *item_id <= MAX_FILE_TASK_ENTRIES
                    && *offset % MAX_TRANSFER_CHUNK_BYTES as u64 == 0
                    && !bytes.is_empty()
                    && bytes.len() <= MAX_TRANSFER_CHUNK_BYTES
                    && offset
                        .checked_add(bytes.len() as u64)
                        .is_some_and(|end| end <= MAX_LARGE_FILE_BYTES)
            }
            Self::BlockAck {
                item_id,
                checkpoint,
                ..
            } => *item_id > 0 && *item_id <= MAX_FILE_TASK_ENTRIES && checkpoint_valid(checkpoint),
            Self::EntryComplete { item_id, .. } | Self::EntryResult { item_id, .. } => {
                *item_id > 0 && *item_id <= MAX_FILE_TASK_ENTRIES
            }
            Self::ManifestComplete {
                entries,
                total_bytes,
                ..
            } => *entries <= MAX_FILE_TASK_ENTRIES && *total_bytes <= MAX_FILE_TASK_BYTES,
            Self::TaskResult {
                completed, failed, ..
            } => completed
                .checked_add(*failed)
                .is_some_and(|total| total <= MAX_FILE_TASK_ENTRIES),
            Self::Action { .. } => true,
            Self::EntrySkipped {
                item_id,
                relative_path,
                size,
                result,
                ..
            } => {
                *item_id > 0
                    && *item_id <= MAX_FILE_TASK_ENTRIES
                    && is_valid_transfer_relative_path(relative_path)
                    && *size <= MAX_LARGE_FILE_BYTES
                    && *result != TransferResult::Completed
            }
            Self::EntryAbort {
                item_id, result, ..
            } => {
                *item_id > 0
                    && *item_id <= MAX_FILE_TASK_ENTRIES
                    && *result != TransferResult::Completed
            }
        }
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use crate::{TransferMessage, decode_transfer, encode_transfer};

    #[test]
    fn file_tasks_round_trip_long_chinese_name_and_manifest_components() {
        let name = format!("{}.txt", "中".repeat(200));
        assert!(name.len() > 255);
        let offer = TransferMessage::FileTask(FileTaskMessage::Offer {
            task_id: [1; 16],
            request_id: None,
            name: name.clone(),
            directory: true,
        });
        assert_eq!(
            decode_transfer(&encode_transfer(&offer).unwrap()).unwrap(),
            offer
        );
        let entry = TransferMessage::FileTask(FileTaskMessage::Entry {
            task_id: [1; 16],
            entry: ManifestEntry {
                item_id: 1,
                relative_path: format!("资料/{name}"),
                kind: TransferEntryKind::File,
                size: 4_294_968_319,
                content_root: Some([2; 32]),
            },
        });
        assert_eq!(
            decode_transfer(&encode_transfer(&entry).unwrap()).unwrap(),
            entry
        );
    }

    #[test]
    fn file_task_paths_reject_device_aliases_escape_and_native_length_overflow() {
        for path in [
            "CONIN$",
            "CONOUT$.txt",
            "CLOCK$",
            "COM¹.txt",
            "LPT²",
            "dir/NUL",
            "C:/absolute",
            "../escape",
            "x//y",
            "x\\y",
            "x:stream",
            "x.",
            "x ",
        ] {
            assert!(!is_valid_transfer_relative_path(path), "{path:?}");
            let offer = TransferMessage::FileTask(FileTaskMessage::Offer {
                task_id: [1; 16],
                request_id: None,
                name: path.to_owned(),
                directory: true,
            });
            assert!(encode_transfer(&offer).is_err());
        }
        assert!(is_valid_transfer_path_segment(&"中".repeat(255)));
        assert!(!is_valid_transfer_path_segment(&"中".repeat(256)));
        assert!(is_valid_transfer_path_segment(&"🦀".repeat(127)));
        assert!(!is_valid_transfer_path_segment(&"🦀".repeat(128)));
        assert!(!is_valid_transfer_relative_path(
            &"a/".repeat(MAX_TRANSFER_DIRECTORY_DEPTH)
        ));
    }
}
