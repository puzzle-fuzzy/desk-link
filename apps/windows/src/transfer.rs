use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use blake2::{Blake2s256, Digest};
#[cfg(windows)]
use desklink_protocol::MAX_CLIPBOARD_TEXT_BYTES;
use desklink_protocol::{
    MAX_TRANSFER_CHUNK_BYTES, MAX_TRANSFER_FILE_BYTES, TransferId, TransferResult,
    is_valid_transfer_file_name,
};
use rand_core::{OsRng, RngCore};
use tokio::sync::{mpsc, oneshot};

use crate::storage::downloads_path;

const INCOMING_STAGING_DIRECTORY: &str = ".desklink-transfers";
const INCOMING_PART_PREFIX: &str = ".incoming-";
const INCOMING_PART_SUFFIX: &str = ".part";
const STALE_INCOMING_FILE_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_STAGING_ENTRIES_TO_CLEAN: usize = 256;
const MIN_FREE_SPACE_AFTER_RECEIVE: u64 = 64 * 1024 * 1024;

/// Two maximum-sized chunks may wait behind the one being written. The sender
/// awaits completed worker writes instead of buffering an entire network file.
pub const INCOMING_DISK_QUEUE_CAPACITY: usize = 2;
const MAX_INCOMING_DISK_WORKERS: usize = 8;
static ACTIVE_INCOMING_DISK_WORKERS: AtomicUsize = AtomicUsize::new(0);
static INCOMING_WRITERS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

/// The path remains owned until the file has been closed and all preserve or
/// cancellation cleanup has completed, even after its async handle is dropped.
struct IncomingWriterPermit(PathBuf);

impl IncomingWriterPermit {
    fn acquire(path: &Path) -> Result<Self, TransferResult> {
        let parent = path.parent().ok_or(TransferResult::InvalidData)?;
        let name = path.file_name().ok_or(TransferResult::InvalidData)?;
        let canonical = fs::canonicalize(parent)
            .map_err(|_| TransferResult::IoFailed)?
            .join(name);
        let mut writers = INCOMING_WRITERS
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !writers.insert(canonical.clone()) {
            return Err(TransferResult::Busy);
        }
        Ok(Self(canonical))
    }
}

impl Drop for IncomingWriterPermit {
    fn drop(&mut self) {
        if let Some(writers) = INCOMING_WRITERS.get() {
            writers
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&self.0);
        }
    }
}

fn incoming_write_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(windows::Win32::Storage::FileSystem::FILE_SHARE_READ.0);
    }
    options
}

fn incoming_open_error(error: std::io::Error) -> TransferResult {
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(32 | 33)) {
        return TransferResult::Busy;
    }
    let _ = error;
    TransferResult::IoFailed
}

pub(crate) struct IncomingDiskWorkerPermit;

impl IncomingDiskWorkerPermit {
    pub(crate) fn acquire() -> Result<Self, TransferResult> {
        ACTIVE_INCOMING_DISK_WORKERS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_INCOMING_DISK_WORKERS).then_some(active + 1)
            })
            .map(|_| Self)
            .map_err(|_| TransferResult::Busy)
    }
}

