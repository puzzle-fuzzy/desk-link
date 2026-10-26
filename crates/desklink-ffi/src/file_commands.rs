//! Bounded file-transfer actor. It owns file IO independently of the controller
//! receive loop, so a slow disk or remote confirmation cannot block input/video.
use crate::file_io::{IncomingFile, OutgoingFile, ReadEvent, hex, read_outgoing};
use crate::{
    ControllerTransferSender, DesklinkEventKind, DesklinkResult, DesklinkState, EventMeta,
    worker::CallbackTarget,
};
use desklink_protocol::{TransferId, TransferMessage, TransferResult};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinHandle};

const QUEUE_CAPACITY: usize = 64;
const DECISION_TIMEOUT: Duration = Duration::from_secs(120);
const SELECTION_TIMEOUT: Duration = Duration::from_secs(300);
const ACTIVITY_TIMEOUT: Duration = Duration::from_secs(60);
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const RESULT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) enum FileAction {
    Send {
        transfer_id: TransferId,
        path: PathBuf,
    },
    Request {
        request_id: u64,
        destination: PathBuf,
    },
    CancelAll,
}
impl FileAction {
    pub(crate) fn fail_stale(&self, callback: CallbackTarget, stream_id: u64) {
        self.fail_with(callback, stream_id, "stale_session");
    }
    fn fail_with(&self, callback: CallbackTarget, stream_id: u64, result: &str) {
        let (id, request, direction, name) = match self {
            Self::Send { transfer_id, path } => (
                *transfer_id,
                0,
                "upload",
                path.file_name().and_then(|s| s.to_str()).unwrap_or("文件"),
            ),
            Self::Request { request_id, .. } => ([0; 16], *request_id, "download", "远端文件"),
            Self::CancelAll => return,
        };
        emit(
            callback, stream_id, id, request, "failed", direction, name, 0, 0, result, None,
        );
    }
}
enum Command {
    Action(FileAction),
    Peer(TransferMessage),
}
pub(crate) struct FileTransfers {
    callback: CallbackTarget,
    stream_id: u64,
    commands: mpsc::Sender<Command>,
    shutdown: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    operation_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    task: JoinHandle<()>,
}
impl FileTransfers {
    pub fn new(callback: CallbackTarget, sender: ControllerTransferSender, stream_id: u64) -> Self {
        Self::start(callback, Transport::Real(sender), stream_id)
    }
    fn start(callback: CallbackTarget, sender: Transport, stream_id: u64) -> Self {
        let (commands, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let shutdown = Arc::new(AtomicBool::new(false));
        let overflow = Arc::new(AtomicBool::new(false));
        let operation_cancel = Arc::new(Mutex::new(None));
        let task = tokio::spawn(
            Actor {
                receiver,
                sender,
                callback,
                stream_id,
                shutdown: shutdown.clone(),
                overflow: overflow.clone(),
                operation_cancel: operation_cancel.clone(),
                active: None,
                last_request_id: 0,
                used_ids: std::collections::HashSet::new(),
            }
            .run(),
        );
        Self {
            callback,
            stream_id,
            commands,
            shutdown,
            overflow,
            operation_cancel,
            task,
        }
    }
    pub fn execute(&self, action: FileAction) -> Result<(), DesklinkResult> {
        if matches!(action, FileAction::CancelAll)
            && let Some(token) = self.operation_cancel.lock().unwrap().as_ref()
        {
            token.store(true, Ordering::Release);
        }
        self.commands
            .try_send(Command::Action(action))
            .map_err(|error| {
                if let Command::Action(action) = error.into_inner() {
                    action.fail_with(self.callback, self.stream_id, "busy");
                }
                DesklinkResult::InternalError
            })
    }
    pub fn observe(&self, message: &TransferMessage) {
        if matches!(
            message,
            TransferMessage::ClipboardSet { .. }
                | TransferMessage::ClipboardRequest { .. }
                | TransferMessage::ClipboardData { .. }
                | TransferMessage::ClipboardResult { .. }
        ) {
            return;
        }
        // Reserve one queue slot for explicit cancellation even under a burst
        // of incoming chunks. Overflow terminates this transfer, not video.
        if self.commands.capacity() <= 1
            || self
                .commands
                .try_send(Command::Peer(message.clone()))
                .is_err()
        {
            self.overflow.store(true, Ordering::Release);
        }
    }
}
impl Drop for FileTransfers {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(token) = self.operation_cancel.lock().unwrap().as_ref() {
            token.store(true, Ordering::Release);
        }
        self.task.abort();
    }
}

