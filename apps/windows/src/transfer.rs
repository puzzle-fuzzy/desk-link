use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use blake2::{Blake2s256, Digest};
use desklink_protocol::{
    MAX_CLIPBOARD_TEXT_BYTES, MAX_TRANSFER_CHUNK_BYTES, MAX_TRANSFER_FILE_BYTES, TransferId,
    TransferResult, is_valid_transfer_file_name,
};
use rand_core::{OsRng, RngCore};

use crate::storage::downloads_path;

const INCOMING_STAGING_DIRECTORY: &str = ".desklink-transfers";
const INCOMING_PART_PREFIX: &str = ".incoming-";
const INCOMING_PART_SUFFIX: &str = ".part";
const STALE_INCOMING_FILE_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_STAGING_ENTRIES_TO_CLEAN: usize = 256;
const MIN_FREE_SPACE_AFTER_RECEIVE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct OutgoingFile {
    pub transfer_id: TransferId,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified_at_unix_ns: u64,
}

pub fn prepare_outgoing_file(path: PathBuf) -> Result<OutgoingFile, TransferResult> {
    let metadata = fs::metadata(&path).map_err(|_| TransferResult::IoFailed)?;
    if !metadata.is_file() {
        return Err(TransferResult::InvalidData);
    }
    if metadata.len() > MAX_TRANSFER_FILE_BYTES {
        return Err(TransferResult::TooLarge);
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| is_valid_transfer_file_name(value))
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
        modified_at_unix_ns: metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
            .unwrap_or(0),
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
    use windows::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree,
            },
            UI::Shell::{
                FOS_ALLOWMULTISELECT, FOS_DONTADDTORECENT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
                FOS_PATHMUSTEXIST, FileOpenDialog, IFileOpenDialog, IShellItem, SIGDN_FILESYSPATH,
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
        if !is_valid_transfer_file_name(&name) {
            return Err(TransferResult::InvalidData);
        }
        if expected_size > MAX_TRANSFER_FILE_BYTES {
            return Err(TransferResult::TooLarge);
        }
        prepare_incoming_staging_directory(&directory)?;
        let temporary_path = incoming_temporary_path(&directory, transfer_id);
        let mut hasher = Blake2s256::new();
        let (file, written) = match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)
        {
            Ok(file) => (file, 0),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata =
                    fs::symlink_metadata(&temporary_path).map_err(|_| TransferResult::IoFailed)?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(TransferResult::IoFailed);
                }
                let written = metadata.len();
                if written > expected_size {
                    let _ = fs::remove_file(&temporary_path);
                    return Err(TransferResult::InvalidData);
                }
                let mut existing =
                    File::open(&temporary_path).map_err(|_| TransferResult::IoFailed)?;
                let mut buffer = vec![0_u8; MAX_TRANSFER_CHUNK_BYTES];
                loop {
                    let read = existing
                        .read(&mut buffer)
                        .map_err(|_| TransferResult::IoFailed)?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                }
                drop(existing);
                let file = OpenOptions::new()
                    .append(true)
                    .open(&temporary_path)
                    .map_err(|_| TransferResult::IoFailed)?;
                (file, written)
            }
            Err(_) => return Err(TransferResult::IoFailed),
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

    pub fn finish(mut self, content_hash: [u8; 32]) -> Result<PathBuf, TransferResult> {
        if self.written != self.expected_size
            || self.hasher.clone().finalize().as_slice() != content_hash
        {
            return Err(TransferResult::InvalidData);
        }
        let file = self.file.take().ok_or(TransferResult::InvalidData)?;
        file.sync_all().map_err(|_| TransferResult::IoFailed)?;
        drop(file);
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
fn ensure_receive_capacity(directory: &Path, remaining: u64) -> Result<(), TransferResult> {
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
fn ensure_receive_capacity(_directory: &Path, _remaining: u64) -> Result<(), TransferResult> {
    Ok(())
}

impl Drop for IncomingFile {
    fn drop(&mut self) {
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
        let _ = fs::remove_file(entry.path());
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
        IncomingFile, MIN_FREE_SPACE_AFTER_RECEIVE, STALE_INCOMING_FILE_AGE, chunk_after_resume,
        cleanup_stale_incoming_files, discard_incoming_resume_in, incoming_staging_directory,
        incoming_temporary_path, prepare_outgoing_file, receive_capacity_is_sufficient,
        verify_resume_prefix,
    };
    use blake2::{Blake2s256, Digest};
    use desklink_protocol::{MAX_TRANSFER_FILE_BYTES, TransferResult};
    use std::{
        fs::{self, File, FileTimes},
        path::PathBuf,
        time::{Duration, SystemTime},
    };

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