impl Drop for IncomingDiskWorkerPermit {
    fn drop(&mut self) {
        ACTIVE_INCOMING_DISK_WORKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

enum IncomingDiskCommand {
    Write {
        offset: u64,
        bytes: Box<[u8]>,
        reply: oneshot::Sender<Result<(), TransferResult>>,
    },
    Finish {
        hash: [u8; 32],
        reply: oneshot::Sender<Result<PathBuf, TransferResult>>,
    },
    Preserve(oneshot::Sender<Result<(), TransferResult>>),
}

/// Owns a single receive file on one disk thread. No filesystem operation,
/// including disconnect cleanup, runs from this handle's async methods or Drop.
/// Dropping an unfinished handle preserves its staged prefix; explicit cancel
/// removes it as soon as an in-flight OS write returns.
pub struct IncomingFileWorker {
    transfer_id: TransferId,
    resume_offset: u64,
    resume_prefix_hash: Option<[u8; 32]>,
    commands: Option<mpsc::Sender<IncomingDiskCommand>>,
    cancelled: Arc<AtomicBool>,
    finishing: bool,
}

impl IncomingFileWorker {
    pub async fn create(
        transfer_id: TransferId,
        name: String,
        expected_size: u64,
    ) -> Result<Self, TransferResult> {
        Self::spawn_with(
            move || IncomingFile::create(transfer_id, name, expected_size),
            || {},
        )
        .await
    }

    async fn spawn_with(
        create: impl FnOnce() -> Result<IncomingFile, TransferResult> + Send + 'static,
        before_write: impl Fn() + Send + 'static,
    ) -> Result<Self, TransferResult> {
        // Cancel cannot interrupt a driver-blocked syscall. Keep repeated
        // reconnects from creating unlimited abandoned disk threads.
        let permit = IncomingDiskWorkerPermit::acquire()?;
        let (commands, mut pending) = mpsc::channel(INCOMING_DISK_QUEUE_CAPACITY);
        let (ready, opened) = oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        std::thread::Builder::new()
            .name("desklink-file-receive".to_owned())
            .spawn(move || {
                let _permit = permit;
                let mut file = match create() {
                    Ok(file) => file,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let metadata = (
                    file.transfer_id(),
                    file.resume_offset(),
                    file.resume_prefix_hash(),
                );
                if ready.send(Ok(metadata)).is_err() {
                    // Initialization may finish after the awaiting create
                    // future is cancelled. Keep an existing validated prefix
                    // just as on a later transport disconnect; cleanup and
                    // permit release must stay on this disk thread.
                    let _ = file.preserve();
                    return;
                }
                while let Some(command) = pending.blocking_recv() {
                    if worker_cancelled.load(Ordering::Acquire) {
                        return;
                    }
                    match command {
                        IncomingDiskCommand::Write {
                            offset,
                            bytes,
                            reply,
                        } => {
                            before_write();
                            let result = if worker_cancelled.load(Ordering::Acquire) {
                                Err(TransferResult::Cancelled)
                            } else {
                                file.write_chunk(offset, &bytes)
                            };
                            let failed = result.is_err();
                            let _ = reply.send(result);
                            if failed {
                                return;
                            }
                        }
                        IncomingDiskCommand::Finish { hash, reply } => {
                            let result = file.finish_unless_cancelled(hash, &worker_cancelled);
                            let _ = reply.send(result);
                            return;
                        }
                        IncomingDiskCommand::Preserve(reply) => {
                            let _ = reply.send(file.preserve());
                            return;
                        }
                    }
                }
                if !worker_cancelled.load(Ordering::Acquire) {
                    let _ = file.preserve();
                }
            })
            .map_err(|_| TransferResult::IoFailed)?;
        let (transfer_id, resume_offset, resume_prefix_hash) =
            opened.await.map_err(|_| TransferResult::IoFailed)??;
        Ok(Self {
            transfer_id,
            resume_offset,
            resume_prefix_hash,
            commands: Some(commands),
            cancelled,
            finishing: false,
        })
    }

    pub fn transfer_id(&self) -> TransferId {
        self.transfer_id
    }

    pub fn resume_offset(&self) -> u64 {
        self.resume_offset
    }

    pub fn resume_prefix_hash(&self) -> Option<[u8; 32]> {
        self.resume_prefix_hash
    }

    pub async fn write_chunk(&mut self, offset: u64, bytes: Vec<u8>) -> Result<(), TransferResult> {
        if bytes.is_empty() || bytes.len() > MAX_TRANSFER_CHUNK_BYTES {
            return Err(TransferResult::InvalidData);
        }
        let (reply, response) = oneshot::channel();
        self.commands
            .as_ref()
            .ok_or(TransferResult::InvalidData)?
            .send(IncomingDiskCommand::Write {
                offset,
                bytes: bytes.into_boxed_slice(),
                reply,
            })
            .await
            .map_err(|_| TransferResult::IoFailed)?;
        response.await.map_err(|_| TransferResult::IoFailed)?
    }

    pub async fn finish(mut self, hash: [u8; 32]) -> Result<PathBuf, TransferResult> {
        self.finishing = true;
        let (reply, response) = oneshot::channel();
        self.commands
            .as_ref()
            .ok_or(TransferResult::InvalidData)?
            .send(IncomingDiskCommand::Finish { hash, reply })
            .await
            .map_err(|_| TransferResult::IoFailed)?;
        let result = response.await.map_err(|_| TransferResult::IoFailed)?;
        self.finishing = false;
        result
    }

    pub async fn preserve(self) -> Result<(), TransferResult> {
        let (reply, response) = oneshot::channel();
        self.commands
            .as_ref()
            .ok_or(TransferResult::InvalidData)?
            .send(IncomingDiskCommand::Preserve(reply))
            .await
            .map_err(|_| TransferResult::IoFailed)?;
        response.await.map_err(|_| TransferResult::IoFailed)?
    }

    pub fn cancel(mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.commands.take();
    }
}

impl Drop for IncomingFileWorker {
    fn drop(&mut self) {
        // A cancelled finish future must not publish a late completed download.
        if self.finishing {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

#[derive(Clone, Debug)]
pub struct OutgoingFile {
    pub transfer_id: TransferId,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified_at_unix_ns: u64,
}

/// One opened source and one incremental hash for both resume proof and final
/// integrity. The already verified prefix is never read a second time.
/// All methods perform blocking IO and must run on a file producer thread.
pub struct OutgoingFileReader {
    file: File,
    hasher: Blake2s256,
    offset: u64,
    expected_size: u64,
    modified_at_unix_ns: u64,
    cancellation: Arc<AtomicBool>,
}

impl OutgoingFileReader {
    pub fn open(
        source: &OutgoingFile,
        resume_offset: u64,
        expected_prefix_hash: Option<[u8; 32]>,
        cancellation: Arc<AtomicBool>,
    ) -> Result<Self, TransferResult> {
        if cancellation.load(Ordering::Acquire) {
            return Err(TransferResult::Cancelled);
        }
        if resume_offset > source.size || (resume_offset == 0) != expected_prefix_hash.is_none() {
            return Err(TransferResult::InvalidData);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Hold a stable source snapshot: another Windows handle cannot
            // modify, replace, or truncate this file while it is being sent.
            options.share_mode(windows::Win32::Storage::FileSystem::FILE_SHARE_READ.0);
        }
        let file = options
            .open(&source.path)
            .map_err(|_| TransferResult::IoFailed)?;
        let mut reader = Self {
            file,
            hasher: Blake2s256::new(),
            offset: 0,
            expected_size: source.size,
            modified_at_unix_ns: source.modified_at_unix_ns,
            cancellation,
        };
        reader.check_source()?;
        let mut buffer = vec![0_u8; MAX_TRANSFER_CHUNK_BYTES];
        while reader.offset < resume_offset {
            reader.check_cancelled()?;
            let limit = usize::try_from((resume_offset - reader.offset).min(buffer.len() as u64))
                .map_err(|_| TransferResult::InvalidData)?;
            let read = reader
                .file
                .read(&mut buffer[..limit])
                .map_err(|_| TransferResult::IoFailed)?;
            if read == 0 {
                return Err(TransferResult::SourceChanged);
            }
            reader.hasher.update(&buffer[..read]);
            reader.offset += read as u64;
        }
        if let Some(expected) = expected_prefix_hash {
            let actual: [u8; 32] = reader.hasher.clone().finalize().into();
            if expected != actual {
                return Err(TransferResult::SourceChanged);
            }
        }
        reader.check_cancelled()?;
        Ok(reader)
    }

    pub fn read_chunk(&mut self) -> Result<Option<(u64, Vec<u8>)>, TransferResult> {
        self.check_cancelled()?;
        if self.offset == self.expected_size {
            return Ok(None);
        }
        let limit = usize::try_from(
            (self.expected_size - self.offset).min(MAX_TRANSFER_CHUNK_BYTES as u64),
        )
        .map_err(|_| TransferResult::InvalidData)?;
        let mut bytes = vec![0_u8; limit];
        let read = self
            .file
            .read(&mut bytes)
            .map_err(|_| TransferResult::IoFailed)?;
        if read == 0 {
            return Err(TransferResult::SourceChanged);
        }
        bytes.truncate(read);
        let offset = self.offset;
        self.offset += read as u64;
        self.hasher.update(&bytes);
        Ok(Some((offset, bytes)))
    }

    pub fn finish(self) -> Result<[u8; 32], TransferResult> {
        self.check_cancelled()?;
        if self.offset != self.expected_size {
            return Err(TransferResult::InvalidData);
        }
        self.check_source()?;
        Ok(self.hasher.finalize().into())
    }

    fn check_cancelled(&self) -> Result<(), TransferResult> {
        if self.cancellation.load(Ordering::Acquire) {
            Err(TransferResult::Cancelled)
        } else {
            Ok(())
        }
    }

    fn check_source(&self) -> Result<(), TransferResult> {
        let metadata = self.file.metadata().map_err(|_| TransferResult::IoFailed)?;
        if !metadata.is_file()
            || metadata.len() != self.expected_size
            || (self.modified_at_unix_ns != 0
                && modified_at_unix_ns(&metadata) != self.modified_at_unix_ns)
        {
            Err(TransferResult::SourceChanged)
        } else {
            Ok(())
        }
    }
}

fn modified_at_unix_ns(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

pub fn prepare_outgoing_file(path: PathBuf) -> Result<OutgoingFile, TransferResult> {
    prepare_outgoing_file_with_budget(path, MAX_TRANSFER_FILE_BYTES)
}

/// Metadata preparation only; the negotiated file actor opens and holds the
/// source snapshot before scanning/sending. This function is blocking IO.
pub fn prepare_outgoing_file_with_budget(
    path: PathBuf,
    maximum: u64,
) -> Result<OutgoingFile, TransferResult> {
    if maximum == 0 || maximum > desklink_protocol::MAX_LARGE_FILE_BYTES {
        return Err(TransferResult::InvalidData);
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| TransferResult::IoFailed)?;
    if metadata.file_type().is_symlink() {
        return Err(TransferResult::PermissionDenied);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT.0
            != 0
        {
            return Err(TransferResult::PermissionDenied);
        }
    }
    if !metadata.is_file() {
        return Err(TransferResult::InvalidData);
    }
    if metadata.len() > maximum {
        return Err(TransferResult::TooLarge);
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| {
            if maximum <= MAX_TRANSFER_FILE_BYTES {
                is_valid_transfer_file_name(value)
            } else {
                desklink_protocol::is_valid_transfer_path_segment(value)
            }
        })
        .ok_or(TransferResult::InvalidData)?
        .to_owned();
    let mut transfer_id = [0_u8; 16];
    OsRng.fill_bytes(&mut transfer_id);
    if transfer_id.iter().all(|byte| *byte == 0) {
        transfer_id[0] = 1;
    }
    Ok(OutgoingFile {
        transfer_id,
        path,
        name,
        size: metadata.len(),
        modified_at_unix_ns: modified_at_unix_ns(&metadata),
    })
}

pub fn chunk_after_resume(
    offset: u64,
    mut bytes: Vec<u8>,
    resume_offset: u64,
) -> Result<Option<(u64, Vec<u8>)>, TransferResult> {
    let end = offset
        .checked_add(bytes.len() as u64)
        .ok_or(TransferResult::InvalidData)?;
    if end <= resume_offset {
        return Ok(None);
    }
    let skipped = resume_offset.saturating_sub(offset);
    let skipped = usize::try_from(skipped).map_err(|_| TransferResult::InvalidData)?;
    if skipped > bytes.len() {
        return Err(TransferResult::InvalidData);
    }
    if skipped > 0 {
        bytes.drain(..skipped);
    }
    Ok(Some((offset.max(resume_offset), bytes)))
}

pub fn verify_resume_prefix(
    path: &Path,
    resume_offset: u64,
    expected_hash: Option<[u8; 32]>,
) -> Result<(), TransferResult> {
    if resume_offset == 0 {
        return if expected_hash.is_none() {
            Ok(())
        } else {
            Err(TransferResult::InvalidData)
        };
    }
    let expected_hash = expected_hash.ok_or(TransferResult::InvalidData)?;
    let mut file = File::open(path).map_err(|_| TransferResult::IoFailed)?;
    let mut hasher = Blake2s256::new();
    let mut remaining = resume_offset;
    let mut buffer = vec![0_u8; MAX_TRANSFER_CHUNK_BYTES];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| TransferResult::InvalidData)?;
        let read = file
            .read(&mut buffer[..limit])
            .map_err(|_| TransferResult::IoFailed)?;
        if read == 0 {
            return Err(TransferResult::SourceChanged);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    let actual: [u8; 32] = hasher.finalize().into();
    if actual == expected_hash {
        Ok(())
    } else {
        Err(TransferResult::SourceChanged)
    }
}

#[cfg(windows)]
pub fn pick_outgoing_file(title: &str) -> Result<Option<PathBuf>, TransferResult> {
    pick_outgoing_files(title, false).map(|mut paths| paths.pop())
}

#[cfg(windows)]
pub fn pick_outgoing_files(
    title: &str,
    allow_multiple: bool,
) -> Result<Vec<PathBuf>, TransferResult> {
    pick_paths(title, allow_multiple, false)
}

#[cfg(windows)]
pub fn pick_outgoing_directory(title: &str) -> Result<Option<PathBuf>, TransferResult> {
    pick_paths(title, false, true).map(|mut paths| paths.pop())
}

#[cfg(not(windows))]
pub fn pick_outgoing_directory(_: &str) -> Result<Option<PathBuf>, TransferResult> {
    Err(TransferResult::Unsupported)
}

#[cfg(windows)]
fn pick_paths(
    title: &str,
    allow_multiple: bool,
    directory: bool,
) -> Result<Vec<PathBuf>, TransferResult> {
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree,
            },
            UI::Shell::{
                FOS_ALLOWMULTISELECT, FOS_DONTADDTORECENT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
                FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem,
                SIGDN_FILESYSPATH,
            },
        },
        core::HSTRING,
    };

    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .map_err(|_| TransferResult::IoFailed)?;
    let _apartment = ComApartment;
    let dialog: IFileOpenDialog = unsafe {
        CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|_| TransferResult::IoFailed)?
    };
    let options = unsafe { dialog.GetOptions() }.map_err(|_| TransferResult::IoFailed)?;
    let title = HSTRING::from(title);
    unsafe {
        let options = options
            | FOS_FORCEFILESYSTEM
            | FOS_FILEMUSTEXIST
            | FOS_PATHMUSTEXIST
            | FOS_DONTADDTORECENT;
        dialog
            .SetOptions(if allow_multiple {
                options | FOS_ALLOWMULTISELECT
            } else if directory {
                options | FOS_PICKFOLDERS
            } else {
                options
            })
            .map_err(|_| TransferResult::IoFailed)?;
        dialog
            .SetTitle(&title)
            .map_err(|_| TransferResult::IoFailed)?;
    }
    if let Err(error) = unsafe { dialog.Show(None) } {
        return if error.code().0 as u32 == 0x8007_04c7 {
            Ok(Vec::new())
        } else {
            Err(TransferResult::IoFailed)
        };
    }
    let item_path = |item: &IShellItem| {
        let path = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
            .map_err(|_| TransferResult::IoFailed)?;
        let result = unsafe { path.to_string() }
            .map(PathBuf::from)
            .map_err(|_| TransferResult::InvalidData);
        unsafe { CoTaskMemFree(Some(path.as_ptr().cast())) };
        result
    };
    if !allow_multiple {
        let item = unsafe { dialog.GetResult() }.map_err(|_| TransferResult::IoFailed)?;
        return item_path(&item).map(|path| vec![path]);
    }
    let items = unsafe { dialog.GetResults() }.map_err(|_| TransferResult::IoFailed)?;
    let count = unsafe { items.GetCount() }.map_err(|_| TransferResult::IoFailed)?;
    let mut paths = Vec::with_capacity(count as usize);
    for index in 0..count {
        let item = unsafe { items.GetItemAt(index) }.map_err(|_| TransferResult::IoFailed)?;
        paths.push(item_path(&item)?);
    }
    Ok(paths)
}

#[cfg(not(windows))]
pub fn pick_outgoing_file(_title: &str) -> Result<Option<PathBuf>, TransferResult> {
    Err(TransferResult::Unsupported)
}

#[cfg(not(windows))]
pub fn pick_outgoing_files(
    _title: &str,
    _allow_multiple: bool,
) -> Result<Vec<PathBuf>, TransferResult> {
    Err(TransferResult::Unsupported)
}

#[cfg(windows)]
struct ComApartment;

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { windows::Win32::System::Com::CoUninitialize() };
    }
}

pub struct IncomingFile {
    transfer_id: TransferId,
    name: String,
    expected_size: u64,
    written: u64,
    hasher: Blake2s256,
    file: Option<File>,
    temporary_path: PathBuf,
    destination_directory: PathBuf,
    remove_on_drop: bool,
    _writer: IncomingWriterPermit,
}

impl IncomingFile {
    pub fn create(
        transfer_id: TransferId,
        name: String,
        expected_size: u64,
    ) -> Result<Self, TransferResult> {
        if !is_valid_transfer_file_name(&name) {
            return Err(TransferResult::InvalidData);
        }
        let destination_directory = downloads_path().ok_or(TransferResult::IoFailed)?;
        let directory = incoming_staging_directory(&destination_directory);
        Self::create_in(
            transfer_id,
            name,
            expected_size,
            directory,
            destination_directory,
        )
    }

