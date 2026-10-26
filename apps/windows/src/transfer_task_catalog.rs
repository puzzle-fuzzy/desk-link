//! Bounded recovery metadata for one authenticated peer's latest file task.
//! Filesystem operations are blocking and belong to the file task disk worker.
//! The catalog never enumerates tasks or opens the original source's contents.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

#[cfg(any(not(windows), test))]
use std::fs::OpenOptions;

#[cfg(windows)]
use crate::transfer_directory::native;
use desklink_protocol::{
    MAX_FILE_TASK_BYTES, MAX_FILE_TASK_ENTRIES, MAX_LARGE_FILE_BYTES, TransferEntryKind,
    TransferId, TransferResult, is_valid_transfer_path_segment, is_valid_transfer_relative_path,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Deserializer, Serialize, de};

pub const RECOVERY_ITEM_WINDOW: usize = 64;
pub const MAX_RECOVERY_JSON_BYTES: u64 = 1024 * 1024;
const MAX_SOURCE_JSON_BYTES: u64 = 128 * 1024;
const MAX_POINTER_JSON_BYTES: u64 = 256;
const MAX_MESSAGE_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDirection {
    Upload,
    Download,
}

impl RecoveryDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Upload => "upload",
            Self::Download => "download",
        }
    }

    pub fn parse(value: &str) -> Result<Self, TransferResult> {
        match value {
            "upload" => Ok(Self::Upload),
            "download" => Ok(Self::Download),
            _ => Err(TransferResult::InvalidData),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryState {
    Scanning,
    Waiting,
    Sending,
    Receiving,
    Verifying,
    Paused,
    Failed,
    Partial,
    Completed,
    Cancelled,
}

impl RecoveryState {
    pub fn parse(value: &str) -> Result<Self, TransferResult> {
        match value {
            "scanning" => Ok(Self::Scanning),
            "waiting" => Ok(Self::Waiting),
            "sending" => Ok(Self::Sending),
            "receiving" => Ok(Self::Receiving),
            "verifying" => Ok(Self::Verifying),
            "paused" => Ok(Self::Paused),
            "failed" => Ok(Self::Failed),
            "partial" => Ok(Self::Partial),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(TransferResult::InvalidData),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scanning => "scanning",
            Self::Waiting => "waiting",
            Self::Sending => "sending",
            Self::Receiving => "receiving",
            Self::Verifying => "verifying",
            Self::Paused => "paused",
            Self::Failed => "failed",
            Self::Partial => "partial",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Failed | Self::Partial | Self::Completed | Self::Cancelled
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryItem {
    pub item_id: u64,
    pub path: String,
    pub kind: TransferEntryKind,
    pub state: RecoveryState,
    pub total_bytes: u64,
    pub sent_bytes: u64,
    pub acknowledged_bytes: u64,
    pub message: Option<String>,
}

impl RecoveryItem {
    fn is_valid(&self) -> bool {
        self.item_id > 0
            && self.item_id <= MAX_FILE_TASK_ENTRIES
            && is_valid_transfer_relative_path(&self.path)
            && self.total_bytes <= MAX_LARGE_FILE_BYTES
            && self.sent_bytes <= self.total_bytes
            && self.acknowledged_bytes <= self.sent_bytes
            && (self.kind != TransferEntryKind::Directory || self.total_bytes == 0)
            && bounded_message(&self.message)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySummary {
    pub task_id: TransferId,
    pub direction: RecoveryDirection,
    /// Local source metadata only. Never serialize this path onto the wire.
    /// Upload requires Some; Download requires None and binds receive.json.
    pub source_path: Option<PathBuf>,
    pub name: String,
    pub directory: bool,
    pub state: RecoveryState,
    pub scan_complete: bool,
    pub scanned_items: u64,
    pub total_bytes: u64,
    pub sent_bytes: u64,
    pub acknowledged_bytes: u64,
    pub completed_items: u64,
    pub failed_items: u64,
    pub active_item: Option<RecoveryItem>,
    #[serde(deserialize_with = "bounded_items")]
    pub items: Vec<RecoveryItem>,
    pub message: Option<String>,
}

impl RecoverySummary {
    pub fn validate(&self) -> Result<(), TransferResult> {
        let mut ids = HashSet::with_capacity(RECOVERY_ITEM_WINDOW);
        let source_valid = match (self.direction, self.source_path.as_ref()) {
            (RecoveryDirection::Upload, Some(path)) => {
                path.is_absolute()
                    && !path.components().any(|c| matches!(c, Component::ParentDir))
                    && path.file_name().and_then(|name| name.to_str()) == Some(self.name.as_str())
            }
            (RecoveryDirection::Download, None) => true,
            _ => false,
        };
        if self.task_id == [0; 16]
            || !source_valid
            || !is_valid_transfer_path_segment(&self.name)
            || self.scanned_items > MAX_FILE_TASK_ENTRIES
            || self.total_bytes > MAX_FILE_TASK_BYTES
            || self.sent_bytes > self.total_bytes
            || self.acknowledged_bytes > self.sent_bytes
            || self
                .completed_items
                .checked_add(self.failed_items)
                .is_none_or(|n| n > self.scanned_items)
            || self.items.len() > RECOVERY_ITEM_WINDOW
            || !bounded_message(&self.message)
            || self
                .active_item
                .as_ref()
                .is_some_and(|item| !item.is_valid())
            || self.items.iter().any(|item| {
                !item.is_valid()
                    || !matches!(item.state, RecoveryState::Completed | RecoveryState::Failed)
                    || !ids.insert(item.item_id)
            })
        {
            return Err(TransferResult::InvalidData);
        }
        Ok(())
    }

    /// A process restart has no live disk or network operation to attach to.
    /// Restore in-flight work as paused, preserving the identity and the last
    /// durable summary. Ledger reconciliation on Retry supplies newer offsets.
    fn normalize_after_restart(&mut self) {
        if !self.state.is_terminal() {
            self.state = RecoveryState::Paused;
        }
        if let Some(item) = self.active_item.as_mut()
            && !item.state.is_terminal()
        {
            item.state = RecoveryState::Paused;
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LastTask {
    task_id: String,
    direction: RecoveryDirection,
}

#[derive(Deserialize)]
struct SourceMetadata {
    path: PathBuf,
    name: String,
    directory: bool,
}

#[derive(Deserialize)]
struct ReceiveMetadata {
    name: String,
    directory: bool,
    root_name: String,
}

pub struct TaskCatalog {
    peer_root: PathBuf,
    _directory_pins: Vec<File>,
}

/// Retain this guard until all task-relative filesystem work has completed.
/// The path is generated from a checked task ID within its authenticated peer.
#[derive(Debug)]
pub struct TaskDirectoryGuard {
    path: PathBuf,
    _directory_pins: Vec<File>,
}

impl TaskDirectoryGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl TaskCatalog {
    /// peer_key must come from the authenticated identity, not a remote label.
    pub fn open(cache_root: &Path, peer_key: &str) -> Result<Self, TransferResult> {
        if peer_key.is_empty()
            || peer_key.len() > 128
            || !peer_key.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(TransferResult::InvalidData);
        }
        let root = if cache_root.is_absolute() {
            cache_root.to_owned()
        } else {
            std::env::current_dir().map_err(io_result)?.join(cache_root)
        };
        let peer_root = root.join(peer_key);
        if peer_root
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(TransferResult::InvalidData);
        }
        let pins = prepare_directory_chain(&peer_root)?;
        Ok(Self {
            peer_root,
            _directory_pins: pins,
        })
    }

    /// Call at scan completion, item terminal states, pause, and task terminal
    /// states. Do not call for every block: the block ledger is already durable.
    /// Summary replacement completes before the O(1) last-task pointer changes.
    pub fn persist(&self, summary: &RecoverySummary) -> Result<(), TransferResult> {
        summary.validate()?;
        let task_root = self.task_root(summary.task_id);
        let _task_pin = open_directory_pin(&task_root)?;
        self.validate_task_metadata(&task_root, summary)?;
        replace_json(
            &task_root.join("recovery.json"),
            summary,
            MAX_RECOVERY_JSON_BYTES,
        )?;
        replace_json(
            &self.peer_root.join("last-task.json"),
            &LastTask {
                task_id: id_text(summary.task_id),
                direction: summary.direction,
            },
            MAX_POINTER_JSON_BYTES,
        )
    }

    pub fn load_last(&self) -> Result<Option<RecoverySummary>, TransferResult> {
        let pointer_path = self.peer_root.join("last-task.json");
        if !path_exists(&pointer_path)? {
            return Ok(None);
        }
        let pointer: LastTask = read_json(&pointer_path, MAX_POINTER_JSON_BYTES)?;
        let task_id = parse_id(&pointer.task_id)?;
        let summary = self.load(task_id)?;
        if summary.direction != pointer.direction {
            return Err(TransferResult::InvalidData);
        }
        Ok(Some(summary))
    }

    pub fn load(&self, task_id: TransferId) -> Result<RecoverySummary, TransferResult> {
        if task_id == [0; 16] {
            return Err(TransferResult::InvalidData);
        }
        let task_root = self.task_root(task_id);
        let _task_pin = open_directory_pin(&task_root)?;
        let mut summary: RecoverySummary =
            read_json(&task_root.join("recovery.json"), MAX_RECOVERY_JSON_BYTES)?;
        summary.validate()?;
        if summary.task_id != task_id {
            return Err(TransferResult::InvalidData);
        }
        self.validate_task_metadata(&task_root, &summary)?;
        summary.normalize_after_restart();
        Ok(summary)
    }

    pub fn persist_source(&self, summary: &RecoverySummary) -> Result<(), TransferResult> {
        if summary.direction != RecoveryDirection::Upload {
            return Err(TransferResult::InvalidData);
        }
        self.persist(summary)
    }

    pub fn load_last_source(&self) -> Result<Option<RecoverySummary>, TransferResult> {
        Ok(self
            .load_last()?
            .filter(|summary| summary.direction == RecoveryDirection::Upload))
    }

    pub fn ensure_task_dir(
        &self,
        task_id: TransferId,
    ) -> Result<TaskDirectoryGuard, TransferResult> {
        if task_id == [0; 16] {
            return Err(TransferResult::InvalidData);
        }
        let path = self.task_root(task_id);
        #[cfg(windows)]
        {
            let pins = native::prepare_chain(&path)?;
            Ok(TaskDirectoryGuard {
                path,
                _directory_pins: pins,
            })
        }
        #[cfg(not(windows))]
        {
            if !path_exists(&path)? {
                fs::create_dir(&path).map_err(io_result)?;
            }
            let task_pin = open_directory_pin(&path)?;
            let mut pins = self
                ._directory_pins
                .iter()
                .map(File::try_clone)
                .collect::<std::io::Result<Vec<_>>>()
                .map_err(io_result)?;
            pins.push(task_pin);
            Ok(TaskDirectoryGuard {
                path,
                _directory_pins: pins,
            })
        }
    }

    fn task_root(&self, task_id: TransferId) -> PathBuf {
        self.peer_root.join(id_text(task_id))
    }

    fn validate_task_metadata(
        &self,
        task_root: &Path,
        summary: &RecoverySummary,
    ) -> Result<(), TransferResult> {
        match summary.direction {
            RecoveryDirection::Upload => {
                let source: SourceMetadata =
                    read_json(&task_root.join("source.json"), MAX_SOURCE_JSON_BYTES)?;
                if Some(&source.path) != summary.source_path.as_ref()
                    || source.name != summary.name
                    || source.directory != summary.directory
                {
                    return Err(TransferResult::InvalidData);
                }
                if summary.scan_complete {
                    let _manifest = open_metadata_file(&task_root.join("manifest.jsonl"))?;
                }
            }
            RecoveryDirection::Download => {
                let received: ReceiveMetadata =
                    read_json(&task_root.join("receive.json"), MAX_SOURCE_JSON_BYTES)?;
                if received.name != summary.name
                    || received.directory != summary.directory
                    || received.root_name != format!("DeskLink-{}", id_text(summary.task_id))
                    || !is_valid_transfer_path_segment(&received.root_name)
                {
                    return Err(TransferResult::InvalidData);
                }
            }
        }
        Ok(())
    }
}

fn bounded_message(value: &Option<String>) -> bool {
    value
        .as_ref()
        .is_none_or(|value| value.len() <= MAX_MESSAGE_BYTES)
}

fn bounded_items<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<RecoveryItem>, D::Error> {
    struct Window;
    impl<'de> de::Visitor<'de> for Window {
        type Value = Vec<RecoveryItem>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            write!(formatter, "at most {RECOVERY_ITEM_WINDOW} recovery items")
        }

        fn visit_seq<A: de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Self::Value, A::Error> {
            let mut items = Vec::with_capacity(RECOVERY_ITEM_WINDOW);
            while let Some(item) = sequence.next_element()? {
                if items.len() == RECOVERY_ITEM_WINDOW {
                    return Err(de::Error::custom("recovery item window exceeded"));
                }
                items.push(item);
            }
            Ok(items)
        }
    }
    deserializer.deserialize_seq(Window)
}

fn read_json<T: de::DeserializeOwned>(path: &Path, maximum: u64) -> Result<T, TransferResult> {
    let file = open_metadata_file(path)?;
    if file.metadata().map_err(io_result)?.len() > maximum {
        return Err(TransferResult::InvalidData);
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(io_result)?;
    if bytes.len() as u64 > maximum {
        return Err(TransferResult::InvalidData);
    }
    serde_json::from_slice(&bytes).map_err(|_| TransferResult::InvalidData)
}

struct PendingFile(PathBuf);

impl Drop for PendingFile {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = native::delete_path(&self.0);
        #[cfg(not(windows))]
        let _ = fs::remove_file(&self.0);
    }
}

fn replace_json(path: &Path, value: &impl Serialize, maximum: u64) -> Result<(), TransferResult> {
    let bytes = serde_json::to_vec(value).map_err(|_| TransferResult::InvalidData)?;
    if bytes.len() as u64 > maximum {
        return Err(TransferResult::InvalidData);
    }
    if path_exists(path)? {
        let metadata = fs::symlink_metadata(path).map_err(io_result)?;
        if is_reparse(&metadata) || !metadata.is_file() {
            return Err(TransferResult::PermissionDenied);
        }
    }
    let parent = path.parent().ok_or(TransferResult::InvalidData)?;
    #[cfg(windows)]
    let parent_pins = native::pin_chain(parent)?;
    #[cfg(not(windows))]
    let _parent_pin = open_directory_pin(parent)?;
    let mut id = [0; 16];
    OsRng
        .try_fill_bytes(&mut id)
        .map_err(|_| TransferResult::IoFailed)?;
    let pending = PendingFile(parent.join(format!(".catalog-{}.pending", id_text(id))));
    #[cfg(not(windows))]
    let mut options = OpenOptions::new();
    #[cfg(not(windows))]
    options.create_new(true).write(true);
    #[cfg(windows)]
    let mut file = native::create_file(
        parent_pins.last().ok_or(TransferResult::InvalidData)?,
        pending.0.file_name().ok_or(TransferResult::InvalidData)?,
    )?;
    #[cfg(not(windows))]
    let mut file = options.open(&pending.0).map_err(io_result)?;
    file.write_all(&bytes).map_err(io_result)?;
    file.sync_all().map_err(io_result)?;
    #[cfg(windows)]
    {
        native::check_chain(&parent_pins)?;
        native::rename(
            &file,
            parent_pins.last().ok_or(TransferResult::InvalidData)?,
            path.file_name().ok_or(TransferResult::InvalidData)?,
            true,
        )?;
        native::check_chain(&parent_pins)
    }
    #[cfg(not(windows))]
    {
        drop(file);
        fs::rename(&pending.0, path).map_err(io_result)
    }
}

fn open_metadata_file(path: &Path) -> Result<File, TransferResult> {
    #[cfg(windows)]
    {
        native::open_file_path(path, false)
    }
    #[cfg(not(windows))]
    {
        let metadata = fs::symlink_metadata(path).map_err(io_result)?;
        if is_reparse(&metadata) || !metadata.is_file() {
            return Err(TransferResult::PermissionDenied);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows::Win32::Storage::FileSystem::{
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
            };
            options
                .share_mode(FILE_SHARE_READ.0)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
        }
        let file = options.open(path).map_err(io_result)?;
        let metadata = file.metadata().map_err(io_result)?;
        if is_reparse(&metadata) || !metadata.is_file() {
            return Err(TransferResult::PermissionDenied);
        }
        Ok(file)
    }
}

fn prepare_directory_chain(path: &Path) -> Result<Vec<File>, TransferResult> {
    #[cfg(windows)]
    {
        native::prepare_chain(path)
    }
    #[cfg(not(windows))]
    {
        let mut current = PathBuf::new();
        let mut pins = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) => current.push(component.as_os_str()),
                Component::RootDir => {
                    current.push(component.as_os_str());
                    pins.push(open_directory_pin(&current)?);
                }
                Component::Normal(name) => {
                    current.push(name);
                    if !path_exists(&current)? {
                        fs::create_dir(&current).map_err(io_result)?;
                    }
                    pins.push(open_directory_pin(&current)?);
                }
                Component::CurDir => {}
                Component::ParentDir => return Err(TransferResult::InvalidData),
            }
        }
        if pins.is_empty() {
            Err(TransferResult::InvalidData)
        } else {
            Ok(pins)
        }
    }
}

fn open_directory_pin(path: &Path) -> Result<File, TransferResult> {
    #[cfg(windows)]
    {
        native::pin_chain(path)?
            .pop()
            .ok_or(TransferResult::InvalidData)
    }
    #[cfg(not(windows))]
    {
        let metadata = fs::symlink_metadata(path).map_err(io_result)?;
        if is_reparse(&metadata) || !metadata.is_dir() {
            return Err(TransferResult::PermissionDenied);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows::Win32::Storage::FileSystem::{
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
                FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
            };
            options
                .access_mode(FILE_READ_ATTRIBUTES.0 | FILE_LIST_DIRECTORY.0)
                .share_mode(FILE_SHARE_READ.0)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0);
        }
        let file = options.open(path).map_err(io_result)?;
        let metadata = file.metadata().map_err(io_result)?;
        if is_reparse(&metadata) || !metadata.is_dir() {
            return Err(TransferResult::PermissionDenied);
        }
        Ok(file)
    }
}

fn is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

fn path_exists(path: &Path) -> Result<bool, TransferResult> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_result(error)),
    }
}