#[derive(Clone)]
enum Transport {
    Real(ControllerTransferSender),
    #[cfg(test)]
    Test(mpsc::UnboundedSender<TransferMessage>),
}
impl Transport {
    async fn send(&self, message: TransferMessage) -> Result<(), &'static str> {
        match self {
            Self::Real(sender) => {
                match tokio::time::timeout(SEND_TIMEOUT, sender.send(message)).await {
                    Ok(Ok(())) => Ok(()),
                    Ok(Err(_)) => Err("transport_error"),
                    Err(_) => Err("timeout"),
                }
            }
            #[cfg(test)]
            Self::Test(sender) => sender.send(message).map_err(|_| "transport_error"),
        }
    }
}
#[derive(Clone)]
struct Metadata {
    id: TransferId,
    request: u64,
    direction: &'static str,
    name: String,
    total: u64,
    transferred: u64,
}
impl Metadata {
    fn emit(&self, a: &Actor, state: &str, result: &str, saved: Option<&str>) {
        emit(
            a.callback,
            a.stream_id,
            self.id,
            self.request,
            state,
            self.direction,
            &self.name,
            self.transferred,
            self.total,
            result,
            saved,
        );
    }
}
enum Active {
    Selection {
        request_id: u64,
        destination: PathBuf,
        deadline: Instant,
    },
    Upload {
        metadata: Metadata,
        file: Option<OutgoingFile>,
        cancel: Arc<AtomicBool>,
        deadline: Instant,
        phase: UploadPhase,
        progress: mpsc::Receiver<UploadEvent>,
        task: Option<JoinHandle<()>>,
        early_result: Option<TransferResult>,
    },
    Download {
        metadata: Metadata,
        file: IncomingFile,
        deadline: Instant,
    },
}
#[derive(Clone, Copy, PartialEq)]
enum UploadPhase {
    Decision,
    Sending,
    Result,
}
enum UploadEvent {
    Progress(u64),
    AwaitingResult,
    Failed(&'static str),
}
impl Active {
    fn metadata(&self) -> Option<&Metadata> {
        match self {
            Self::Upload { metadata, .. } | Self::Download { metadata, .. } => Some(metadata),
            _ => None,
        }
    }
    fn cancel(&mut self) {
        if let Self::Upload { cancel, task, .. } = self {
            cancel.store(true, Ordering::Release);
            if let Some(task) = task.take() {
                task.abort();
            }
        }
    }
}

struct Actor {
    receiver: mpsc::Receiver<Command>,
    sender: Transport,
    callback: CallbackTarget,
    stream_id: u64,
    shutdown: Arc<AtomicBool>,
    overflow: Arc<AtomicBool>,
    operation_cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    active: Option<Active>,
    last_request_id: u64,
    used_ids: std::collections::HashSet<TransferId>,
}
impl Drop for Actor {
    fn drop(&mut self) {
        if let Some(active) = self.active.as_mut() {
            active.cancel();
        }
    }
}
impl Actor {
    async fn run(mut self) {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                command=self.receiver.recv()=>match command{Some(Command::Action(action))=>self.action(action).await,Some(Command::Peer(message))=>self.peer(message).await,None=>break},
                _=tick.tick()=>self.tick().await,
            }
            if self.shutdown.load(Ordering::Acquire) {
                break;
            }
            if self.overflow.swap(false, Ordering::AcqRel) {
                self.stop("failed", "queue_overflow", true).await;
                while let Ok(command) = self.receiver.try_recv() {
                    if let Command::Action(action) = command {
                        action.fail_stale(self.callback, self.stream_id);
                    }
                }
            }
        }
    }
    async fn action(&mut self, action: FileAction) {
        if matches!(action, FileAction::CancelAll) {
            self.stop("cancelled", "cancelled", true).await;
            return;
        }
        if self.active.is_some() {
            self.action_failure(&action, "busy");
            return;
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        *self.operation_cancel.lock().unwrap() = Some(cancellation.clone());
        match action {
            FileAction::Send { transfer_id, path } => {
                let action = FileAction::Send {
                    transfer_id,
                    path: path.clone(),
                };
                if transfer_id == [0; 16] || self.used_ids.contains(&transfer_id) {
                    self.action_failure(&action, "invalid_data");
                    return;
                }
                // Cap lifetime metadata even when a hostile UI repeatedly retries.
                if self.used_ids.len() >= 1024 {
                    self.action_failure(&action, "busy");
                    return;
                }
                self.used_ids.insert(transfer_id);
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("文件")
                    .to_owned();
                emit(
                    self.callback,
                    self.stream_id,
                    transfer_id,
                    0,
                    "preparing",
                    "upload",
                    &name,
                    0,
                    0,
                    "pending",
                    None,
                );
                let file =
                    match tokio::task::spawn_blocking(move || OutgoingFile::open(&path)).await {
                        Ok(Ok(file)) => file,
                        Ok(Err(result)) => {
                            self.action_failure(&action, result_name(result));
                            return;
                        }
                        Err(_) => {
                            self.action_failure(&action, "io_failed");
                            return;
                        }
                    };
                let metadata = Metadata {
                    id: transfer_id,
                    request: 0,
                    direction: "upload",
                    name: file.name.clone(),
                    total: file.size,
                    transferred: 0,
                };
                if cancellation.load(Ordering::Acquire) {
                    metadata.emit(self, "cancelled", "cancelled", None);
                    return;
                }
                if let Err(result) = self
                    .sender
                    .send(TransferMessage::FileOffer {
                        transfer_id,
                        request_id: None,
                        name: file.name.clone(),
                        size: file.size,
                    })
                    .await
                {
                    metadata.emit(self, "failed", result, None);
                    return;
                }
                metadata.emit(self, "waiting", "pending", None);
                let (_, progress) = mpsc::channel(1);
                self.active = Some(Active::Upload {
                    metadata,
                    file: Some(file),
                    cancel: cancellation,
                    deadline: Instant::now() + DECISION_TIMEOUT,
                    phase: UploadPhase::Decision,
                    progress,
                    task: None,
                    early_result: None,
                });
            }
            FileAction::Request {
                request_id,
                destination,
            } => {
                let action = FileAction::Request {
                    request_id,
                    destination: destination.clone(),
                };
                if request_id == 0 || request_id <= self.last_request_id {
                    self.action_failure(&action, "invalid_data");
                    return;
                }
                self.last_request_id = request_id;
                // Validate the chosen directory before asking the remote user.
                let checked = destination.clone();
                if !tokio::task::spawn_blocking(move || validate_destination(&checked))
                    .await
                    .unwrap_or(false)
                {
                    self.action_failure(&action, "invalid_data");
                    return;
                }
                if cancellation.load(Ordering::Acquire) {
                    self.action_failure(&action, "cancelled");
                    return;
                }
                if let Err(result) = self
                    .sender
                    .send(TransferMessage::FileSelectionRequest {
                        request_id,
                        resume: None,
                    })
                    .await
                {
                    self.action_failure(&action, result);
                    return;
                }
                emit(
                    self.callback,
                    self.stream_id,
                    [0; 16],
                    request_id,
                    "waiting",
                    "download",
                    "远端文件",
                    0,
                    0,
                    "pending",
                    None,
                );
                self.active = Some(Active::Selection {
                    request_id,
                    destination,
                    deadline: Instant::now() + SELECTION_TIMEOUT,
                });
            }
            FileAction::CancelAll => unreachable!(),
        }
    }
    fn action_failure(&self, action: &FileAction, result: &str) {
        match action {
            FileAction::Send { transfer_id, path } => emit(
                self.callback,
                self.stream_id,
                *transfer_id,
                0,
                "failed",
                "upload",
                path.file_name().and_then(|s| s.to_str()).unwrap_or("文件"),
                0,
                0,
                result,
                None,
            ),
            FileAction::Request { request_id, .. } => emit(
                self.callback,
                self.stream_id,
                [0; 16],
                *request_id,
                "failed",
                "download",
                "远端文件",
                0,
                0,
                result,
                None,
            ),
            FileAction::CancelAll => {}
        }
    }
    async fn peer(&mut self, message: TransferMessage) {
        if self.active.is_some()
            && self
                .operation_cancel
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Acquire))
        {
            self.stop("cancelled", "cancelled", true).await;
        }
        match message {
            TransferMessage::FileOffer { transfer_id, request_id, name, size } => {
                let requested = self.active.as_ref().is_some_and(|a| matches!(a, Active::Selection { request_id: pending, .. } if Some(*pending) == request_id));
                if !requested {
                    let _ = self.sender.send(TransferMessage::FileDecision { transfer_id, result: TransferResult::Rejected, resume_offset: 0, resume_prefix_hash: None }).await;
                    return;
                }
                let Active::Selection { request_id, destination, .. } = self.active.take().expect("matching selection") else { unreachable!() };
                let metadata = Metadata { id: transfer_id, request: request_id, direction: "download", name: name.clone(), total: size, transferred: 0 };
                let result = tokio::task::spawn_blocking(move || IncomingFile::create(&destination, transfer_id, name, size)).await.unwrap_or(Err(TransferResult::IoFailed));
                match result {
                    Ok(file) => {
                        if let Err(result) = self.sender.send(TransferMessage::FileDecision { transfer_id, result: TransferResult::Completed, resume_offset: 0, resume_prefix_hash: None }).await {
                            metadata.emit(self, "failed", result, None);
                            return;
                        }
                        metadata.emit(self, "receiving", "pending", None);
                        self.active = Some(Active::Download { metadata, file, deadline: Instant::now() + ACTIVITY_TIMEOUT });
                    }
                    Err(result) => {
                        let _ = self.sender.send(TransferMessage::FileDecision { transfer_id, result, resume_offset: 0, resume_prefix_hash: None }).await;
                        metadata.emit(self, "failed", result_name(result), None);
                    }
                }
            }
            TransferMessage::FileSelectionResult { request_id, result } => {
                if self.active.as_ref().is_some_and(|a| matches!(a, Active::Selection { request_id: pending, .. } if *pending == request_id)) {
                    self.active.take();
                    let result = if result == TransferResult::Completed { TransferResult::InvalidData } else { result };
                    emit(self.callback, self.stream_id, [0; 16], request_id, terminal_state(result), "download", "远端文件", 0, 0, result_name(result), None);
                }
            }
            TransferMessage::FileDecision { transfer_id, result, resume_offset, resume_prefix_hash } => {
                let awaiting = self.active.as_ref().is_some_and(|a| matches!(a, Active::Upload { metadata, phase: UploadPhase::Decision, .. } if metadata.id == transfer_id));
                if !awaiting { return; }
                let Some(Active::Upload { metadata, file, cancel, deadline, phase, progress, task, .. }) = self.active.as_mut() else { unreachable!() };
                if result != TransferResult::Completed || resume_offset > metadata.total || ((resume_offset == 0) != resume_prefix_hash.is_none()) {
                    let invalid = result == TransferResult::Completed;
                    let result = if invalid { TransferResult::InvalidData } else { result };
                    self.stop(terminal_state(result), result_name(result), invalid).await;
                    return;
                }
                let source = file.take().expect("outgoing source");
                let (events, receiver) = mpsc::channel(4);
                *progress = receiver;
                *phase = UploadPhase::Sending;
                *deadline = Instant::now() + ACTIVITY_TIMEOUT;
                metadata.transferred = resume_offset;
                let sender = self.sender.clone();
                let cancel = cancel.clone();
                let id = metadata.id;
                *task = Some(tokio::spawn(async move { send_outgoing(sender, source, id, resume_offset, resume_prefix_hash, cancel, events).await }));
                self.active.as_ref().and_then(Active::metadata).expect("upload metadata").emit(self, "sending", "pending", None);
            }
            TransferMessage::FileChunk { transfer_id, offset, bytes } => {
                if !self.active.as_ref().is_some_and(|a| matches!(a, Active::Download { metadata, .. } if metadata.id == transfer_id)) { return; }
                let Active::Download { mut metadata, mut file, .. } = self.active.take().expect("matching download") else { unreachable!() };
                let result = tokio::task::spawn_blocking(move || { let result = file.write_chunk(offset, &bytes); (file, result) }).await;
                match result {
                    Ok((file, Ok(()))) => {
                        metadata.transferred = file.written;
                        metadata.emit(self, "receiving", "pending", None);
                        self.active = Some(Active::Download { metadata, file, deadline: Instant::now() + ACTIVITY_TIMEOUT });
                    }
                    Ok((file, Err(result))) => {
                        drop(file);
                        let _ = self.sender.send(TransferMessage::FileResult { transfer_id, result }).await;
                        metadata.emit(self, "failed", result_name(result), None);
                    }
                    Err(_) => {
                        let _ = self.sender.send(TransferMessage::FileResult { transfer_id, result: TransferResult::IoFailed }).await;
                        metadata.emit(self, "failed", "io_failed", None);
                    }
                }
            }
            TransferMessage::FileComplete { transfer_id, content_hash } => {
                if !self.active.as_ref().is_some_and(|a| matches!(a, Active::Download { metadata, .. } if metadata.id == transfer_id)) { return; }
                let Active::Download { mut metadata, file, .. } = self.active.take().expect("matching download") else { unreachable!() };
                metadata.emit(self, "verifying", "pending", None);
                let cancel = self.operation_cancel.lock().unwrap().as_ref().expect("current operation token").clone();
                let result = tokio::task::spawn_blocking(move || file.finish(content_hash, &cancel)).await.unwrap_or(Err(TransferResult::IoFailed));
                match result {
                    Ok(path) => {
                        let _ = self.sender.send(TransferMessage::FileResult { transfer_id, result: TransferResult::Completed }).await;
                        metadata.transferred = metadata.total;
                        metadata.emit(self, "completed", "completed", path.to_str());
                    }
                    Err(result) => {
                        let _ = self.sender.send(TransferMessage::FileResult { transfer_id, result }).await;
                        metadata.emit(self, terminal_state(result), result_name(result), None);
                    }
                }
            }
            TransferMessage::FileResult { transfer_id, result } => {
                if let Some(Active::Upload { metadata, phase, early_result, .. }) = self.active.as_mut() {
                    if metadata.id != transfer_id { return; }
                    if *phase == UploadPhase::Sending {
                        if result == TransferResult::Completed { *early_result = Some(result); }
                        else { self.stop(terminal_state(result), result_name(result), false).await; }
                        return;
                    }
                    if *phase == UploadPhase::Result {
                        if result == TransferResult::Completed { metadata.transferred = metadata.total; }
                        self.stop(terminal_state(result), result_name(result), false).await;
                    }
                }
            }
            TransferMessage::Cancel { transfer_id } => {
                if self.active.as_ref().and_then(Active::metadata).is_some_and(|m| m.id == transfer_id) {
                    self.stop("cancelled", "cancelled", false).await;
                }
            }
            TransferMessage::FileSelectionRequest { request_id, .. } => {
                let _ = self.sender.send(TransferMessage::FileSelectionResult { request_id, result: TransferResult::Unsupported }).await;
            }
            _ => {}
        }
    }
    async fn tick(&mut self) {
        if self.active.is_some()
            && self
                .operation_cancel
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Acquire))
        {
            self.stop("cancelled", "cancelled", true).await;
            return;
        }
        let mut terminal = None;
        let mut progress_snapshot = None;
        if let Some(Active::Upload {
            metadata,
            phase,
            deadline,
            progress,
            early_result,
            ..
        }) = self.active.as_mut()
        {
            while let Ok(event) = progress.try_recv() {
                match event {
                    UploadEvent::Progress(transferred) => {
                        metadata.transferred = transferred;
                        *deadline = Instant::now() + ACTIVITY_TIMEOUT;
                    }
                    UploadEvent::AwaitingResult => {
                        *phase = UploadPhase::Result;
                        metadata.transferred = metadata.total;
                        *deadline = Instant::now() + RESULT_TIMEOUT;
                        if let Some(result) = early_result.take() {
                            terminal = Some((terminal_state(result), result_name(result)));
                        }
                    }
                    UploadEvent::Failed(result) => terminal = Some(("failed", result)),
                }
            }
            if terminal.is_none() {
                let state = if *phase == UploadPhase::Result {
                    "verifying"
                } else if *phase == UploadPhase::Sending {
                    "sending"
                } else {
                    "waiting"
                };
                progress_snapshot = Some((metadata.clone(), state));
            }
        }
        if let Some((metadata, state)) = progress_snapshot {
            metadata.emit(self, state, "pending", None);
        }
        if let Some((state, result)) = terminal {
            self.stop(state, result, false).await;
            return;
        }
        let expired = self.active.as_ref().is_some_and(|a| match a {
            Active::Selection { deadline, .. }
            | Active::Upload { deadline, .. }
            | Active::Download { deadline, .. } => *deadline <= Instant::now(),
        });
        if expired {
            self.stop("failed", "timeout", true).await;
        }
    }
    async fn stop(&mut self, state: &str, result: &str, notify: bool) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        active.cancel();
        match &active {
            Active::Selection { request_id, .. } => {
                if notify {
                    let _ = self
                        .sender
                        .send(TransferMessage::FileSelectionCancel {
                            request_id: *request_id,
                        })
                        .await;
                }
                emit(
                    self.callback,
                    self.stream_id,
                    [0; 16],
                    *request_id,
                    state,
                    "download",
                    "远端文件",
                    0,
                    0,
                    result,
                    None,
                );
            }
            Active::Upload { metadata, .. } | Active::Download { metadata, .. } => {
                if notify {
                    let _ = self
                        .sender
                        .send(TransferMessage::Cancel {
                            transfer_id: metadata.id,
                        })
                        .await;
                }
                metadata.emit(self, state, result, None);
            }
        }
    }
}
// Validating a directory creates nothing. IncomingFile repeats descriptor-based
// validation immediately before accepting the remote offer.
fn validate_destination(destination: &std::path::Path) -> bool {
    destination.is_absolute()
        && std::fs::symlink_metadata(destination)
            .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
}
async fn send_outgoing(
    sender: Transport,
    file: OutgoingFile,
    id: TransferId,
    resume_offset: u64,
    prefix: Option<[u8; 32]>,
    cancel: Arc<AtomicBool>,
    events: mpsc::Sender<UploadEvent>,
) {
    let (chunks, mut receiver) = mpsc::channel(2);
    let producer_cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        read_outgoing(file, resume_offset, prefix, producer_cancel, chunks)
    });
    while let Some(event) = receiver.recv().await {
        if cancel.load(Ordering::Acquire) {
            return;
        }
        let result: Result<(), &'static str> = match event {
            ReadEvent::Chunk { offset, bytes } => {
                let end = offset + bytes.len() as u64;
                match sender
                    .send(TransferMessage::FileChunk {
                        transfer_id: id,
                        offset,
                        bytes,
                    })
                    .await
                {
                    Ok(()) => {
                        // Progress is cosmetic. Coalesce it under a fast send
                        // instead of making the disk/network wait for the UI.
                        if events.is_closed() {
                            return;
                        }
                        let _ = events.try_send(UploadEvent::Progress(end));
                        continue;
                    }
                    Err(result) => Err(result),
                }
            }
            ReadEvent::Complete(content_hash) => {
                match sender
                    .send(TransferMessage::FileComplete {
                        transfer_id: id,
                        content_hash,
                    })
                    .await
                {
                    Ok(()) => {
                        let _ = events.send(UploadEvent::AwaitingResult).await;
                        return;
                    }
                    Err(result) => Err(result),
                }
            }
            ReadEvent::Failed(result) => Err(result_name(result)),
        };
        if let Err(result) = result {
            let _ = sender
                .send(TransferMessage::Cancel { transfer_id: id })
                .await;
            let _ = events.send(UploadEvent::Failed(result)).await;
            return;
        }
    }
}
#[allow(clippy::too_many_arguments)]
fn emit(
    callback: CallbackTarget,
    stream: u64,
    id: TransferId,
    request: u64,
    state: &str,
    direction: &str,
    name: &str,
    transferred: u64,
    total: u64,
    result: &str,
    saved: Option<&str>,
) {
    let mut value = json!({"kind":"file_transfer","transfer_id":if id==[0;16]{String::new()}else{hex(&id)},"request_id":request,"state":state,"direction":direction,"name":name,"transferred":transferred,"total":total,"result":result});
    if let Some(saved) = saved {
        value["saved_path"] = json!(saved);
    }
    if let Ok(bytes) = serde_json::to_vec(&value) {
        callback.emit(
            DesklinkEventKind::File,
            &bytes,
            EventMeta::for_stream(stream),
            DesklinkState::Connected,
        );
    }
}
fn terminal_state(result: TransferResult) -> &'static str {
    match result {
        TransferResult::Completed => "completed",
        TransferResult::Rejected => "rejected",
        TransferResult::Cancelled => "cancelled",
        _ => "failed",
    }
}
fn result_name(result: TransferResult) -> &'static str {
    match result {
        TransferResult::Completed => "completed",
        TransferResult::Rejected => "rejected",
        TransferResult::Cancelled => "cancelled",
        TransferResult::TooLarge => "too_large",
        TransferResult::InvalidData => "invalid_data",
        TransferResult::PermissionDenied => "permission_denied",
        TransferResult::IoFailed => "io_failed",
        TransferResult::InsufficientSpace => "insufficient_space",
        TransferResult::SourceChanged => "source_changed",
        TransferResult::Unsupported => "unsupported",
        TransferResult::Busy => "busy",
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use blake2::{Blake2s256, Digest};
    type Events = Mutex<Vec<serde_json::Value>>;
    extern "C" fn record(context: *mut std::ffi::c_void, event: *const crate::DesklinkEvent) {
        let event = unsafe { &*event };
        let events = unsafe { &*context.cast::<Events>() };
        let bytes = unsafe { std::slice::from_raw_parts(event.data, event.data_len) };
        events
            .lock()
            .unwrap()
            .push(serde_json::from_slice(bytes).unwrap());
    }
    struct Rig {
        files: FileTransfers,
        wire: mpsc::UnboundedReceiver<TransferMessage>,
        events: Box<Events>,
        root: PathBuf,
    }
    impl Rig {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "desklink-file-actor-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir(&root).unwrap();
            let events = Box::<Events>::default();
            let callback = CallbackTarget {
                callback: Some(record),
                context: (&*events as *const Events) as usize,
            };
            let (tx, wire) = mpsc::unbounded_channel();
            let files = FileTransfers::start(callback, Transport::Test(tx), 44);
            Self {
                files,
                wire,
                events,
                root,
            }
        }
        async fn next(&mut self) -> TransferMessage {
            tokio::time::timeout(Duration::from_secs(3), self.wire.recv())
                .await
                .unwrap()
                .unwrap()
        }
        async fn wait_state(&self, state: &str) {
            for _ in 0..100 {
                if self
                    .events
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|v| v["state"] == state)
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("missing {state}: {:?}", self.events.lock().unwrap());
        }
        async fn close(mut self) {
            self.files.shutdown.store(true, Ordering::Release);
            if !self.files.task.is_finished() {
                self.files.task.abort();
                let _ = (&mut self.files.task).await;
            }
            std::fs::remove_dir_all(&self.root).unwrap();
        }
        async fn request(&mut self, id: u64) {
            self.files
                .execute(FileAction::Request {
                    request_id: id,
                    destination: self.root.clone(),
                })
                .unwrap();
            assert_eq!(
                self.next().await,
                TransferMessage::FileSelectionRequest {
                    request_id: id,
                    resume: None
                }
            );
        }
        async fn offer(&mut self, request: u64, id: TransferId, name: &str, size: u64) {
            self.files.observe(&TransferMessage::FileOffer {
                transfer_id: id,
                request_id: Some(request),
                name: name.into(),
                size,
            });
            assert_eq!(
                self.next().await,
                TransferMessage::FileDecision {
                    transfer_id: id,
                    result: TransferResult::Completed,
                    resume_offset: 0,
                    resume_prefix_hash: None
                }
            );
        }
    }
    #[tokio::test]
    async fn unsolicited_and_wrong_request_offers_are_rejected_without_touching_disk() {
        let mut rig = Rig::new("unsolicited");
        rig.files.observe(&TransferMessage::FileOffer {
            transfer_id: [1; 16],
            request_id: None,
            name: "secret.txt".into(),
            size: 7,
        });
        assert!(matches!(
            rig.next().await,
            TransferMessage::FileDecision {
                result: TransferResult::Rejected,
                ..
            }
        ));
        assert_eq!(std::fs::read_dir(&rig.root).unwrap().count(), 0);
        rig.request(1).await;
        rig.files.observe(&TransferMessage::FileOffer {
            transfer_id: [2; 16],
            request_id: Some(99),
            name: "wrong.txt".into(),
            size: 0,
        });
        assert!(matches!(
            rig.next().await,
            TransferMessage::FileDecision {
                result: TransferResult::Rejected,
                ..
            }
        ));
        rig.offer(1, [3; 16], "empty.txt", 0).await;
        rig.files.observe(&TransferMessage::FileComplete {
            transfer_id: [3; 16],
            content_hash: Blake2s256::digest([]).into(),
        });
        assert!(matches!(
            rig.next().await,
            TransferMessage::FileResult {
                result: TransferResult::Completed,
                ..
            }
        ));
        rig.wait_state("completed").await;
        assert_eq!(std::fs::read(rig.root.join("empty.txt")).unwrap(), b"");
        rig.close().await;
    }
    #[tokio::test]
    async fn download_requires_order_and_hash_then_cleans_for_explicit_retry() {
        let mut rig = Rig::new("invalid");
        rig.request(1).await;
        rig.offer(1, [1; 16], "bad.txt", 3).await;
        rig.files.observe(&TransferMessage::FileChunk {
            transfer_id: [1; 16],
            offset: 1,
            bytes: b"abc".to_vec(),
        });
        assert!(matches!(
            rig.next().await,
            TransferMessage::FileResult {
                result: TransferResult::InvalidData,
                ..
            }
        ));
        rig.wait_state("failed").await;
        assert_eq!(std::fs::read_dir(&rig.root).unwrap().count(), 0);
        rig.request(2).await;
        rig.offer(2, [2; 16], "good.txt", 3).await;
        rig.files.observe(&TransferMessage::FileChunk {
            transfer_id: [2; 16],
            offset: 0,
            bytes: b"abc".to_vec(),
        });
        rig.files.observe(&TransferMessage::FileComplete {
            transfer_id: [2; 16],
            content_hash: [0; 32],
        });
        assert!(matches!(
            rig.next().await,
            TransferMessage::FileResult {
                result: TransferResult::InvalidData,
                ..
            }
        ));
        assert!(!rig.root.join("good.txt").exists());
        rig.request(3).await;
        rig.offer(3, [3; 16], "good.txt", 3).await;
        rig.files.observe(&TransferMessage::FileChunk {
            transfer_id: [3; 16],
            offset: 0,
            bytes: b"abc".to_vec(),
        });
        rig.files.observe(&TransferMessage::FileComplete {
            transfer_id: [3; 16],
            content_hash: Blake2s256::digest(b"abc").into(),
        });
        assert!(matches!(
            rig.next().await,
            TransferMessage::FileResult {
                result: TransferResult::Completed,
                ..
            }
        ));
        rig.wait_state("completed").await;
        assert_eq!(std::fs::read(rig.root.join("good.txt")).unwrap(), b"abc");
        let event = rig
            .events
            .lock()
            .unwrap()
            .iter()
            .find(|v| v["state"] == "completed")
            .unwrap()
            .clone();
        assert_eq!(event["request_id"], 3);
        assert_eq!(event["transferred"], 3);
        assert!(event["saved_path"].as_str().unwrap().ends_with("good.txt"));
        rig.close().await;
    }
    #[tokio::test]
    async fn upload_uses_separate_bounded_reader_and_waits_for_peer_integrity_ack() {
        let mut rig = Rig::new("upload");
        let bytes = vec![42; desklink_protocol::MAX_TRANSFER_CHUNK_BYTES * 2 + 7];
        let source = rig.root.join("发送.bin");
        std::fs::write(&source, &bytes).unwrap();
        rig.files
            .execute(FileAction::Send {
                transfer_id: [4; 16],
                path: source,
            })
            .unwrap();
        assert!(
            matches!(rig.next().await,TransferMessage::FileOffer{transfer_id,request_id:None,size,..} if transfer_id==[4;16] && size==bytes.len() as u64)
        );
        rig.files.observe(&TransferMessage::FileDecision {
            transfer_id: [4; 16],
            result: TransferResult::Completed,
            resume_offset: 0,
            resume_prefix_hash: None,
        });
        let mut received = Vec::new();
        loop {
            match rig.next().await {
                TransferMessage::FileChunk { offset, bytes, .. } => {
                    assert_eq!(offset, received.len() as u64);
                    assert!(bytes.len() <= desklink_protocol::MAX_TRANSFER_CHUNK_BYTES);
                    received.extend(bytes);
                }
                TransferMessage::FileComplete { content_hash, .. } => {
                    assert_eq!(content_hash, Blake2s256::digest(&bytes).as_slice());
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(received, bytes);
        rig.wait_state("verifying").await;
        assert!(
            !rig.events
                .lock()
                .unwrap()
                .iter()
                .any(|v| v["state"] == "completed")
        );
        rig.files.observe(&TransferMessage::FileResult {
            transfer_id: [4; 16],
            result: TransferResult::Completed,
        });
        rig.wait_state("completed").await;
        rig.close().await;
    }
    #[tokio::test]
    async fn cancel_selection_and_download_ignore_late_frames_and_preserve_retry_order() {
        let mut rig = Rig::new("cancel");
        rig.request(1).await;
        rig.files.execute(FileAction::CancelAll).unwrap();
        assert_eq!(
            rig.next().await,
            TransferMessage::FileSelectionCancel { request_id: 1 }
        );
        rig.wait_state("cancelled").await;
        rig.files.observe(&TransferMessage::FileOffer {
            transfer_id: [1; 16],
            request_id: Some(1),
            name: "late.txt".into(),
            size: 1,
        });
        assert!(matches!(
            rig.next().await,
            TransferMessage::FileDecision {
                result: TransferResult::Rejected,
                ..
            }
        ));
        rig.request(2).await;
        rig.offer(2, [2; 16], "cancel.txt", 3).await;
        rig.files.observe(&TransferMessage::FileChunk {
            transfer_id: [2; 16],
            offset: 0,
            bytes: b"abc".to_vec(),
        });
        rig.files.execute(FileAction::CancelAll).unwrap();
        assert_eq!(
            rig.next().await,
            TransferMessage::Cancel {
                transfer_id: [2; 16]
            }
        );
        rig.files.observe(&TransferMessage::FileComplete {
            transfer_id: [2; 16],
            content_hash: Blake2s256::digest(b"abc").into(),
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(std::fs::read_dir(&rig.root).unwrap().count(), 0);
        rig.close().await;
    }
    #[tokio::test]
    async fn deadlines_are_correlated_and_reset_to_allow_new_explicit_action() {
        let mut rig = Rig::new("deadline");
        // Construct the actor directly so deadline behavior is tested without
        // sleeping for the two-minute user confirmation deadline.
        rig.files.task.abort();
        let _ = (&mut rig.files.task).await;
        let (_, receiver) = mpsc::channel(1);
        let (tx, mut wire) = mpsc::unbounded_channel();
        let mut actor = Actor {
            receiver,
            sender: Transport::Test(tx),
            callback: CallbackTarget {
                callback: Some(record),
                context: (&*rig.events as *const Events) as usize,
            },
            stream_id: 44,
            shutdown: Arc::new(AtomicBool::new(false)),
            overflow: Arc::new(AtomicBool::new(false)),
            operation_cancel: Arc::new(Mutex::new(None)),
            active: Some(Active::Selection {
                request_id: 17,
                destination: rig.root.clone(),
                deadline: Instant::now(),
            }),
            last_request_id: 17,
            used_ids: Default::default(),
        };
        actor.tick().await;
        assert_eq!(
            wire.recv().await.unwrap(),
            TransferMessage::FileSelectionCancel { request_id: 17 }
        );
        assert!(actor.active.is_none());
        let value = rig.events.lock().unwrap().last().unwrap().clone();
        assert_eq!(value["request_id"], 17);
        assert_eq!(value["result"], "timeout");
        actor
            .action(FileAction::Request {
                request_id: 18,
                destination: rig.root.clone(),
            })
            .await;
        assert_eq!(
            wire.recv().await.unwrap(),
            TransferMessage::FileSelectionRequest {
                request_id: 18,
                resume: None
            }
        );
        drop(actor);
        rig.close().await;
    }
    #[tokio::test]
    async fn rejected_new_ids_do_not_grow_a_full_lifetime_metadata_set() {
        let mut rig = Rig::new("id-cap");
        rig.files.task.abort();
        let _ = (&mut rig.files.task).await;
        let (_, receiver) = mpsc::channel(1);
        let (tx, mut wire) = mpsc::unbounded_channel();
        let mut actor = Actor {
            receiver,
            sender: Transport::Test(tx),
            callback: CallbackTarget {
                callback: Some(record),
                context: (&*rig.events as *const Events) as usize,
            },
            stream_id: 44,
            shutdown: Arc::new(AtomicBool::new(false)),
            overflow: Arc::new(AtomicBool::new(false)),
            operation_cancel: Arc::new(Mutex::new(None)),
            active: None,
            last_request_id: 0,
            used_ids: (1_u128..=1024).map(u128::to_be_bytes).collect(),
        };
        for number in 1025_u128..=1056 {
            actor
                .action(FileAction::Send {
                    transfer_id: number.to_be_bytes(),
                    path: rig.root.join("never-opened.txt"),
                })
                .await;
            assert_eq!(actor.used_ids.len(), 1024);
            assert!(!actor.used_ids.contains(&number.to_be_bytes()));
            assert!(actor.active.is_none());
            let event = rig.events.lock().unwrap().last().unwrap().clone();
            assert_eq!(event["transfer_id"], hex(&number.to_be_bytes()));
            assert_eq!(event["state"], "failed");
            assert_eq!(event["result"], "busy");
        }
        assert!(
            wire.try_recv().is_err(),
            "capacity rejection must happen before source IO or network offers"
        );
        actor
            .action(FileAction::Send {
                transfer_id: 1_u128.to_be_bytes(),
                path: rig.root.join("old.txt"),
            })
            .await;
        assert_eq!(actor.used_ids.len(), 1024);
        assert_eq!(
            rig.events.lock().unwrap().last().unwrap()["result"],
            "invalid_data"
        );
        drop(actor);
        rig.close().await;
    }

    #[tokio::test]
    async fn full_action_queue_emits_correlated_terminal_results_without_accepting_tasks() {
        let events = Box::<Events>::default();
        let callback = CallbackTarget {
            callback: Some(record),
            context: (&*events as *const Events) as usize,
        };
        let (commands, _receiver) = mpsc::channel(1);
        assert!(
            commands
                .try_send(Command::Peer(TransferMessage::Cancel {
                    transfer_id: [9; 16],
                }))
                .is_ok()
        );
        let mut files = FileTransfers {
            callback,
            stream_id: 44,
            commands,
            shutdown: Arc::new(AtomicBool::new(false)),
            overflow: Arc::new(AtomicBool::new(false)),
            operation_cancel: Arc::new(Mutex::new(None)),
            task: tokio::spawn(std::future::pending()),
        };
        assert!(
            files
                .execute(FileAction::Send {
                    transfer_id: [3; 16],
                    path: PathBuf::from("/never-opened.txt")
                })
                .is_err()
        );
        assert!(
            files
                .execute(FileAction::Request {
                    request_id: 71,
                    destination: PathBuf::from("/never-opened")
                })
                .is_err()
        );
        {
            let recorded = events.lock().unwrap();
            assert_eq!(recorded.len(), 2);
            assert_eq!(recorded[0]["transfer_id"], hex(&[3; 16]));
            assert_eq!(recorded[0]["direction"], "upload");
            assert_eq!(recorded[1]["request_id"], 71);
            assert_eq!(recorded[1]["transfer_id"], "");
            assert_eq!(recorded[1]["direction"], "download");
            assert!(
                recorded
                    .iter()
                    .all(|event| event["state"] == "failed" && event["result"] == "busy")
            );
        }
        files.task.abort();
        let _ = (&mut files.task).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn encrypted_transfer_lane_upload_and_download_round_trip() {
        use desklink_crypto::SecureLane;
        use desklink_protocol::{decode_transfer, encode_transfer};
        tokio::time::timeout(Duration::from_secs(8),async {
            let (mut runtime,host,mut secure,relay_task)=crate::session_commands::tests::encrypted_runtime().await;
            assert!(matches!(runtime.next_event().await.unwrap(),crate::ControllerEvent::VideoConfig(_)));
            let mut rig=Rig::new("encrypted");rig.files.task.abort();let _=(&mut rig.files.task).await;
            rig.files=FileTransfers::new(CallbackTarget{callback:Some(record),context:(&*rig.events as *const Events) as usize},runtime.transfer_sender(),9);
            let source=rig.root.join("wire.bin");let bytes=vec![22;desklink_protocol::MAX_TRANSFER_CHUNK_BYTES+7];std::fs::write(&source,&bytes).unwrap();
            rig.files.execute(FileAction::Send{transfer_id:[7;16],path:source}).unwrap();
            let ciphertext=host.next_transfer().await.unwrap();let plaintext=secure.open(SecureLane::Transfer,&ciphertext).unwrap();
            assert!(matches!(decode_transfer(&plaintext).unwrap(),TransferMessage::FileOffer{transfer_id,request_id:None,..} if transfer_id==[7;16]));
            let message=TransferMessage::FileDecision{transfer_id:[7;16],result:TransferResult::Completed,resume_offset:0,resume_prefix_hash:None};host.send_transfer(secure.seal(SecureLane::Transfer,&encode_transfer(&message).unwrap()).unwrap()).await.unwrap();
            let crate::ControllerEvent::Transfer(message)=runtime.next_event().await.unwrap() else{panic!("transfer event")};rig.files.observe(&message);
            let mut received=Vec::new();loop{let ciphertext=host.next_transfer().await.unwrap();let plaintext=secure.open(SecureLane::Transfer,&ciphertext).unwrap();match decode_transfer(&plaintext).unwrap(){TransferMessage::FileChunk{offset,bytes,..}=>{assert_eq!(offset,received.len() as u64);received.extend(bytes);},TransferMessage::FileComplete{content_hash,..}=>{assert_eq!(content_hash,Blake2s256::digest(&bytes).as_slice());break;},other=>panic!("{other:?}")}}
            assert_eq!(received,bytes);rig.wait_state("verifying").await;
            let message=TransferMessage::FileResult{transfer_id:[7;16],result:TransferResult::Completed};host.send_transfer(secure.seal(SecureLane::Transfer,&encode_transfer(&message).unwrap()).unwrap()).await.unwrap();let crate::ControllerEvent::Transfer(message)=runtime.next_event().await.unwrap() else{panic!("transfer event")};rig.files.observe(&message);rig.wait_state("completed").await;
            rig.files.execute(FileAction::Request{request_id:18,destination:rig.root.clone()}).unwrap();let ciphertext=host.next_transfer().await.unwrap();let plaintext=secure.open(SecureLane::Transfer,&ciphertext).unwrap();assert_eq!(decode_transfer(&plaintext).unwrap(),TransferMessage::FileSelectionRequest{request_id:18,resume:None});
            let offer=TransferMessage::FileOffer{transfer_id:[8;16],request_id:Some(18),name:"远端.txt".into(),size:6};host.send_transfer(secure.seal(SecureLane::Transfer,&encode_transfer(&offer).unwrap()).unwrap()).await.unwrap();let crate::ControllerEvent::Transfer(message)=runtime.next_event().await.unwrap() else{panic!("transfer event")};rig.files.observe(&message);let ciphertext=host.next_transfer().await.unwrap();let plaintext=secure.open(SecureLane::Transfer,&ciphertext).unwrap();assert!(matches!(decode_transfer(&plaintext).unwrap(),TransferMessage::FileDecision{result:TransferResult::Completed,..}));
            for message in [TransferMessage::FileChunk{transfer_id:[8;16],offset:0,bytes:b"remote".to_vec()},TransferMessage::FileComplete{transfer_id:[8;16],content_hash:Blake2s256::digest(b"remote").into()}]{host.send_transfer(secure.seal(SecureLane::Transfer,&encode_transfer(&message).unwrap()).unwrap()).await.unwrap();let crate::ControllerEvent::Transfer(message)=runtime.next_event().await.unwrap() else{panic!("transfer event")};rig.files.observe(&message);}
            let ciphertext=host.next_transfer().await.unwrap();let plaintext=secure.open(SecureLane::Transfer,&ciphertext).unwrap();assert_eq!(decode_transfer(&plaintext).unwrap(),TransferMessage::FileResult{transfer_id:[8;16],result:TransferResult::Completed});assert_eq!(std::fs::read(rig.root.join("远端.txt")).unwrap(),b"remote");
            rig.close().await;drop(runtime);drop(host);relay_task.abort();let _=relay_task.await;
        }).await.expect("encrypted file roundtrip timeout");
    }
}