    fn create_in(
        transfer_id: TransferId,
        name: String,
        expected_size: u64,
        directory: PathBuf,
        destination_directory: PathBuf,
    ) -> Result<Self, TransferResult> {
        Self::create_in_with_open_hook(
            transfer_id,
            name,
            expected_size,
            directory,
            destination_directory,
            || {},
        )
    }

    fn create_in_with_open_hook(
        transfer_id: TransferId,
        name: String,
        expected_size: u64,
        directory: PathBuf,
        destination_directory: PathBuf,
        after_open: impl FnOnce(),
    ) -> Result<Self, TransferResult> {
        if !is_valid_transfer_file_name(&name) {
            return Err(TransferResult::InvalidData);
        }
        if expected_size > MAX_TRANSFER_FILE_BYTES {
            return Err(TransferResult::TooLarge);
        }
        prepare_incoming_staging_directory(&directory)?;
        let temporary_path = incoming_temporary_path(&directory, transfer_id);
        let writer = IncomingWriterPermit::acquire(&temporary_path)?;
        let mut hasher = Blake2s256::new();
        let (file, written) = match incoming_write_options()
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => {
                after_open();
                (file, 0)
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata =
                    fs::symlink_metadata(&temporary_path).map_err(|_| TransferResult::IoFailed)?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(TransferResult::IoFailed);
                }
                // Acquire exclusive writer ownership before reading metadata or
                // hashing the prefix. The same handle then appends new data.
                let mut file = incoming_write_options()
                    .append(true)
                    .read(true)
                    .open(&temporary_path)
                    .map_err(incoming_open_error)?;
                let written = file.metadata().map_err(|_| TransferResult::IoFailed)?.len();
                if written > expected_size {
                    drop(file);
                    let _ = fs::remove_file(&temporary_path);
                    return Err(TransferResult::InvalidData);
                }
                after_open();
                let mut buffer = vec![0_u8; MAX_TRANSFER_CHUNK_BYTES];
                loop {
                    let read = file
                        .read(&mut buffer)
                        .map_err(|_| TransferResult::IoFailed)?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                }
                (file, written)
            }
            Err(error) => return Err(incoming_open_error(error)),
        };
        let remaining = expected_size.saturating_sub(written);
        if let Err(result) = ensure_receive_capacity(&directory, remaining) {
            drop(file);
            if written == 0 {
                let _ = fs::remove_file(&temporary_path);
            }
            return Err(result);
        }
        Ok(Self {
            transfer_id,
            name,
            expected_size,
            written,
            hasher,
            file: Some(file),
            temporary_path,
            destination_directory,
            remove_on_drop: true,
            _writer: writer,
        })
    }

    pub fn transfer_id(&self) -> TransferId {
        self.transfer_id
    }

    pub fn resume_offset(&self) -> u64 {
        self.written
    }

    pub fn resume_prefix_hash(&self) -> Option<[u8; 32]> {
        (self.written > 0).then(|| self.hasher.clone().finalize().into())
    }

    /// Keep the staged bytes for an explicit retry after a transport failure.
    /// Cancellation, validation failures, and normal drops still remove them.
    pub fn preserve(mut self) -> Result<(), TransferResult> {
        let file = self.file.take().ok_or(TransferResult::InvalidData)?;
        file.sync_data().map_err(|_| TransferResult::IoFailed)?;
        drop(file);
        self.remove_on_drop = false;
        Ok(())
    }

    pub fn write_chunk(&mut self, offset: u64, bytes: &[u8]) -> Result<(), TransferResult> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or(TransferResult::InvalidData)?;
        if offset != self.written || bytes.is_empty() || end > self.expected_size {
            return Err(TransferResult::InvalidData);
        }
        self.file
            .as_mut()
            .ok_or(TransferResult::InvalidData)?
            .write_all(bytes)
            .map_err(|_| TransferResult::IoFailed)?;
        self.hasher.update(bytes);
        self.written = end;
        Ok(())
    }

    pub fn finish(self, content_hash: [u8; 32]) -> Result<PathBuf, TransferResult> {
        self.finish_unless_cancelled(content_hash, &AtomicBool::new(false))
    }

    fn finish_unless_cancelled(
        mut self,
        content_hash: [u8; 32],
        cancelled: &AtomicBool,
    ) -> Result<PathBuf, TransferResult> {
        if cancelled.load(Ordering::Acquire) {
            return Err(TransferResult::Cancelled);
        }
        if self.written != self.expected_size
            || self.hasher.clone().finalize().as_slice() != content_hash
        {
            return Err(TransferResult::InvalidData);
        }
        let file = self.file.take().ok_or(TransferResult::InvalidData)?;
        file.sync_all().map_err(|_| TransferResult::IoFailed)?;
        drop(file);
        if cancelled.load(Ordering::Acquire) {
            return Err(TransferResult::Cancelled);
        }
        fs::create_dir_all(&self.destination_directory).map_err(|_| TransferResult::IoFailed)?;
        let destination = available_destination(&self.destination_directory, &self.name)?;
        fs::rename(&self.temporary_path, &destination).map_err(|_| TransferResult::IoFailed)?;
        Ok(destination)
    }
}

