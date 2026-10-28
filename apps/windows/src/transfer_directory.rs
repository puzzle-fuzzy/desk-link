//! Streaming directory manifests and destination confinement for file tasks.
//!
//! Every method here performs blocking filesystem IO. Call it on a bounded
//! scanner/disk worker, never from the input, audio or video consumer. Windows
//! source directory handles deny write/delete sharing. Receiver operations use
//! directory-relative handles, reject reparse points and deny directory delete
//! sharing. Non-Windows callers must not advertise this
//! destination implementation as a platform security capability.

use std::{
    fs::{self, File, OpenOptions, ReadDir},
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::UNIX_EPOCH,
};

use desklink_protocol::{
    MAX_FILE_TASK_BYTES, MAX_FILE_TASK_ENTRIES, MAX_LARGE_FILE_BYTES, MAX_TRANSFER_DIRECTORY_DEPTH,
    MAX_TRANSFER_RELATIVE_PATH_BYTES, ManifestEntry, TransferEntryKind, TransferResult,
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[cfg(windows)]
#[path = "transfer_directory_native.rs"]
pub(crate) mod native;

pub const DIRECTORY_SCAN_QUEUE_CAPACITY: usize = 2;
pub const DIRECTORY_MAX_DEPTH: usize = MAX_TRANSFER_DIRECTORY_DEPTH;
pub const DIRECTORY_MAX_RELATIVE_PATH_BYTES: usize = MAX_TRANSFER_RELATIVE_PATH_BYTES;
const PART_PREFIX: &str = ".desklink-part-";
const MAX_DIRECTORY_SCAN_WORKERS: usize = 8;
static ACTIVE_DIRECTORY_SCAN_WORKERS: AtomicUsize = AtomicUsize::new(0);

struct DirectoryScanWorkerPermit;

impl DirectoryScanWorkerPermit {
    fn acquire() -> Result<Self, TransferResult> {
        ACTIVE_DIRECTORY_SCAN_WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_DIRECTORY_SCAN_WORKERS).then_some(active + 1)
            })
            .map(|_| Self)
            .map_err(|_| TransferResult::Busy)
    }
}

