//! A peer-scoped file task actor. All filesystem work, hashing, prompts and
//! checkpoints run on one bounded disk worker, independently of media/input.
use crate::transfer_checkpoint::{
    CheckpointLedger, FILE_BLOCK_BYTES, SourceIndex, source_index_bytes,
};
use crate::transfer_directory::{
    DirectoryDestination, DirectoryEntry, DirectoryScanFailure, DirectoryScanFailureScope,
    DirectoryScanLimits, DirectoryScanner, GuardedDestinationFile, GuardedSourceDirectory,
};
use crate::transfer_task_catalog::TaskCatalog;
use crate::transfer_task_recovery::{restore_progress, summarize_progress};
use desklink_protocol::{
    FileTaskAction, FileTaskMessage, FileTaskPhase, FileTransferCapabilities,
    MAX_FILE_TASK_SCAN_WORK_PASSES, ManifestEntry, ResumeCheckpoint, TransferEntryKind, TransferId,
    TransferResult,
};
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    collections::VecDeque,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc as sync,
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

#[path = "transfer_task_manifest.rs"]
mod manifest;
use manifest::{ManifestFailure, ManifestRecord, ManifestSpool};
#[path = "transfer_manifest_index.rs"]
mod manifest_index;
use manifest_index::ManifestPathIndex;

const COMMAND_CAPACITY: usize = 2;
const MAX_CACHE_JSON_BYTES: usize = 128 * 1024;
const MAX_MANIFEST_JSON_BYTES: usize = 256 * 1024;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

pub fn id_text(id: TransferId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}
fn number<S: serde::Serializer>(n: &u64, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&n.to_string())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTaskItem {
    pub id: String,
    pub path: String,
    pub state: &'static str,
    pub kind: &'static str,
    #[serde(serialize_with = "number")]
    pub total_bytes: u64,
    #[serde(serialize_with = "number")]
    pub sent_bytes: u64,
    #[serde(serialize_with = "number")]
    pub acknowledged_bytes: u64,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTaskProgress {
    #[serde(skip)]
    pub source_admitted: bool,
    pub task_id: String,
    #[serde(serialize_with = "number")]
    pub revision: u64,
    pub direction: &'static str,
    pub name: String,
    pub state: &'static str,
    pub scan_complete: bool,
    pub scanned_items: u64,
    #[serde(serialize_with = "number")]
    pub total_bytes: u64,
    #[serde(serialize_with = "number")]
    pub sent_bytes: u64,
    #[serde(serialize_with = "number")]
    pub acknowledged_bytes: u64,
    pub completed_items: u64,
    pub failed_items: u64,
    pub active_item: Option<FileTaskItem>,
    pub items: VecDeque<FileTaskItem>,
    pub message: Option<String>,
}

impl FileTaskProgress {
    fn new(id: TransferId, name: String, direction: &'static str) -> Self {
        Self {
            source_admitted: false,
            task_id: id_text(id),
            revision: 0,
            direction,
            name,
            state: "scanning",
            scan_complete: false,
            scanned_items: 0,
            total_bytes: 0,
            sent_bytes: 0,
            acknowledged_bytes: 0,
            completed_items: 0,
            failed_items: 0,
            active_item: None,
            items: VecDeque::new(),
            message: None,
        }
    }
    fn item(&mut self, entry: &ManifestEntry, state: &'static str) {
        self.active_item = Some(FileTaskItem {
            id: entry.item_id.to_string(),
            path: entry.relative_path.clone(),
            state,
            kind: if entry.kind == TransferEntryKind::File {
                "file"
            } else {
                "directory"
            },
            total_bytes: entry.size,
            sent_bytes: 0,
            acknowledged_bytes: 0,
            message: None,
        });
        self.state = state;
    }
    fn finish_item(&mut self, result: TransferResult) {
        if result == TransferResult::Completed {
            self.completed_items += 1;
        } else {
            self.failed_items += 1;
        }
        if let Some(mut item) = self.active_item.take() {
            item.state = if result == TransferResult::Completed {
                "completed"
            } else {
                "failed"
            };
            if result != TransferResult::Completed {
                item.message = Some(result_message(result).to_owned());
            } else {
                item.message = None;
            }
            if self.items.len() == 64 {
                self.items.pop_front();
            }
            self.items.push_back(item);
        }
    }
}

#[derive(Clone)]
pub struct FileTaskCancellation {
    pub cancelled: Arc<AtomicBool>,
    pub stopped: Arc<AtomicBool>,
}
impl FileTaskCancellation {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire)
    }
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire)
    }
}

pub type FileTaskReceiveAuthorization =
    dyn Fn(&str, bool, FileTaskCancellation) -> bool + Send + Sync;
pub type FileTaskSourceSelection =
    dyn Fn(bool, FileTaskCancellation) -> Result<Option<PathBuf>, TransferResult> + Send + Sync;
pub type FileTaskSourceResumeAuthorization =
    dyn Fn(&Path, bool, FileTaskCancellation) -> bool + Send + Sync;

pub struct FileTaskOptions {
    /// Must be derived from the authenticated remote identity, never its label.
    pub peer_key: String,
    pub cache_root: PathBuf,
    pub receive_parent: PathBuf,
    pub authorize: Arc<FileTaskReceiveAuthorization>,
    pub select: Option<Arc<FileTaskSourceSelection>>,
    pub authorize_source_resume: Option<Arc<FileTaskSourceResumeAuthorization>>,
}