fn receive_capacity_is_sufficient(available: u64, remaining: u64) -> bool {
    remaining == 0
        || remaining
            .checked_add(MIN_FREE_SPACE_AFTER_RECEIVE)
            .is_some_and(|required| available >= required)
}

#[cfg(windows)]
pub(crate) fn ensure_receive_capacity(
    directory: &Path,
    remaining: u64,
) -> Result<(), TransferResult> {
    use windows::{Win32::Storage::FileSystem::GetDiskFreeSpaceExW, core::HSTRING};

    if remaining == 0 {
        return Ok(());
    }
    let directory = HSTRING::from(directory.as_os_str());
    let mut available = 0_u64;
    unsafe { GetDiskFreeSpaceExW(&directory, Some(&mut available), None, None) }
        .map_err(|_| TransferResult::IoFailed)?;
    if receive_capacity_is_sufficient(available, remaining) {
        Ok(())
    } else {
        Err(TransferResult::InsufficientSpace)
    }
}

#[cfg(not(windows))]
pub(crate) fn ensure_receive_capacity(
    _directory: &Path,
    _remaining: u64,
) -> Result<(), TransferResult> {
    Ok(())
}

impl Drop for IncomingFile {
    fn drop(&mut self) {
        // Windows cannot remove an open staging file without delete sharing.
        self.file.take();
        if self.remove_on_drop && self.temporary_path.exists() {
            let _ = fs::remove_file(&self.temporary_path);
        }
        if self.remove_on_drop
            && let Some(directory) = self.temporary_path.parent()
        {
            let _ = fs::remove_dir(directory);
        }
    }
}

fn incoming_staging_directory(destination_directory: &Path) -> PathBuf {
    destination_directory.join(INCOMING_STAGING_DIRECTORY)
}

fn incoming_temporary_path(directory: &Path, transfer_id: TransferId) -> PathBuf {
    directory.join(format!(
        "{INCOMING_PART_PREFIX}{}{INCOMING_PART_SUFFIX}",
        transfer_id
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

pub fn discard_incoming_resume(transfer_id: TransferId) -> Result<bool, TransferResult> {
    if transfer_id.iter().all(|byte| *byte == 0) {
        return Err(TransferResult::InvalidData);
    }
    let destination_directory = downloads_path().ok_or(TransferResult::IoFailed)?;
    discard_incoming_resume_in(&destination_directory, transfer_id)
}

fn discard_incoming_resume_in(
    destination_directory: &Path,
    transfer_id: TransferId,
) -> Result<bool, TransferResult> {
    let directory = incoming_staging_directory(destination_directory);
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(TransferResult::IoFailed);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(TransferResult::IoFailed),
    }
    let temporary_path = incoming_temporary_path(&directory, transfer_id);
    let _writer = IncomingWriterPermit::acquire(&temporary_path)?;
    match fs::symlink_metadata(&temporary_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(TransferResult::IoFailed);
        }
        Ok(_) => fs::remove_file(&temporary_path).map_err(|_| TransferResult::IoFailed)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(TransferResult::IoFailed),
    }
    let _ = fs::remove_dir(directory);
    Ok(true)
}

fn prepare_incoming_staging_directory(directory: &Path) -> Result<(), TransferResult> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(TransferResult::IoFailed);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(directory).map_err(|_| TransferResult::IoFailed)?;
        }
        Err(_) => return Err(TransferResult::IoFailed),
    }
    hide_incoming_staging_directory(directory);
    cleanup_stale_incoming_files(directory);
    Ok(())
}

fn cleanup_stale_incoming_files(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten().take(MAX_STAGING_ENTRIES_TO_CLEAN) {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_incoming_part_name(name) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_file() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_none_or(|age| age < STALE_INCOMING_FILE_AGE)
        {
            continue;
        }
        if let Ok(_writer) = IncomingWriterPermit::acquire(&entry.path()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn is_incoming_part_name(name: &str) -> bool {
    name.strip_prefix(INCOMING_PART_PREFIX)
        .and_then(|name| name.strip_suffix(INCOMING_PART_SUFFIX))
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[cfg(windows)]
fn hide_incoming_staging_directory(directory: &Path) {
    use windows::{
        Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW},
        core::HSTRING,
    };

    let directory = HSTRING::from(directory.as_os_str());
    let _ = unsafe { SetFileAttributesW(&directory, FILE_ATTRIBUTE_HIDDEN) };
}

#[cfg(not(windows))]
fn hide_incoming_staging_directory(_directory: &Path) {}

fn available_destination(directory: &Path, name: &str) -> Result<PathBuf, TransferResult> {
    let original = directory.join(name);
    if !original.exists() {
        return Ok(original);
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or(TransferResult::InvalidData)?;
    let extension = path.extension().and_then(|value| value.to_str());
    for suffix in 1..=9_999_u32 {
        let candidate_name = match extension {
            Some(extension) => format!("{stem} ({suffix}).{extension}"),
            None => format!("{stem} ({suffix})"),
        };
        let candidate = directory.join(candidate_name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(TransferResult::IoFailed)
}

#[cfg(windows)]
pub fn confirm_file_offer(name: &str, size: u64) -> bool {
    use windows::{
        Win32::UI::WindowsAndMessaging::{
            IDYES, MB_DEFBUTTON2, MB_ICONQUESTION, MB_YESNO, MessageBoxW,
        },
        core::HSTRING,
    };

    let size = human_size(size);
    let text = HSTRING::from(format!(
        "控制端希望向此电脑发送文件：\n\n{name}\n大小：{size}\n\n接收后将保存到“下载”文件夹。是否接收？"
    ));
    let title = HSTRING::from("DeskLink 文件传输");
    unsafe {
        MessageBoxW(
            None,
            &text,
            &title,
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
        ) == IDYES
    }
}

#[cfg(not(windows))]
pub fn confirm_file_offer(_name: &str, _size: u64) -> bool {
    false
}

#[cfg(windows)]
pub fn notify_file_received(path: &Path) {
    use windows::{
        Win32::UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW},
        core::HSTRING,
    };
    let text = HSTRING::from(format!("文件已安全保存到：\n{}", path.display()));
    let title = HSTRING::from("DeskLink 文件接收完成");
    unsafe {
        let _ = MessageBoxW(None, &text, &title, MB_OK | MB_ICONINFORMATION);
    }
}

#[cfg(not(windows))]
pub fn notify_file_received(_path: &Path) {}

#[cfg(windows)]
pub fn open_downloads_folder() -> Result<(), TransferResult> {
    use std::process::Command;

    let directory = downloads_path().ok_or(TransferResult::IoFailed)?;
    fs::create_dir_all(&directory).map_err(|_| TransferResult::IoFailed)?;
    Command::new("explorer.exe")
        .arg(directory)
        .spawn()
        .map(|_| ())
        .map_err(|_| TransferResult::IoFailed)
}

#[cfg(not(windows))]
pub fn open_downloads_folder() -> Result<(), TransferResult> {
    Err(TransferResult::Unsupported)
}

#[cfg(windows)]
pub fn read_clipboard_text() -> Result<String, TransferResult> {
    use windows::Win32::{
        System::Ole::CF_UNICODETEXT,
        System::{
            DataExchange::{GetClipboardData, IsClipboardFormatAvailable},
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
        },
    };

    let _clipboard = ClipboardGuard::open()?;
    unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }
        .map_err(|_| TransferResult::Unsupported)?;
    let handle = unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32) }
        .map_err(|_| TransferResult::IoFailed)?;
    let global = windows::Win32::Foundation::HGLOBAL(handle.0);
    let size = unsafe { GlobalSize(global) };
    if size == 0 || size > (MAX_CLIPBOARD_TEXT_BYTES + 1) * 2 {
        return Err(TransferResult::TooLarge);
    }
    let pointer = unsafe { GlobalLock(global) }.cast::<u16>();
    if pointer.is_null() {
        return Err(TransferResult::IoFailed);
    }
    let units = unsafe { std::slice::from_raw_parts(pointer, size / 2) };
    let length = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    let text = String::from_utf16(&units[..length]).map_err(|_| TransferResult::InvalidData);
    let _ = unsafe { GlobalUnlock(global) };
    let text = text?;
    if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
        Err(TransferResult::TooLarge)
    } else {
        Ok(text)
    }
}

#[cfg(not(windows))]
pub fn read_clipboard_text() -> Result<String, TransferResult> {
    Err(TransferResult::Unsupported)
}

#[cfg(windows)]
pub fn write_clipboard_text(text: &str) -> Result<(), TransferResult> {
    use windows::Win32::{
        Foundation::{GlobalFree, HANDLE, HGLOBAL},
        System::{
            DataExchange::{EmptyClipboard, SetClipboardData},
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::CF_UNICODETEXT,
        },
    };

    if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
        return Err(TransferResult::TooLarge);
    }
    let mut wide: Vec<u16> = text.encode_utf16().collect();
    wide.push(0);
    let global = unsafe { GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2) }
        .map_err(|_| TransferResult::IoFailed)?;
    let pointer = unsafe { GlobalLock(global) }.cast::<u16>();
    if pointer.is_null() {
        let _ = unsafe { GlobalFree(Some(global)) };
        return Err(TransferResult::IoFailed);
    }
    unsafe { pointer.copy_from_nonoverlapping(wide.as_ptr(), wide.len()) };
    let _ = unsafe { GlobalUnlock(global) };

    let clipboard = ClipboardGuard::open();
    let Ok(_clipboard) = clipboard else {
        let _ = unsafe { GlobalFree(Some(global)) };
        return Err(TransferResult::IoFailed);
    };
    if unsafe { EmptyClipboard() }.is_err()
        || unsafe { SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(global.0))) }.is_err()
    {
        let _ = unsafe { GlobalFree(Some(HGLOBAL(global.0))) };
        return Err(TransferResult::IoFailed);
    }
    // Ownership of the allocation belongs to the clipboard after SetClipboardData succeeds.
    Ok(())
}