impl Drop for DirectoryScanWorkerPermit {
    fn drop(&mut self) {
        ACTIVE_DIRECTORY_SCAN_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DirectoryScanLimits {
    pub max_entries: u64,
    pub max_total_bytes: u64,
    pub max_file_bytes: u64,
    pub max_depth: usize,
    pub max_relative_path_bytes: usize,
}

impl DirectoryScanLimits {
    pub fn validate(self) -> Result<Self, TransferResult> {
        if self.max_entries == 0
            || self.max_entries > MAX_FILE_TASK_ENTRIES
            || self.max_file_bytes == 0
            || self.max_file_bytes > MAX_LARGE_FILE_BYTES
            || self.max_total_bytes > MAX_FILE_TASK_BYTES
            || self.max_depth == 0
            || self.max_depth > DIRECTORY_MAX_DEPTH
            || self.max_relative_path_bytes == 0
            || self.max_relative_path_bytes > DIRECTORY_MAX_RELATIVE_PATH_BYTES
            || self.max_file_bytes > self.max_total_bytes
        {
            Err(TransferResult::InvalidData)
        } else {
            Ok(self)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct SourceIdentity {
    size: u64,
    modified_at_unix_ns: u64,
    source_stamp: Option<[u8; 32]>,
}

impl SourceIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            size: metadata.len(),
            modified_at_unix_ns: modified_at_unix_ns(metadata),
            source_stamp: None,
        }
    }

    fn from_file(file: &File) -> Result<Self, TransferResult> {
        let metadata = file.metadata().map_err(io_result)?;
        let (size, stamp) = crate::transfer_checkpoint::source_stamp(file)?;
        Ok(Self {
            size,
            modified_at_unix_ns: modified_at_unix_ns(&metadata),
            source_stamp: Some(stamp),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub item_id: u64,
    /// UTF-8, '/' separated and relative to the selected source directory.
    pub relative_path: String,
    pub kind: TransferEntryKind,
    pub size: u64,
    pub modified_at_unix_ns: u64,
    pub source_path: PathBuf,
    identity: SourceIdentity,
}

impl DirectoryEntry {
    pub fn single_file(path: &Path) -> Result<Self, TransferResult> {
        let source_path = absolute_path(path)?;
        let parent = source_path.parent().ok_or(TransferResult::InvalidData)?;
        let _pins = pin_directory_chain(parent)?;
        let name = source_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(TransferResult::InvalidData)?;
        if !is_valid_path_segment(name) {
            return Err(TransferResult::InvalidData);
        }
        let file = open_source_file(&source_path)?;
        let identity = SourceIdentity::from_file(&file)?;
        Ok(Self {
            item_id: 1,
            relative_path: name.to_owned(),
            kind: TransferEntryKind::File,
            size: identity.size,
            modified_at_unix_ns: identity.modified_at_unix_ns,
            source_path,
            identity,
        })
    }

    pub fn manifest(&self, content_root: Option<[u8; 32]>) -> ManifestEntry {
        ManifestEntry {
            item_id: self.item_id,
            relative_path: self.relative_path.clone(),
            kind: self.kind,
            size: self.size,
            content_root,
        }
    }

    /// Reopens through pinned, non-reparse ancestors and compares the actual
    /// opened source identity with the scanner's observation. Keep this guard
    /// alive until the sender finishes; its File can be borrowed by the index.
    pub fn open_verified_source(&self) -> Result<GuardedSourceFile, TransferResult> {
        if self.kind != TransferEntryKind::File {
            return Err(TransferResult::InvalidData);
        }
        let parent = self
            .source_path
            .parent()
            .ok_or(TransferResult::InvalidData)?;
        let pins = pin_directory_chain(parent)?;
        let metadata = fs::symlink_metadata(&self.source_path).map_err(io_result)?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(TransferResult::SourceChanged);
        }
        let file = open_source_file(&self.source_path)?;
        let metadata = file.metadata().map_err(io_result)?;
        if is_link_or_reparse(&metadata)
            || !metadata.is_file()
            || SourceIdentity::from_file(&file)? != self.identity
        {
            return Err(TransferResult::SourceChanged);
        }
        Ok(GuardedSourceFile {
            file,
            _directory_pins: pins,
        })
    }

    /// Pin an observed directory through its send handshake. Child repairs can
    /// change directory times, so only its actual kind/location is checked.
    pub fn verify_directory_source(&self) -> Result<GuardedSourceDirectory, TransferResult> {
        if self.kind != TransferEntryKind::Directory {
            return Err(TransferResult::InvalidData);
        }
        GuardedSourceDirectory::open(&self.source_path)
    }
}

pub struct GuardedSourceDirectory {
    _directory_pins: Vec<File>,
}

impl GuardedSourceDirectory {
    /// Also guard the selected source root when its manifest has no entries.
    pub fn open(path: &Path) -> Result<Self, TransferResult> {
        let path = absolute_path(path)?;
        let mut pins = match path.parent() {
            Some(parent) => pin_directory_chain(parent)?,
            None => Vec::new(),
        };
        let metadata = fs::symlink_metadata(&path).map_err(io_result)?;
        if is_link_or_reparse(&metadata) || !metadata.is_dir() {
            return Err(TransferResult::SourceChanged);
        }
        pins.push(open_directory_pin(&path)?);
        Ok(Self {
            _directory_pins: pins,
        })
    }

    pub fn verify_location(&self) -> Result<(), TransferResult> {
        #[cfg(windows)]
        {
            native::check_chain(&self._directory_pins)
        }
        #[cfg(not(windows))]
        {
            Ok(())
        }
    }
}

pub struct GuardedSourceFile {
    pub file: File,
    _directory_pins: Vec<File>,
}

impl GuardedSourceFile {
    pub fn verify_location(&self) -> Result<(), TransferResult> {
        #[cfg(windows)]
        {
            native::check_chain(&self._directory_pins)
        }
        #[cfg(not(windows))]
        {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DirectoryScanFailureScope {
    Entry,
    Subtree,
    /// An already yielded directory could not enumerate one or more children.
    Enumeration,
    /// Cancellation or a global discovery limit ended the remaining scan.
    Task,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirectoryScanFailure {
    /// Present only when the observed entry has a valid transfer path.
    pub relative_path: Option<String>,
    pub result: TransferResult,
    pub scope: DirectoryScanFailureScope,
    pub kind: Option<TransferEntryKind>,
    /// Observed size, including files that exceed the negotiated byte limits.
    /// This is local recovery metadata, not an admitted wire byte count.
    pub size: Option<u64>,
    pub terminal: bool,
    /// Valid relative prefix. An empty prefix means the selected source root.
    /// Enumeration retries omit this directory itself; entry/subtree retries
    /// include it. Never construct an absolute retry path from display text.
    pub retry_relative_path: Option<String>,
}

impl DirectoryScanFailure {
    fn task(result: TransferResult) -> Self {
        Self {
            relative_path: None,
            result,
            scope: DirectoryScanFailureScope::Task,
            kind: None,
            size: None,
            terminal: true,
            retry_relative_path: None,
        }
    }

    fn entry(
        relative_path: String,
        result: TransferResult,
        kind: Option<TransferEntryKind>,
        size: Option<u64>,
    ) -> Self {
        Self {
            relative_path: Some(relative_path.clone()),
            result,
            scope: if kind == Some(TransferEntryKind::Directory) {
                DirectoryScanFailureScope::Subtree
            } else {
                DirectoryScanFailureScope::Entry
            },
            kind,
            size,
            terminal: false,
            retry_relative_path: Some(relative_path),
        }
    }

    fn enumeration(relative_path: String, result: TransferResult) -> Self {
        Self {
            relative_path: (!relative_path.is_empty()).then(|| relative_path.clone()),
            result,
            scope: DirectoryScanFailureScope::Enumeration,
            kind: Some(TransferEntryKind::Directory),
            size: Some(0),
            terminal: false,
            retry_relative_path: Some(relative_path),
        }
    }
}

struct DirectoryFrame {
    path: PathBuf,
    relative_path: String,
    entries: ReadDir,
    // Pin each open DFS ancestor. Memory/handle usage is O(max_depth).
    _pin: File,
}

pub struct DirectoryScanner {
    frames: Vec<DirectoryFrame>,
    _root_ancestor_pins: Vec<File>,
    cancellation: Arc<AtomicBool>,
    limits: DirectoryScanLimits,
    next_item_id: u64,
    scanned_entries: u64,
    total_bytes: u64,
    terminal: bool,
    pending_entry: Option<DirectoryEntry>,
    last_yielded_directory: Option<String>,
}

impl DirectoryScanner {
    pub fn open_with_limits(
        root: &Path,
        cancellation: Arc<AtomicBool>,
        limits: DirectoryScanLimits,
    ) -> Result<Self, TransferResult> {
        let limits = limits.validate()?;
        check_cancelled(&cancellation)?;
        let root = absolute_path(root)?;
        let root_ancestor_pins = pin_directory_chain(&root)?;
        let pin = open_directory_pin(&root)?;
        let entries = fs::read_dir(&root).map_err(io_result)?;
        Ok(Self {
            frames: vec![DirectoryFrame {
                path: root,
                relative_path: String::new(),
                entries,
                _pin: pin,
            }],
            _root_ancestor_pins: root_ancestor_pins,
            cancellation,
            limits,
            next_item_id: 1,
            scanned_entries: 0,
            total_bytes: 0,
            terminal: false,
            pending_entry: None,
            last_yielded_directory: None,
        })
    }

    /// Re-scan one failed entry/subtree without discovering unrelated siblings.
    /// Prefixes and depth remain relative to the originally authorized root.
    /// The caller assigns durable task/item IDs when merging these outcomes.
    pub fn open_subtree_with_limits(
        root: &Path,
        relative_prefix: &str,
        include_root: bool,
        cancellation: Arc<AtomicBool>,
        limits: DirectoryScanLimits,
    ) -> Result<Self, TransferResult> {
        let limits = limits.validate()?;
        if relative_prefix.is_empty() {
            return if include_root {
                Err(TransferResult::InvalidData)
            } else {
                Self::open_with_limits(root, cancellation, limits)
            };
        }
        if !is_valid_relative_path_with_limits(
            relative_prefix,
            limits.max_depth,
            limits.max_relative_path_bytes,
        ) {
            return Err(TransferResult::InvalidData);
        }
        check_cancelled(&cancellation)?;
        let root = absolute_path(root)?;
        let mut pins = pin_directory_chain(&root)?;
        let target = relative_prefix
            .split('/')
            .fold(root, |path, part| path.join(part));
        let parent = target.parent().ok_or(TransferResult::InvalidData)?;
        // Keep the original root and every newly traversed ancestor pinned.
        pins.extend(pin_directory_chain(parent)?);
        let metadata = fs::symlink_metadata(&target).map_err(io_result)?;
        if is_link_or_reparse(&metadata) {
            return Err(TransferResult::Unsupported);
        }
        let (kind, size, identity, frames) = if metadata.is_dir() {
            let pin = open_directory_pin(&target)?;
            let entries = fs::read_dir(&target).map_err(io_result)?;
            (
                TransferEntryKind::Directory,
                0,
                SourceIdentity::from_metadata(&metadata),
                vec![DirectoryFrame {
                    path: target.clone(),
                    relative_path: relative_prefix.to_owned(),
                    entries,
                    _pin: pin,
                }],
            )
        } else if metadata.is_file() && include_root {
            let file = open_source_file(&target)?;
            let identity = SourceIdentity::from_file(&file)?;
            if identity.size != metadata.len()
                || identity.modified_at_unix_ns != modified_at_unix_ns(&metadata)
            {
                return Err(TransferResult::SourceChanged);
            }
            if identity.size > limits.max_file_bytes || identity.size > limits.max_total_bytes {
                return Err(TransferResult::TooLarge);
            }
            (TransferEntryKind::File, identity.size, identity, Vec::new())
        } else {
            return Err(TransferResult::Unsupported);
        };
        let pending_entry = include_root.then(|| DirectoryEntry {
            item_id: 1,
            relative_path: relative_prefix.to_owned(),
            kind,
            size,
            modified_at_unix_ns: identity.modified_at_unix_ns,
            source_path: target,
            identity,
        });
        Ok(Self {
            frames,
            _root_ancestor_pins: pins,
            cancellation,
            limits,
            next_item_id: if include_root { 2 } else { 1 },
            scanned_entries: u64::from(include_root),
            total_bytes: size,
            terminal: false,
            pending_entry,
            last_yielded_directory: None,
        })
    }

    /// Prune only the directory just yielded by this iterator. The caller uses
    /// this when a retry observes a directory where an immutable file entry was
    /// already admitted; its new children must not become phantom task items.
    pub fn skip_current_subtree(&mut self, relative_prefix: &str) -> Result<(), TransferResult> {
        if self.last_yielded_directory.as_deref() != Some(relative_prefix)
            || self
                .frames
                .last()
                .is_none_or(|frame| frame.relative_path != relative_prefix)
        {
            return Err(TransferResult::InvalidData);
        }
        self.frames.pop();
        self.last_yielded_directory = None;
        Ok(())
    }

    pub fn scanned_entries(&self) -> u64 {
        self.scanned_entries
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn active_directory_handles(&self) -> usize {
        self.frames.len()
    }

    fn failure(
        &mut self,
        failure: DirectoryScanFailure,
    ) -> Option<Result<DirectoryEntry, DirectoryScanFailure>> {
        self.terminal = failure.terminal;
        if failure.terminal {
            self.frames.clear();
            self._root_ancestor_pins.clear();
            self.pending_entry = None;
        }
        Some(Err(failure))
    }
}

impl Iterator for DirectoryScanner {
    type Item = Result<DirectoryEntry, DirectoryScanFailure>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.terminal {
            return None;
        }
        self.last_yielded_directory = None;
        loop {
            if check_cancelled(&self.cancellation).is_err() {
                return self.failure(DirectoryScanFailure::task(TransferResult::Cancelled));
            }
            if let Some(entry) = self.pending_entry.take() {
                if entry.kind == TransferEntryKind::Directory {
                    self.last_yielded_directory = Some(entry.relative_path.clone());
                }
                return Some(Ok(entry));
            }
            let frame = self.frames.last_mut()?;
            let entry = match frame.entries.next() {
                Some(Ok(entry)) => entry,
                Some(Err(error)) => {
                    self.scanned_entries += 1;
                    if self.scanned_entries > self.limits.max_entries {
                        return self.failure(DirectoryScanFailure::task(TransferResult::TooLarge));
                    }
                    let path = frame.relative_path.clone();
                    return self.failure(DirectoryScanFailure::enumeration(path, io_result(error)));
                }
                None => {
                    self.frames.pop();
                    continue;
                }
            };
            // Count failures too: hostile entries cannot evade the scan budget.
            self.scanned_entries += 1;
            if self.scanned_entries > self.limits.max_entries {
                return self.failure(DirectoryScanFailure::task(TransferResult::TooLarge));
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                let path = frame.relative_path.clone();
                return self.failure(DirectoryScanFailure::enumeration(
                    path,
                    TransferResult::InvalidData,
                ));
            };
            let relative_path = if frame.relative_path.is_empty() {
                name
            } else {
                format!("{}/{}", frame.relative_path, name)
            };
            if !is_valid_relative_path_with_limits(
                &relative_path,
                self.limits.max_depth,
                self.limits.max_relative_path_bytes,
            ) {
                // An unsafe name cannot be put on the wire or used as a retry
                // path. Retry its safe parent to discover a repaired name.
                let path = frame.relative_path.clone();
                return self.failure(DirectoryScanFailure::enumeration(
                    path,
                    TransferResult::InvalidData,
                ));
            }
            let source_path = frame.path.join(entry.file_name());
            let metadata = match fs::symlink_metadata(&source_path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    return self.failure(DirectoryScanFailure::entry(
                        relative_path,
                        io_result(error),
                        None,
                        None,
                    ));
                }
            };
            if is_link_or_reparse(&metadata) {
                let kind = if metadata.is_dir() {
                    Some(TransferEntryKind::Directory)
                } else if metadata.is_file() {
                    Some(TransferEntryKind::File)
                } else {
                    None
                };
                let size = kind.map(|kind| {
                    if kind == TransferEntryKind::File {
                        metadata.len()
                    } else {
                        0
                    }
                });
                return self.failure(DirectoryScanFailure::entry(
                    relative_path,
                    TransferResult::Unsupported,
                    kind,
                    size,
                ));
            }
            let (kind, size) = if metadata.is_dir() {
                // Yield the directory even when empty, before its children.
                let pin = match open_directory_pin(&source_path) {
                    Ok(pin) => pin,
                    Err(error) => {
                        return self.failure(DirectoryScanFailure::entry(
                            relative_path,
                            error,
                            Some(TransferEntryKind::Directory),
                            Some(0),
                        ));
                    }
                };
                let entries = match fs::read_dir(&source_path) {
                    Ok(entries) => entries,
                    Err(error) => {
                        return self.failure(DirectoryScanFailure::entry(
                            relative_path,
                            io_result(error),
                            Some(TransferEntryKind::Directory),
                            Some(0),
                        ));
                    }
                };
                self.frames.push(DirectoryFrame {
                    path: source_path.clone(),
                    relative_path: relative_path.clone(),
                    entries,
                    _pin: pin,
                });
                (TransferEntryKind::Directory, 0)
            } else if metadata.is_file() {
                if metadata.len() > self.limits.max_file_bytes {
                    return self.failure(DirectoryScanFailure::entry(
                        relative_path,
                        TransferResult::TooLarge,
                        Some(TransferEntryKind::File),
                        Some(metadata.len()),
                    ));
                }
                let Some(total_bytes) = self.total_bytes.checked_add(metadata.len()) else {
                    return self.failure(DirectoryScanFailure::entry(
                        relative_path,
                        TransferResult::TooLarge,
                        Some(TransferEntryKind::File),
                        Some(metadata.len()),
                    ));
                };
                if total_bytes > self.limits.max_total_bytes {
                    return self.failure(DirectoryScanFailure::entry(
                        relative_path,
                        TransferResult::TooLarge,
                        Some(TransferEntryKind::File),
                        Some(metadata.len()),
                    ));
                }
                (TransferEntryKind::File, metadata.len())
            } else {
                return self.failure(DirectoryScanFailure::entry(
                    relative_path,
                    TransferResult::Unsupported,
                    None,
                    None,
                ));
            };
            let item_id = self.next_item_id;
            self.next_item_id += 1;
            let identity = if kind == TransferEntryKind::File {
                let file = match open_source_file(&source_path) {
                    Ok(file) => file,
                    Err(error) => {
                        return self.failure(DirectoryScanFailure::entry(
                            relative_path,
                            error,
                            Some(kind),
                            Some(size),
                        ));
                    }
                };
                match SourceIdentity::from_file(&file) {
                    Ok(identity)
                        if identity.size == metadata.len()
                            && identity.modified_at_unix_ns == modified_at_unix_ns(&metadata) =>
                    {
                        identity
                    }
                    Ok(_) => {
                        return self.failure(DirectoryScanFailure::entry(
                            relative_path,
                            TransferResult::SourceChanged,
                            Some(kind),
                            Some(size),
                        ));
                    }
                    Err(error) => {
                        return self.failure(DirectoryScanFailure::entry(
                            relative_path,
                            error,
                            Some(kind),
                            Some(size),
                        ));
                    }
                }
            } else {
                SourceIdentity::from_metadata(&metadata)
            };
            // Failed opens and identity checks do not consume the admitted byte
            // budget and cannot prevent a later, smaller sibling from fitting.
            self.total_bytes += size;
            if kind == TransferEntryKind::Directory {
                self.last_yielded_directory = Some(relative_path.clone());
            }
            return Some(Ok(DirectoryEntry {
                item_id,
                relative_path,
                kind,
                size,
                modified_at_unix_ns: identity.modified_at_unix_ns,
                source_path,
                identity,
            }));
        }
    }
}

/// Scanner backpressure is independent of the network and media consumers.
/// Dropping the receiver wakes a blocked producer. The caller's cancellation
/// flag is also checked between filesystem operations; OS read_dir itself is
/// not interruptible.
pub fn spawn_directory_scanner(
    root: PathBuf,
    cancellation: Arc<AtomicBool>,
    limits: DirectoryScanLimits,
) -> Result<mpsc::Receiver<Result<DirectoryEntry, DirectoryScanFailure>>, TransferResult> {
    let permit = DirectoryScanWorkerPermit::acquire()?;
    let (sender, receiver) = mpsc::channel(DIRECTORY_SCAN_QUEUE_CAPACITY);
    std::thread::Builder::new()
        .name("desklink-directory-scan".to_owned())
        .spawn(move || {
            let _permit = permit;
            let scanner =
                match DirectoryScanner::open_with_limits(&root, cancellation.clone(), limits) {
                    Ok(scanner) => scanner,
                    Err(result) => {
                        let _ = sender.blocking_send(Err(DirectoryScanFailure::task(result)));
                        return;
                    }
                };
            for entry in scanner {
                let mut pending = entry;
                loop {
                    match sender.try_send(pending) {
                        Ok(()) => break,
                        Err(mpsc::error::TrySendError::Closed(_)) => return,
                        Err(mpsc::error::TrySendError::Full(entry)) => {
                            if cancellation.load(Ordering::Acquire) {
                                return;
                            }
                            pending = entry;
                            std::thread::park_timeout(std::time::Duration::from_millis(10));
                        }
                    }
                }
            }
        })
        .map_err(io_result)?;
    Ok(receiver)
}

/// An explicit receiver authorization creates one fresh root. Existing files
/// are never replaced; every individual path is validated again at disk IO.
pub struct DirectoryDestination {
    root: PathBuf,
    _root_ancestor_pins: Vec<File>,
    limits: DirectoryScanLimits,
    accepted_entries: u64,
}

impl DirectoryDestination {
    pub fn create_new(
        authorized_parent: &Path,
        name: &str,
        limits: DirectoryScanLimits,
    ) -> Result<Self, TransferResult> {
        let limits = limits.validate()?;
        if !is_valid_path_segment(name) {
            return Err(TransferResult::InvalidData);
        }
        let parent = absolute_path(authorized_parent)?;
        let mut pins = destination_pin_chain(&parent)?;
        ensure_sibling_absent(&parent, name)?;
        let root = parent.join(name);
        #[cfg(windows)]
        pins.push(native::create_dir(
            pins.last().ok_or(TransferResult::InvalidData)?,
            std::ffi::OsStr::new(name),
        )?);
        #[cfg(not(windows))]
        {
            fs::create_dir(&root).map_err(io_result)?;
            pins.push(open_directory_pin(&root)?);
        }
        check_destination_pins(&pins)?;
        Ok(Self {
            root,
            _root_ancestor_pins: pins,
            limits,
            accepted_entries: 0,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Re-authorize a locally journaled root after restart. This method does
    /// not grant permission by itself: the coordinator must bind it to the
    /// authenticated task and a new explicit receiver decision.
    pub fn open_existing(
        authorized_parent: &Path,
        name: &str,
        limits: DirectoryScanLimits,
    ) -> Result<Self, TransferResult> {
        let limits = limits.validate()?;
        if !is_valid_path_segment(name) {
            return Err(TransferResult::InvalidData);
        }
        let parent = absolute_path(authorized_parent)?;
        let mut pins = destination_pin_chain(&parent)?;
        ensure_exact_sibling(&parent, name)?;
        let root = parent.join(name);
        #[cfg(windows)]
        pins.push(native::open_dir(
            pins.last().ok_or(TransferResult::InvalidData)?,
            std::ffi::OsStr::new(name),
        )?);
        #[cfg(not(windows))]
        pins.push(open_directory_pin(&root)?);
        check_destination_pins(&pins)?;
        Ok(Self {
            root,
            _root_ancestor_pins: pins,
            limits,
            accepted_entries: 0,
        })
    }

    pub fn create_directory(&mut self, relative_path: &str) -> Result<PathBuf, TransferResult> {
        self.reserve_entry(relative_path)?;
        let (parent, name, pins) = self.resolve_parent(relative_path)?;
        ensure_sibling_absent(&parent, &name)?;
        let path = parent.join(name);
        #[cfg(windows)]
        let _pin = native::create_dir(
            pins.last().ok_or(TransferResult::InvalidData)?,
            path.file_name().ok_or(TransferResult::InvalidData)?,
        )?;
        #[cfg(not(windows))]
        {
            fs::create_dir(&path).map_err(io_result)?;
            let _pin = open_directory_pin(&path)?;
        }
        check_destination_pins(&pins)?;
        Ok(path)
    }

    pub fn create_file(
        &mut self,
        relative_path: &str,
    ) -> Result<GuardedDestinationFile, TransferResult> {
        self.reserve_entry(relative_path)?;
        let (parent, name, pins) = self.resolve_parent(relative_path)?;
        ensure_sibling_absent(&parent, &name)?;
        let destination_path = parent.join(name);
        let mut random = [0_u8; 16];
        OsRng.fill_bytes(&mut random);
        let staging_name = format!("{PART_PREFIX}{}", hex_id(&random));
        let staging_path = parent.join(staging_name);
        #[cfg(not(windows))]
        let mut options = OpenOptions::new();
        #[cfg(not(windows))]
        options.create_new(true).read(true).write(true);
        #[cfg(windows)]
        let file = native::create_file(
            pins.last().ok_or(TransferResult::InvalidData)?,
            staging_path
                .file_name()
                .ok_or(TransferResult::InvalidData)?,
        )?;
        #[cfg(not(windows))]
        let file = options.open(&staging_path).map_err(io_result)?;
        #[allow(clippy::question_mark)] // windows cleanup must run before returning
        if let Err(error) = check_destination_pins(&pins) {
            #[cfg(windows)]
            let _ = native::delete_owned(&file);
            return Err(error);
        }
        Ok(GuardedDestinationFile {
            file: Some(file),
            staging_path,
            destination_path,
            _directory_pins: pins,
            remove_on_drop: true,
        })
    }

    /// Only a trusted local journal may supply the staging path. It must be a
    /// previously generated part in this exact item's pinned parent directory.
    /// Existing active writers deny this open on Windows before any cleanup.
    pub fn resume_file(
        &mut self,
        relative_path: &str,
        staging_path: &Path,
    ) -> Result<GuardedDestinationFile, TransferResult> {
        self.reserve_entry(relative_path)?;
        let (parent, name, pins) = self.resolve_parent(relative_path)?;
        let staging_name = staging_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(TransferResult::InvalidData)?;
        if staging_path.parent() != Some(parent.as_path()) || !is_staging_name(staging_name) {
            return Err(TransferResult::InvalidData);
        }
        ensure_sibling_absent(&parent, &name)?;
        #[cfg(not(windows))]
        let metadata = fs::symlink_metadata(staging_path).map_err(io_result)?;
        #[cfg(not(windows))]
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(TransferResult::PermissionDenied);
        }
        #[cfg(not(windows))]
        let mut options = OpenOptions::new();
        #[cfg(not(windows))]
        options.read(true).write(true);
        #[cfg(windows)]
        let file = native::open_file(
            pins.last().ok_or(TransferResult::InvalidData)?,
            std::ffi::OsStr::new(staging_name),
            true,
        )?;
        #[cfg(not(windows))]
        let file = options.open(staging_path).map_err(io_result)?;
        let metadata = file.metadata().map_err(io_result)?;
        if is_link_or_reparse(&metadata) || !metadata.is_file() {
            return Err(TransferResult::PermissionDenied);
        }
        check_destination_pins(&pins)?;
        Ok(GuardedDestinationFile {
            file: Some(file),
            staging_path: staging_path.to_owned(),
            destination_path: parent.join(name),
            _directory_pins: pins,
            remove_on_drop: true,
        })
    }

    /// Opens a final file only for journal reconciliation. The coordinator must
    /// verify its complete expected size and content root before considering
    /// the item complete; path existence alone is never completion evidence.
    pub fn open_existing_file(
        &self,
        relative_path: &str,
    ) -> Result<GuardedSourceFile, TransferResult> {
        if !is_valid_relative_path_with_limits(
            relative_path,
            self.limits.max_depth,
            self.limits.max_relative_path_bytes,
        ) {
            return Err(TransferResult::InvalidData);
        }
        let (parent, name, pins) = self.resolve_parent(relative_path)?;
        ensure_exact_sibling(&parent, &name)?;
        #[cfg(windows)]
        let file = native::open_file(
            pins.last().ok_or(TransferResult::InvalidData)?,
            std::ffi::OsStr::new(&name),
            false,
        )?;
        #[cfg(not(windows))]
        let file = open_source_file(&parent.join(name))?;
        check_destination_pins(&pins)?;
        Ok(GuardedSourceFile {
            file,
            _directory_pins: pins,
        })
    }

    pub fn validate_existing_directory(&self, relative_path: &str) -> Result<(), TransferResult> {
        if !is_valid_relative_path_with_limits(
            relative_path,
            self.limits.max_depth,
            self.limits.max_relative_path_bytes,
        ) {
            return Err(TransferResult::InvalidData);
        }
        let (parent, name, pins) = self.resolve_parent(relative_path)?;
        ensure_exact_sibling(&parent, &name)?;
        #[cfg(windows)]
        let _pin = native::open_dir(
            pins.last().ok_or(TransferResult::InvalidData)?,
            std::ffi::OsStr::new(&name),
        )?;
        #[cfg(not(windows))]
        let _pin = open_directory_pin(&parent.join(name))?;
        check_destination_pins(&pins)?;
        Ok(())
    }

    fn reserve_entry(&mut self, relative_path: &str) -> Result<(), TransferResult> {
        if !is_valid_relative_path_with_limits(
            relative_path,
            self.limits.max_depth,
            self.limits.max_relative_path_bytes,
        ) {
            return Err(TransferResult::InvalidData);
        }
        if self.accepted_entries >= self.limits.max_entries {
            return Err(TransferResult::TooLarge);
        }
        self.accepted_entries += 1;
        Ok(())
    }

    fn resolve_parent(
        &self,
        relative_path: &str,
    ) -> Result<(PathBuf, String, Vec<File>), TransferResult> {
        let mut parts = relative_path.split('/').peekable();
        let mut parent = self.root.clone();
        let mut pins = destination_pin_chain(&parent)?;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                check_destination_pins(&pins)?;
                return Ok((parent, part.to_owned(), pins));
            }
            // Require exact parent spelling and an already announced directory.
            // This prevents a case alias from changing manifest identities.
            ensure_exact_sibling(&parent, part)?;
            parent.push(part);
            #[cfg(windows)]
            pins.push(native::open_dir(
                pins.last().ok_or(TransferResult::InvalidData)?,
                std::ffi::OsStr::new(part),
            )?);
            #[cfg(not(windows))]
            pins.push(open_directory_pin(&parent)?);
        }
        Err(TransferResult::InvalidData)
    }
}

pub struct GuardedDestinationFile {
    file: Option<File>,
    staging_path: PathBuf,
    destination_path: PathBuf,
    _directory_pins: Vec<File>,
    remove_on_drop: bool,
}

impl GuardedDestinationFile {
    pub fn file_mut(&mut self) -> Result<&mut File, TransferResult> {
        check_destination_pins(&self._directory_pins)?;
        self.file.as_mut().ok_or(TransferResult::InvalidData)
    }

    pub fn staging_path(&self) -> &Path {
        &self.staging_path
    }

    pub fn destination_path(&self) -> &Path {
        &self.destination_path
    }

    /// The checkpoint worker must validate the complete content root first.
    /// Publication flushes data and creates the final path without replacement.
    /// Windows renames the owned handle relative to its pinned parent.
    pub fn publish(self) -> Result<PathBuf, TransferResult> {
        self.publish_with(|_| Ok(()))
    }

    /// Runs after publication while the original writer handle is still owned.
    /// A verified checkpoint can persist its source index using the final file
    /// identity. Callback failure preserves the published data for recovery.
    pub fn publish_with(
        mut self,
        after_publish: impl FnOnce(&File) -> Result<(), TransferResult>,
    ) -> Result<PathBuf, TransferResult> {
        self.file_mut()?.sync_all().map_err(io_result)?;
        let parent = self
            .destination_path
            .parent()
            .ok_or(TransferResult::InvalidData)?;
        let name = self
            .destination_path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(TransferResult::InvalidData)?;
        ensure_sibling_absent(parent, name)?;
        #[cfg(windows)]
        {
            check_destination_pins(&self._directory_pins)?;
            native::rename(
                self.file.as_ref().ok_or(TransferResult::InvalidData)?,
                self._directory_pins
                    .last()
                    .ok_or(TransferResult::InvalidData)?,
                std::ffi::OsStr::new(name),
                false,
            )?;
            check_destination_pins(&self._directory_pins)?;
        }
        #[cfg(not(windows))]
        {
            fs::hard_link(&self.staging_path, &self.destination_path).map_err(io_result)?;
            fs::remove_file(&self.staging_path).map_err(io_result)?;
        }
        self.remove_on_drop = false;
        after_publish(self.file.as_ref().ok_or(TransferResult::InvalidData)?)?;
        check_destination_pins(&self._directory_pins)?;
        self.file.take();
        Ok(self.destination_path.clone())
    }

    pub fn preserve(mut self) -> Result<PathBuf, TransferResult> {
        self.file_mut()?.sync_all().map_err(io_result)?;
        self.file.take();
        self.remove_on_drop = false;
        Ok(self.staging_path.clone())
    }
}

impl Drop for GuardedDestinationFile {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            if self.remove_on_drop
                && let Some(file) = self.file.as_ref()
            {
                let _ = native::delete_owned(file);
            }
            self.file.take();
        }
        #[cfg(not(windows))]
        if self.remove_on_drop {
            self.file.take();
            // Random, create_new staging names are never reused by a later
            // worker. Directory pins outlive this cleanup.
            let _ = fs::remove_file(&self.staging_path);
        }
    }
}

fn destination_pin_chain(path: &Path) -> Result<Vec<File>, TransferResult> {
    #[cfg(windows)]
    {
        native::pin_chain(path)
    }
    #[cfg(not(windows))]
    {
        pin_directory_chain(path)
    }
}

fn check_destination_pins(pins: &[File]) -> Result<(), TransferResult> {
    #[cfg(windows)]
    {
        native::check_chain(pins)
    }
    #[cfg(not(windows))]
    {
        let _ = pins;
        Ok(())
    }
}

pub fn is_valid_relative_path(relative_path: &str) -> bool {
    is_valid_relative_path_with_limits(
        relative_path,
        DIRECTORY_MAX_DEPTH,
        DIRECTORY_MAX_RELATIVE_PATH_BYTES,
    )
}

fn is_valid_relative_path_with_limits(
    relative_path: &str,
    max_depth: usize,
    max_bytes: usize,
) -> bool {
    !relative_path.is_empty()
        && relative_path.len() <= max_bytes
        && relative_path.split('/').count() <= max_depth
        && relative_path.split('/').all(is_valid_path_segment)
}

fn is_valid_path_segment(name: &str) -> bool {
    desklink_protocol::is_valid_transfer_path_segment(name)
}

fn absolute_path(path: &Path) -> Result<PathBuf, TransferResult> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir().map_err(io_result)?.join(path))
    }
}

fn pin_directory_chain(path: &Path) -> Result<Vec<File>, TransferResult> {
    directory_chain_paths(path)?
        .iter()
        .map(|path| open_directory_pin(path))
        .collect()
}

fn directory_chain_paths(path: &Path) -> Result<Vec<PathBuf>, TransferResult> {
    let mut current = PathBuf::new();
    let mut paths = Vec::new();
    for component in path.components() {
        match component {
            // A verbatim drive prefix reports has_root(), but `\\?\C:`
            // itself is not an openable filesystem root. Wait for RootDir.
            Component::Prefix(_) => current.push(component.as_os_str()),
            Component::RootDir => {
                current.push(component.as_os_str());
                paths.push(current.clone());
            }
            Component::Normal(name) => {
                current.push(name);
                paths.push(current.clone());
            }
            Component::CurDir => {}
            Component::ParentDir => return Err(TransferResult::InvalidData),
        }
    }
    if paths.is_empty() || !path.is_absolute() {
        Err(TransferResult::InvalidData)
    } else {
        Ok(paths)
    }
}

fn open_directory_pin(path: &Path) -> Result<File, TransferResult> {
    let metadata = fs::symlink_metadata(path).map_err(io_result)?;
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
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
    if is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err(TransferResult::PermissionDenied);
    }
    Ok(file)
}

fn open_source_file(path: &Path) -> Result<File, TransferResult> {
    let metadata = fs::symlink_metadata(path).map_err(io_result)?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(TransferResult::Unsupported);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
        options
            .share_mode(FILE_SHARE_READ.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
    }
    let file = options.open(path).map_err(io_result)?;
    let metadata = file.metadata().map_err(io_result)?;
    if is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(TransferResult::Unsupported);
    }
    Ok(file)
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
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

#[cfg(windows)]
fn ensure_sibling_absent(parent: &Path, name: &str) -> Result<(), TransferResult> {
    if matching_sibling(parent, name)?.is_some() {
        Err(TransferResult::Busy)
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn ensure_sibling_absent(parent: &Path, name: &str) -> Result<(), TransferResult> {
    let collision_key = name.to_uppercase();
    for entry in fs::read_dir(parent).map_err(io_result)? {
        let entry = entry.map_err(io_result)?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|existing| existing.to_uppercase() == collision_key)
        {
            return Err(TransferResult::Busy);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn ensure_exact_sibling(parent: &Path, name: &str) -> Result<(), TransferResult> {
    if matching_sibling(parent, name)?.as_deref() == Some(name) {
        Ok(())
    } else {
        Err(TransferResult::InvalidData)
    }
}

#[cfg(not(windows))]
fn ensure_exact_sibling(parent: &Path, name: &str) -> Result<(), TransferResult> {
    let collision_key = name.to_uppercase();
    for entry in fs::read_dir(parent).map_err(io_result)? {
        let entry = entry.map_err(io_result)?;
        if let Some(existing) = entry.file_name().to_str()
            && existing.to_uppercase() == collision_key
        {
            return if existing == name {
                Ok(())
            } else {
                Err(TransferResult::InvalidData)
            };
        }
    }
    Err(TransferResult::InvalidData)
}

#[cfg(windows)]
fn matching_sibling(parent: &Path, name: &str) -> Result<Option<String>, TransferResult> {
    use windows::{
        Win32::{
            Foundation::ERROR_FILE_NOT_FOUND,
            Storage::FileSystem::{FindClose, FindFirstFileW, WIN32_FIND_DATAW},
        },
        core::HSTRING,
    };

    // canonicalize supplies the verbatim long-path prefix. The parent is held
    // by non-reparse directory pins for this lookup and the following create.
    // No wildcard is possible after relative-path validation, so Windows does
    // one name lookup instead of enumerating every sibling for every entry.
    let pattern = fs::canonicalize(parent).map_err(io_result)?.join(name);
    let pattern = HSTRING::from(pattern.as_os_str());
    let mut data = WIN32_FIND_DATAW::default();
    let handle = match unsafe { FindFirstFileW(&pattern, &mut data) } {
        Ok(handle) => handle,
        Err(error) if error.code() == ERROR_FILE_NOT_FOUND.to_hresult() => return Ok(None),
        Err(_) => return Err(TransferResult::IoFailed),
    };
    unsafe { FindClose(handle) }.map_err(|_| TransferResult::IoFailed)?;
    let length = data
        .cFileName
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(data.cFileName.len());
    String::from_utf16(&data.cFileName[..length])
        .map(Some)
        .map_err(|_| TransferResult::InvalidData)
}

fn modified_at_unix_ns(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

fn check_cancelled(cancellation: &AtomicBool) -> Result<(), TransferResult> {
    if cancellation.load(Ordering::Acquire) {
        Err(TransferResult::Cancelled)
    } else {
        Ok(())
    }
}

fn io_result(error: std::io::Error) -> TransferResult {
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(32 | 33)) {
        return TransferResult::Busy;
    }
    match error.kind() {
        std::io::ErrorKind::AlreadyExists => TransferResult::Busy,
        std::io::ErrorKind::PermissionDenied => TransferResult::PermissionDenied,
        _ => TransferResult::IoFailed,
    }
}

fn is_staging_name(name: &str) -> bool {
    name.strip_prefix(PART_PREFIX).is_some_and(|suffix| {
        suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn hex_id(id: &[u8; 16]) -> String {
    id.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    struct Fixture(PathBuf, PathBuf);

    impl Fixture {
        fn new() -> Self {
            let mut id = [0; 16];
            OsRng.fill_bytes(&mut id);
            let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join(".build/directory-fixtures");
            fs::create_dir_all(&base).unwrap();
            let base = fs::canonicalize(base).unwrap();
            let path = base.join(format!("desklink-directory-{}", hex_id(&id)));
            fs::create_dir(&path).unwrap();
            Self(path, base)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let metadata = fs::symlink_metadata(&self.0).unwrap();
            assert!(!is_link_or_reparse(&metadata));
            let target = fs::canonicalize(&self.0).unwrap();
            assert_eq!(target.parent(), Some(self.1.as_path()));
            fs::remove_dir_all(target).unwrap();
        }
    }

    fn limits() -> DirectoryScanLimits {
        DirectoryScanLimits {
            max_entries: 10_000,
            max_total_bytes: 1 << 40,
            max_file_bytes: 1 << 40,
            max_depth: DIRECTORY_MAX_DEPTH,
            max_relative_path_bytes: DIRECTORY_MAX_RELATIVE_PATH_BYTES,
        }
    }

    fn scan(root: &Path) -> DirectoryScanner {
        DirectoryScanner::open_with_limits(root, Arc::new(AtomicBool::new(false)), limits())
            .unwrap()
    }

    #[test]
    fn relative_paths_reject_escape_device_names_ads_and_empty_segments() {
        for invalid in [
            "",
            "/root",
            "C:/root",
            "../escape",
            "dir/../escape",
            "a//b",
            "a\\b",
            "a/",
            "a/CON.txt",
            "NUL",
            "LPT².log",
            "COM¹",
            "CLOCK$",
            "file:stream",
            "a.",
            "a ",
            "a/\0b",
            "//server/share",
            "a/<bad>",
        ] {
            assert!(!is_valid_relative_path(invalid), "{invalid:?}");
        }
        assert!(is_valid_relative_path("资料/空目录"));
        assert!(is_valid_relative_path(&format!(
            "长目录/{}.txt",
            "中".repeat(200)
        )));
        assert!(!is_valid_relative_path(&"a/".repeat(DIRECTORY_MAX_DEPTH)));
        assert!(!is_valid_relative_path(&"中".repeat(256)));
    }

    #[test]
    fn scanner_yields_empty_directories_and_parent_before_children() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("空目录")).unwrap();
        fs::create_dir(fixture.0.join("资料")).unwrap();
        fs::write(fixture.0.join("资料/文件.txt"), "你好").unwrap();
        let entries = scan(&fixture.0).collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(entries.len(), 3);
        let parent = entries
            .iter()
            .position(|entry| entry.relative_path == "资料")
            .unwrap();
        let file = entries
            .iter()
            .position(|entry| entry.relative_path == "资料/文件.txt")
            .unwrap();
        assert!(parent < file);
        assert!(
            entries.iter().any(|entry| entry.relative_path == "空目录"
                && entry.kind == TransferEntryKind::Directory)
        );
        assert_eq!(entries[file].size, 6);
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.item_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn scanner_handles_long_paths_without_flattening_directory() {
        let fixture = Fixture::new();
        let mut path = fixture.0.clone();
        for number in 0..8 {
            path.push(format!("中文目录{number}{}", "层".repeat(12)));
            fs::create_dir(&path).unwrap();
        }
        fs::write(path.join("内容.bin"), [42]).unwrap();
        let mut scanner = scan(&fixture.0);
        let mut count = 0;
        for entry in scanner.by_ref() {
            let entry = entry.unwrap();
            count += 1;
            if entry.kind == TransferEntryKind::File {
                let mut source = entry.open_verified_source().unwrap();
                let mut content = Vec::new();
                source.file.read_to_end(&mut content).unwrap();
                assert_eq!(content, [42]);
            }
        }
        assert_eq!(count, 9);
        assert_eq!(scanner.total_bytes(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn canonical_verbatim_file_and_directory_sources_open_and_scan() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("中文文件.bin"), b"canonical source").unwrap();
        let canonical_root = fs::canonicalize(&fixture.0).unwrap();
        let canonical_file = fs::canonicalize(fixture.0.join("中文文件.bin")).unwrap();
        assert!(canonical_root.to_string_lossy().starts_with(r"\\?\"));
        let entry = DirectoryEntry::single_file(&canonical_file).unwrap();
        let mut source = entry.open_verified_source().unwrap();
        let mut content = Vec::new();
        source.file.read_to_end(&mut content).unwrap();
        assert_eq!(content, b"canonical source");
        drop(source);
        let entries = scan(&canonical_root)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].relative_path, "中文文件.bin");
        assert_eq!(entries[0].size, 16);
    }

    #[cfg(windows)]
    #[test]
    fn drive_and_unc_pin_steps_wait_for_rootdir_including_verbatim_prefixes() {
        // Structural UNC validation only: this fixture does not contact a share.
        for (path, root) in [
            (r"C:\folder", r"C:\"),
            (r"\\?\C:\folder", r"\\?\C:\"),
            (r"\\server\share\folder", r"\\server\share\"),
            (r"\\?\UNC\server\share\folder", r"\\?\UNC\server\share\"),
        ] {
            let paths = directory_chain_paths(Path::new(path)).unwrap();
            assert_eq!(paths, [PathBuf::from(root), PathBuf::from(path)], "{path}");
        }
    }

    #[test]
    fn large_directory_scanner_keeps_only_depth_handles_and_obeys_entry_budget() {
        let fixture = Fixture::new();
        for number in 0..1000 {
            fs::write(fixture.0.join(format!("file-{number}")), []).unwrap();
        }
        let mut budget = limits();
        budget.max_entries = 7;
        let mut scanner = DirectoryScanner::open_with_limits(
            &fixture.0,
            Arc::new(AtomicBool::new(false)),
            budget,
        )
        .unwrap();
        for _ in 0..7 {
            assert!(scanner.next().unwrap().is_ok());
            assert_eq!(scanner.active_directory_handles(), 1);
        }
        let failure = scanner.next().unwrap().unwrap_err();
        assert_eq!(failure.result, TransferResult::TooLarge);
        assert_eq!(failure.scope, DirectoryScanFailureScope::Task);
        assert!(failure.terminal);
        assert!(failure.relative_path.is_none());
        assert!(failure.retry_relative_path.is_none());
        assert!(scanner.next().is_none());
        assert_eq!(scanner.active_directory_handles(), 0);
    }

    #[test]
    fn scanner_limits_sparse_file_size_without_narrowing_to_u32() {
        let fixture = Fixture::new();
        let file = File::create(fixture.0.join("large.bin")).unwrap();
        let size = u64::from(u32::MAX) + 1024;
        file.set_len(size).unwrap();
        drop(file);
        let entry = scan(&fixture.0).next().unwrap().unwrap();
        assert_eq!(entry.size, size);
        let mut budget = limits();
        budget.max_file_bytes = size - 1;
        let failure = DirectoryScanner::open_with_limits(
            &fixture.0,
            Arc::new(AtomicBool::new(false)),
            budget,
        )
        .unwrap()
        .next()
        .unwrap()
        .unwrap_err();
        assert_eq!(failure.result, TransferResult::TooLarge);
        assert_eq!(failure.scope, DirectoryScanFailureScope::Entry);
        assert_eq!(failure.kind, Some(TransferEntryKind::File));
        assert_eq!(failure.size, Some(size));
        assert!(!failure.terminal);
    }

    #[test]
    fn cumulative_byte_budget_skips_one_file_and_keeps_fitting_siblings() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("large-a"), [1; 4]).unwrap();
        fs::write(fixture.0.join("large-b"), [2; 4]).unwrap();
        fs::write(fixture.0.join("small"), [3; 1]).unwrap();
        let mut budget = limits();
        budget.max_file_bytes = 4;
        budget.max_total_bytes = 5;
        let mut scanner = DirectoryScanner::open_with_limits(
            &fixture.0,
            Arc::new(AtomicBool::new(false)),
            budget,
        )
        .unwrap();
        let mut success_bytes = 0;
        let mut successes = 0;
        let mut failures = 0;
        for outcome in scanner.by_ref() {
            match outcome {
                Ok(entry) => {
                    success_bytes += entry.size;
                    successes += 1;
                }
                Err(failure) => {
                    failures += 1;
                    assert_eq!(failure.scope, DirectoryScanFailureScope::Entry);
                    assert_eq!(failure.result, TransferResult::TooLarge);
                    assert_eq!(failure.kind, Some(TransferEntryKind::File));
                    assert_eq!(failure.size, Some(4));
                    assert!(!failure.terminal);
                    assert_eq!(failure.relative_path, failure.retry_relative_path);
                }
            }
        }
        assert_eq!((successes, failures, success_bytes), (2, 1, 5));
        assert_eq!(scanner.total_bytes(), success_bytes);
        assert_eq!(scanner.scanned_entries(), 3);
    }

    #[test]
    fn subtree_retry_preserves_original_prefix_and_excludes_siblings() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.0.join("资料/失败子树/空目录")).unwrap();
        fs::write(fixture.0.join("资料/失败子树/文件.txt"), "恢复内容").unwrap();
        fs::write(fixture.0.join("资料/不要重发.txt"), "不相关").unwrap();
        let entries = DirectoryScanner::open_subtree_with_limits(
            &fixture.0,
            "资料/失败子树",
            true,
            Arc::new(AtomicBool::new(false)),
            limits(),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].relative_path, "资料/失败子树");
        assert_eq!(entries[0].kind, TransferEntryKind::Directory);
        assert!(
            entries
                .iter()
                .all(|entry| entry.relative_path == "资料/失败子树"
                    || entry.relative_path.starts_with("资料/失败子树/"))
        );
        let file = entries
            .iter()
            .find(|entry| entry.kind == TransferEntryKind::File)
            .unwrap();
        let mut source = file.open_verified_source().unwrap();
        let mut contents = String::new();
        source.file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "恢复内容");
        source.verify_location().unwrap();
    }

    #[test]
    fn enumeration_retry_omits_parent_and_retains_original_depth_limit() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.0.join("a/b/c")).unwrap();
        fs::write(fixture.0.join("a/sibling"), []).unwrap();
        let mut budget = limits();
        budget.max_depth = 2;
        let outcomes = DirectoryScanner::open_subtree_with_limits(
            &fixture.0,
            "a",
            false,
            Arc::new(AtomicBool::new(false)),
            budget,
        )
        .unwrap()
        .collect::<Vec<_>>();
        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.iter().any(|outcome| {
            outcome
                .as_ref()
                .is_ok_and(|entry| entry.relative_path == "a/sibling")
        }));
        assert!(!outcomes.iter().any(|outcome| {
            outcome
                .as_ref()
                .is_ok_and(|entry| entry.relative_path == "a")
        }));
        let failure = outcomes
            .iter()
            .find_map(|outcome| outcome.as_ref().err())
            .unwrap();
        assert_eq!(failure.scope, DirectoryScanFailureScope::Enumeration);
        assert_eq!(failure.relative_path.as_deref(), Some("a/b"));
        assert_eq!(failure.retry_relative_path.as_deref(), Some("a/b"));
        assert_eq!(failure.kind, Some(TransferEntryKind::Directory));
        assert_eq!(failure.size, Some(0));
        assert!(!failure.terminal);
        let encoded = serde_json::to_vec(failure).unwrap();
        assert_eq!(
            serde_json::from_slice::<DirectoryScanFailure>(&encoded).unwrap(),
            *failure
        );
    }

    #[test]
    fn subtree_retry_rejects_traversal_and_cancels_before_yielding_root() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("child")).unwrap();
        for prefix in [
            "../outside",
            "C:/outside",
            "/outside",
            "child/../outside",
            "child\\outside",
            "NUL",
            "child:stream",
        ] {
            assert!(
                matches!(
                    DirectoryScanner::open_subtree_with_limits(
                        &fixture.0,
                        prefix,
                        true,
                        Arc::new(AtomicBool::new(false)),
                        limits()
                    ),
                    Err(TransferResult::InvalidData)
                ),
                "{prefix}"
            );
        }
        assert!(matches!(
            DirectoryScanner::open_subtree_with_limits(
                &fixture.0,
                "",
                true,
                Arc::new(AtomicBool::new(false)),
                limits()
            ),
            Err(TransferResult::InvalidData)
        ));
        let cancellation = Arc::new(AtomicBool::new(false));
        let mut scanner = DirectoryScanner::open_subtree_with_limits(
            &fixture.0,
            "child",
            true,
            cancellation.clone(),
            limits(),
        )
        .unwrap();
        cancellation.store(true, Ordering::Release);
        let failure = scanner.next().unwrap().unwrap_err();
        assert!(failure.terminal);
        assert_eq!(failure.scope, DirectoryScanFailureScope::Task);
        assert_eq!(failure.result, TransferResult::Cancelled);
        assert!(failure.retry_relative_path.is_none());
        assert_eq!(scanner.active_directory_handles(), 0);
        assert!(scanner.next().is_none());
    }

    #[test]
    fn retry_can_prune_only_the_just_yielded_directory() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("changed-kind")).unwrap();
        fs::write(fixture.0.join("changed-kind/not-admitted"), []).unwrap();
        fs::write(fixture.0.join("sibling"), []).unwrap();
        let mut scanner = scan(&fixture.0);
        let mut yielded = Vec::new();
        while let Some(outcome) = scanner.next() {
            let entry = outcome.unwrap();
            yielded.push(entry.relative_path.clone());
            if entry.kind == TransferEntryKind::Directory {
                assert_eq!(
                    scanner.skip_current_subtree("unrelated"),
                    Err(TransferResult::InvalidData)
                );
                scanner.skip_current_subtree(&entry.relative_path).unwrap();
                assert_eq!(
                    scanner.skip_current_subtree(&entry.relative_path),
                    Err(TransferResult::InvalidData)
                );
            }
        }
        assert!(yielded.iter().any(|path| path == "changed-kind"));
        assert!(yielded.iter().any(|path| path == "sibling"));
        assert!(!yielded.iter().any(|path| path.ends_with("not-admitted")));