enum Command {
    Start(PathBuf, TransferId),
    StartQueued(crate::transfer::OutgoingFile),
    Retry(TransferId, Option<u64>),
    CancelStored(TransferId),
    Wire(FileTaskMessage),
}
struct Shared {
    stopped: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    action: AtomicU8,
    active: Mutex<Option<TransferId>>,
    controls: watch::Sender<Option<(TransferId, FileTaskAction)>>,
    pending_request: AtomicU64,
    active_request: AtomicU64,
    selector_request: AtomicU64,
    cancelled_selection: AtomicU64,
    pending_retry: Mutex<Option<TransferId>>,
    cancelled_retry: Mutex<Option<TransferId>>,
    peer_capabilities: Mutex<Option<FileTransferCapabilities>>,
}
impl Drop for Shared {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.cancelled.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct FileTaskHandle {
    commands: sync::SyncSender<Command>,
    shared: Arc<Shared>,
    requests: mpsc::Sender<FileTaskMessage>,
}
impl FileTaskHandle {
    fn enqueue(&self, c: Command) -> Result<(), TransferResult> {
        self.commands.try_send(c).map_err(|e| match e {
            sync::TrySendError::Full(_) => TransferResult::Busy,
            sync::TrySendError::Disconnected(_) => TransferResult::IoFailed,
        })
    }
    pub fn start(&self, path: PathBuf, id: TransferId) -> Result<(), TransferResult> {
        if id == [0; 16] {
            return Err(TransferResult::InvalidData);
        }
        self.admit(Command::Start(path, id), id)
    }
    pub fn start_queued(&self, file: crate::transfer::OutgoingFile) -> Result<(), TransferResult> {
        if file.transfer_id == [0; 16] {
            return Err(TransferResult::InvalidData);
        }
        let id = file.transfer_id;
        self.admit(Command::StartQueued(file), id)
    }
    pub fn is_busy(&self) -> bool {
        self.shared
            .active
            .lock()
            .map_or(true, |active| active.is_some())
            || self.shared.pending_request.load(Ordering::Acquire) != 0
            || self.shared.selector_request.load(Ordering::Acquire) != 0
            || self
                .shared
                .pending_retry
                .lock()
                .map_or(true, |pending| pending.is_some())
    }
    pub fn retry(&self, id: TransferId, item: Option<u64>) -> Result<(), TransferResult> {
        self.admit(Command::Retry(id, item), id)
    }
    fn admit(&self, command: Command, id: TransferId) -> Result<(), TransferResult> {
        if id == [0; 16] {
            return Err(TransferResult::InvalidData);
        }
        let mut active = self
            .shared
            .active
            .lock()
            .map_err(|_| TransferResult::IoFailed)?;
        if active.is_some()
            || self.shared.pending_request.load(Ordering::Acquire) != 0
            || self.shared.selector_request.load(Ordering::Acquire) != 0
            || self
                .shared
                .pending_retry
                .lock()
                .map_err(|_| TransferResult::IoFailed)?
                .is_some()
        {
            return Err(TransferResult::Busy);
        }
        self.shared.action.store(0, Ordering::Release);
        self.shared.cancelled.store(false, Ordering::Release);
        self.shared.active_request.store(0, Ordering::Release);
        *active = Some(id);
        if let Err(e) = self.enqueue(command) {
            *active = None;
            return Err(e);
        }
        Ok(())
    }
    pub fn action(&self, id: TransferId, action: FileTaskAction) -> Result<(), TransferResult> {
        let active = *self
            .shared
            .active
            .lock()
            .map_err(|_| TransferResult::IoFailed)?;
        if active.is_none() && action == FileTaskAction::Resume {
            return self.retry(id, None);
        }
        if active.is_none() && action == FileTaskAction::Cancel {
            let mut pending = self
                .shared
                .pending_retry
                .lock()
                .map_err(|_| TransferResult::IoFailed)?;
            if *pending == Some(id) {
                *pending = None;
                *self
                    .shared
                    .cancelled_retry
                    .lock()
                    .map_err(|_| TransferResult::IoFailed)? = Some(id);
                self.shared.controls.send_replace(Some((id, action)));
            }
            drop(pending);
            return self.admit(Command::CancelStored(id), id);
        }
        if active != Some(id) {
            return Err(TransferResult::InvalidData);
        }
        self.apply_action(action);
        self.shared.controls.send_replace(Some((id, action)));
        Ok(())
    }
    fn apply_action(&self, action: FileTaskAction) {
        if action == FileTaskAction::Cancel {
            self.shared.cancelled.store(true, Ordering::Release);
        }
        self.shared.action.store(
            match action {
                FileTaskAction::Pause => 1,
                FileTaskAction::Resume => 0,
                FileTaskAction::Cancel => 2,
            },
            Ordering::Release,
        );
    }
    pub fn request_remote(&self, request_id: u64, directory: bool) -> Result<(), TransferResult> {
        if request_id == 0 {
            return Err(TransferResult::InvalidData);
        }
        let active = self
            .shared
            .active
            .lock()
            .map_err(|_| TransferResult::IoFailed)?;
        if active.is_some()
            || self.shared.selector_request.load(Ordering::Acquire) != 0
            || self
                .shared
                .pending_retry
                .lock()
                .map_err(|_| TransferResult::IoFailed)?
                .is_some()
        {
            return Err(TransferResult::Busy);
        }
        self.shared
            .pending_request
            .compare_exchange(0, request_id, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| TransferResult::Busy)?;
        if let Err(error) = self.requests.try_send(FileTaskMessage::SelectionRequest {
            request_id,
            directory,
        }) {
            let _ = self.shared.pending_request.compare_exchange(
                request_id,
                0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => TransferResult::Busy,
                mpsc::error::TrySendError::Closed(_) => TransferResult::IoFailed,
            });
        }
        Ok(())
    }
    pub fn cancel_remote(&self) -> Result<bool, TransferResult> {
        // Serialize with Offer admission: consuming a request and assigning its
        // active task must be one critical section from cancellation's view.
        let active = self
            .shared
            .active
            .lock()
            .map_err(|_| TransferResult::IoFailed)?;
        let request_id = self.shared.pending_request.load(Ordering::Acquire);
        if request_id == 0 {
            if self.shared.active_request.load(Ordering::Acquire) != 0
                && let Some(task_id) = *active
            {
                self.apply_action(FileTaskAction::Cancel);
                self.shared
                    .controls
                    .send_replace(Some((task_id, FileTaskAction::Cancel)));
                return Ok(true);
            }
            return Ok(false);
        }
        self.requests
            .try_send(FileTaskMessage::SelectionCancel { request_id })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TransferResult::Busy,
                mpsc::error::TrySendError::Closed(_) => TransferResult::IoFailed,
            })?;
        // A full/closed output queue leaves the request cancellable. A peer's
        // terminal SelectionResult may have consumed it meanwhile; no new
        // local request can enter while the active mutex is held here.
        let _ = self.shared.pending_request.compare_exchange(
            request_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        Ok(true)
    }
    pub fn ingest(&self, message: FileTaskMessage) -> Result<(), TransferResult> {
        if !message.is_valid() {
            return Err(TransferResult::InvalidData);
        }
        if let FileTaskMessage::Capabilities(capabilities) = &message {
            let mut peer = self
                .shared
                .peer_capabilities
                .lock()
                .map_err(|_| TransferResult::IoFailed)?;
            if peer.is_some() {
                return Ok(());
            }
            let capabilities = *capabilities;
            self.enqueue(Command::Wire(message))?;
            *peer = Some(capabilities);
            return Ok(());
        }
        if let FileTaskMessage::SelectionCancel { request_id } = &message {
            if self.shared.selector_request.load(Ordering::Acquire) == *request_id {
                self.shared
                    .cancelled_selection
                    .store(*request_id, Ordering::Release);
                self.apply_action(FileTaskAction::Cancel);
            }
            return Ok(());
        }
        if let FileTaskMessage::SelectionRequest { request_id, .. } = &message {
            let active = self
                .shared
                .active
                .lock()
                .map_err(|_| TransferResult::IoFailed)?;
            if active.is_some()
                || self
                    .shared
                    .pending_retry
                    .lock()
                    .map_err(|_| TransferResult::IoFailed)?
                    .is_some()
                || self
                    .shared
                    .selector_request
                    .compare_exchange(0, *request_id, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
            {
                return self
                    .requests
                    .try_send(FileTaskMessage::SelectionResult {
                        request_id: *request_id,
                        result: TransferResult::Busy,
                    })
                    .map_err(|_| TransferResult::Busy);
            }
            self.shared.cancelled_selection.store(0, Ordering::Release);
            if let Err(error) = self.enqueue(Command::Wire(message)) {
                self.shared.selector_request.store(0, Ordering::Release);
                return Err(error);
            }
            return Ok(());
        }
        if let FileTaskMessage::RetryRequest { task_id, .. } = &message {
            let mut active = self
                .shared
                .active
                .lock()
                .map_err(|_| TransferResult::IoFailed)?;
            if active.is_some() || self.shared.selector_request.load(Ordering::Acquire) != 0 {
                return self
                    .requests
                    .try_send(FileTaskMessage::RetryResult {
                        task_id: *task_id,
                        result: TransferResult::Busy,
                    })
                    .map_err(|_| TransferResult::Busy);
            }
            self.shared.action.store(0, Ordering::Release);
            self.shared.cancelled.store(false, Ordering::Release);
            *active = Some(*task_id);
            if let Err(error) = self.enqueue(Command::Wire(message)) {
                *active = None;
                return Err(error);
            }
            return Ok(());
        }
        if let FileTaskMessage::RetryResult { task_id, .. } = &message {
            let id = *task_id;
            let mut pending = self
                .shared
                .pending_retry
                .lock()
                .map_err(|_| TransferResult::IoFailed)?;
            if *pending != Some(*task_id) {
                return Err(TransferResult::InvalidData);
            }
            *pending = None;
            let result = self.enqueue(Command::Wire(message));
            if result.is_err() {
                *pending = Some(id);
            }
            return result;
        }
        if let FileTaskMessage::Action { task_id, action } = message {
            if *self
                .shared
                .active
                .lock()
                .map_err(|_| TransferResult::IoFailed)?
                == Some(task_id)
            {
                self.apply_action(action);
            }
            return Ok(());
        }
        if let FileTaskMessage::SelectionResult { request_id, .. } = &message {
            if self
                .shared
                .pending_request
                .compare_exchange(*request_id, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return Err(TransferResult::InvalidData);
            }
            return Ok(());
        }
        if let FileTaskMessage::Offer {
            task_id,
            request_id,
            ..
        } = &message
        {
            if *self
                .shared
                .cancelled_retry
                .lock()
                .map_err(|_| TransferResult::IoFailed)?
                == Some(*task_id)
            {
                return self
                    .requests
                    .try_send(FileTaskMessage::Decision {
                        task_id: *task_id,
                        result: TransferResult::Rejected,
                        budget: FileTransferCapabilities::WINDOWS,
                    })
                    .map_err(|_| TransferResult::Busy);
            }
            let mut active = self
                .shared
                .active
                .lock()
                .map_err(|_| TransferResult::IoFailed)?;
            if request_id.is_some_and(|request_id| {
                self.shared
                    .pending_request
                    .compare_exchange(request_id, 0, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
            }) {
                return self
                    .requests
                    .try_send(FileTaskMessage::Decision {
                        task_id: *task_id,
                        result: TransferResult::Rejected,
                        budget: FileTransferCapabilities::WINDOWS,
                    })
                    .map_err(|_| TransferResult::Busy);
            }
            let mut pending_retry = self
                .shared
                .pending_retry
                .lock()
                .map_err(|_| TransferResult::IoFailed)?;
            if active.is_some()
                || self.shared.selector_request.load(Ordering::Acquire) != 0
                || pending_retry.is_some_and(|id| id != *task_id)
            {
                return self
                    .requests
                    .try_send(FileTaskMessage::Decision {
                        task_id: *task_id,
                        result: TransferResult::Busy,
                        budget: FileTransferCapabilities::WINDOWS,
                    })
                    .map_err(|_| TransferResult::Busy);
            }
            if *pending_retry == Some(*task_id) {
                *pending_retry = None;
            }
            self.shared.action.store(0, Ordering::Release);
            self.shared.cancelled.store(false, Ordering::Release);
            self.shared
                .active_request
                .store(request_id.unwrap_or(0), Ordering::Release);
            *active = Some(*task_id);
            if let Err(error) = self.enqueue(Command::Wire(message)) {
                *active = None;
                self.shared.active_request.store(0, Ordering::Release);
                return Err(error);
            }
            return Ok(());
        }
        self.enqueue(Command::Wire(message))
    }
}

pub struct FileTaskService {
    pub handle: FileTaskHandle,
    pub outgoing: mpsc::Receiver<FileTaskMessage>,
    pub progress: watch::Receiver<Option<FileTaskProgress>>,
    pub controls: watch::Receiver<Option<(TransferId, FileTaskAction)>>,
}

impl FileTaskService {
    pub fn spawn(options: FileTaskOptions) -> Result<Self, TransferResult> {
        if options.peer_key.is_empty()
            || options.peer_key.len() > 128
            || !options.peer_key.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(TransferResult::InvalidData);
        }
        let permit = crate::transfer::IncomingDiskWorkerPermit::acquire()?;
        let (commands, pending) = sync::sync_channel(COMMAND_CAPACITY);
        let (send, outgoing) = mpsc::channel(2);
        let (progress_tx, progress) = watch::channel(None);
        let stopped = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (controls_tx, controls) = watch::channel(None);
        let shared = Arc::new(Shared {
            stopped: stopped.clone(),
            cancelled: cancelled.clone(),
            action: AtomicU8::new(0),
            active: Mutex::new(None),
            controls: controls_tx,
            pending_request: AtomicU64::new(0),
            active_request: AtomicU64::new(0),
            selector_request: AtomicU64::new(0),
            cancelled_selection: AtomicU64::new(0),
            pending_retry: Mutex::new(None),
            cancelled_retry: Mutex::new(None),
            peer_capabilities: Mutex::new(None),
        });
        let weak = Arc::downgrade(&shared);
        let requests = send.clone();
        std::thread::Builder::new()
            .name("desklink-file-task-v14".into())
            .spawn(move || {
                let _permit = permit;
                let mut worker = Worker {
                    options,
                    pending,
                    send,
                    progress_tx,
                    stopped,
                    cancelled,
                    shared: weak,
                    peer: None,
                    current: None,
                    pending_request: None,
                    last_emission: None,
                    revision: 0,
                    catalog: None,
                    responding_retry: None,
                    last_activity: None,
                    activity_error: None,
                    scan_record_work: 0,
                };
                worker.run();
            })
            .map_err(|_| TransferResult::IoFailed)?;
        Ok(Self {
            handle: FileTaskHandle {
                commands,
                shared,
                requests,
            },
            outgoing,
            progress,
            controls,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct SourceTask {
    path: PathBuf,
    name: String,
    directory: bool,
    #[serde(default)]
    expected_size: Option<u64>,
    #[serde(default)]
    expected_modified_at_unix_ns: Option<u64>,
}
#[derive(Serialize, Deserialize)]
struct ReceivedTask {
    name: String,
    directory: bool,
    root_name: String,
}
#[derive(Serialize, Deserialize)]
struct ReceivedEntry {
    entry: ManifestEntry,
    staging: Option<PathBuf>,
    completed: bool,
}

struct Worker {
    options: FileTaskOptions,
    pending: sync::Receiver<Command>,
    send: mpsc::Sender<FileTaskMessage>,
    progress_tx: watch::Sender<Option<FileTaskProgress>>,
    stopped: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    shared: std::sync::Weak<Shared>,
    peer: Option<FileTransferCapabilities>,
    current: Option<FileTaskProgress>,
    pending_request: Option<u64>,
    last_emission: Option<ProgressEmission>,
    revision: u64,
    catalog: Option<TaskCatalog>,
    responding_retry: Option<TransferId>,
    last_activity: Option<SentActivity>,
    activity_error: Option<TransferResult>,
    scan_record_work: u64,
}

struct SentActivity {
    at: Instant,
    task_id: TransferId,
    item_id: Option<u64>,
    phase: FileTaskPhase,
    completed: u64,
}

#[derive(Clone, Copy)]
struct ActivityPoint {
    item_id: Option<u64>,
    completed: u64,
    total: u64,
}

struct ProgressEmission {
    at: Instant,
    task_id: String,
    state: &'static str,
    active: Option<(String, &'static str)>,
    scan_complete: bool,
}
impl Worker {
    fn cancellation(&self) -> FileTaskCancellation {
        FileTaskCancellation {
            cancelled: self.cancelled.clone(),
            stopped: self.stopped.clone(),
        }
    }
    fn restore(&mut self) {
        let Ok(catalog) = TaskCatalog::open(&self.options.cache_root, &self.options.peer_key)
        else {
            return;
        };
        let summary = catalog.load_last();
        self.catalog = Some(catalog);
        if let Ok(Some(summary)) = summary {
            self.current = Some(restore_progress(&summary));
            self.emit();
        }
    }
    fn persist_required(&self) -> Result<(), TransferResult> {
        (|| -> Result<(), TransferResult> {
            let Some(p) = self.current.as_ref() else {
                return Ok(());
            };
            let id = parse_id(&p.task_id)?;
            let directory;
            let source;
            if p.direction == "upload" {
                let metadata = self.task_path(id).join("source.json");
                if !metadata.exists() {
                    return Ok(());
                }
                let meta: SourceTask = read_json(&metadata)?;
                directory = meta.directory;
                source = Some(meta.path);
            } else {
                let metadata = self.task_path(id).join("receive.json");
                if !metadata.exists() {
                    return Ok(());
                }
                let meta: ReceivedTask = read_json(&metadata)?;
                directory = meta.directory;
                source = None;
            }
            let summary = summarize_progress(p, id, source, directory)?;
            self.catalog
                .as_ref()
                .ok_or(TransferResult::IoFailed)?
                .persist(&summary)
        })()
    }
    fn persist(&mut self) {
        let result = self.persist_required();
        if result.is_err()
            && let Some(p) = self.current.as_mut()
        {
            let note = "恢复记录未能保存；当前任务仍可继续，请保持本次连接。";
            if !p
                .message
                .as_ref()
                .is_some_and(|message| message.contains(note))
            {
                p.message = Some(match p.message.take() {
                    Some(message) => format!("{message} {note}"),
                    None => note.into(),
                });
            }
        }
    }
    fn selected_retry_allowed(
        &self,
        id: TransferId,
        item: Option<u64>,
    ) -> Result<(), TransferResult> {
        let Some(item) = item else {
            return Ok(());
        };
        let current = self.current.as_ref().is_some_and(|p| {
            p.task_id == id_text(id)
                && p.items
                    .iter()
                    .any(|entry| entry.id == item.to_string() && entry.state == "failed")
        });
        if current {
            return Ok(());
        }
        let saved = self
            .catalog
            .as_ref()
            .ok_or(TransferResult::InvalidData)?
            .load(id)?;
        if saved
            .items
            .iter()
            .any(|entry| entry.item_id == item && entry.state.as_str() == "failed")
        {
            Ok(())
        } else {
            Err(TransferResult::InvalidData)
        }
    }
    fn request_receiver_retry(
        &mut self,
        id: TransferId,
        item: Option<u64>,
    ) -> Result<(), TransferResult> {
        self.selected_retry_allowed(id, item)?;
        let metadata = self.task_path(id).join("receive.json");
        let meta = if metadata.exists() {
            Some(read_json::<ReceivedTask>(&metadata)?)
        } else {
            None
        };
        if self
            .current
            .as_ref()
            .is_none_or(|p| p.task_id != id_text(id))
        {
            let saved = self
                .catalog
                .as_ref()
                .ok_or(TransferResult::InvalidData)?
                .load(id)?;
            self.current = Some(restore_progress(&saved));
        }
        let shared = self.shared.upgrade().ok_or(TransferResult::IoFailed)?;
        *shared
            .cancelled_retry
            .lock()
            .map_err(|_| TransferResult::IoFailed)? = None;
        *shared
            .pending_retry
            .lock()
            .map_err(|_| TransferResult::IoFailed)? = Some(id);
        drop(shared);
        self.set_active(None);
        {
            let p = self.current.as_mut().unwrap();
            p.direction = "download";
            if let Some(meta) = meta {
                p.name = meta.name;
            }
            p.state = "waiting";
            p.message = Some("正在请求原发送端恢复任务；接收端仍需确认。".into());
        }
        self.persist();
        self.emit();
        if let Err(error) = self.wire(FileTaskMessage::RetryRequest {
            task_id: id,
            item_id: item,
        }) {
            if let Some(shared) = self.shared.upgrade() {
                *shared
                    .pending_retry
                    .lock()
                    .map_err(|_| TransferResult::IoFailed)? = None;
            }
            return Err(error);
        }
        Ok(())
    }
    fn emit(&mut self) {
        if let Some(p) = self.current.as_mut() {
            let active = p
                .active_item
                .as_ref()
                .map(|item| (item.id.as_str(), item.state));
            let changed = self.last_emission.as_ref().is_none_or(|last| {
                last.task_id != p.task_id
                    || last.state != p.state
                    || last.scan_complete != p.scan_complete
                    || last
                        .active
                        .as_ref()
                        .map(|(id, state)| (id.as_str(), *state))
                        != active
            });
            if !changed
                && self
                    .last_emission
                    .as_ref()
                    .is_some_and(|last| last.at.elapsed() < Duration::from_millis(100))
            {
                return;
            }
            self.revision = self.revision.saturating_add(1);
            p.revision = self.revision;
            self.last_emission = Some(ProgressEmission {
                at: Instant::now(),
                task_id: p.task_id.clone(),
                state: p.state,
                active: active.map(|(id, state)| (id.to_owned(), state)),
                scan_complete: p.scan_complete,
            });
            self.progress_tx.send_replace(Some(p.clone()));
        }
    }
    fn hash_progress(&mut self, checked: u64, total: u64, source: bool, last: &mut Instant) {
        if last.elapsed() < Duration::from_millis(100) {
            return;
        }
        if let Some(p) = self.current.as_mut() {
            p.state = if source { "scanning" } else { "verifying" };
            let message = format!(
                "{}：已检查 {checked} / {total} 字节",
                if source {
                    "正在校验源文件"
                } else {
                    "正在校验接收内容"
                }
            );
            p.message = Some(message.clone());
            if let Some(item) = p.active_item.as_mut() {
                item.state = p.state;
                item.message = Some(message);
            }
        }
        self.emit();
        let item_id = self
            .current
            .as_ref()
            .and_then(|p| p.active_item.as_ref())
            .and_then(|item| item.id.parse().ok());
        if checked > 0
            && item_id.is_some()
            && let Err(error) = self.send_activity(
                if source {
                    FileTaskPhase::Hashing
                } else {
                    FileTaskPhase::Verifying
                },
                item_id,
                checked,
                total,
            )
            && error != TransferResult::Cancelled
        {
            self.activity_error = Some(error);
            self.cancelled.store(true, Ordering::Release);
        }
        *last = Instant::now();
    }
    fn send_activity(
        &mut self,
        phase: FileTaskPhase,
        item_id: Option<u64>,
        completed: u64,
        total: u64,
    ) -> Result<(), TransferResult> {
        if completed == 0 {
            return Ok(());
        }
        let id = parse_id(
            &self
                .current
                .as_ref()
                .ok_or(TransferResult::InvalidData)?
                .task_id,
        )?;
        if self.last_activity.as_ref().is_some_and(|last| {
            last.at.elapsed() < Duration::from_secs(1)
                || (last.task_id == id
                    && last.phase == phase
                    && last.item_id == item_id
                    && completed <= last.completed)
        }) {
            return Ok(());
        }
        self.wire(FileTaskMessage::Activity {
            task_id: id,
            item_id,
            phase,
            completed,
            total,
        })?;
        self.last_activity = Some(SentActivity {
            at: Instant::now(),
            task_id: id,
            item_id,
            phase,
            completed,
        });
        Ok(())
    }
    fn indexing_progress(
        &mut self,
        covered: u64,
        total_bytes: u64,
        last: &mut Instant,
    ) -> Result<(), TransferResult> {
        self.check()?;
        if last.elapsed() < Duration::from_millis(100) {
            return Ok(());
        }
        let item_id = self
            .current
            .as_ref()
            .and_then(|p| p.active_item.as_ref())
            .and_then(|item| item.id.parse().ok())
            .ok_or(TransferResult::InvalidData)?;
        if let Some(progress) = self.current.as_mut() {
            progress.state = "verifying";
            progress.message = Some("正在完成文件保存…".into());
            if let Some(item) = progress.active_item.as_mut() {
                item.state = "verifying";
                item.message = progress.message.clone();
            }
        }
        self.emit();
        self.send_activity(
            FileTaskPhase::Indexing,
            Some(item_id),
            covered.div_ceil(u64::from(FILE_BLOCK_BYTES)),
            total_bytes.div_ceil(u64::from(FILE_BLOCK_BYTES)),
        )?;
        *last = Instant::now();
        Ok(())
    }
    fn accept_activity(
        &mut self,
        task_id: TransferId,
        item_id: Option<u64>,
        phase: FileTaskPhase,
        completed: u64,
        total: u64,
        points: &mut [Option<ActivityPoint>; 4],
    ) -> bool {
        let Some(p) = self.current.as_ref() else {
            return false;
        };
        if p.task_id != id_text(task_id)
            || completed == 0
            || completed > total && phase != FileTaskPhase::Scanning
        {
            return false;
        }
        let slot = match phase {
            FileTaskPhase::Scanning => 0,
            FileTaskPhase::Hashing => 1,
            FileTaskPhase::Verifying => 2,
            FileTaskPhase::Indexing => 3,
        };
        let valid = if p.direction == "upload" {
            matches!(p.state, "waiting" | "verifying")
                && p.active_item.as_ref().is_some_and(|item| {
                    item.id.parse::<u64>().ok() == item_id
                        && match phase {
                            FileTaskPhase::Verifying => item.total_bytes == total,
                            FileTaskPhase::Indexing => {
                                item.total_bytes.div_ceil(u64::from(FILE_BLOCK_BYTES)) == total
                            }
                            _ => false,
                        }
                })
        } else {
            p.active_item.is_none()
                && match phase {
                    FileTaskPhase::Scanning => {
                        item_id.is_none()
                            && total == 0
                            && completed
                                <= self.peer.map_or(0, |budget| {
                                    budget
                                        .max_entries
                                        .saturating_mul(MAX_FILE_TASK_SCAN_WORK_PASSES)
                                })
                            && points[1].is_none()
                    }
                    FileTaskPhase::Hashing => {
                        item_id == p.scanned_items.checked_add(1)
                            && total <= self.peer.map_or(0, |budget| budget.max_file_bytes)
                    }
                    FileTaskPhase::Verifying | FileTaskPhase::Indexing => false,
                }
        };
        if !valid
            || points[slot].is_some_and(|prior| {
                prior.item_id != item_id || prior.total != total || completed <= prior.completed
            })
        {
            return false;
        }
        points[slot] = Some(ActivityPoint {
            item_id,
            completed,
            total,
        });
        if let Some(p) = self.current.as_mut() {
            p.message = Some(if phase == FileTaskPhase::Scanning {
                "对方正在检查文件列表…".into()
            } else if phase == FileTaskPhase::Indexing {
                "对方正在完成文件保存…".into()
            } else {
                format!(
                    "对方正在{}：{completed} 字节",
                    match phase {
                        FileTaskPhase::Scanning | FileTaskPhase::Indexing => unreachable!(),
                        FileTaskPhase::Hashing => "校验源文件",
                        FileTaskPhase::Verifying => "校验接收内容",
                    }
                )
            });
        }
        self.emit();
        true
    }
    fn resumed_progress(&mut self, offset: u64) -> Result<(), TransferResult> {
        let p = self.current.as_mut().ok_or(TransferResult::InvalidData)?;
        let item = p.active_item.as_mut().ok_or(TransferResult::InvalidData)?;
        if offset > item.total_bytes || offset < item.acknowledged_bytes {
            return Err(TransferResult::InvalidData);
        }
        let acknowledged = offset - item.acknowledged_bytes;
        let sent = offset.saturating_sub(item.sent_bytes);
        p.acknowledged_bytes = p
            .acknowledged_bytes
            .checked_add(acknowledged)
            .ok_or(TransferResult::TooLarge)?;
        p.sent_bytes = p
            .sent_bytes
            .checked_add(sent)
            .ok_or(TransferResult::TooLarge)?;
        if p.acknowledged_bytes > p.total_bytes || p.sent_bytes > p.total_bytes {
            return Err(TransferResult::InvalidData);
        }
        item.acknowledged_bytes = offset;
        item.sent_bytes = item.sent_bytes.max(offset);
        self.emit();
        Ok(())
    }
    fn wire(&self, m: FileTaskMessage) -> Result<(), TransferResult> {
        if !m.is_valid() {
            return Err(TransferResult::InvalidData);
        }
        let control = matches!(
            m,
            FileTaskMessage::Action { .. } | FileTaskMessage::SelectionResult { .. }
        );
        let mut pending = m;
        let began = Instant::now();
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return Err(TransferResult::IoFailed);
            }
            if !control && self.cancelled.load(Ordering::Acquire) {
                return Err(TransferResult::Cancelled);
            }
            match self.send.try_send(pending) {
                Ok(()) => return Ok(()),
                Err(mpsc::error::TrySendError::Closed(_)) => return Err(TransferResult::IoFailed),
                Err(mpsc::error::TrySendError::Full(m)) => pending = m,
            }
            if began.elapsed() > Duration::from_secs(30) {
                return Err(TransferResult::IoFailed);
            }
            std::thread::park_timeout(Duration::from_millis(10));
        }
    }
    fn limits(&self) -> Result<(FileTransferCapabilities, DirectoryScanLimits), TransferResult> {
        let c = self
            .peer
            .and_then(|p| p.intersect(FileTransferCapabilities::WINDOWS))
            .filter(|c| c.large_files)
            .ok_or(TransferResult::Unsupported)?;
        Ok((
            c,
            DirectoryScanLimits {
                max_entries: c.max_entries,
                max_total_bytes: c.max_task_bytes,
                max_file_bytes: c.max_file_bytes,
                max_depth: 64,
                max_relative_path_bytes: 4096,
            },
        ))
    }
    fn finish_source_scan(
        &mut self,
        manifest: &Path,
        budget: FileTransferCapabilities,
    ) -> Result<(), TransferResult> {
        if self.current.as_ref().unwrap().scanned_items == 0 {
            let mut count = 0_u64;
            let mut total = 0_u64;
            let mut reader = BufReader::new(cache_open_read(manifest)?);
            while let Some(entry) = read_line::<ManifestRecord>(&mut reader)? {
                self.check()?;
                count += 1;
                total = total
                    .checked_add(entry.size())
                    .ok_or(TransferResult::TooLarge)?;
                if count > budget.max_entries
                    || total > budget.max_task_bytes
                    || entry.size() > budget.max_file_bytes
                {
                    return Err(TransferResult::TooLarge);
                }
                self.record_scan_work()?;
            }
            let p = self.current.as_mut().unwrap();
            p.scanned_items = count;
            p.total_bytes = total;
        }
        let p = self.current.as_mut().unwrap();
        p.scan_complete = true;
        p.state = "waiting";
        p.message = None;
        self.persist();
        self.emit();
        Ok(())
    }

    fn scan_progress(&mut self, entries: u64, total_bytes: u64) -> Result<(), TransferResult> {
        let progress = self.current.as_mut().unwrap();
        progress.scanned_items = entries;
        progress.total_bytes = total_bytes;
        self.emit();
        Ok(())
    }

    fn record_scan_work(&mut self) -> Result<(), TransferResult> {
        let limit = self
            .peer
            .ok_or(TransferResult::Unsupported)?
            .max_entries
            .checked_mul(MAX_FILE_TASK_SCAN_WORK_PASSES)
            .ok_or(TransferResult::TooLarge)?;
        self.scan_record_work = self
            .scan_record_work
            .checked_add(1)
            .filter(|work| *work <= limit)
            .ok_or(TransferResult::TooLarge)?;
        self.send_activity(FileTaskPhase::Scanning, None, self.scan_record_work, 0)
    }

    fn append_scan_outcome(
        &mut self,
        outcome: Result<DirectoryEntry, DirectoryScanFailure>,
        spool: &mut ManifestSpool,
        index: &mut Option<ManifestPathIndex>,
        budget: FileTransferCapabilities,
    ) -> Result<(), TransferResult> {
        match outcome {
            Ok(entry)
                if entry.size <= budget.max_file_bytes
                    && spool
                        .total_bytes
                        .checked_add(entry.size)
                        .is_some_and(|total| total <= budget.max_task_bytes) =>
            {
                let path = entry.relative_path.clone();
                let offset = spool.entry(entry)?;
                if let Some(index) = index {
                    index.insert(&path, spool.successes, offset, |at| spool.read_path(at))?;
                }
            }
            Ok(entry) => {
                spool.failure(DirectoryScanFailure {
                    relative_path: Some(entry.relative_path.clone()),
                    result: TransferResult::TooLarge,
                    scope: if entry.kind == TransferEntryKind::Directory {
                        DirectoryScanFailureScope::Subtree
                    } else {
                        DirectoryScanFailureScope::Entry
                    },
                    kind: Some(entry.kind),
                    size: Some(entry.size),
                    terminal: false,
                    retry_relative_path: Some(entry.relative_path),
                })?;
            }
            Err(failure) => {
                spool.failure(failure)?;
            }
        }
        self.scan_progress(spool.count(), spool.total_bytes)
    }

    #[allow(clippy::too_many_arguments)]
    fn merge_scan(
        &mut self,
        mut scanner: DirectoryScanner,
        spool: &mut ManifestSpool,
        index: &mut Option<ManifestPathIndex>,
        budget: FileTransferCapabilities,
        scan_work: &mut u64,
        original_failure: Option<&DirectoryScanFailure>,
    ) -> Result<(), TransferResult> {
        let mut pending = None;
        while let Some(mut outcome) = scanner.next() {
            self.check()?;
            let mut changed_kind = false;
            if matches!(&outcome, Err(error) if error.result == TransferResult::Cancelled) {
                return Err(TransferResult::Cancelled);
            }
            let scan_limit_hit = *scan_work >= budget.max_entries;
            if scan_limit_hit {
                outcome = Err(scan_limit_failure());
            } else {
                *scan_work += 1;
                self.record_scan_work()?;
            }
            if let Ok(entry) = &outcome {
                if let Some(index) = index.as_mut()
                    && let Some((_, at)) =
                        index.find(&entry.relative_path, |at| spool.read_path(at))?
                {
                    let original = spool.read_entry(at)?;
                    if original.kind != entry.kind && entry.kind == TransferEntryKind::Directory {
                        // Replaying the immutable old record reports its changed
                        // kind. Never adopt new children from a replacement.
                        scanner.skip_current_subtree(&entry.relative_path)?;
                    }
                    continue;
                }
                if let Some(failure) = original_failure
                    && matches!(
                        failure.scope,
                        DirectoryScanFailureScope::Entry | DirectoryScanFailureScope::Subtree
                    )
                    && failure.retry_relative_path.as_deref() == Some(entry.relative_path.as_str())
                    && failure.kind.is_some_and(|kind| kind != entry.kind)
                {
                    let mut changed = failure.clone();
                    changed.result = TransferResult::SourceChanged;
                    outcome = Err(changed);
                    changed_kind = true;
                } else if !spool.resolve_entry_failures(&entry.relative_path, entry.kind)? {
                    if entry.kind == TransferEntryKind::Directory {
                        scanner.skip_current_subtree(&entry.relative_path)?;
                    }
                    continue;
                }
            } else if let Err(failure) = &outcome
                && let Some(slot) = spool.find_failure_slot(failure)?
                && spool.failure_active(slot)?
            {
                continue;
            }
            let available = budget
                .max_entries
                .checked_sub(spool.count())
                .ok_or(TransferResult::TooLarge)?;
            // Keep the last possible outcome in memory until discovery ends.
            // If another outcome exists, replace only this as-yet-unsent slot
            // with a failure marker; durable successful identities never move.
            if available == 0 {
                return Err(TransferResult::TooLarge);
            }
            if pending.is_some() && available == 1 {
                self.append_scan_outcome(Err(scan_limit_failure()), spool, index, budget)?;
                return Ok(());
            }
            if let Some(previous) = pending.take() {
                self.append_scan_outcome(previous, spool, index, budget)?;
            }
            pending = Some(outcome);
            if scan_limit_hit || changed_kind {
                break;
            }
        }
        if let Some(outcome) = pending {
            self.append_scan_outcome(outcome, spool, index, budget)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn prepare_source_manifest(
        &mut self,
        path: &Path,
        directory: bool,
        task_dir: &Path,
        manifest: &Path,
        retry_item: Option<u64>,
        budget: FileTransferCapabilities,
        limits: DirectoryScanLimits,
    ) -> Result<(), TransferResult> {
        let existed = path_exists(manifest)?;
        let mut old_successes = 0_u64;
        let mut old_failures = 0_u64;
        let mut selected_failure = false;
        let temporary = task_dir.join("manifest.pending");
        let failures = task_dir.join("manifest-failures.pending");
        let lookup = task_dir.join("manifest-lookup.pending");
        let mut cached_spool: Option<ManifestSpool> = None;
        let mut original = if existed {
            Some(BufReader::new(cache_open_read(manifest)?))
        } else {
            None
        };
        if let Some(reader) = original.as_mut() {
            let mut total = 0_u64;
            while let Some(record) = read_line::<ManifestRecord>(reader)? {
                self.check()?;
                self.record_scan_work()?;
                record.validate(path, directory)?;
                if record.item_id() != old_successes + old_failures + 1 {
                    return Err(TransferResult::InvalidData);
                }
                total = total
                    .checked_add(record.size())
                    .ok_or(TransferResult::TooLarge)?;
                match record {
                    ManifestRecord::Entry(entry)
                        if old_failures == 0 && entry.size <= budget.max_file_bytes =>
                    {
                        old_successes += 1
                    }
                    ManifestRecord::Entry(_) => return Err(TransferResult::InvalidData),
                    ManifestRecord::Failure(failure) => {
                        selected_failure |= retry_item == Some(failure.item_id);
                        old_failures += 1;
                        if cached_spool.is_none() {
                            cached_spool = Some(ManifestSpool::create(
                                &temporary,
                                &failures,
                                budget.max_entries,
                            )?);
                        }
                        cached_spool.as_mut().unwrap().failure(failure.failure)?;
                    }
                }
                if old_successes + old_failures > budget.max_entries
                    || total > budget.max_task_bytes
                {
                    return Err(TransferResult::TooLarge);
                }
            }
            if old_failures == 0 || (retry_item.is_some() && !selected_failure) {
                return Ok(());
            }
            reader.seek(SeekFrom::Start(0)).map_err(io_result)?;
        }

        if path_exists(&lookup)? {
            cache_remove(&lookup)?;
        }
        let result = (|| {
            let mut spool = match cached_spool.take() {
                Some(spool) => spool,
                None => ManifestSpool::create(&temporary, &failures, budget.max_entries)?,
            };
            let mut index = if existed {
                Some(ManifestPathIndex::create(&lookup, budget.max_entries)?)
            } else {
                None
            };
            let mut scan_work = 0;
            if existed {
                // Copy the immutable successful prefix before any failed scope
                // is reopened. Source identities remain the original observations.
                let reader = original.as_mut().ok_or(TransferResult::InvalidData)?;
                while let Some(record) = read_line::<ManifestRecord>(reader)? {
                    self.check()?;
                    self.record_scan_work()?;
                    match record {
                        ManifestRecord::Entry(entry) => {
                            let path = entry.relative_path.clone();
                            let expected = entry.item_id;
                            let offset = spool.entry(entry)?;
                            if spool.successes != expected {
                                return Err(TransferResult::InvalidData);
                            }
                            index
                                .as_mut()
                                .unwrap()
                                .insert(&path, expected, offset, |at| spool.read_path(at))?;
                            self.scan_progress(spool.count(), spool.total_bytes)?;
                        }
                        ManifestRecord::Failure(failure) => {
                            let slot = spool
                                .find_failure_slot(&failure.failure)?
                                .ok_or(TransferResult::InvalidData)?;
                            if !spool.failure_active(slot)? {
                                continue;
                            }
                            if retry_item.is_some_and(|id| id != failure.item_id)
                                || scan_work >= budget.max_entries
                            {
                                continue;
                            }
                            if retry_item.is_none()
                                && self.scan_failure_covered(&mut spool, &failure.failure)?
                            {
                                continue;
                            }
                            spool.deactivate_failure(slot)?;
                            let prefix =
                                failure.failure.retry_relative_path.as_deref().unwrap_or("");
                            let include_root = matches!(
                                failure.failure.scope,
                                DirectoryScanFailureScope::Entry
                                    | DirectoryScanFailureScope::Subtree
                            );
                            let mut scope_limits = limits;
                            scope_limits.max_entries = budget.max_entries - scan_work;
                            let scanner = DirectoryScanner::open_subtree_with_limits(
                                path,
                                prefix,
                                include_root,
                                self.cancelled.clone(),
                                scope_limits,
                            );
                            match scanner {
                                Ok(scanner) => self.merge_scan(
                                    scanner,
                                    &mut spool,
                                    &mut index,
                                    budget,
                                    &mut scan_work,
                                    Some(&failure.failure),
                                )?,
                                Err(error) => {
                                    self.check()?;
                                    let mut retained = failure.failure;
                                    retained.result = error;
                                    spool.failure(retained)?;
                                }
                            }
                        }
                    }
                }
            } else if directory {
                match DirectoryScanner::open_with_limits(path, self.cancelled.clone(), limits) {
                    Ok(scanner) => self.merge_scan(
                        scanner,
                        &mut spool,
                        &mut index,
                        budget,
                        &mut scan_work,
                        None,
                    )?,
                    Err(error) => {
                        self.check()?;
                        spool.failure(DirectoryScanFailure {
                            relative_path: None,
                            result: error,
                            scope: DirectoryScanFailureScope::Enumeration,
                            kind: Some(TransferEntryKind::Directory),
                            size: Some(0),
                            terminal: false,
                            retry_relative_path: Some(String::new()),
                        })?;
                    }
                }
            } else {
                let entry = DirectoryEntry::single_file(path)?;
                if entry.size > budget.max_file_bytes || entry.size > budget.max_task_bytes {
                    return Err(TransferResult::TooLarge);
                }
                spool.entry(entry)?;
            }
            let (output, entries, total) = spool.finish(|| {
                self.check()?;
                self.record_scan_work()
            })?;
            if entries > budget.max_entries || total > budget.max_task_bytes {
                return Err(TransferResult::TooLarge);
            }
            drop(index);
            drop(original.take());
            cache_publish_file(&output, &temporary, manifest, existed)?;
            drop(output);
            self.scan_progress(entries, total)
        })();
        if path_exists(&lookup).unwrap_or(false) {
            let _ = cache_remove(&lookup);
        }
        result
    }

    fn scan_failure_covered(
        &mut self,
        spool: &mut ManifestSpool,
        failure: &DirectoryScanFailure,
    ) -> Result<bool, TransferResult> {
        if failure.scope == DirectoryScanFailureScope::Task {
            return Ok(false);
        }
        self.check()?;
        if let Some(slot) = spool.find_failure_slot(&scan_limit_failure())?
            && spool.failure_active(slot)?
        {
            return Ok(true);
        }
        let Some(mut prefix) = failure.retry_relative_path.as_deref() else {
            return Ok(false);
        };
        loop {
            for scope in [
                DirectoryScanFailureScope::Subtree,
                DirectoryScanFailureScope::Enumeration,
            ] {
                self.check()?;
                if (scope == failure.scope
                    && Some(prefix) == failure.retry_relative_path.as_deref())
                    || (scope == DirectoryScanFailureScope::Subtree && prefix.is_empty())
                {
                    continue;
                }
                let probe = DirectoryScanFailure {
                    relative_path: (!prefix.is_empty()).then(|| prefix.to_owned()),
                    result: TransferResult::IoFailed,
                    scope,
                    kind: Some(TransferEntryKind::Directory),
                    size: Some(0),
                    terminal: false,
                    retry_relative_path: Some(prefix.to_owned()),
                };
                if let Some(slot) = spool.find_failure_slot(&probe)?
                    && spool.failure_active(slot)?
                {
                    return Ok(true);
                }
            }
            if prefix.is_empty() {
                break;
            }
            prefix = prefix.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
        Ok(false)
    }

    fn send_scan_failure(
        &mut self,
        id: TransferId,
        failure: &ManifestFailure,
    ) -> Result<TransferResult, TransferResult> {
        failure.validate()?;
        self.current
            .as_mut()
            .unwrap()
            .item(&failure.manifest(), "scanning");
        self.emit();
        self.wire(FileTaskMessage::EntrySkipped {
            task_id: id,
            item_id: failure.item_id,
            relative_path: failure.relative_path.clone(),
            size: 0,
            result: failure.failure.result,
        })?;
        match self.receive()? {
            FileTaskMessage::EntryResult {
                task_id,
                item_id,
                result,
            } if task_id == id
                && item_id == failure.item_id
                && result == failure.failure.result =>
            {
                Ok(result)
            }
            _ => Err(TransferResult::InvalidData),
        }
    }
    fn task_path(&self, id: TransferId) -> PathBuf {
        self.options
            .cache_root
            .join(&self.options.peer_key)
            .join(id_text(id))
    }
    fn set_active(&self, id: Option<TransferId>) {
        if let Some(s) = self.shared.upgrade() {
            let mut active = s.active.lock().unwrap_or_else(|p| p.into_inner());
            if id.is_some() && *active != id {
                s.action.store(0, Ordering::Release);
                s.cancelled.store(false, Ordering::Release);
                s.active_request.store(0, Ordering::Release);
            }
            if id.is_none() {
                s.active_request.store(0, Ordering::Release);
            }
            *active = id;
        }
    }
    fn claim_active(&self, id: TransferId) -> Result<(), TransferResult> {
        let shared = self.shared.upgrade().ok_or(TransferResult::IoFailed)?;
        let mut active = shared.active.lock().map_err(|_| TransferResult::IoFailed)?;
        if active.is_some() {
            return Err(TransferResult::Busy);
        }
        *active = Some(id);
        shared.active_request.store(0, Ordering::Release);
        shared.action.store(0, Ordering::Release);
        shared.cancelled.store(false, Ordering::Release);
        Ok(())
    }
    fn set_selector_request(&self, request_id: u64) {
        if let Some(shared) = self.shared.upgrade() {
            shared.selector_request.store(request_id, Ordering::Release);
        }
    }
    fn check(&mut self) -> Result<(), TransferResult> {
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return Err(TransferResult::IoFailed);
            }
            if self.cancelled.load(Ordering::Acquire) {
                return Err(TransferResult::Cancelled);
            }
            let Some(s) = self.shared.upgrade() else {
                return Err(TransferResult::IoFailed);
            };
            match s.action.load(Ordering::Acquire) {
                0 => return Ok(()),
                2 => return Err(TransferResult::Cancelled),
                _ => {
                    if self.current.as_ref().is_some_and(|p| p.state != "paused") {
                        if let Some(p) = self.current.as_mut() {
                            p.state = "paused";
                        }
                        self.persist();
                        self.emit();
                    }
                    std::thread::park_timeout(Duration::from_millis(50));
                }
            }
        }
    }
    fn receive(&mut self) -> Result<FileTaskMessage, TransferResult> {
        self.receive_with_budget(RESPONSE_TIMEOUT)
    }
    fn receive_with_budget(&mut self, budget: Duration) -> Result<FileTaskMessage, TransferResult> {
        let mut began = Instant::now();
        let mut suspended = Duration::ZERO;
        let mut activity = [None; 4];
        loop {
            let checking = Instant::now();
            self.check()?;
            suspended = suspended.saturating_add(checking.elapsed());
            match self.pending.recv_timeout(Duration::from_millis(50)) {
                Ok(Command::Wire(FileTaskMessage::Capabilities(c))) => {
                    if self.peer.is_none() {
                        self.peer = Some(c);
                    }
                }
                Ok(Command::Wire(FileTaskMessage::Activity {
                    task_id,
                    item_id,
                    phase,
                    completed,
                    total,
                })) => {
                    if self.accept_activity(
                        task_id,
                        item_id,
                        phase,
                        completed,
                        total,
                        &mut activity,
                    ) {
                        began = Instant::now();
                        suspended = Duration::ZERO;
                    }
                }
                Ok(Command::Wire(FileTaskMessage::Offer { task_id, .. })) => {
                    self.wire(FileTaskMessage::Decision {
                        task_id,
                        result: TransferResult::Busy,
                        budget: FileTransferCapabilities::WINDOWS,
                    })?;
                }
                Ok(Command::Wire(m)) => return Ok(m),
                Ok(_) => return Err(TransferResult::Busy),
                Err(sync::RecvTimeoutError::Disconnected) => return Err(TransferResult::IoFailed),
                Err(sync::RecvTimeoutError::Timeout) => {}
            }
            if began.elapsed().saturating_sub(suspended) > budget {
                return Err(TransferResult::IoFailed);
            }
        }
    }
    fn run(&mut self) {
        if self
            .wire(FileTaskMessage::Capabilities(
                FileTransferCapabilities::WINDOWS,
            ))
            .is_err()
        {
            return;
        }
        self.restore();
        while !self.stopped.load(Ordering::Acquire) {
            let command = match self.pending.recv_timeout(Duration::from_millis(100)) {
                Ok(c) => c,
                Err(sync::RecvTimeoutError::Timeout) => continue,
                Err(_) => break,
            };
            let (id, result) = match command {
                Command::Wire(FileTaskMessage::Capabilities(c)) => {
                    if self.peer.is_none() {
                        self.peer = Some(c);
                    }
                    continue;
                }
                Command::Wire(FileTaskMessage::SelectionRequest {
                    request_id,
                    directory,
                }) => {
                    let mut id = [0; 16];
                    let reservation = rand_core::OsRng
                        .try_fill_bytes(&mut id)
                        .map_err(|_| TransferResult::IoFailed)
                        .and_then(|()| {
                            if id == [0; 16] {
                                Err(TransferResult::IoFailed)
                            } else {
                                self.claim_active(id)
                            }
                        });
                    if let Err(result) = reservation {
                        self.set_selector_request(0);
                        let _ = self.wire(FileTaskMessage::SelectionResult { request_id, result });
                        continue;
                    }
                    self.set_selector_request(request_id);
                    let cancelled = self.shared.upgrade().is_some_and(|shared| {
                        shared.cancelled_selection.load(Ordering::Acquire) == request_id
                    });
                    if cancelled {
                        self.set_selector_request(0);
                        self.set_active(None);
                        let _ = self.wire(FileTaskMessage::SelectionResult {
                            request_id,
                            result: TransferResult::Cancelled,
                        });
                        continue;
                    }
                    let available = self.limits().and_then(|(budget, _)| {
                        if !directory || budget.directories {
                            Ok(())
                        } else {
                            Err(TransferResult::Unsupported)
                        }
                    });
                    let selected = match available {
                        Err(error) => Err(error),
                        Ok(()) => match self.options.select.clone() {
                            Some(select) => select(directory, self.cancellation()),
                            None => Err(TransferResult::Unsupported),
                        },
                    };
                    let path = match selected {
                        Ok(Some(path)) if path.is_dir() == directory => path,
                        Ok(Some(_)) => {
                            self.set_selector_request(0);
                            self.set_active(None);
                            let _ = self.wire(FileTaskMessage::SelectionResult {
                                request_id,
                                result: TransferResult::InvalidData,
                            });
                            continue;
                        }
                        other => {
                            self.set_selector_request(0);
                            self.set_active(None);
                            let result = other.err().unwrap_or(TransferResult::Cancelled);
                            let _ =
                                self.wire(FileTaskMessage::SelectionResult { request_id, result });
                            continue;
                        }
                    };
                    if self.cancelled.load(Ordering::Acquire)
                        || self.stopped.load(Ordering::Acquire)
                    {
                        self.set_selector_request(0);
                        self.set_active(None);
                        let _ = self.wire(FileTaskMessage::SelectionResult {
                            request_id,
                            result: TransferResult::Cancelled,
                        });
                        continue;
                    }
                    self.pending_request = Some(request_id);
                    let result = self.send_task(path, id, None);
                    self.set_selector_request(0);
                    if self.pending_request.take().is_some() {
                        let _ = self.wire(FileTaskMessage::SelectionResult {
                            request_id,
                            result: result.err().unwrap_or(TransferResult::InvalidData),
                        });
                    }
                    (id, result)
                }
                Command::Start(path, id) => {
                    self.set_active(Some(id));
                    (id, self.send_task(path, id, None))
                }
                Command::StartQueued(file) => {
                    let id = file.transfer_id;
                    self.set_active(Some(id));
                    self.current = Some(FileTaskProgress::new(id, file.name.clone(), "upload"));
                    let result = (|| {
                        let task_guard = self
                            .catalog
                            .as_ref()
                            .ok_or(TransferResult::IoFailed)?
                            .ensure_task_dir(id)?;
                        let metadata = task_guard.path().join("source.json");
                        if task_guard.path().join("receive.json").exists() {
                            return Err(TransferResult::InvalidData);
                        }
                        if !metadata.exists() {
                            write_json_new(
                                &metadata,
                                &SourceTask {
                                    path: file.path.clone(),
                                    name: file.name.clone(),
                                    directory: false,
                                    expected_size: Some(file.size),
                                    expected_modified_at_unix_ns: Some(file.modified_at_unix_ns),
                                },
                            )?;
                        }
                        self.persist_required()?;
                        self.current.as_mut().unwrap().source_admitted = true;
                        self.emit();
                        let current = crate::transfer::prepare_outgoing_file_with_budget(
                            file.path.clone(),
                            desklink_protocol::MAX_LARGE_FILE_BYTES,
                        )?;
                        if current.name != file.name
                            || current.size != file.size
                            || current.modified_at_unix_ns != file.modified_at_unix_ns
                        {
                            return Err(TransferResult::SourceChanged);
                        }
                        let entry = DirectoryEntry::single_file(&file.path)?;
                        let _source_guard = entry.open_verified_source()?;
                        if entry.size != file.size
                            || entry.modified_at_unix_ns != file.modified_at_unix_ns
                            || entry.relative_path != file.name
                        {
                            return Err(TransferResult::SourceChanged);
                        }
                        self.send_task(file.path, id, None)
                    })();
                    (id, result)
                }
                Command::Retry(id, item) => {
                    self.set_active(Some(id));
                    let source_exists = self.task_path(id).join("source.json").exists();
                    let receive_exists = self.task_path(id).join("receive.json").exists();
                    let rejected_download = self
                        .current
                        .as_ref()
                        .is_some_and(|p| p.task_id == id_text(id) && p.direction == "download");
                    if source_exists && receive_exists {
                        (id, Err(TransferResult::InvalidData))
                    } else if !source_exists && (receive_exists || rejected_download) {
                        match self.request_receiver_retry(id, item) {
                            Ok(()) => continue,
                            Err(error) => (id, Err(error)),
                        }
                    } else {
                        let result =
                            read_json::<SourceTask>(&self.task_path(id).join("source.json"))
                                .and_then(|meta| self.send_task(meta.path, id, item));
                        (id, result)
                    }
                }
                Command::CancelStored(id) => {
                    if self
                        .current
                        .as_ref()
                        .is_some_and(|p| p.task_id == id_text(id))
                    {
                        let p = self.current.as_mut().unwrap();
                        p.state = "cancelled";
                        p.message = Some("任务已取消，已确认的数据暂时保留，可重试。".into());
                        self.persist();
                        self.emit();
                        self.set_active(None);
                        continue;
                    }
                    (id, Err(TransferResult::InvalidData))
                }
                Command::Wire(FileTaskMessage::RetryRequest { task_id, item_id }) => {
                    self.set_active(Some(task_id));
                    self.responding_retry = Some(task_id);
                    let result =
                        read_json::<SourceTask>(&self.task_path(task_id).join("source.json"))
                            .and_then(|meta| {
                                self.check()?;
                                let approve = self
                                    .options
                                    .authorize_source_resume
                                    .clone()
                                    .ok_or(TransferResult::Rejected)?;
                                if !approve(&meta.path, meta.directory, self.cancellation()) {
                                    return Err(
                                        if self.cancelled.load(Ordering::Acquire)
                                            || self.stopped.load(Ordering::Acquire)
                                        {
                                            TransferResult::Cancelled
                                        } else {
                                            TransferResult::Rejected
                                        },
                                    );
                                }
                                self.check()?;
                                self.send_task(meta.path, task_id, item_id)
                            });
                    if self.responding_retry.take().is_some() {
                        let _ = self.wire(FileTaskMessage::RetryResult {
                            task_id,
                            result: result.err().unwrap_or(TransferResult::InvalidData),
                        });
                    }
                    (task_id, result)
                }
                Command::Wire(FileTaskMessage::RetryResult { task_id, result }) => {
                    if self
                        .current
                        .as_ref()
                        .is_some_and(|p| p.task_id == id_text(task_id))
                    {
                        let p = self.current.as_mut().unwrap();
                        p.state = "failed";
                        p.message = Some(result_message(result).into());
                        self.persist();
                        self.emit();
                    }
                    continue;
                }
                Command::Wire(FileTaskMessage::Offer {
                    task_id,
                    request_id: _,
                    name,
                    directory,
                }) => {
                    self.set_active(Some(task_id));
                    (task_id, self.receive_task(task_id, name, directory))
                }
                _ => continue,
            };
            if let Err(error) = result {
                if self
                    .current
                    .as_ref()
                    .is_none_or(|p| p.task_id != id_text(id))
                {
                    self.current = Some(FileTaskProgress::new(id, "文件任务".into(), "upload"));
                }
                if let Some(p) = self.current.as_mut() {
                    p.state = if self.stopped.load(Ordering::Acquire) {
                        "paused"
                    } else if error == TransferResult::Cancelled {
                        "cancelled"
                    } else {
                        "failed"
                    };
                    p.message = Some(result_message(error).to_owned());
                }
                self.persist();
                self.emit();
                let _ = self.wire(FileTaskMessage::Action {
                    task_id: id,
                    action: FileTaskAction::Cancel,
                });
            }
            self.persist();
            self.set_active(None);
            self.last_emission = None;
            self.emit();
        }
    }
    fn offer_task(
        &mut self,
        id: TransferId,
        meta: &SourceTask,
        budget: FileTransferCapabilities,
    ) -> Result<(), TransferResult> {
        self.current.as_mut().unwrap().state = "waiting";
        self.emit();
        let request_id = self.pending_request.take();
        self.wire(FileTaskMessage::Offer {
            task_id: id,
            request_id,
            name: meta.name.clone(),
            directory: meta.directory,
        })?;
        self.responding_retry = None;
        match self.receive()? {
            FileTaskMessage::Decision {
                task_id,
                result: TransferResult::Completed,
                budget: agreed,
            } if task_id == id && budget == agreed => {}
            FileTaskMessage::Decision {
                task_id, result, ..
            } if task_id == id && result != TransferResult::Completed => return Err(result),
            _ => return Err(TransferResult::InvalidData),
        }
        self.current.as_mut().unwrap().state = "scanning";
        self.emit();
        Ok(())
    }
    fn send_task(
        &mut self,
        path: PathBuf,
        id: TransferId,
        retry_item: Option<u64>,
    ) -> Result<(), TransferResult> {
        self.selected_retry_allowed(id, retry_item)?;
        let (budget, limits) = self.limits()?;
        let task_guard = self
            .catalog
            .as_ref()
            .ok_or(TransferResult::IoFailed)?
            .ensure_task_dir(id)?;
        let task_dir = task_guard.path().to_owned();
        let source_meta = task_dir.join("source.json");
        if task_dir.join("receive.json").exists() {
            return Err(TransferResult::InvalidData);
        }
        let meta = if source_meta.exists() {
            let m: SourceTask = read_json(&source_meta)?;
            if m.path != path {
                return Err(TransferResult::SourceChanged);
            }
            m
        } else {
            let m = SourceTask {
                name: path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .filter(|n| desklink_protocol::is_valid_transfer_path_segment(n))
                    .ok_or(TransferResult::InvalidData)?
                    .into(),
                directory: path.is_dir(),
                path: path.clone(),
                expected_size: None,
                expected_modified_at_unix_ns: None,
            };
            write_json_new(&source_meta, &m)?;
            m
        };
        if meta.directory && !budget.directories {
            return Err(TransferResult::Unsupported);
        }
        // The selected root has no manifest item, so pin it even for an empty
        // directory and keep it the same kind/location for the whole attempt.
        let _source_root_guard = if meta.directory {
            Some(GuardedSourceDirectory::open(&path)?)
        } else {
            None
        };
        let _queued_guard =
            if meta.expected_size.is_some() || meta.expected_modified_at_unix_ns.is_some() {
                let entry = DirectoryEntry::single_file(&path)?;
                let guard = entry.open_verified_source()?;
                if meta.expected_size != Some(entry.size)
                    || meta.expected_modified_at_unix_ns != Some(entry.modified_at_unix_ns)
                {
                    return Err(TransferResult::SourceChanged);
                }
                Some(guard)
            } else {
                None
            };
        let admitted = self
            .current
            .as_ref()
            .is_some_and(|p| p.task_id == id_text(id) && p.source_admitted);
        self.current = Some(FileTaskProgress::new(id, meta.name.clone(), "upload"));
        self.current.as_mut().unwrap().source_admitted = admitted;
        self.last_activity = None;
        self.activity_error = None;
        self.scan_record_work = 0;
        self.persist();
        self.emit();
        self.offer_task(id, &meta, budget)?;
        let manifest = task_dir.join("manifest.jsonl");
        self.prepare_source_manifest(
            &path,
            meta.directory,
            &task_dir,
            &manifest,
            retry_item,
            budget,
            limits,
        )?;
        self.finish_source_scan(&manifest, budget)?;
        let mut reader = BufReader::new(cache_open_read(&manifest)?);
        let mut entries = 0;
        let mut total = 0_u64;
        while let Some(record) = read_line::<ManifestRecord>(&mut reader)? {
            self.check()?;
            self.record_scan_work()?;
            entries += 1;
            record.validate(&path, meta.directory)?;
            if record.item_id() != entries {
                return Err(TransferResult::InvalidData);
            }
            total = total
                .checked_add(record.size())
                .ok_or(TransferResult::TooLarge)?;
            if entries > budget.max_entries || total > budget.max_task_bytes {
                return Err(TransferResult::TooLarge);
            }
            // Retrying a selected failed item keeps task/item identity. Other entries
            // are replayed as well so the receiver can validate its durable journal.
            let result = match record {
                ManifestRecord::Entry(entry) => self.send_entry(id, &task_dir, &entry),
                ManifestRecord::Failure(failure) => self.send_scan_failure(id, &failure),
            };
            let status = match result {
                Ok(status) => status,
                Err(error) => error,
            };
            let p = self.current.as_mut().unwrap();
            p.scanned_items = p.scanned_items.max(entries);
            p.total_bytes = p.total_bytes.max(total);
            p.message = None;
            p.finish_item(status);
            self.persist();
            self.emit();
            result?;
        }
        self.wire(FileTaskMessage::ManifestComplete {
            task_id: id,
            entries,
            total_bytes: total,
        })?;
        match self.receive()? {
            FileTaskMessage::TaskResult {
                task_id,
                completed,
                failed,
            } if task_id == id
                && completed.checked_add(failed) == Some(entries)
                && self.current.as_ref().is_some_and(|progress| {
                    progress.completed_items == completed && progress.failed_items == failed
                }) =>
            {
                let p = self.current.as_mut().unwrap();
                p.completed_items = completed;
                p.failed_items = failed;
                p.state = if failed == 0 { "completed" } else { "partial" };
                self.emit();
                self.persist();
                Ok(())
            }
            _ => Err(TransferResult::InvalidData),
        }
    }
    fn send_entry(
        &mut self,
        id: TransferId,
        task_dir: &Path,
        source: &DirectoryEntry,
    ) -> Result<TransferResult, TransferResult> {
        self.current
            .as_mut()
            .unwrap()
            .item(&source.manifest(None), "scanning");
        self.emit();
        let prepared: Result<_, TransferResult> = (|| {
            let mut index = None;
            let mut guard = None;
            let mut directory_guard = None;
            if source.kind == TransferEntryKind::File {
                let path = task_dir.join(format!("{}.index", source.item_id));
                guard = Some(source.open_verified_source()?);
                let file = guard
                    .as_ref()
                    .unwrap()
                    .file
                    .try_clone()
                    .map_err(io_result)?;
                index = Some({
                    let cancellation = self.cancelled.clone();
                    let mut last = Instant::now();
                    let result = SourceIndex::recover_or_build_from_file(
                        file,
                        &path,
                        &cancellation,
                        |checked| {
                            let _ = self.check();
                            self.hash_progress(checked, source.size, true, &mut last);
                        },
                    );
                    if let Some(error) = self.activity_error.take() {
                        return Err(error);
                    }
                    result?
                });
            } else {
                directory_guard = Some(source.verify_directory_source()?);
            }
            Ok((index, guard, directory_guard))
        })();
        let (mut index, guard, directory_guard) = match prepared {
            Ok(value) => value,
            Err(error) => {
                if self.cancelled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
                    return Err(error);
                }
                self.wire(FileTaskMessage::EntrySkipped {
                    task_id: id,
                    item_id: source.item_id,
                    relative_path: source.relative_path.clone(),
                    size: source.size,
                    result: error,
                })?;
                return match self.receive()? {
                    FileTaskMessage::EntryResult {
                        task_id,
                        item_id,
                        result,
                    } if task_id == id && item_id == source.item_id && result == error => Ok(error),
                    _ => Err(TransferResult::InvalidData),
                };
            }
        };
        let entry = source.manifest(index.as_ref().map(SourceIndex::content_root));
        {
            let p = self.current.as_mut().unwrap();
            p.message = None;
            p.item(&entry, "waiting");
        }
        self.emit();
        self.wire(FileTaskMessage::Entry {
            task_id: id,
            entry: entry.clone(),
        })?;
        let checkpoint = match self.receive()? {
            FileTaskMessage::EntryDecision {
                task_id,
                item_id,
                result,
                checkpoint,
            } if task_id == id && item_id == entry.item_id => {
                if result != TransferResult::Completed {
                    return Ok(result);
                }
                checkpoint
            }
            _ => return Err(TransferResult::InvalidData),
        };
        let mut peer_finished = false;
        let body_result: Result<(), TransferResult> = (|| {
            if let Some(index) = index.as_mut() {
                if index.resume_proof(checkpoint.offset)? != checkpoint.proof {
                    return Err(TransferResult::SourceChanged);
                }
                self.resumed_progress(checkpoint.offset)?;
                let mut offset = checkpoint.offset;
                loop {
                    self.check()?;
                    let Some(block) = index.read_block(offset)? else {
                        break;
                    };
                    let end = offset + block.bytes.len() as u64;
                    self.wire(FileTaskMessage::Block {
                        task_id: id,
                        item_id: entry.item_id,
                        offset,
                        bytes: block.bytes,
                        digest: block.hash,
                    })?;
                    {
                        let p = self.current.as_mut().unwrap();
                        p.state = "sending";
                        let item = p.active_item.as_mut().unwrap();
                        item.state = "sending";
                        item.sent_bytes = end;
                        p.sent_bytes = p.sent_bytes.saturating_add(end - offset);
                    }
                    self.emit();
                    match self.receive()? {
                        FileTaskMessage::BlockAck {
                            task_id,
                            item_id,
                            checkpoint: c,
                        } if task_id == id
                            && item_id == entry.item_id
                            && c.offset == end
                            && c.proof == index.resume_proof(end)? => {}
                        FileTaskMessage::EntryResult {
                            task_id,
                            item_id,
                            result,
                        } if task_id == id
                            && item_id == entry.item_id
                            && result != TransferResult::Completed =>
                        {
                            peer_finished = true;
                            return Err(result);
                        }
                        _ => return Err(TransferResult::InvalidData),
                    };
                    {
                        let p = self.current.as_mut().unwrap();
                        p.active_item.as_mut().unwrap().acknowledged_bytes = end;
                        p.acknowledged_bytes = p.acknowledged_bytes.saturating_add(end - offset);
                    }
                    self.emit();
                    offset = end;
                }
                index.verify_source()?;
                self.current.as_mut().unwrap().state = "verifying";
                self.current
                    .as_mut()
                    .unwrap()
                    .active_item
                    .as_mut()
                    .unwrap()
                    .state = "verifying";
                self.emit();
                self.wire(FileTaskMessage::EntryComplete {
                    task_id: id,
                    item_id: entry.item_id,
                    content_root: index.content_root(),
                })?;
            } else {
                directory_guard
                    .as_ref()
                    .ok_or(TransferResult::InvalidData)?
                    .verify_location()?;
                self.wire(FileTaskMessage::EntryComplete {
                    task_id: id,
                    item_id: entry.item_id,
                    content_root: [0; 32],
                })?;
            }
            Ok(())
        })();
        drop(index);
        drop(guard);
        drop(directory_guard);
        if let Err(error) = body_result {
            if self.cancelled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
                return Err(error);
            }
            if peer_finished {
                return Ok(error);
            }
            self.wire(FileTaskMessage::EntryAbort {
                task_id: id,
                item_id: entry.item_id,
                result: error,
            })?;
            return match self.receive()? {
                FileTaskMessage::EntryResult {
                    task_id,
                    item_id,
                    result,
                } if task_id == id && item_id == entry.item_id && result == error => Ok(error),
                _ => Err(TransferResult::InvalidData),
            };
        }
        match self.receive()? {
            FileTaskMessage::EntryResult {
                task_id,
                item_id,
                result,
            } if task_id == id && item_id == entry.item_id => Ok(result),
            _ => Err(TransferResult::InvalidData),
        }
    }
    fn receive_task(
        &mut self,
        id: TransferId,
        name: String,
        directory: bool,
    ) -> Result<(), TransferResult> {
        let (budget, limits) = self.limits()?;
        if directory && !budget.directories {
            return Err(TransferResult::Unsupported);
        }
        if self.task_path(id).join("source.json").exists() {
            self.wire(FileTaskMessage::Decision {
                task_id: id,
                result: TransferResult::Rejected,
                budget,
            })?;
            return Ok(());
        }
        self.current = Some(FileTaskProgress::new(id, name.clone(), "download"));
        self.last_activity = None;
        self.activity_error = None;
        self.current.as_mut().unwrap().state = "waiting";
        self.emit();
        self.check()?;
        if !(self.options.authorize)(&name, directory, self.cancellation()) {
            self.wire(FileTaskMessage::Decision {
                task_id: id,
                result: TransferResult::Rejected,
                budget,
            })?;
            return Err(TransferResult::Rejected);
        }
        self.check()?;
        let task_guard = self
            .catalog
            .as_ref()
            .ok_or(TransferResult::IoFailed)?
            .ensure_task_dir(id)?;
        let task_dir = task_guard.path().to_owned();
        let meta_path = task_dir.join("receive.json");
        let mut destination = if meta_path.exists() {
            let m: ReceivedTask = read_json(&meta_path)?;
            if m.name != name
                || m.directory != directory
                || m.root_name != format!("DeskLink-{}", id_text(id))
            {
                return Err(TransferResult::InvalidData);
            }
            DirectoryDestination::open_existing(&self.options.receive_parent, &m.root_name, limits)?
        } else {
            let root_name = format!("DeskLink-{}", id_text(id));
            let destination =
                DirectoryDestination::create_new(&self.options.receive_parent, &root_name, limits)?;
            write_json_new(
                &meta_path,
                &ReceivedTask {
                    name,
                    directory,
                    root_name,
                },
            )?;
            destination
        };
        self.persist();
        self.wire(FileTaskMessage::Decision {
            task_id: id,
            result: TransferResult::Completed,
            budget,
        })?;
        let mut entries = 0_u64;
        let mut total = 0_u64;
        loop {
            match self.receive()? {
                FileTaskMessage::Entry { task_id, entry } if task_id == id => {
                    if entry.item_id != entries + 1 {
                        return Err(TransferResult::InvalidData);
                    }
                    entries += 1;
                    total = total
                        .checked_add(entry.size)
                        .ok_or(TransferResult::TooLarge)?;
                    if entries > budget.max_entries
                        || total > budget.max_task_bytes
                        || entry.size > budget.max_file_bytes
                        || (!directory && (entries != 1 || entry.kind != TransferEntryKind::File))
                    {
                        return Err(TransferResult::TooLarge);
                    }
                    let p = self.current.as_mut().unwrap();
                    p.scanned_items = entries;
                    p.total_bytes = total;
                    p.item(&entry, "receiving");
                    self.emit();
                    let result = self.receive_entry(id, &task_dir, &mut destination, &entry);
                    let status = match result {
                        Ok(status) => status,
                        Err(error) => error,
                    };
                    self.current.as_mut().unwrap().finish_item(status);
                    self.persist();
                    self.emit();
                    result?;
                }
                FileTaskMessage::EntrySkipped {
                    task_id,
                    item_id,
                    relative_path,
                    size,
                    result,
                } if task_id == id => {
                    if item_id != entries + 1
                        || result == TransferResult::Completed
                        || !desklink_protocol::is_valid_transfer_relative_path(&relative_path)
                    {
                        return Err(TransferResult::InvalidData);
                    }
                    entries += 1;
                    total = total.checked_add(size).ok_or(TransferResult::TooLarge)?;
                    if entries > budget.max_entries
                        || total > budget.max_task_bytes
                        || size > budget.max_file_bytes
                        || (!directory && entries != 1)
                    {
                        return Err(TransferResult::TooLarge);
                    }
                    let entry = ManifestEntry {
                        item_id,
                        relative_path,
                        kind: TransferEntryKind::File,
                        size,
                        content_root: None,
                    };
                    let p = self.current.as_mut().unwrap();
                    p.scanned_items = entries;
                    p.total_bytes = total;
                    p.item(&entry, "failed");
                    p.finish_item(result);
                    self.persist();
                    self.emit();
                    self.wire(FileTaskMessage::EntryResult {
                        task_id: id,
                        item_id,
                        result,
                    })?;
                }
                FileTaskMessage::ManifestComplete {
                    task_id,
                    entries: count,
                    total_bytes,
                } if task_id == id
                    && count == entries
                    && total_bytes == total
                    && (directory || entries == 1) =>
                {
                    let p = self.current.as_mut().unwrap();
                    p.scan_complete = true;
                    p.state = if p.failed_items == 0 {
                        "completed"
                    } else {
                        "partial"
                    };
                    let (completed, failed) = (p.completed_items, p.failed_items);
                    self.persist();
                    self.emit();
                    self.wire(FileTaskMessage::TaskResult {
                        task_id: id,
                        completed,
                        failed,
                    })?;
                    return Ok(());
                }
                _ => return Err(TransferResult::InvalidData),
            }
        }
    }
    fn entry_decision(
        &self,
        id: TransferId,
        item_id: u64,
        result: TransferResult,
        checkpoint: ResumeCheckpoint,
        phase: &Cell<u8>,
    ) -> Result<(), TransferResult> {
        // 0=not admitted, 1=admitted, 2=response send attempted, 3=closed.
        phase.set(2);
        self.wire(FileTaskMessage::EntryDecision {
            task_id: id,
            item_id,
            result,
            checkpoint,
        })?;
        phase.set(if result == TransferResult::Completed {
            1
        } else {
            3
        });
        Ok(())
    }
    fn entry_result(
        &self,
        id: TransferId,
        item_id: u64,
        result: TransferResult,
        phase: &Cell<u8>,
    ) -> Result<(), TransferResult> {
        phase.set(2);
        self.wire(FileTaskMessage::EntryResult {
            task_id: id,
            item_id,
            result,
        })?;
        phase.set(3);
        Ok(())
    }
    fn receive_entry(
        &mut self,
        id: TransferId,
        dir: &Path,
        destination: &mut DirectoryDestination,
        entry: &ManifestEntry,
    ) -> Result<TransferResult, TransferResult> {
        let phase = Cell::new(0);
        match self.receive_entry_inner(id, dir, destination, entry, &phase) {
            Ok(()) => Ok(TransferResult::Completed),
            Err(error) => {
                if self.cancelled.load(Ordering::Acquire) || self.stopped.load(Ordering::Acquire) {
                    return Err(error);
                }
                match phase.get() {
                    0 => self.entry_decision(
                        id,
                        entry.item_id,
                        error,
                        ResumeCheckpoint {
                            offset: 0,
                            proof: [0; 32],
                        },
                        &phase,
                    )?,
                    1 => self.entry_result(id, entry.item_id, error, &phase)?,
                    2 => return Err(error),
                    _ => {}
                }
                Ok(error)
            }
        }
    }
    fn receive_entry_inner(
        &mut self,
        id: TransferId,
        dir: &Path,
        destination: &mut DirectoryDestination,
        entry: &ManifestEntry,
        phase: &Cell<u8>,
    ) -> Result<(), TransferResult> {
        let meta_path = dir.join(format!("{}.received.json", entry.item_id));
        let previous = if meta_path.exists() {
            Some(read_json::<ReceivedEntry>(&meta_path)?)
        } else {
            None
        };
        if previous.as_ref().is_some_and(|p| p.entry != *entry) {
            return Err(TransferResult::SourceChanged);
        }
        if entry.kind == TransferEntryKind::Directory {
            if previous.is_none() {
                write_json_new(
                    &meta_path,
                    &ReceivedEntry {
                        entry: entry.clone(),
                        staging: None,
                        completed: false,
                    },
                )?;
            }
            if previous.is_some() && path_exists(&destination.root().join(&entry.relative_path))? {
                destination.validate_existing_directory(&entry.relative_path)?;
            } else {
                destination.create_directory(&entry.relative_path)?;
            }
            let destination_guard =
                GuardedSourceDirectory::open(&destination.root().join(&entry.relative_path))?;
            self.entry_decision(
                id,
                entry.item_id,
                TransferResult::Completed,
                ResumeCheckpoint {
                    offset: 0,
                    proof: [0; 32],
                },
                phase,
            )?;
            match self.receive()? {
                FileTaskMessage::EntryComplete {
                    task_id,
                    item_id,
                    content_root,
                } if task_id == id && item_id == entry.item_id && content_root == [0; 32] => {}
                FileTaskMessage::EntryAbort {
                    task_id,
                    item_id,
                    result,
                } if task_id == id
                    && item_id == entry.item_id
                    && result != TransferResult::Completed =>
                {
                    return Err(result);
                }
                _ => return Err(TransferResult::InvalidData),
            }
            self.check()?;
            destination_guard.verify_location()?;
            replace_json(
                &meta_path,
                &ReceivedEntry {
                    entry: entry.clone(),
                    staging: None,
                    completed: true,
                },
            )?;
            self.entry_result(id, entry.item_id, TransferResult::Completed, phase)?;
            return Ok(());
        }
        // Reconcile publication interrupted between hard-link creation and the
        // completed journal update. The guarded source pins every ancestor.
        if let Some(previous) = previous.as_ref()
            && (previous.completed || path_exists(&destination.root().join(&entry.relative_path))?)
        {
            let guard = destination.open_existing_file(&entry.relative_path)?;
            let file = guard.file.try_clone().map_err(io_result)?;
            let index_path = dir.join(format!("{}.published.index", entry.item_id));
            let index = {
                let cancellation = self.cancelled.clone();
                let mut last = Instant::now();
                let result = SourceIndex::recover_or_build_from_file(
                    file,
                    &index_path,
                    &cancellation,
                    |checked| {
                        let _ = self.check();
                        self.hash_progress(checked, entry.size, false, &mut last);
                    },
                );
                if let Some(error) = self.activity_error.take() {
                    return Err(error);
                }
                result?
            };
            if index.size() != entry.size || Some(index.content_root()) != entry.content_root {
                return Err(TransferResult::SourceChanged);
            }
            guard.verify_location()?;
            self.check()?;
            if !previous.completed {
                replace_json(
                    &meta_path,
                    &ReceivedEntry {
                        entry: entry.clone(),
                        staging: None,
                        completed: true,
                    },
                )?;
            }
            self.resumed_progress(entry.size)?;
            self.entry_decision(
                id,
                entry.item_id,
                TransferResult::Completed,
                ResumeCheckpoint {
                    offset: entry.size,
                    proof: index.content_root(),
                },
                phase,
            )?;
            match self.receive()? {
                FileTaskMessage::EntryComplete {
                    task_id,
                    item_id,
                    content_root,
                } if task_id == id
                    && item_id == entry.item_id
                    && Some(content_root) == entry.content_root => {}
                FileTaskMessage::EntryAbort {
                    task_id,
                    item_id,
                    result,
                } if task_id == id
                    && item_id == entry.item_id
                    && result != TransferResult::Completed =>
                {
                    return Err(result);
                }
                _ => return Err(TransferResult::InvalidData),
            }
            self.entry_result(id, entry.item_id, TransferResult::Completed, phase)?;
            return Ok(());
        }
        let missing_part = previous
            .as_ref()
            .is_some_and(|previous| previous.staging.as_ref().is_some_and(|path| !path.exists()));
        let opened = if let Some(previous) = previous.as_ref().filter(|_| !missing_part) {
            destination.resume_file(
                &entry.relative_path,
                previous
                    .staging
                    .as_ref()
                    .ok_or(TransferResult::InvalidData)?,
            )
        } else {
            destination.create_file(&entry.relative_path)
        };
        let mut part = opened?;
        let ledger_path = dir.join(format!("{}.ledger", entry.item_id));
        if missing_part {
            if path_exists(&ledger_path)? {
                cache_remove(&ledger_path)?;
            }
            replace_json(
                &meta_path,
                &ReceivedEntry {
                    entry: entry.clone(),
                    staging: Some(part.staging_path().to_owned()),
                    completed: false,
                },
            )?;
        }
        if previous.is_none() {
            write_json_new(
                &meta_path,
                &ReceivedEntry {
                    entry: entry.clone(),
                    staging: Some(part.staging_path().to_owned()),
                    completed: false,
                },
            )?;
        }
        let result = self.receive_file(id, entry, &ledger_path, &mut part, phase);
        match result {
            Ok(mut ledger) => {
                self.check()?;
                let cancellation = self.cancelled.clone();
                let mut last = Instant::now() - Duration::from_millis(100);
                let index_path = dir.join(format!("{}.published.index", entry.item_id));
                part.publish_with(|file| {
                    let result = ledger.export_verified_source_index(
                        file,
                        &index_path,
                        &cancellation,
                        |covered| {
                            if let Err(error) =
                                self.indexing_progress(covered, entry.size, &mut last)
                            {
                                self.activity_error = Some(error);
                                self.cancelled.store(true, Ordering::Release);
                            }
                        },
                    );
                    if let Some(error) = self.activity_error.take() {
                        return Err(error);
                    }
                    result.map(|_| ())
                })?;
                self.current.as_mut().unwrap().message = None;
                replace_json(
                    &meta_path,
                    &ReceivedEntry {
                        entry: entry.clone(),
                        staging: None,
                        completed: true,
                    },
                )?;
                self.entry_result(id, entry.item_id, TransferResult::Completed, phase)?;
                Ok(())
            }
            Err(e) => {
                if e != TransferResult::Cancelled || self.stopped.load(Ordering::Acquire) {
                    let _ = part.preserve();
                }
                Err(e)
            }
        }
    }
    fn receive_file(
        &mut self,
        id: TransferId,
        entry: &ManifestEntry,
        ledger_path: &Path,
        part: &mut GuardedDestinationFile,
        phase: &Cell<u8>,
    ) -> Result<CheckpointLedger, TransferResult> {
        let mut ledger = CheckpointLedger::open_or_create(
            ledger_path,
            part.file_mut()?,
            entry.size,
            entry.content_root.unwrap(),
        )?;
        let remaining_payload_bytes = entry.size.saturating_sub(ledger.resume_offset());
        let remaining_cache_bytes = ledger
            .remaining_journal_bytes()?
            .checked_add(source_index_bytes(entry.size)?)
            .ok_or(TransferResult::TooLarge)?;
        let same_volume = ledger.cache_shares_volume(part.file_mut()?)?;
        let cache_budget = remaining_cache_bytes
            .checked_add(if same_volume {
                remaining_payload_bytes
            } else {
                0
            })
            .ok_or(TransferResult::TooLarge)?;
        crate::transfer::ensure_receive_capacity(
            ledger_path.parent().ok_or(TransferResult::InvalidData)?,
            cache_budget,
        )?;
        crate::transfer::ensure_receive_capacity(
            part.staging_path()
                .parent()
                .ok_or(TransferResult::InvalidData)?,
            remaining_payload_bytes,
        )?;
        self.resumed_progress(ledger.resume_offset())?;
        self.entry_decision(
            id,
            entry.item_id,
            TransferResult::Completed,
            ResumeCheckpoint {
                offset: ledger.resume_offset(),
                proof: ledger.resume_proof(),
            },
            phase,
        )?;
        loop {
            match self.receive()? {
                FileTaskMessage::Block {
                    task_id,
                    item_id,
                    offset,
                    bytes,
                    digest,
                } if task_id == id && item_id == entry.item_id => {
                    ledger.write_block_unless_cancelled(
                        part.file_mut()?,
                        offset,
                        &bytes,
                        digest,
                        &self.cancelled,
                    )?;
                    {
                        let p = self.current.as_mut().unwrap();
                        let item = p.active_item.as_mut().unwrap();
                        item.sent_bytes = ledger.resume_offset();
                        item.acknowledged_bytes = ledger.resume_offset();
                        p.sent_bytes += bytes.len() as u64;
                        p.acknowledged_bytes += bytes.len() as u64;
                    }
                    self.emit();
                    self.wire(FileTaskMessage::BlockAck {
                        task_id: id,
                        item_id,
                        checkpoint: ResumeCheckpoint {
                            offset: ledger.resume_offset(),
                            proof: ledger.resume_proof(),
                        },
                    })?;
                }
                FileTaskMessage::EntryComplete {
                    task_id,
                    item_id,
                    content_root,
                } if task_id == id
                    && item_id == entry.item_id
                    && Some(content_root) == entry.content_root =>
                {
                    self.current.as_mut().unwrap().state = "verifying";
                    self.current
                        .as_mut()
                        .unwrap()
                        .active_item
                        .as_mut()
                        .unwrap()
                        .state = "verifying";
                    self.emit();
                    let cancellation = self.cancelled.clone();
                    let mut last = Instant::now();
                    let result = ledger.verify_all(part.file_mut()?, &cancellation, |checked| {
                        let _ = self.check();
                        self.hash_progress(checked, entry.size, false, &mut last);
                    });
                    if let Some(error) = self.activity_error.take() {
                        return Err(error);
                    }
                    result?;
                    self.current.as_mut().unwrap().message = None;
                    return Ok(ledger);
                }
                FileTaskMessage::EntryAbort {
                    task_id,
                    item_id,
                    result,
                } if task_id == id
                    && item_id == entry.item_id
                    && result != TransferResult::Completed =>
                {
                    return Err(result);
                }
                _ => return Err(TransferResult::InvalidData),
            }
        }
    }
}

fn io_result(_: std::io::Error) -> TransferResult {
    TransferResult::IoFailed
}
fn scan_limit_failure() -> DirectoryScanFailure {
    DirectoryScanFailure {
        relative_path: None,
        result: TransferResult::TooLarge,
        scope: DirectoryScanFailureScope::Task,
        kind: None,
        size: None,
        terminal: true,
        retry_relative_path: None,
    }
}
fn parse_id(value: &str) -> Result<TransferId, TransferResult> {
    if value.len() != 32 || !value.is_ascii() {
        return Err(TransferResult::InvalidData);
    }
    let mut id = [0; 16];
    for (byte, pair) in id.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let high = (pair[0] as char)
            .to_digit(16)
            .ok_or(TransferResult::InvalidData)?;
        let low = (pair[1] as char)
            .to_digit(16)
            .ok_or(TransferResult::InvalidData)?;
        *byte = ((high << 4) | low) as u8;
    }
    if id == [0; 16] {
        Err(TransferResult::InvalidData)
    } else {
        Ok(id)
    }
}
fn result_message(result: TransferResult) -> &'static str {
    match result {
        TransferResult::Completed => "已校验并保存。",
        TransferResult::Rejected => "接收端已拒绝此项。",
        TransferResult::Cancelled => "已取消。",
        TransferResult::TooLarge => "此项超过本次传输的资源上限。",
        TransferResult::InvalidData => "内容或断点校验失败，请重试。",
        TransferResult::PermissionDenied => "没有访问文件或目录的权限。",
        TransferResult::IoFailed => "读写失败，请检查磁盘和目录权限后重试。",
        TransferResult::InsufficientSpace => "磁盘空间不足，请清理后重试。",
        TransferResult::SourceChanged => "源文件已变化，请重新选择后发送。",
        TransferResult::Unsupported => "当前连接尚未支持此项功能。",
        TransferResult::Busy => "另一项文件任务尚在处理，请稍后重试。",
    }
}
fn path_exists(path: &Path) -> Result<bool, TransferResult> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_result(error)),
    }
}
fn write_line(file: &mut File, value: &impl Serialize) -> Result<(), TransferResult> {
    let bytes = encode_metadata(value, MAX_MANIFEST_JSON_BYTES - 1)?;
    file.write_all(&bytes).map_err(io_result)?;
    file.write_all(b"\n").map_err(io_result)
}
fn read_line<T: serde::de::DeserializeOwned>(
    reader: &mut impl BufRead,
) -> Result<Option<T>, TransferResult> {
    let mut bytes = Vec::new();
    let count = reader
        .take(MAX_MANIFEST_JSON_BYTES as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(io_result)?;
    if count == 0 {
        return Ok(None);
    }
    if bytes.last() != Some(&b'\n') {
        return Err(TransferResult::InvalidData);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| TransferResult::InvalidData)
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, TransferResult> {
    let f = cache_open_read(path)?;
    if f.metadata().map_err(io_result)?.len() > MAX_CACHE_JSON_BYTES as u64 {
        return Err(TransferResult::InvalidData);
    }
    serde_json::from_reader(f).map_err(|_| TransferResult::InvalidData)
}
fn write_json_new(path: &Path, value: &impl Serialize) -> Result<(), TransferResult> {
    write_json_atomic(path, value, false)
}
fn replace_json(path: &Path, value: &impl Serialize) -> Result<(), TransferResult> {
    write_json_atomic(path, value, true)
}

fn write_json_atomic(
    path: &Path,
    value: &impl Serialize,
    replace: bool,
) -> Result<(), TransferResult> {
    let bytes = encode_metadata(value, MAX_CACHE_JSON_BYTES)?;
    let mut nonce = [0; 16];
    rand_core::OsRng
        .try_fill_bytes(&mut nonce)
        .map_err(|_| TransferResult::IoFailed)?;
    let parent = path.parent().ok_or(TransferResult::InvalidData)?;
    let temporary = parent.join(format!(".desklink-metadata-{}.pending", id_text(nonce)));
    let result = (|| {
        let mut file = cache_create_new(&temporary)?;
        file.write_all(&bytes).map_err(io_result)?;
        file.sync_all().map_err(io_result)?;
        cache_publish_file(&file, &temporary, path, replace)
    })();
    if result.is_err() {
        let _ = cache_remove(&temporary);
    }
    result
}

struct LimitedMetadataWriter {
    bytes: Vec<u8>,
    maximum: usize,
}
impl Write for LimitedMetadataWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.maximum - self.bytes.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "file task metadata budget exceeded",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode_metadata(value: &impl Serialize, maximum: usize) -> Result<Vec<u8>, TransferResult> {
    let mut writer = LimitedMetadataWriter {
        bytes: Vec::with_capacity(maximum.min(4096)),
        maximum,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| TransferResult::InvalidData)?;
    Ok(writer.bytes)
}

fn cache_open_read(path: &Path) -> Result<File, TransferResult> {
    #[cfg(windows)]
    {
        crate::transfer_directory::native::open_file_path(path, false)
    }
    #[cfg(not(windows))]
    {
        File::open(path).map_err(io_result)
    }
}
fn cache_publish_file(
    file: &File,
    temporary: &Path,
    path: &Path,
    replace: bool,
) -> Result<(), TransferResult> {
    #[cfg(windows)]
    {
        let _ = temporary;
        crate::transfer_directory::native::rename_file_path(file, path, replace)
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        cache_rename(temporary, path, replace)
    }
}
fn cache_create_new(path: &Path) -> Result<File, TransferResult> {
    #[cfg(windows)]
    {
        crate::transfer_directory::native::create_file_path(path)
    }
    #[cfg(not(windows))]
    {
        std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(path)
            .map_err(io_result)
    }
}
fn cache_remove(path: &Path) -> Result<(), TransferResult> {
    #[cfg(windows)]
    {
        crate::transfer_directory::native::delete_path(path)
    }
    #[cfg(not(windows))]
    {
        fs::remove_file(path).map_err(io_result)
    }
}
#[cfg(not(windows))]
fn cache_rename(source: &Path, destination: &Path, replace: bool) -> Result<(), TransferResult> {
    if !replace && path_exists(destination)? {
        return Err(TransferResult::InvalidData);
    }
    fs::rename(source, destination).map_err(io_result)
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    fn worker(size: u64) -> Worker {
        let (_commands, pending) = sync::sync_channel(2);
        let (send, _outgoing) = mpsc::channel(2);
        let (progress_tx, _progress) = watch::channel(None);
        let mut current = FileTaskProgress::new([1; 16], "progress fixture".into(), "upload");
        current.total_bytes = size;
        current.scan_complete = true;
        current.item(
            &ManifestEntry {
                item_id: 1,
                relative_path: "中文.bin".into(),
                kind: TransferEntryKind::File,
                size,
                content_root: Some([1; 32]),
            },
            "waiting",
        );
        Worker {
            options: FileTaskOptions {
                peer_key: "aa".into(),
                cache_root: PathBuf::new(),
                receive_parent: PathBuf::new(),
                authorize: Arc::new(|_, _, _| true),
                select: None,
                authorize_source_resume: None,
            },
            pending,
            send,
            progress_tx,
            stopped: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            shared: std::sync::Weak::new(),
            peer: None,
            current: Some(current),
            pending_request: None,
            last_emission: None,
            revision: 0,
            catalog: None,
            responding_retry: None,
            last_activity: None,
            activity_error: None,
            scan_record_work: 0,
        }
    }

    #[test]
    fn resumed_prefix_counts_once_in_item_and_aggregate_without_narrowing() {
        let size = 4_294_967_297;
        let mut worker = worker(size);
        worker.resumed_progress(4_294_967_296).unwrap();
        worker.resumed_progress(4_294_967_296).unwrap();
        let progress = worker.current.as_ref().unwrap();
        assert_eq!(progress.sent_bytes, 4_294_967_296);
        assert_eq!(progress.acknowledged_bytes, 4_294_967_296);
        assert_eq!(
            progress.active_item.as_ref().unwrap().acknowledged_bytes,
            4_294_967_296
        );
        assert_eq!(
            worker.resumed_progress(size + 1),
            Err(TransferResult::InvalidData)
        );
        assert_eq!(worker.resumed_progress(0), Err(TransferResult::InvalidData));
    }

    #[test]
    fn hashing_heartbeat_is_throttled_and_never_advances_transfer_bytes() {
        let mut worker = worker(1_000);
        let mut last = Instant::now();
        for checked in 0..1_000 {
            worker.hash_progress(checked, 1_000, true, &mut last);
        }
        assert_eq!(worker.current.as_ref().unwrap().revision, 0);
        last = Instant::now() - Duration::from_millis(100);
        worker.hash_progress(999, 1_000, false, &mut last);
        let progress = worker.current.as_ref().unwrap();
        assert_eq!(progress.revision, 1);
        assert_eq!(progress.sent_bytes, 0);
        assert_eq!(progress.acknowledged_bytes, 0);
        assert_eq!(
            progress.message.as_deref(),
            Some("正在校验接收内容：已检查 999 / 1000 字节")
        );
    }

    #[test]
    fn failed_prehash_keeps_identity_and_result_window_is_bounded() {
        let mut progress = worker(1).current.unwrap();
        for item_id in 1..=1_000 {
            progress.item(
                &ManifestEntry {
                    item_id,
                    relative_path: format!("{item_id}.bin"),
                    kind: TransferEntryKind::File,
                    size: 1,
                    content_root: None,
                },
                "scanning",
            );
            progress.finish_item(TransferResult::SourceChanged);
        }
        assert_eq!(progress.failed_items, 1_000);
        assert_eq!(progress.items.len(), 64);
        assert_eq!(progress.items.front().unwrap().id, "937");
        assert_eq!(progress.items.back().unwrap().id, "1000");
        assert_eq!(progress.items.back().unwrap().state, "failed");
    }

    #[test]
    fn ordinary_block_progress_is_coalesced_and_retry_revisions_remain_monotonic() {
        let mut worker = worker(1_000);
        worker.emit();
        let before = worker.current.as_ref().unwrap().revision;
        for sent in 1..=1_000 {
            worker.current.as_mut().unwrap().sent_bytes = sent;
            worker.emit();
        }
        assert_eq!(worker.current.as_ref().unwrap().revision, before);
        worker.current = Some(FileTaskProgress::new([1; 16], "retry".into(), "upload"));
        worker.emit();
        assert!(worker.current.as_ref().unwrap().revision > before);
    }

    fn handle() -> (FileTaskHandle, mpsc::Receiver<FileTaskMessage>) {
        let (commands, _pending) = sync::sync_channel(2);
        let (requests, outgoing) = mpsc::channel(2);
        let (controls, _receiver) = watch::channel(None);
        let shared = Arc::new(Shared {
            stopped: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            action: AtomicU8::new(0),
            active: Mutex::new(None),
            controls,
            pending_request: AtomicU64::new(0),
            active_request: AtomicU64::new(0),
            selector_request: AtomicU64::new(0),
            cancelled_selection: AtomicU64::new(0),
            pending_retry: Mutex::new(None),
            cancelled_retry: Mutex::new(None),
            peer_capabilities: Mutex::new(None),
        });
        (
            FileTaskHandle {
                commands,
                shared,
                requests,
            },
            outgoing,
        )
    }

    #[test]
    fn cancelled_remote_request_rejects_late_offer_before_receiver_authorization() {
        let (handle, mut outgoing) = handle();
        handle.request_remote(9, true).unwrap();
        assert!(handle.cancel_remote().unwrap());
        assert!(matches!(
            outgoing.try_recv().unwrap(),
            FileTaskMessage::SelectionRequest {
                request_id: 9,
                directory: true
            }
        ));
        assert!(matches!(
            outgoing.try_recv().unwrap(),
            FileTaskMessage::SelectionCancel { request_id: 9 }
        ));
        handle
            .ingest(FileTaskMessage::Offer {
                task_id: [3; 16],
                request_id: Some(9),
                name: "late".into(),
                directory: true,
            })
            .unwrap();
        assert!(
            matches!(outgoing.try_recv().unwrap(),FileTaskMessage::Decision {task_id,result:TransferResult::Rejected,..} if task_id==[3;16])
        );
        assert_eq!(*handle.shared.active.lock().unwrap(), None);
        assert!(!handle.cancel_remote().unwrap());
    }

    #[test]
    fn selection_cancellation_is_bound_to_exact_request() {
        let (handle, _outgoing) = handle();
        handle.shared.selector_request.store(10, Ordering::Release);
        handle
            .ingest(FileTaskMessage::SelectionCancel { request_id: 9 })
            .unwrap();
        assert!(!handle.shared.cancelled.load(Ordering::Acquire));
        handle
            .ingest(FileTaskMessage::SelectionCancel { request_id: 10 })
            .unwrap();
        assert!(handle.shared.cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn a_long_pause_does_not_spend_the_response_timeout_budget() {
        let mut worker = worker(1);
        let (commands, pending) = sync::sync_channel(2);
        worker.pending = pending;
        let (controls, _receiver) = watch::channel(None);
        let shared = Arc::new(Shared {
            stopped: worker.stopped.clone(),
            cancelled: worker.cancelled.clone(),
            action: AtomicU8::new(1),
            active: Mutex::new(Some([1; 16])),
            controls,
            pending_request: AtomicU64::new(0),
            active_request: AtomicU64::new(0),
            selector_request: AtomicU64::new(0),
            cancelled_selection: AtomicU64::new(0),
            pending_retry: Mutex::new(None),
            cancelled_retry: Mutex::new(None),
            peer_capabilities: Mutex::new(None),
        });
        worker.shared = Arc::downgrade(&shared);
        let receive =
            std::thread::spawn(move || worker.receive_with_budget(Duration::from_millis(500)));
        std::thread::sleep(Duration::from_secs(1));
        shared.action.store(0, Ordering::Release);
        std::thread::sleep(Duration::from_millis(100));
        commands
            .send(Command::Wire(FileTaskMessage::TaskResult {
                task_id: [1; 16],
                completed: 0,
                failed: 0,
            }))
            .unwrap();
        assert!(
            matches!(receive.join().unwrap(),Ok(FileTaskMessage::TaskResult {task_id,..}) if task_id==[1;16])
        );
    }

    #[test]
    fn activity_requires_exact_task_phase_item_and_real_monotonic_progress() {
        let mut receiver = worker(100);
        receiver.peer = Some(FileTransferCapabilities::WINDOWS);
        {
            let p = receiver.current.as_mut().unwrap();
            p.direction = "download";
            p.active_item = None;
            p.scanned_items = 0;
        }
        let mut points = [None; 4];
        assert!(!receiver.accept_activity(
            [2; 16],
            Some(1),
            FileTaskPhase::Hashing,
            10,
            100,
            &mut points
        ));
        assert!(!receiver.accept_activity(
            [1; 16],
            Some(2),
            FileTaskPhase::Hashing,
            10,
            100,
            &mut points
        ));
        assert!(receiver.accept_activity(
            [1; 16],
            None,
            FileTaskPhase::Scanning,
            1,
            0,
            &mut points
        ));
        assert!(!receiver.accept_activity(
            [1; 16],
            None,
            FileTaskPhase::Scanning,
            1,
            0,
            &mut points
        ));
        assert!(receiver.accept_activity(
            [1; 16],
            Some(1),
            FileTaskPhase::Hashing,
            10,
            100,
            &mut points
        ));
        assert!(!receiver.accept_activity(
            [1; 16],
            Some(1),
            FileTaskPhase::Hashing,
            9,
            100,
            &mut points
        ));
        assert!(!receiver.accept_activity(
            [1; 16],
            Some(1),
            FileTaskPhase::Hashing,
            11,
            101,
            &mut points
        ));
        assert!(!receiver.accept_activity(
            [1; 16],
            None,
            FileTaskPhase::Scanning,
            2,
            0,
            &mut points
        ));
        assert!(!receiver.accept_activity(
            [1; 16],
            Some(1),
            FileTaskPhase::Verifying,
            12,
            100,
            &mut points
        ));
        assert_eq!(receiver.current.as_ref().unwrap().acknowledged_bytes, 0);
        let mut sender = worker(100);
        let mut verify = [None; 4];
        assert!(sender.accept_activity(
            [1; 16],
            Some(1),
            FileTaskPhase::Verifying,
            50,
            100,
            &mut verify
        ));
        sender.current.as_mut().unwrap().state = "sending";
        assert!(!sender.accept_activity(
            [1; 16],
            Some(1),
            FileTaskPhase::Verifying,
            51,
            100,
            &mut verify
        ));
    }

    #[test]
    fn repeated_manifest_passes_use_bounded_monotonic_work_without_inventing_file_or_byte_counts() {
        let mut receiver = worker(0);
        receiver.peer = Some(FileTransferCapabilities {
            max_entries: 2,
            ..FileTransferCapabilities::WINDOWS
        });
        let task = receiver.current.as_mut().unwrap();
        task.direction = "download";
        task.active_item = None;
        let mut points = [None; 4];
        let limit = 2 * MAX_FILE_TASK_SCAN_WORK_PASSES;
        assert!(receiver.accept_activity(
            [1; 16],
            None,
            FileTaskPhase::Scanning,
            limit - 1,
            0,
            &mut points
        ));
        assert!(receiver.accept_activity(
            [1; 16],
            None,
            FileTaskPhase::Scanning,
            limit,
            0,
            &mut points
        ));
        for completed in [0, limit - 1, limit, limit + 1, u64::MAX] {
            assert!(!receiver.accept_activity(
                [1; 16],
                None,
                FileTaskPhase::Scanning,
                completed,
                0,
                &mut points
            ));
        }
        let progress = receiver.current.as_ref().unwrap();
        assert_eq!(
            (
                progress.scanned_items,
                progress.sent_bytes,
                progress.acknowledged_bytes
            ),
            (0, 0, 0)
        );
        assert_eq!(progress.message.as_deref(), Some("对方正在检查文件列表…"));
        let mut sender = worker(0);
        sender.peer = receiver.peer;
        sender.last_activity = Some(SentActivity {
            at: Instant::now(),
            task_id: [1; 16],
            item_id: None,
            phase: FileTaskPhase::Scanning,
            completed: 0,
        });
        for _ in 0..limit {
            sender.record_scan_work().unwrap();
        }
        assert_eq!(sender.record_scan_work(), Err(TransferResult::TooLarge));
        assert_eq!(sender.scan_record_work, limit);
    }

    #[test]
    fn indexing_activity_counts_exact_active_proof_records_and_never_payload_bytes() {
        let mut sender = worker(u64::from(FILE_BLOCK_BYTES) * 2 + 1);
        let mut points = [None; 4];
        assert!(sender.accept_activity(
            [1; 16],
            Some(1),
            FileTaskPhase::Indexing,
            1,
            3,
            &mut points
        ));
        assert!(sender.accept_activity(
            [1; 16],
            Some(1),
            FileTaskPhase::Indexing,
            3,
            3,
            &mut points
        ));
        for (id, completed, total) in [(1, 3, 3), (1, 2, 3), (1, 4, 3), (1, 1, 4), (2, 1, 3)] {
            assert!(!sender.accept_activity(
                [1; 16],
                Some(id),
                FileTaskPhase::Indexing,
                completed,
                total,
                &mut points
            ));
        }
        let progress = sender.current.as_ref().unwrap();
        assert_eq!((progress.sent_bytes, progress.acknowledged_bytes), (0, 0));
        assert_eq!(progress.message.as_deref(), Some("对方正在完成文件保存…"));
        sender.current.as_mut().unwrap().state = "sending";
        assert!(!sender.accept_activity(
            [1; 16],
            Some(1),
            FileTaskPhase::Indexing,
            1,
            3,
            &mut [None; 4]
        ));
        let mut receiver = worker(100);
        receiver.current.as_mut().unwrap().direction = "download";
        assert!(!receiver.accept_activity(
            [1; 16],
            Some(1),
            FileTaskPhase::Indexing,
            1,
            1,
            &mut [None; 4]
        ));
    }

    #[test]
    fn metadata_limits_are_checked_before_any_file_write_and_admission_flag_is_native_only() {
        assert!(encode_metadata(&"x".repeat(MAX_CACHE_JSON_BYTES), MAX_CACHE_JSON_BYTES).is_err());
        assert_eq!(
            encode_metadata(&"中文", MAX_CACHE_JSON_BYTES).unwrap(),
            "\"中文\"".as_bytes()
        );
        let mut task = worker(1).current.unwrap();
        task.source_admitted = true;
        let json = serde_json::to_value(task).unwrap();
        assert!(json.get("sourceAdmitted").is_none());
    }

    #[test]
    fn typed_ingress_rejects_invalid_ids_and_freezes_the_first_capability_contract() {
        let (commands, pending) = sync::sync_channel(2);
        let (requests, _outgoing) = mpsc::channel(2);
        let (controls, _receiver) = watch::channel(None);
        let shared = Arc::new(Shared {
            stopped: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(AtomicBool::new(false)),
            action: AtomicU8::new(0),
            active: Mutex::new(None),
            controls,
            pending_request: AtomicU64::new(0),
            active_request: AtomicU64::new(0),
            selector_request: AtomicU64::new(0),
            cancelled_selection: AtomicU64::new(0),
            pending_retry: Mutex::new(None),
            cancelled_retry: Mutex::new(None),
            peer_capabilities: Mutex::new(None),
        });
        let handle = FileTaskHandle {
            commands,
            shared,
            requests,
        };
        assert_eq!(
            handle.ingest(FileTaskMessage::Offer {
                task_id: [0; 16],
                request_id: None,
                name: "invalid".into(),
                directory: false
            }),
            Err(TransferResult::InvalidData)
        );
        handle
            .ingest(FileTaskMessage::Capabilities(
                FileTransferCapabilities::WINDOWS,
            ))
            .unwrap();
        let mut changed = FileTransferCapabilities::WINDOWS;
        changed.max_file_bytes /= 2;
        handle
            .ingest(FileTaskMessage::Capabilities(changed))
            .unwrap();
        assert!(
            matches!(pending.try_recv().unwrap(),Command::Wire(FileTaskMessage::Capabilities(capabilities)) if capabilities==FileTransferCapabilities::WINDOWS)
        );
        assert!(pending.try_recv().is_err());
    }
}