fn io_result(error: std::io::Error) -> TransferResult {
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(32 | 33)) {
        return TransferResult::Busy;
    }
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => TransferResult::PermissionDenied,
        _ => TransferResult::IoFailed,
    }
}

fn id_text(id: TransferId) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_id(text: &str) -> Result<TransferId, TransferResult> {
    if text.len() != 32
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(TransferResult::InvalidData);
    }
    let mut id = [0; 16];
    for (index, pair) in text.as_bytes().chunks_exact(2).enumerate() {
        let value = std::str::from_utf8(pair).map_err(|_| TransferResult::InvalidData)?;
        id[index] = u8::from_str_radix(value, 16).map_err(|_| TransferResult::InvalidData)?;
    }
    if id == [0; 16] {
        Err(TransferResult::InvalidData)
    } else {
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let mut id = [0; 16];
            OsRng.fill_bytes(&mut id);
            let path = std::env::temp_dir().join(format!("desklink-catalog-{}", id_text(id)));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn catalog(&self) -> TaskCatalog {
            TaskCatalog::open(&self.0, "aabb").unwrap()
        }

        fn task(&self, id: TransferId) -> RecoverySummary {
            let source = self.0.join("中文源目录");
            let task = self.0.join("aabb").join(id_text(id));
            fs::create_dir_all(&task).unwrap();
            fs::write(
                task.join("source.json"),
                serde_json::to_vec(&serde_json::json!({
                    "path": source, "name": "中文源目录", "directory": true,
                }))
                .unwrap(),
            )
            .unwrap();
            fs::write(task.join("manifest.jsonl"), b"").unwrap();
            RecoverySummary {
                task_id: id,
                direction: RecoveryDirection::Upload,
                source_path: Some(source),
                name: "中文源目录".into(),
                directory: true,
                state: RecoveryState::Sending,
                scan_complete: true,
                scanned_items: 100,
                total_bytes: u64::from(u32::MAX) + 1024,
                sent_bytes: u64::from(u32::MAX) + 12,
                acknowledged_bytes: u64::from(u32::MAX) + 8,
                completed_items: 2,
                failed_items: 1,
                active_item: Some(item(4, RecoveryState::Sending)),
                items: vec![
                    item(1, RecoveryState::Completed),
                    item(3, RecoveryState::Failed),
                ],
                message: None,
            }
        }

        fn received_task(&self, id: TransferId) -> RecoverySummary {
            let mut summary = self.task(id);
            summary.direction = RecoveryDirection::Download;
            summary.source_path = None;
            summary.state = RecoveryState::Receiving;
            let task = self.0.join("aabb").join(id_text(id));
            fs::remove_file(task.join("source.json")).unwrap();
            fs::remove_file(task.join("manifest.jsonl")).unwrap();
            fs::write(
                task.join("receive.json"),
                serde_json::to_vec(&serde_json::json!({
                    "name": summary.name, "directory": summary.directory,
                    "root_name": format!("DeskLink-{}", id_text(id)),
                }))
                .unwrap(),
            )
            .unwrap();
            summary
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn item(id: u64, state: RecoveryState) -> RecoveryItem {
        RecoveryItem {
            item_id: id,
            path: format!("资料/文件-{id}.bin"),
            kind: TransferEntryKind::File,
            state,
            total_bytes: 100,
            sent_bytes: 80,
            acknowledged_bytes: 40,
            message: (state == RecoveryState::Failed).then(|| "可重试失败项".into()),
        }
    }

    #[test]
    fn restart_preserves_task_identity_large_counts_and_failed_retry_window() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let summary = fixture.task([1; 16]);
        catalog.persist_source(&summary).unwrap();
        drop(catalog);
        let catalog = fixture.catalog();
        let recovered = catalog.load_last_source().unwrap().unwrap();
        assert_eq!(recovered.task_id, [1; 16]);
        assert_eq!(recovered.total_bytes, summary.total_bytes);
        assert_eq!(recovered.acknowledged_bytes, summary.acknowledged_bytes);
        assert_eq!(recovered.source_path, summary.source_path);
        assert_eq!(recovered.state, RecoveryState::Paused);
        assert_eq!(recovered.active_item.unwrap().state, RecoveryState::Paused);
        assert_eq!(recovered.items[1].item_id, 3);
        assert_eq!(recovered.items[1].state, RecoveryState::Failed);
        assert!(recovered.scan_complete);
    }

    #[cfg(windows)]
    #[test]
    fn canonical_verbatim_cache_root_persists_and_recovers() {
        let fixture = Fixture::new();
        let canonical = fs::canonicalize(&fixture.0).unwrap();
        let catalog = TaskCatalog::open(&canonical, "aabb").unwrap();
        let summary = fixture.task([1; 16]);
        catalog.persist_source(&summary).unwrap();
        assert_eq!(
            catalog.load_last_source().unwrap().unwrap().task_id,
            summary.task_id
        );
    }

    #[cfg(windows)]
    #[test]
    fn long_chinese_source_path_metadata_remains_bounded_and_restorable() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let mut summary = fixture.task([1; 16]);
        let mut source = PathBuf::from(r"C:\");
        for _ in 0..100 {
            source.push("中".repeat(200));
        }
        source.push(&summary.name);
        summary.source_path = Some(source.clone());
        let metadata = serde_json::to_vec(
            &serde_json::json!({"path":source,"name":summary.name,"directory":true}),
        )
        .unwrap();
        assert!(metadata.len() > 32 * 1024 && metadata.len() < MAX_SOURCE_JSON_BYTES as usize);
        fs::write(
            catalog.task_root(summary.task_id).join("source.json"),
            metadata,
        )
        .unwrap();
        catalog.persist_source(&summary).unwrap();
        assert_eq!(
            catalog.load_last_source().unwrap().unwrap().source_path,
            summary.source_path
        );
    }

    #[test]
    fn pointer_replacement_restores_latest_task_without_scanning_other_task_directories() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        assert_eq!(catalog.load_last_source().unwrap(), None);
        let first = fixture.task([1; 16]);
        catalog.persist_source(&first).unwrap();
        let second = fixture.task([2; 16]);
        catalog.persist_source(&second).unwrap();
        fs::write(
            catalog.task_root(first.task_id).join("recovery.json"),
            b"poison unrelated old task",
        )
        .unwrap();
        fs::create_dir(catalog.peer_root.join("not-a-task")).unwrap();
        assert_eq!(
            catalog.load_last_source().unwrap().unwrap().task_id,
            second.task_id
        );
        assert_eq!(
            fs::read_dir(&catalog.peer_root)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with("pending"))
                .count(),
            0
        );
    }

    #[test]
    fn terminal_states_survive_restart_and_inflight_states_become_paused() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let mut summary = fixture.task([1; 16]);
        for state in [
            RecoveryState::Scanning,
            RecoveryState::Waiting,
            RecoveryState::Sending,
            RecoveryState::Receiving,
            RecoveryState::Verifying,
            RecoveryState::Paused,
            RecoveryState::Failed,
            RecoveryState::Partial,
            RecoveryState::Completed,
            RecoveryState::Cancelled,
        ] {
            summary.state = state;
            catalog.persist_source(&summary).unwrap();
            let expected = if state.is_terminal() {
                state
            } else {
                RecoveryState::Paused
            };
            assert_eq!(catalog.load_last_source().unwrap().unwrap().state, expected);
            assert_eq!(RecoveryState::parse(state.as_str()).unwrap(), state);
        }
        assert_eq!(
            RecoveryState::parse("bogus"),
            Err(TransferResult::InvalidData)
        );
    }

    #[test]
    fn corrupt_pointer_ids_are_rejected_before_path_resolution() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        for id in [
            "../outside",
            "",
            "00",
            "00000000000000000000000000000000",
            "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
            "gggggggggggggggggggggggggggggggg",
        ] {
            fs::write(
                catalog.peer_root.join("last-task.json"),
                serde_json::to_vec(&serde_json::json!({"task_id":id})).unwrap(),
            )
            .unwrap();
            assert_eq!(catalog.load_last_source(), Err(TransferResult::InvalidData));
        }
        for key in ["", "../peer", "aa/bb", "nothex", "aa:bb"] {
            assert!(matches!(
                TaskCatalog::open(&fixture.0, key),
                Err(TransferResult::InvalidData)
            ));
        }
    }

    #[test]
    fn invalid_summary_does_not_replace_previous_valid_pointer_or_snapshot() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let mut summary = fixture.task([1; 16]);
        catalog.persist_source(&summary).unwrap();
        summary.items = (1..=65).map(|id| item(id, RecoveryState::Failed)).collect();
        assert_eq!(
            catalog.persist_source(&summary),
            Err(TransferResult::InvalidData)
        );
        assert_eq!(catalog.load_last_source().unwrap().unwrap().items.len(), 2);
        summary.items = vec![
            item(1, RecoveryState::Failed),
            item(1, RecoveryState::Completed),
        ];
        assert_eq!(summary.validate(), Err(TransferResult::InvalidData));
        summary.items.clear();
        summary.acknowledged_bytes = summary.sent_bytes + 1;
        assert_eq!(summary.validate(), Err(TransferResult::InvalidData));
        summary.acknowledged_bytes = 0;
        summary.completed_items = u64::MAX;
        assert_eq!(summary.validate(), Err(TransferResult::InvalidData));
    }

    #[test]
    fn sixty_four_failed_items_remain_eligible_after_restart() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let mut summary = fixture.task([1; 16]);
        summary.failed_items = 64;
        summary.items = (1..=64).map(|id| item(id, RecoveryState::Failed)).collect();
        summary.state = RecoveryState::Partial;
        catalog.persist_source(&summary).unwrap();
        let recovered = catalog.load_last_source().unwrap().unwrap();
        assert_eq!(recovered.items.len(), 64);
        assert_eq!(recovered.items[0].item_id, 1);
        assert_eq!(recovered.items[63].item_id, 64);
        assert!(
            recovered
                .items
                .iter()
                .all(|item| item.state == RecoveryState::Failed)
        );
        assert_eq!(recovered.state, RecoveryState::Partial);
        assert_eq!(recovered.active_item.unwrap().state, RecoveryState::Paused);
    }

    #[test]
    fn deserialize_rejects_sixty_fifth_item_and_oversized_metadata() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let mut summary = fixture.task([1; 16]);
        catalog.persist_source(&summary).unwrap();
        let recovery = catalog.task_root(summary.task_id).join("recovery.json");
        summary.items = (1..=65).map(|id| item(id, RecoveryState::Failed)).collect();
        fs::write(&recovery, serde_json::to_vec(&summary).unwrap()).unwrap();
        assert_eq!(catalog.load_last_source(), Err(TransferResult::InvalidData));
        let oversized = File::create(&recovery).unwrap();
        oversized.set_len(MAX_RECOVERY_JSON_BYTES + 1).unwrap();
        drop(oversized);
        assert_eq!(catalog.load_last_source(), Err(TransferResult::InvalidData));
    }

    #[test]
    fn task_and_source_identity_mismatches_are_rejected() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let summary = fixture.task([1; 16]);
        catalog.persist_source(&summary).unwrap();
        let mut different = summary.clone();
        different.task_id = [2; 16];
        fs::write(
            catalog.task_root(summary.task_id).join("recovery.json"),
            serde_json::to_vec(&different).unwrap(),
        )
        .unwrap();
        assert_eq!(catalog.load_last_source(), Err(TransferResult::InvalidData));
        fs::write(
            catalog.task_root(summary.task_id).join("recovery.json"),
            serde_json::to_vec(&summary).unwrap(),
        )
        .unwrap();
        fs::write(
            catalog.task_root(summary.task_id).join("source.json"),
            serde_json::to_vec(
                &serde_json::json!({"path":summary.source_path,"name":"another","directory":true}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(catalog.load_last_source(), Err(TransferResult::InvalidData));
    }

    #[test]
    fn incomplete_scan_can_resume_but_complete_scan_requires_regular_manifest() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let mut summary = fixture.task([1; 16]);
        fs::remove_file(catalog.task_root(summary.task_id).join("manifest.jsonl")).unwrap();
        assert!(catalog.persist_source(&summary).is_err());
        summary.scan_complete = false;
        catalog.persist_source(&summary).unwrap();
        assert!(!catalog.load_last_source().unwrap().unwrap().scan_complete);
    }

    #[test]
    fn receiver_restart_recovers_exact_task_without_inventing_a_source_or_manifest() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let summary = fixture.received_task([3; 16]);
        catalog.persist(&summary).unwrap();
        assert_eq!(
            catalog.persist_source(&summary),
            Err(TransferResult::InvalidData)
        );
        assert_eq!(catalog.load_last_source().unwrap(), None);
        drop(catalog);
        let catalog = fixture.catalog();
        let recovered = catalog.load_last().unwrap().unwrap();
        assert_eq!(recovered.direction, RecoveryDirection::Download);
        assert_eq!(recovered.task_id, summary.task_id);
        assert_eq!(recovered.state, RecoveryState::Paused);
        assert_eq!(recovered.source_path, None);
        assert_eq!(recovered.items[1].item_id, 3);
        assert_eq!(recovered.items[1].state, RecoveryState::Failed);
        assert_eq!(recovered.acknowledged_bytes, summary.acknowledged_bytes);
        assert_eq!(
            RecoveryDirection::parse(recovered.direction.as_str()).unwrap(),
            RecoveryDirection::Download
        );
    }

    #[test]
    fn receiver_metadata_cannot_redirect_root_or_change_task_direction() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let summary = fixture.received_task([3; 16]);
        catalog.persist(&summary).unwrap();
        let task = catalog.task_root(summary.task_id);
        for root_name in [
            "../outside",
            "C:/outside",
            "other-task",
            "DeskLink-00000000000000000000000000000000",
        ] {
            fs::write(
                task.join("receive.json"),
                serde_json::to_vec(&serde_json::json!({
                    "name":summary.name,"directory":summary.directory,"root_name":root_name,
                }))
                .unwrap(),
            )
            .unwrap();
            assert_eq!(catalog.load_last(), Err(TransferResult::InvalidData));
        }
        let mut invalid = summary.clone();
        invalid.source_path = Some(fixture.0.join(&summary.name));
        assert_eq!(invalid.validate(), Err(TransferResult::InvalidData));
        invalid.source_path = None;
        invalid.direction = RecoveryDirection::Upload;
        assert_eq!(invalid.validate(), Err(TransferResult::InvalidData));
        assert_eq!(
            RecoveryDirection::parse("unknown"),
            Err(TransferResult::InvalidData)
        );
    }

    #[test]
    fn source_and_receiver_share_one_latest_task_pointer_with_bound_direction() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let source = fixture.task([1; 16]);
        catalog.persist_source(&source).unwrap();
        let received = fixture.received_task([3; 16]);
        catalog.persist(&received).unwrap();
        assert_eq!(
            catalog.load_last().unwrap().unwrap().direction,
            RecoveryDirection::Download
        );
        fs::write(
            catalog.peer_root.join("last-task.json"),
            serde_json::to_vec(&serde_json::json!({
                "task_id": id_text(received.task_id), "direction":"upload",
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(catalog.load_last(), Err(TransferResult::InvalidData));
        catalog.persist_source(&source).unwrap();
        assert_eq!(
            catalog.load_last().unwrap().unwrap().task_id,
            source.task_id
        );
    }

    #[test]
    fn task_directory_guard_creates_only_valid_ids_under_its_authenticated_peer() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        assert_eq!(
            catalog.ensure_task_dir([0; 16]).unwrap_err(),
            TransferResult::InvalidData
        );
        let guard = catalog.ensure_task_dir([4; 16]).unwrap();
        assert_eq!(guard.path(), fixture.0.join("aabb").join(id_text([4; 16])));
        assert!(guard.path().is_dir());
        let again = catalog.ensure_task_dir([4; 16]).unwrap();
        assert_eq!(again.path(), guard.path());
    }

    #[cfg(windows)]
    #[test]
    fn task_directory_guard_keeps_ancestor_pins_after_catalog_drop() {
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let guard = catalog.ensure_task_dir([4; 16]).unwrap();
        let task = guard.path().to_owned();
        drop(catalog);
        assert!(fs::rename(&task, task.with_extension("moved")).is_err());
        drop(guard);
        fs::rename(&task, task.with_extension("moved")).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn empty_task_directory_junction_mutation_cannot_redirect_catalog_io() {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows::Win32::{
            Foundation::{GENERIC_WRITE, HANDLE},
            Storage::FileSystem::{
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
                FILE_SHARE_READ, FILE_SHARE_WRITE,
            },
            System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_REPARSE_POINT},
        };
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::write(outside.0.join("untouched"), b"outside").unwrap();
        let catalog = fixture.catalog();
        let guard = catalog.ensure_task_dir([4; 16]).unwrap();
        let path = guard.path().to_owned();
        let printable = outside.0.to_string_lossy().to_string();
        let substitute = format!(r"\??\{printable}");
        let substitute: Vec<u16> = substitute.encode_utf16().collect();
        let print: Vec<u16> = printable.encode_utf16().collect();
        let data_length = 8 + (substitute.len() + 1 + print.len() + 1) * 2;
        let mut data = Vec::new();
        data.extend(0xA0000003_u32.to_le_bytes());
        data.extend((data_length as u16).to_le_bytes());
        data.extend(0_u16.to_le_bytes());
        data.extend(0_u16.to_le_bytes());
        data.extend((substitute.len() as u16 * 2).to_le_bytes());
        data.extend(((substitute.len() as u16 + 1) * 2).to_le_bytes());
        data.extend((print.len() as u16 * 2).to_le_bytes());
        for value in substitute.into_iter().chain([0]).chain(print).chain([0]) {
            data.extend(value.to_le_bytes());
        }
        let writer = OpenOptions::new()
            .access_mode(GENERIC_WRITE.0)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
            .open(&path);
        let result = writer.as_ref().map(|writer| {
            let mut returned = 0;
            unsafe {
                DeviceIoControl(
                    HANDLE(writer.as_raw_handle()),
                    FSCTL_SET_REPARSE_POINT,
                    Some(data.as_ptr().cast()),
                    data.len() as u32,
                    None,
                    0,
                    Some(&mut returned),
                    None,
                )
            }
        });
        let mutated = is_reparse(&fs::symlink_metadata(&path).unwrap());
        let attempted_child = native::create_file_path(&path.join("escape-probe"));
        let child_blocked = matches!(attempted_child, Err(TransferResult::PermissionDenied));
        let outside_written = outside.0.join("escape-probe").exists();
        if let Ok(file) = attempted_child {
            let _ = native::delete_file(&file);
        }
        let mut summary = fixture.task([5; 16]);
        summary.task_id = [4; 16];
        let persist_result = catalog.persist(&summary);
        let diagnostic = format!("writer={:?},set={:?}", writer.as_ref().map(|_| ()), result);
        drop(writer);
        drop(guard);
        if mutated {
            fs::remove_dir(&path).unwrap();
        }
        assert_eq!(fs::read(outside.0.join("untouched")).unwrap(), b"outside");
        assert!(mutated, "junction fixture did not mutate: {diagnostic}");
        assert!(
            child_blocked && !outside_written,
            "catalog child IO followed a mutated root"
        );
        assert_eq!(persist_result, Err(TransferResult::PermissionDenied));
    }

    #[cfg(windows)]
    #[test]
    fn interrupted_pointer_replacement_keeps_old_pointer_and_cleans_unique_temp() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let fixture = Fixture::new();
        let catalog = fixture.catalog();
        let first = fixture.task([1; 16]);
        catalog.persist_source(&first).unwrap();
        let pointer = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ.0)
            .open(catalog.peer_root.join("last-task.json"))
            .unwrap();
        let second = fixture.task([2; 16]);
        assert!(matches!(
            catalog.persist_source(&second),
            Err(TransferResult::Busy | TransferResult::PermissionDenied)
        ));
        assert_eq!(
            catalog.load_last_source().unwrap().unwrap().task_id,
            first.task_id
        );
        assert_eq!(
            fs::read_dir(&catalog.peer_root)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().ends_with("pending"))
                .count(),
            0
        );
        drop(pointer);
        catalog.persist_source(&second).unwrap();
        assert_eq!(
            catalog.load_last_source().unwrap().unwrap().task_id,
            second.task_id
        );
    }

    #[cfg(windows)]
    #[test]
    fn junction_cache_ancestors_and_task_roots_are_rejected() {
        use std::process::Command;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let link = fixture.0.join("cache-junction");
        let output = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside.0)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(matches!(
            TaskCatalog::open(&link, "aabb"),
            Err(TransferResult::PermissionDenied)
        ));
        fs::remove_dir(&link).unwrap();
        let catalog = fixture.catalog();
        let summary = fixture.task([1; 16]);
        catalog.persist_source(&summary).unwrap();
        let task_root = catalog.task_root(summary.task_id);
        fs::remove_dir_all(&task_root).unwrap();
        let output = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&task_root)
            .arg(&outside.0)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            catalog.load_last_source(),
            Err(TransferResult::PermissionDenied)
        );
        fs::remove_dir(&task_root).unwrap();
    }
}