        let mut scanner = DirectoryScanner::open_subtree_with_limits(
            &fixture.0,
            "changed-kind",
            true,
            Arc::new(AtomicBool::new(false)),
            limits(),
        )
        .unwrap();
        let parent = scanner.next().unwrap().unwrap();
        let child = scanner.next().unwrap().unwrap();
        assert_eq!(parent.kind, TransferEntryKind::Directory);
        assert_eq!(child.kind, TransferEntryKind::File);
        assert_eq!(
            scanner.skip_current_subtree(&parent.relative_path),
            Err(TransferResult::InvalidData)
        );
    }

    #[test]
    fn directory_send_guard_allows_child_repairs_and_rejects_kind_replacement() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("directory");
        fs::create_dir(&directory).unwrap();
        let entry = scan(&fixture.0).next().unwrap().unwrap();
        fs::write(directory.join("repaired-child"), b"new child").unwrap();
        let guard = entry.verify_directory_source().unwrap();
        guard.verify_location().unwrap();
        #[cfg(windows)]
        assert!(fs::rename(&directory, fixture.0.join("moved")).is_err());
        drop(guard);
        fs::remove_file(directory.join("repaired-child")).unwrap();
        fs::remove_dir(&directory).unwrap();
        fs::write(&directory, b"now a file").unwrap();
        assert!(matches!(
            entry.verify_directory_source(),
            Err(TransferResult::SourceChanged)
        ));

        let file_entry = DirectoryEntry::single_file(&directory).unwrap();
        fs::remove_file(&directory).unwrap();
        fs::create_dir(&directory).unwrap();
        assert!(matches!(
            file_entry.open_verified_source(),
            Err(TransferResult::SourceChanged)
        ));
    }

    #[test]
    fn empty_source_root_guard_rejects_replacement_before_zero_entry_replay() {
        let fixture = Fixture::new();
        assert!(scan(&fixture.0).next().is_none());
        let guard = GuardedSourceDirectory::open(&fixture.0).unwrap();
        guard.verify_location().unwrap();
        let saved = fixture.0.with_extension("saved");
        assert_eq!(saved.parent(), Some(fixture.1.as_path()));
        #[cfg(windows)]
        assert!(fs::rename(&fixture.0, &saved).is_err());
        drop(guard);
        fs::rename(&fixture.0, &saved).unwrap();
        fs::write(&fixture.0, b"replacement file").unwrap();
        let replacement = GuardedSourceDirectory::open(&fixture.0);
        fs::remove_file(&fixture.0).unwrap();
        fs::rename(&saved, &fixture.0).unwrap();
        assert!(matches!(replacement, Err(TransferResult::SourceChanged)));
    }

    #[test]
    fn scanner_cancellation_releases_stack_and_is_terminal() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("child")).unwrap();
        fs::write(fixture.0.join("child/file"), []).unwrap();
        let cancellation = Arc::new(AtomicBool::new(false));
        let mut scanner =
            DirectoryScanner::open_with_limits(&fixture.0, cancellation.clone(), limits()).unwrap();
        assert!(scanner.next().unwrap().is_ok());
        assert_eq!(scanner.active_directory_handles(), 2);
        cancellation.store(true, Ordering::Release);
        assert_eq!(
            scanner.next().unwrap().unwrap_err().result,
            TransferResult::Cancelled
        );
        assert_eq!(scanner.active_directory_handles(), 0);
        assert!(scanner.next().is_none());
    }

    #[tokio::test]
    async fn scanner_queue_is_bounded_and_dropping_it_releases_source_lock() {
        let fixture = Fixture::new();
        for number in 0..20 {
            fs::write(fixture.0.join(format!("file-{number}")), []).unwrap();
        }
        let mut receiver = spawn_directory_scanner(
            fixture.0.clone(),
            Arc::new(AtomicBool::new(false)),
            limits(),
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(receiver.len() <= DIRECTORY_SCAN_QUEUE_CAPACITY);
        drop(receiver);
        // Worker termination, including all directory-handle drops, is eventual.
        let renamed = fixture.0.with_extension("released");
        for attempt in 0..100 {
            if fs::rename(&fixture.0, &renamed).is_ok() {
                fs::rename(&renamed, &fixture.0).unwrap();
                return;
            }
            assert!(
                attempt < 99,
                "scanner retained source lock after receiver drop"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[test]
    fn scanner_detects_replaced_source_before_open() {
        let fixture = Fixture::new();
        fs::write(fixture.0.join("file"), "before").unwrap();
        let entry = scan(&fixture.0).next().unwrap().unwrap();
        fs::remove_file(&entry.source_path).unwrap();
        fs::write(&entry.source_path, "after!").unwrap();
        assert!(matches!(
            entry.open_verified_source(),
            Err(TransferResult::SourceChanged)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn busy_source_file_keeps_siblings_and_can_be_retried_alone() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = Fixture::new();
        let blocked = fixture.0.join("暂时占用.bin");
        fs::write(&blocked, b"held").unwrap();
        fs::write(fixture.0.join("有效.bin"), b"good").unwrap();
        let owner = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&blocked)
            .unwrap();
        let mut budget = limits();
        budget.max_file_bytes = 8;
        budget.max_total_bytes = 8;
        let mut scanner = DirectoryScanner::open_with_limits(
            &fixture.0,
            Arc::new(AtomicBool::new(false)),
            budget,
        )
        .unwrap();
        let outcomes = scanner.by_ref().collect::<Vec<_>>();
        assert_eq!(outcomes.len(), 2);
        let failure = outcomes
            .iter()
            .find_map(|outcome| outcome.as_ref().err())
            .unwrap();
        assert_eq!(failure.result, TransferResult::Busy);
        assert_eq!(failure.scope, DirectoryScanFailureScope::Entry);
        assert_eq!(failure.kind, Some(TransferEntryKind::File));
        assert_eq!(failure.size, Some(4));
        assert_eq!(failure.relative_path.as_deref(), Some("暂时占用.bin"));
        assert!(!failure.terminal);
        assert_eq!(scanner.total_bytes(), 4);
        assert!(outcomes.iter().any(|outcome| {
            outcome
                .as_ref()
                .is_ok_and(|entry| entry.relative_path == "有效.bin")
        }));
        drop(scanner);
        drop(owner);
        let retried = DirectoryScanner::open_subtree_with_limits(
            &fixture.0,
            failure.retry_relative_path.as_deref().unwrap(),
            true,
            Arc::new(AtomicBool::new(false)),
            budget,
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].relative_path, "暂时占用.bin");
        let mut guarded = retried[0].open_verified_source().unwrap();
        let mut contents = Vec::new();
        guarded.file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"held");
        guarded.verify_location().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn busy_subtree_keeps_siblings_and_retry_discovers_only_its_children() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        let fixture = Fixture::new();
        let blocked = fixture.0.join("暂时占用目录");
        fs::create_dir(&blocked).unwrap();
        fs::write(blocked.join("恢复.txt"), b"child").unwrap();
        fs::write(fixture.0.join("有效.txt"), b"sibling").unwrap();
        let owner = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(&blocked)
            .unwrap();
        let outcomes = scan(&fixture.0).collect::<Vec<_>>();
        assert_eq!(outcomes.len(), 2);
        let failure = outcomes
            .iter()
            .find_map(|outcome| outcome.as_ref().err())
            .unwrap();
        assert_eq!(failure.result, TransferResult::Busy);
        assert_eq!(failure.scope, DirectoryScanFailureScope::Subtree);
        assert_eq!(failure.kind, Some(TransferEntryKind::Directory));
        assert_eq!(failure.size, Some(0));
        assert!(!failure.terminal);
        assert!(outcomes.iter().any(|outcome| {
            outcome
                .as_ref()
                .is_ok_and(|entry| entry.relative_path == "有效.txt")
        }));
        drop(owner);
        let retried = DirectoryScanner::open_subtree_with_limits(
            &fixture.0,
            failure.retry_relative_path.as_deref().unwrap(),
            true,
            Arc::new(AtomicBool::new(false)),
            limits(),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        assert_eq!(retried.len(), 2);
        assert_eq!(retried[0].relative_path, "暂时占用目录");
        assert_eq!(retried[1].relative_path, "暂时占用目录/恢复.txt");
    }

    #[test]
    fn destination_stages_flushes_publishes_and_preserves_empty_directory() {
        let fixture = Fixture::new();
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "收到的目录", limits()).unwrap();
        destination.create_directory("空目录").unwrap();
        destination.create_directory("资料").unwrap();
        let mut staged = destination.create_file("资料/文件.txt").unwrap();
        staged
            .file_mut()
            .unwrap()
            .write_all("你好".as_bytes())
            .unwrap();
        assert!(!staged.destination_path().exists());
        let final_path = staged.publish().unwrap();
        assert_eq!(fs::read(final_path).unwrap(), "你好".as_bytes());
        assert!(destination.root().join("空目录").is_dir());
        assert_eq!(
            fs::read_dir(destination.root().join("资料"))
                .unwrap()
                .count(),
            1
        );
    }

    #[test]
    fn publish_callback_owns_writer_until_after_rename_and_failure_preserves_data() {
        let fixture = Fixture::new();
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        let mut staged = destination.create_file("final.bin").unwrap();
        staged.file_mut().unwrap().write_all(b"verified").unwrap();
        let temporary = staged.staging_path().to_owned();
        let final_path = staged.destination_path().to_owned();
        let result = staged.publish_with(|file| {
            assert!(!temporary.exists());
            assert!(final_path.exists());
            assert_eq!(file.metadata().unwrap().len(), 8);
            #[cfg(windows)]
            assert!(matches!(
                native::open_file_path(&final_path, false),
                Err(TransferResult::Busy)
            ));
            Err(TransferResult::IoFailed)
        });
        assert_eq!(result, Err(TransferResult::IoFailed));
        assert_eq!(fs::read(&final_path).unwrap(), b"verified");
        assert!(!temporary.exists());
        #[cfg(windows)]
        assert!(native::open_file_path(&final_path, false).is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn owned_cache_rename_keeps_identity_after_its_old_name_is_reused() {
        let fixture = Fixture::new();
        let temporary = fixture.0.join("temporary");
        let first = fixture.0.join("first");
        let final_path = fixture.0.join("final");
        let existing = fixture.0.join("existing");
        fs::write(&existing, b"do not overwrite").unwrap();
        let mut file = native::create_file_path(&temporary).unwrap();
        file.write_all(b"owned metadata").unwrap();
        file.sync_all().unwrap();
        assert!(fs::remove_file(&temporary).is_err());
        native::rename_file_path(&file, &first, false).unwrap();
        assert!(!temporary.exists());
        fs::write(&temporary, b"replacement object").unwrap();
        assert_eq!(
            native::rename_file_path(&file, &existing, false),
            Err(TransferResult::Busy)
        );
        native::rename_file_path(&file, &final_path, false).unwrap();
        assert!(!first.exists());
        drop(file);
        assert_eq!(fs::read(&final_path).unwrap(), b"owned metadata");
        assert_eq!(fs::read(&temporary).unwrap(), b"replacement object");
        assert_eq!(fs::read(&existing).unwrap(), b"do not overwrite");
    }

    #[test]
    fn destination_never_overwrites_existing_roots_files_or_case_aliases() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.0.join("Existing")).unwrap();
        assert!(matches!(
            DirectoryDestination::create_new(&fixture.0, "existing", limits()),
            Err(TransferResult::Busy)
        ));
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        destination.create_directory("Child").unwrap();
        assert_eq!(
            destination.create_directory("child"),
            Err(TransferResult::Busy)
        );
        assert!(matches!(
            destination.create_file("child/file"),
            Err(TransferResult::InvalidData)
        ));
        fs::write(destination.root().join("original"), "untouched").unwrap();
        assert!(matches!(
            destination.create_file("ORIGINAL"),
            Err(TransferResult::Busy)
        ));
        assert_eq!(
            fs::read(destination.root().join("original")).unwrap(),
            b"untouched"
        );
    }

    #[test]
    fn racing_final_file_prevents_publication_without_overwrite() {
        let fixture = Fixture::new();
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        let mut staged = destination.create_file("final").unwrap();
        staged.file_mut().unwrap().write_all(b"new").unwrap();
        fs::write(staged.destination_path(), b"existing").unwrap();
        assert_eq!(staged.publish(), Err(TransferResult::Busy));
        assert_eq!(
            fs::read(destination.root().join("final")).unwrap(),
            b"existing"
        );
        assert_eq!(fs::read_dir(destination.root()).unwrap().count(), 1);
    }

    #[test]
    fn cancelled_old_guard_cannot_delete_new_workers_part() {
        let fixture = Fixture::new();
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        let old = destination.create_file("final").unwrap();
        let old_path = old.staging_path().to_owned();
        let new = destination.create_file("final").unwrap();
        let new_path = new.staging_path().to_owned();
        assert_ne!(old_path, new_path);
        drop(old);
        assert!(!old_path.exists());
        assert!(new_path.exists());
        drop(new);
        assert!(!new_path.exists());
    }

    #[test]
    fn preserve_retains_only_explicit_staging_file() {
        let fixture = Fixture::new();
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        let mut staged = destination.create_file("final").unwrap();
        staged.file_mut().unwrap().write_all(b"partial").unwrap();
        let path = staged.preserve().unwrap();
        assert_eq!(fs::read(path).unwrap(), b"partial");
        assert!(!destination.root().join("final").exists());
    }

    #[test]
    fn local_journal_round_trip_retains_source_identity_and_detects_change() {
        let fixture = Fixture::new();
        let path = fixture.0.join("文件.txt");
        fs::write(&path, b"original").unwrap();
        let entry = DirectoryEntry::single_file(&path).unwrap();
        let journal = serde_json::to_vec(&entry).unwrap();
        let entry: DirectoryEntry = serde_json::from_slice(&journal).unwrap();
        let mut source = entry.open_verified_source().unwrap();
        let mut bytes = Vec::new();
        source.file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original");
        drop(source);
        fs::write(&path, b"modified").unwrap();
        assert!(matches!(
            entry.open_verified_source(),
            Err(TransferResult::SourceChanged)
        ));
    }

    #[test]
    fn restarted_authorized_root_resumes_only_its_preserved_part() {
        let fixture = Fixture::new();
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        destination.create_directory("资料").unwrap();
        let mut staged = destination.create_file("资料/final").unwrap();
        staged.file_mut().unwrap().write_all(b"partial").unwrap();
        let staging_path = staged.preserve().unwrap();
        drop(destination);
        let mut destination =
            DirectoryDestination::open_existing(&fixture.0, "receive", limits()).unwrap();
        let mut resumed = destination
            .resume_file("资料/final", &staging_path)
            .unwrap();
        let mut bytes = Vec::new();
        resumed.file_mut().unwrap().read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"partial");
        resumed.file_mut().unwrap().write_all(b"-complete").unwrap();
        let final_path = resumed.publish().unwrap();
        assert_eq!(fs::read(final_path).unwrap(), b"partial-complete");
        assert!(!staging_path.exists());
    }

    #[test]
    fn publication_can_be_opened_under_pins_after_crash_before_journal_update() {
        let fixture = Fixture::new();
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        destination.create_directory("parent").unwrap();
        let mut staged = destination.create_file("parent/final").unwrap();
        staged
            .file_mut()
            .unwrap()
            .write_all(b"verified content")
            .unwrap();
        let staging_path = staged.staging_path().to_owned();
        staged.publish().unwrap();
        assert!(!staging_path.exists());
        drop(destination);
        let destination =
            DirectoryDestination::open_existing(&fixture.0, "receive", limits()).unwrap();
        let mut final_file = destination.open_existing_file("parent/final").unwrap();
        let mut bytes = Vec::new();
        final_file.file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"verified content");
        assert!(matches!(
            destination.open_existing_file("Parent/final"),
            Err(TransferResult::InvalidData)
        ));
    }

    #[test]
    fn resume_rejects_foreign_parts_and_unrecognized_names() {
        let fixture = Fixture::new();
        let foreign = Fixture::new();
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        let generated_name = format!("{PART_PREFIX}{}", "a".repeat(32));
        let foreign_part = foreign.0.join(&generated_name);
        fs::write(&foreign_part, b"foreign").unwrap();
        assert!(matches!(
            destination.resume_file("final", &foreign_part),
            Err(TransferResult::InvalidData)
        ));
        let arbitrary = destination.root().join("local-file");
        fs::write(&arbitrary, b"unrelated").unwrap();
        assert!(matches!(
            destination.resume_file("final", &arbitrary),
            Err(TransferResult::InvalidData)
        ));
        assert_eq!(fs::read(foreign_part).unwrap(), b"foreign");
        assert_eq!(fs::read(arbitrary).unwrap(), b"unrelated");
    }

    #[tokio::test]
    async fn scanner_cancel_unblocks_full_queue_without_receiver_drop() {
        let fixture = Fixture::new();
        for number in 0..20 {
            fs::write(fixture.0.join(format!("file-{number}")), []).unwrap();
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        let mut receiver =
            spawn_directory_scanner(fixture.0.clone(), cancellation.clone(), limits()).unwrap();
        for _ in 0..100 {
            if receiver.len() == DIRECTORY_SCAN_QUEUE_CAPACITY {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(receiver.len(), DIRECTORY_SCAN_QUEUE_CAPACITY);
        cancellation.store(true, Ordering::Release);
        let renamed = fixture.0.with_extension("cancelled");
        for attempt in 0..100 {
            if fs::rename(&fixture.0, &renamed).is_ok() {
                fs::rename(&renamed, &fixture.0).unwrap();
                while receiver.recv().await.is_some() {}
                return;
            }
            assert!(attempt < 99, "cancelled scanner retained directory handles");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[cfg(windows)]
    #[test]
    fn resume_is_busy_until_previous_worker_releases_owned_part() {
        let fixture = Fixture::new();
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        let staged = destination.create_file("final").unwrap();
        let staging_path = staged.staging_path().to_owned();
        assert!(matches!(
            destination.resume_file("final", &staging_path),
            Err(TransferResult::Busy)
        ));
        let staging_path = staged.preserve().unwrap();
        let resumed = destination.resume_file("final", &staging_path).unwrap();
        assert!(staging_path.exists());
        drop(resumed);
        assert!(!staging_path.exists());
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_rejected_for_scan_resume_and_destination_ancestors() {
        use std::process::Command;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::write(outside.0.join("private"), b"untouched").unwrap();
        let link = fixture.0.join("junction");
        let output = Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&link)
            .arg(&outside.0)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "junction fixture creation failed: {:?}",
            output
        );
        {
            assert_eq!(
                scan(&fixture.0).next().unwrap().unwrap_err().result,
                TransferResult::Unsupported
            );
            assert!(matches!(
                DirectoryScanner::open_subtree_with_limits(
                    &fixture.0,
                    "junction",
                    true,
                    Arc::new(AtomicBool::new(false)),
                    limits()
                ),
                Err(TransferResult::Unsupported)
            ));
            assert!(matches!(
                DirectoryScanner::open_subtree_with_limits(
                    &fixture.0,
                    "junction/private",
                    true,
                    Arc::new(AtomicBool::new(false)),
                    limits()
                ),
                Err(TransferResult::PermissionDenied)
            ));
            assert!(matches!(
                DirectoryDestination::create_new(&link, "escape", limits()),
                Err(TransferResult::PermissionDenied)
            ));
            let mut destination =
                DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
            let destination_link = destination.root().join("junction");
            let output = Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(&destination_link)
                .arg(&outside.0)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "destination junction fixture creation failed: {:?}",
                output
            );
            assert!(matches!(
                destination.create_file("junction/private"),
                Err(TransferResult::PermissionDenied)
            ));
            assert_eq!(fs::read(outside.0.join("private")).unwrap(), b"untouched");
            fs::remove_dir(&destination_link).unwrap();
        }
        fs::remove_dir(&link).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_destination_pins_prevent_ancestor_rename_while_worker_owns_file() {
        let fixture = Fixture::new();
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        destination.create_directory("parent").unwrap();
        let staged = destination.create_file("parent/file").unwrap();
        let parent = destination.root().join("parent");
        let moved = destination.root().join("moved");
        assert!(fs::rename(&parent, &moved).is_err());
        drop(staged);
        fs::rename(&parent, &moved).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_entries_and_destinations_are_rejected_without_following_them() {
        use std::os::unix::fs::symlink;
        let fixture = Fixture::new();
        let outside = Fixture::new();
        fs::write(outside.0.join("private"), b"untouched").unwrap();
        symlink(&outside.0, fixture.0.join("link")).unwrap();
        let failure = scan(&fixture.0).next().unwrap().unwrap_err();
        assert_eq!(failure.result, TransferResult::Unsupported);
        let mut destination =
            DirectoryDestination::create_new(&fixture.0, "receive", limits()).unwrap();
        symlink(&outside.0, destination.root().join("escape")).unwrap();
        assert!(matches!(
            destination.create_file("escape/private"),
            Err(TransferResult::PermissionDenied)
        ));
        assert_eq!(fs::read(outside.0.join("private")).unwrap(), b"untouched");
    }
}