#[cfg(not(windows))]
pub fn write_clipboard_text(_text: &str) -> Result<(), TransferResult> {
    Err(TransferResult::Unsupported)
}

#[cfg(windows)]
struct ClipboardGuard;

#[cfg(windows)]
impl ClipboardGuard {
    fn open() -> Result<Self, TransferResult> {
        use windows::Win32::System::DataExchange::OpenClipboard;
        for _ in 0..5 {
            if unsafe { OpenClipboard(None) }.is_ok() {
                return Ok(Self);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(TransferResult::IoFailed)
    }
}

#[cfg(windows)]
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        use windows::Win32::System::DataExchange::CloseClipboard;
        let _ = unsafe { CloseClipboard() };
    }
}

fn human_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        INCOMING_DISK_QUEUE_CAPACITY, IncomingDiskCommand, IncomingFile, IncomingFileWorker,
        MIN_FREE_SPACE_AFTER_RECEIVE, OutgoingFileReader, STALE_INCOMING_FILE_AGE,
        chunk_after_resume, cleanup_stale_incoming_files, discard_incoming_resume_in,
        incoming_staging_directory, incoming_temporary_path, prepare_outgoing_file,
        receive_capacity_is_sufficient, verify_resume_prefix,
    };
    use blake2::{Blake2s256, Digest};
    use desklink_protocol::{MAX_TRANSFER_FILE_BYTES, TransferResult};
    use std::{
        fs::{self, File, FileTimes},
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    // Worker tests share the production process-wide eight-thread admission
    // budget. Serialize these fixtures while unrelated library tests stay
    // parallel, so resource contention is not mistaken for an IO failure.
    static DISK_WORKER_TEST_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn roots(test: &str) -> (PathBuf, PathBuf, PathBuf) {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "desklink-transfer-{test}-{}-{nonce}",
            std::process::id()
        ));
        let staging = root.join("staging");
        let downloads = root.join("downloads");
        (root, staging, downloads)
    }

    struct DiskThreadExit(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for DiskThreadExit {
        fn drop(&mut self) {
            if let Some(exit) = self.0.take() {
                let _ = exit.send(());
            }
        }
    }

    fn disk_thread_exit_notice() -> (impl Fn() + Send, tokio::sync::oneshot::Receiver<()>) {
        let (exit, exited) = tokio::sync::oneshot::channel();
        let notice = DiskThreadExit(Some(exit));
        (
            move || {
                let _ = &notice;
            },
            exited,
        )
    }

    async fn abandon_opening_worker(
        transfer_id: [u8; 16],
        expected_size: u64,
        staging: &std::path::Path,
        downloads: &std::path::Path,
    ) {
        use std::{
            future::Future,
            sync::{Arc, Condvar, Mutex},
            task::Poll,
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let worker_staging = staging.to_owned();
                let worker_downloads = downloads.to_owned();
                let gate = Arc::new((Mutex::new(false), Condvar::new()));
                let worker_gate = gate.clone();
                let (entered, opened) = tokio::sync::oneshot::channel();
                let (before_write, mut exited) = disk_thread_exit_notice();
                let mut creating = Box::pin(IncomingFileWorker::spawn_with(
                    move || {
                        IncomingFile::create_in_with_open_hook(
                            transfer_id,
                            "initializing.bin".to_owned(),
                            expected_size,
                            worker_staging,
                            worker_downloads,
                            move || {
                                let _ = entered.send(());
                                let (lock, wake) = &*worker_gate;
                                let mut open = lock.lock().unwrap();
                                while !*open {
                                    open = wake.wait(open).unwrap();
                                }
                            },
                        )
                    },
                    before_write,
                ));
                let initial =
                    std::future::poll_fn(|cx| Poll::Ready(creating.as_mut().poll(cx))).await;
                if matches!(initial, Poll::Ready(Err(TransferResult::Busy))) {
                    // Other worker tests can temporarily use all eight slots.
                    drop(creating);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    continue;
                }
                assert!(matches!(initial, Poll::Pending));
                opened.await.unwrap();
                assert_eq!(
                    exited.try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                );
                assert!(matches!(
                    IncomingFile::create_in(
                        transfer_id,
                        "initializing.bin".to_owned(),
                        expected_size,
                        staging.to_owned(),
                        downloads.to_owned(),
                    ),
                    Err(TransferResult::Busy)
                ));
                // This is the real opened.await future, not a synthetic ready
                // channel: cancellation happens while the disk thread owns an
                // open file and has not yet hashed its existing prefix.
                drop(creating);
                *gate.0.lock().unwrap() = true;
                gate.1.notify_one();
                // before_write remains owned by the worker closure; its exit
                // notice drops after local file and worker-permit cleanup.
                exited.await.unwrap();
                break;
            }
        })
        .await
        .expect("cancelled initialization must eventually release its worker");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_creation_preserves_verified_prefix_and_releases_worker_and_path_ownership() {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        let (root, staging, downloads) = roots("cancel-create-resume");
        let transfer_id = [29; 16];
        let partial = incoming_temporary_path(&staging, transfer_id);
        let mut first = IncomingFile::create_in(
            transfer_id,
            "initializing.bin".to_owned(),
            8,
            staging.clone(),
            downloads.clone(),
        )
        .unwrap();
        first.write_chunk(0, b"desk").unwrap();
        first.preserve().unwrap();
        // More cancellations than the worker cap prove cancelled initialization
        // does not retain a thread slot. Do not inspect unrelated live workers.
        for _ in 0..=super::MAX_INCOMING_DISK_WORKERS {
            abandon_opening_worker(transfer_id, 8, &staging, &downloads).await;
            assert_eq!(fs::read(&partial).unwrap(), b"desk");
            let reopened = IncomingFile::create_in(
                transfer_id,
                "initializing.bin".to_owned(),
                8,
                staging.clone(),
                downloads.clone(),
            )
            .unwrap();
            assert_eq!(reopened.transfer_id(), transfer_id);
            assert_eq!(reopened.name, "initializing.bin");
            assert_eq!(reopened.expected_size, 8);
            assert_eq!(reopened.resume_offset(), 4);
            assert_eq!(
                reopened.resume_prefix_hash(),
                Some(Blake2s256::digest(b"desk").into())
            );
            reopened.preserve().unwrap();
        }
        let mut resumed = IncomingFile::create_in(
            transfer_id,
            "initializing.bin".to_owned(),
            8,
            staging,
            downloads,
        )
        .unwrap();
        resumed.write_chunk(4, b"link").unwrap();
        let published = resumed
            .finish(Blake2s256::digest(b"desklink").into())
            .unwrap();
        assert_eq!(fs::read(published).unwrap(), b"desklink");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_empty_creation_preserves_an_empty_part_but_explicit_cancel_still_removes_it()
    {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        let (root, staging, downloads) = roots("cancel-create-empty");
        let transfer_id = [30; 16];
        let partial = incoming_temporary_path(&staging, transfer_id);
        abandon_opening_worker(transfer_id, 3, &staging, &downloads).await;
        assert_eq!(fs::metadata(&partial).unwrap().len(), 0);
        let (before_write, exited) = disk_thread_exit_notice();
        let worker = IncomingFileWorker::spawn_with(
            move || {
                IncomingFile::create_in(
                    transfer_id,
                    "initializing.bin".to_owned(),
                    3,
                    staging,
                    downloads,
                )
            },
            before_write,
        )
        .await
        .unwrap();
        assert_eq!(worker.resume_offset(), 0);
        assert_eq!(worker.resume_prefix_hash(), None);
        worker.cancel();
        tokio::time::timeout(Duration::from_secs(2), exited)
            .await
            .unwrap()
            .unwrap();
        assert!(!partial.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn initialization_failure_reports_its_error_without_deleting_a_staged_prefix() {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        let (root, staging, downloads) = roots("create-failed");
        let transfer_id = [31; 16];
        let partial = incoming_temporary_path(&staging, transfer_id);
        let mut first = IncomingFile::create_in(
            transfer_id,
            "initializing.bin".to_owned(),
            8,
            staging.clone(),
            downloads.clone(),
        )
        .unwrap();
        first.write_chunk(0, b"desk").unwrap();
        first.preserve().unwrap();
        let (before_write, exited) = disk_thread_exit_notice();
        let result = IncomingFileWorker::spawn_with(
            move || {
                IncomingFile::create_in(
                    transfer_id,
                    "initializing.bin".to_owned(),
                    MAX_TRANSFER_FILE_BYTES + 1,
                    staging,
                    downloads,
                )
            },
            before_write,
        )
        .await;
        assert!(matches!(result, Err(TransferResult::TooLarge)));
        tokio::time::timeout(Duration::from_secs(2), exited)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(partial).unwrap(), b"desk");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[tokio::test(flavor = "current_thread")]
    async fn preserve_flush_failure_is_reported_without_publishing_completion() {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        let (root, staging, downloads) = roots("preserve-flush-failed");
        let published = downloads.join("not-published.bin");
        let (before_write, exited) = disk_thread_exit_notice();
        let worker = IncomingFileWorker::spawn_with(
            move || {
                let mut file = IncomingFile::create_in(
                    [32; 16],
                    "not-published.bin".to_owned(),
                    3,
                    staging,
                    downloads,
                )?;
                file.write_chunk(0, b"new")?;
                file.file.take();
                // Windows FlushFileBuffers requires write access. Reopen the
                // real staging handle read-only to exercise an actual failure.
                file.file =
                    Some(File::open(&file.temporary_path).map_err(|_| TransferResult::IoFailed)?);
                Ok(file)
            },
            before_write,
        )
        .await
        .unwrap();
        assert_eq!(worker.preserve().await, Err(TransferResult::IoFailed));
        tokio::time::timeout(Duration::from_secs(2), exited)
            .await
            .unwrap()
            .unwrap();
        assert!(!published.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancelled_source_reader_does_not_open_a_source_or_take_a_share_lock() {
        let source = super::OutgoingFile {
            transfer_id: [39; 16],
            path: "must-not-be-opened.bin".into(),
            name: "must-not-be-opened.bin".into(),
            size: 1,
            modified_at_unix_ns: 0,
        };
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        assert!(matches!(
            super::OutgoingFileReader::open(&source, 0, None, cancelled),
            Err(desklink_protocol::TransferResult::Cancelled)
        ));
    }

    #[test]
    fn one_pass_source_reader_resumes_at_exact_chunk_boundaries_and_keeps_final_hash() {
        use std::{
            io::Seek,
            sync::{Arc, atomic::AtomicBool},
        };
        let (root, _, _) = roots("reader-boundaries");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("source.bin");
        let chunk = desklink_protocol::MAX_TRANSFER_CHUNK_BYTES;
        let payload: Vec<u8> = (0..chunk * 2 + 17)
            .map(|index| (index % 251) as u8)
            .collect();
        fs::write(&path, &payload).unwrap();
        let source = prepare_outgoing_file(path).unwrap();
        let expected: [u8; 32] = Blake2s256::digest(&payload).into();
        for resumed in [0, 1, chunk - 1, chunk, chunk + 3, payload.len()] {
            let proof = (resumed > 0).then(|| Blake2s256::digest(&payload[..resumed]).into());
            let mut reader = OutgoingFileReader::open(
                &source,
                resumed as u64,
                proof,
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
            assert_eq!(reader.file.stream_position().unwrap(), resumed as u64);
            let mut sent = Vec::new();
            let mut next_offset = resumed as u64;
            while let Some((offset, bytes)) = reader.read_chunk().unwrap() {
                assert_eq!(offset, next_offset);
                assert!(bytes.len() <= chunk);
                next_offset += bytes.len() as u64;
                sent.extend(bytes);
            }
            assert_eq!(reader.file.stream_position().unwrap(), payload.len() as u64);
            assert_eq!(sent, payload[resumed..]);
            assert_eq!(reader.finish().unwrap(), expected);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn source_reader_checks_proof_and_metadata_before_sending_any_suffix() {
        use std::sync::{Arc, atomic::AtomicBool};
        let (root, _, _) = roots("reader-changed");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("source.bin");
        fs::write(&path, b"desklink").unwrap();
        let source = prepare_outgoing_file(path.clone()).unwrap();
        assert!(matches!(
            OutgoingFileReader::open(&source, 4, Some([0; 32]), Arc::new(AtomicBool::new(false))),
            Err(TransferResult::SourceChanged)
        ));
        assert!(matches!(
            OutgoingFileReader::open(&source, 0, Some([0; 32]), Arc::new(AtomicBool::new(false))),
            Err(TransferResult::InvalidData)
        ));
        fs::write(&path, b"replaced").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
            )
            .unwrap();
        assert!(matches!(
            OutgoingFileReader::open(&source, 0, None, Arc::new(AtomicBool::new(false))),
            Err(TransferResult::SourceChanged)
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn source_reader_stops_on_cancel_and_never_finalizes_a_partial_read() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let (root, _, _) = roots("reader-cancel");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("source.bin");
        fs::write(&path, b"desklink").unwrap();
        let source = prepare_outgoing_file(path).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let reader = OutgoingFileReader::open(&source, 0, None, cancelled.clone()).unwrap();
        assert_eq!(reader.finish(), Err(TransferResult::InvalidData));
        let mut reader = OutgoingFileReader::open(&source, 0, None, cancelled.clone()).unwrap();
        cancelled.store(true, Ordering::Release);
        assert_eq!(reader.read_chunk(), Err(TransferResult::Cancelled));
        assert_eq!(reader.finish(), Err(TransferResult::Cancelled));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn source_reader_holds_a_read_only_windows_snapshot_until_finished() {
        use std::sync::{Arc, atomic::AtomicBool};
        let (root, _, _) = roots("reader-snapshot");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("source.bin");
        fs::write(&path, b"desklink").unwrap();
        let source = prepare_outgoing_file(path.clone()).unwrap();
        let mut reader =
            OutgoingFileReader::open(&source, 0, None, Arc::new(AtomicBool::new(false))).unwrap();
        assert!(File::options().write(true).open(&path).is_err());
        assert!(fs::remove_file(&path).is_err());
        assert_eq!(reader.read_chunk().unwrap().unwrap().1, b"desklink");
        reader.finish().unwrap();
        fs::write(&path, b"changed!").unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disk_worker_orders_chunks_and_only_acknowledges_verified_publication() {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        let (root, staging, downloads) = roots("worker-publish");
        let target = downloads.join("worker.bin");
        let mut worker = IncomingFileWorker::spawn_with(
            move || {
                IncomingFile::create_in([21; 16], "worker.bin".to_owned(), 8, staging, downloads)
            },
            || {},
        )
        .await
        .unwrap();
        assert_eq!(worker.resume_offset(), 0);
        worker.write_chunk(0, b"desk".to_vec()).await.unwrap();
        worker.write_chunk(4, b"link".to_vec()).await.unwrap();
        assert!(!target.exists());
        let saved = worker
            .finish(Blake2s256::digest(b"desklink").into())
            .await
            .unwrap();
        assert_eq!(saved, target);
        assert_eq!(fs::read(&saved).unwrap(), b"desklink");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_disk_worker_does_not_block_async_timers() {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        use std::sync::{Arc, Condvar, Mutex};
        let (root, staging, downloads) = roots("worker-slow");
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = gate.clone();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let entered = Mutex::new(Some(entered));
        let mut worker = IncomingFileWorker::spawn_with(
            move || IncomingFile::create_in([22; 16], "slow.bin".to_owned(), 1, staging, downloads),
            move || {
                if let Some(entered) = entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                let (lock, wake) = &*worker_gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = wake.wait(open).unwrap();
                }
            },
        )
        .await
        .unwrap();
        let pending = tokio::spawn(async move {
            worker.write_chunk(0, vec![42]).await.unwrap();
            worker.finish(Blake2s256::digest([42]).into()).await
        });
        ready.await.unwrap();
        tokio::time::timeout(
            Duration::from_millis(250),
            tokio::time::sleep(Duration::from_millis(1)),
        )
        .await
        .expect("disk wait must not block a single-thread Tokio runtime");
        assert!(!pending.is_finished());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_one();
        pending.await.unwrap().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disk_worker_preserve_then_resume_keeps_only_acknowledged_prefix() {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        let (root, staging, downloads) = roots("worker-resume");
        let worker_staging = staging.clone();
        let worker_downloads = downloads.clone();
        let mut worker = IncomingFileWorker::spawn_with(
            move || {
                IncomingFile::create_in(
                    [23; 16],
                    "resume.bin".to_owned(),
                    8,
                    worker_staging,
                    worker_downloads,
                )
            },
            || {},
        )
        .await
        .unwrap();
        worker.write_chunk(0, b"desk".to_vec()).await.unwrap();
        worker.preserve().await.unwrap();
        let resumed = IncomingFileWorker::spawn_with(
            move || {
                IncomingFile::create_in([23; 16], "resume.bin".to_owned(), 8, staging, downloads)
            },
            || {},
        )
        .await
        .unwrap();
        assert_eq!(resumed.resume_offset(), 4);
        assert_eq!(
            resumed.resume_prefix_hash(),
            Some(Blake2s256::digest(b"desk").into())
        );
        resumed.cancel();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disk_queue_has_a_fixed_chunk_budget_and_cancel_discards_pending_work() {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        use std::sync::{Arc, Condvar, Mutex};
        let (root, staging, downloads) = roots("worker-budget");
        let partial = incoming_temporary_path(&staging, [24; 16]);
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = gate.clone();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let entered = Mutex::new(Some(entered));
        let worker = IncomingFileWorker::spawn_with(
            move || {
                IncomingFile::create_in([24; 16], "cancel.bin".to_owned(), 4, staging, downloads)
            },
            move || {
                if let Some(entered) = entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                let (lock, wake) = &*worker_gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = wake.wait(open).unwrap();
                }
            },
        )
        .await
        .unwrap();
        let sender = worker.commands.as_ref().unwrap().clone();
        let command = |offset| {
            let (reply, _) = tokio::sync::oneshot::channel();
            IncomingDiskCommand::Write {
                offset,
                bytes: vec![0].into_boxed_slice(),
                reply,
            }
        };
        sender.send(command(0)).await.unwrap();
        ready.await.unwrap();
        for offset in 1..=INCOMING_DISK_QUEUE_CAPACITY {
            sender.try_send(command(offset as u64)).unwrap();
        }
        assert!(matches!(
            sender.try_send(command(3)),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_))
        ));
        worker.cancel();
        drop(sender);
        *gate.0.lock().unwrap() = true;
        gate.1.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            while partial.exists() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn disk_worker_rejects_oversized_chunks_and_invalid_hashes() {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        let (root, staging, downloads) = roots("worker-invalid");
        let mut worker = IncomingFileWorker::spawn_with(
            move || {
                IncomingFile::create_in([25; 16], "invalid.bin".to_owned(), 1, staging, downloads)
            },
            || {},
        )
        .await
        .unwrap();
        assert_eq!(
            worker
                .write_chunk(0, vec![0; desklink_protocol::MAX_TRANSFER_CHUNK_BYTES + 1])
                .await,
            Err(TransferResult::InvalidData)
        );
        worker.write_chunk(0, vec![42]).await.unwrap();
        assert_eq!(
            worker.finish([0; 32]).await,
            Err(TransferResult::InvalidData)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropped_worker_keeps_writer_ownership_until_pending_write_and_preserve_finish() {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        use std::sync::{Arc, Condvar, Mutex};
        let (root, staging, downloads) = roots("worker-reconnect-preserve");
        let worker_staging = staging.clone();
        let worker_downloads = downloads.clone();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = gate.clone();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let entered = Mutex::new(Some(entered));
        let worker = IncomingFileWorker::spawn_with(
            move || {
                IncomingFile::create_in(
                    [26; 16],
                    "race.bin".to_owned(),
                    8,
                    worker_staging,
                    worker_downloads,
                )
            },
            move || {
                if let Some(entered) = entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                let (lock, wake) = &*worker_gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = wake.wait(open).unwrap();
                }
            },
        )
        .await
        .unwrap();
        let (reply, response) = tokio::sync::oneshot::channel();
        worker
            .commands
            .as_ref()
            .unwrap()
            .send(IncomingDiskCommand::Write {
                offset: 0,
                bytes: b"old!".to_vec().into_boxed_slice(),
                reply,
            })
            .await
            .unwrap();
        ready.await.unwrap();
        drop(worker);
        // A lexical alias must resolve to the same owned staging path.
        assert!(matches!(
            IncomingFile::create_in(
                [26; 16],
                "race.bin".to_owned(),
                8,
                staging.join("."),
                downloads.clone()
            ),
            Err(TransferResult::Busy)
        ));
        assert!(!downloads.join("race.bin").exists());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_one();
        assert_eq!(response.await.unwrap(), Ok(()));
        let mut resumed = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match IncomingFile::create_in(
                    [26; 16],
                    "race.bin".to_owned(),
                    8,
                    staging.clone(),
                    downloads.clone(),
                ) {
                    Ok(file) => break file,
                    Err(TransferResult::Busy) => tokio::time::sleep(Duration::from_millis(2)).await,
                    Err(error) => panic!("unexpected resume failure: {error:?}"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(resumed.resume_offset(), 4);
        assert_eq!(
            resumed.resume_prefix_hash(),
            Some(Blake2s256::digest(b"old!").into())
        );
        resumed.write_chunk(4, b"new!").unwrap();
        let saved = resumed
            .finish(Blake2s256::digest(b"old!new!").into())
            .unwrap();
        assert_eq!(fs::read(saved).unwrap(), b"old!new!");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_worker_cannot_delete_a_new_receiver_staging_file() {
        let _test = DISK_WORKER_TEST_GATE.lock().await;
        use std::sync::{Arc, Condvar, Mutex};
        let (root, staging, downloads) = roots("worker-reconnect-cancel");
        let worker_staging = staging.clone();
        let worker_downloads = downloads.clone();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_gate = gate.clone();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let entered = Mutex::new(Some(entered));
        let worker = IncomingFileWorker::spawn_with(
            move || {
                IncomingFile::create_in(
                    [27; 16],
                    "cancel.bin".to_owned(),
                    3,
                    worker_staging,
                    worker_downloads,
                )
            },
            move || {
                if let Some(entered) = entered.lock().unwrap().take() {
                    let _ = entered.send(());
                }
                let (lock, wake) = &*worker_gate;
                let mut open = lock.lock().unwrap();
                while !*open {
                    open = wake.wait(open).unwrap();
                }
            },
        )
        .await
        .unwrap();
        let (reply, response) = tokio::sync::oneshot::channel();
        worker
            .commands
            .as_ref()
            .unwrap()
            .send(IncomingDiskCommand::Write {
                offset: 0,
                bytes: vec![0].into_boxed_slice(),
                reply,
            })
            .await
            .unwrap();
        ready.await.unwrap();
        worker.cancel();
        assert!(matches!(
            IncomingFile::create_in(
                [27; 16],
                "cancel.bin".to_owned(),
                3,
                staging.clone(),
                downloads.clone()
            ),
            Err(TransferResult::Busy)
        ));
        *gate.0.lock().unwrap() = true;
        gate.1.notify_one();
        assert_eq!(response.await.unwrap(), Err(TransferResult::Cancelled));
        let mut replacement = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match IncomingFile::create_in(
                    [27; 16],
                    "cancel.bin".to_owned(),
                    3,
                    staging.clone(),
                    downloads.clone(),
                ) {
                    Ok(file) => break file,
                    Err(TransferResult::Busy | TransferResult::IoFailed) => {
                        tokio::time::sleep(Duration::from_millis(2)).await
                    }
                    Err(error) => panic!("unexpected replacement failure: {error:?}"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(replacement.resume_offset(), 0);
        replacement.write_chunk(0, b"new").unwrap();
        let saved = replacement
            .finish(Blake2s256::digest(b"new").into())
            .unwrap();
        assert_eq!(fs::read(saved).unwrap(), b"new");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn incoming_writer_denies_bypassing_registry_with_another_windows_write_or_delete() {
        let (root, staging, downloads) = roots("writer-sharing");
        let partial = incoming_temporary_path(&staging, [28; 16]);
        let mut file =
            IncomingFile::create_in([28; 16], "locked.bin".to_owned(), 3, staging, downloads)
                .unwrap();
        assert!(File::options().write(true).open(&partial).is_err());
        assert!(fs::remove_file(&partial).is_err());
        file.write_chunk(0, b"one").unwrap();
        file.preserve().unwrap();
        File::options().append(true).open(&partial).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn incoming_file_requires_sequential_chunks_and_valid_hash() {
        let (root, staging, downloads) = roots("integrity");
        let mut file = IncomingFile::create_in(
            [1; 16],
            "报告.txt".to_owned(),
            5,
            staging,
            downloads.clone(),
        )
        .unwrap();
        assert_eq!(
            file.write_chunk(1, b"hello"),
            Err(TransferResult::InvalidData)
        );
        file.write_chunk(0, b"hello").unwrap();
        assert_eq!(file.finish([0; 32]), Err(TransferResult::InvalidData));
        assert!(!downloads.join("报告.txt").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn verified_file_is_published_without_overwriting_existing_download() {
        let (root, staging, downloads) = roots("publish");
        fs::create_dir_all(&downloads).unwrap();
        fs::write(downloads.join("report.txt"), b"existing").unwrap();
        let payload = b"desklink";
        let mut file = IncomingFile::create_in(
            [2; 16],
            "report.txt".to_owned(),
            payload.len() as u64,
            staging,
            downloads.clone(),
        )
        .unwrap();
        file.write_chunk(0, payload).unwrap();
        let hash: [u8; 32] = Blake2s256::digest(payload).into();
        let saved = file.finish(hash).unwrap();
        assert_eq!(saved, downloads.join("report (1).txt"));
        assert_eq!(fs::read(downloads.join("report.txt")).unwrap(), b"existing");
        assert_eq!(fs::read(saved).unwrap(), payload);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_incoming_file_resumes_and_verifies_the_complete_hash() {
        let (root, staging, downloads) = roots("resume");
        let payload = b"desklink-resume";
        let transfer_id = [9; 16];
        let mut first = IncomingFile::create_in(
            transfer_id,
            "resume.bin".to_owned(),
            payload.len() as u64,
            staging.clone(),
            downloads.clone(),
        )
        .unwrap();
        first.write_chunk(0, &payload[..8]).unwrap();
        assert_eq!(first.resume_offset(), 8);
        assert_eq!(
            first.resume_prefix_hash(),
            Some(Blake2s256::digest(&payload[..8]).into())
        );
        first.preserve().unwrap();

        let mut resumed = IncomingFile::create_in(
            transfer_id,
            "resume.bin".to_owned(),
            payload.len() as u64,
            staging,
            downloads.clone(),
        )
        .unwrap();
        assert_eq!(resumed.resume_offset(), 8);
        resumed.write_chunk(8, &payload[8..]).unwrap();
        let hash: [u8; 32] = Blake2s256::digest(payload).into();
        let saved = resumed.finish(hash).unwrap();
        assert_eq!(fs::read(saved).unwrap(), payload);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn discarding_a_resume_removes_only_the_matching_staged_file() {
        let (root, _, downloads) = roots("discard-resume");
        let staging = incoming_staging_directory(&downloads);
        fs::create_dir_all(&staging).unwrap();
        let target = incoming_temporary_path(&staging, [6; 16]);
        let other = incoming_temporary_path(&staging, [7; 16]);
        fs::write(&target, b"partial").unwrap();
        fs::write(&other, b"keep").unwrap();

        assert!(discard_incoming_resume_in(&downloads, [6; 16]).unwrap());
        assert!(!target.exists());
        assert!(other.exists());
        assert!(!discard_incoming_resume_in(&downloads, [6; 16]).unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn oversized_partial_file_is_rejected_and_removed() {
        let (root, staging, downloads) = roots("resume-invalid");
        fs::create_dir_all(&staging).unwrap();
        let partial = staging.join(".incoming-08080808080808080808080808080808.part");
        fs::write(&partial, b"too-long").unwrap();
        let result =
            IncomingFile::create_in([8; 16], "resume.bin".to_owned(), 3, staging, downloads);
        assert!(matches!(result, Err(TransferResult::InvalidData)));
        assert!(!partial.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn outgoing_file_preparation_rejects_directories_and_keeps_safe_metadata() {
        let (root, _, _) = roots("outgoing");
        fs::create_dir_all(&root).unwrap();
        assert_eq!(
            prepare_outgoing_file(root.clone()).unwrap_err(),
            TransferResult::InvalidData
        );

        let path = root.join("发送报告.txt");
        fs::write(&path, b"desklink").unwrap();
        let prepared = prepare_outgoing_file(path.clone()).unwrap();
        assert_eq!(prepared.path, path);
        assert_eq!(prepared.name, "发送报告.txt");
        assert_eq!(prepared.size, 8);
        assert!(prepared.transfer_id.iter().any(|byte| *byte != 0));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn outgoing_chunks_skip_the_staged_prefix_including_mid_chunk_offsets() {
        assert_eq!(chunk_after_resume(0, vec![1, 2, 3], 3).unwrap(), None);
        assert_eq!(
            chunk_after_resume(0, vec![1, 2, 3, 4], 2).unwrap(),
            Some((2, vec![3, 4]))
        );
        assert_eq!(
            chunk_after_resume(4, vec![5, 6], 2).unwrap(),
            Some((4, vec![5, 6]))
        );
    }

    #[test]
    fn resume_prefix_proof_rejects_a_changed_same_sized_source() {
        let (root, _, _) = roots("resume-proof");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("source.bin");
        fs::write(&path, b"desklink-source").unwrap();
        let offset = 8;
        let hash: [u8; 32] = Blake2s256::digest(b"desklink").into();
        verify_resume_prefix(&path, offset, Some(hash)).unwrap();

        fs::write(&path, b"changed!-source").unwrap();
        assert_eq!(
            verify_resume_prefix(&path, offset, Some(hash)),
            Err(TransferResult::SourceChanged)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn receive_capacity_keeps_headroom_and_allows_completed_parts() {
        assert!(receive_capacity_is_sufficient(0, 0));
        assert!(!receive_capacity_is_sufficient(
            MIN_FREE_SPACE_AFTER_RECEIVE,
            1
        ));
        assert!(receive_capacity_is_sufficient(
            MIN_FREE_SPACE_AFTER_RECEIVE + 1,
            1
        ));
    }

    #[test]
    fn incoming_staging_stays_on_the_download_volume_and_cleans_only_stale_parts() {
        let (root, _, downloads) = roots("staging-cleanup");
        let staging = incoming_staging_directory(&downloads);
        assert_eq!(staging.parent(), Some(downloads.as_path()));
        fs::create_dir_all(&staging).unwrap();

        let stale = staging.join(".incoming-11111111111111111111111111111111.part");
        let fresh = staging.join(".incoming-22222222222222222222222222222222.part");
        let unrelated = staging.join("keep.txt");
        fs::write(&stale, b"stale").unwrap();
        fs::write(&fresh, b"fresh").unwrap();
        fs::write(&unrelated, b"keep").unwrap();
        let stale_time = SystemTime::now()
            .checked_sub(STALE_INCOMING_FILE_AGE + Duration::from_secs(1))
            .unwrap();
        File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_times(FileTimes::new().set_modified(stale_time))
            .unwrap();

        cleanup_stale_incoming_files(&staging);
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(unrelated.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn incoming_file_rejects_oversized_offers_before_creating_staging_data() {
        let (root, staging, downloads) = roots("incoming-limit");
        let result = IncomingFile::create_in(
            [3; 16],
            "large.bin".to_owned(),
            MAX_TRANSFER_FILE_BYTES + 1,
            staging.clone(),
            downloads,
        );
        assert!(matches!(result, Err(TransferResult::TooLarge)));
        assert!(!staging.exists());
        let _ = fs::remove_dir_all(root);
    }
}
