//! Protocol 14 file tasks. The legacy single-file messages retain their 256 MiB
//! budget; these messages require an explicit capability and receiver decision.
use serde::{Deserialize, Serialize};

use crate::{MAX_TRANSFER_CHUNK_BYTES, TransferId, TransferResult, is_valid_transfer_file_name};

pub const MAX_LARGE_FILE_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
pub const MAX_FILE_TASK_BYTES: u64 = 16 * MAX_LARGE_FILE_BYTES;
pub const MAX_FILE_TASK_ENTRIES: u64 = 1_000_000;
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
}

pub fn is_valid_transfer_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_TRANSFER_RELATIVE_PATH_BYTES
        && path.split('/').count() <= MAX_TRANSFER_DIRECTORY_DEPTH
        && path.split('/').all(is_valid_transfer_file_name)
}

impl FileTaskMessage {
    pub fn task_id(&self) -> Option<TransferId> {
        match self {
            Self::Capabilities(_) | Self::SelectionRequest { .. } => None,
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
            | Self::Action { task_id, .. } => Some(*task_id),
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        if self.task_id().is_some_and(|id| id == [0; 16]) {
            return false;
        }
        let checkpoint_valid = |c: &ResumeCheckpoint| c.offset <= MAX_LARGE_FILE_BYTES;
        match self {
            Self::Capabilities(c) => c.is_valid(),
            Self::SelectionRequest { request_id, .. } => *request_id != 0,
            Self::Offer {
                request_id, name, ..
            } => request_id.is_none_or(|id| id != 0) && is_valid_transfer_file_name(name),
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
        }
    }
}
