use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use blake2::{Blake2s256, Digest};
use desklink_protocol::{
    MAX_CLIPBOARD_TEXT_BYTES, MAX_TRANSFER_FILE_BYTES, TransferId, TransferResult,
    is_valid_transfer_file_name,
};
use rand_core::{OsRng, RngCore};

use crate::storage::{downloads_path, local_app_data_path};

#[derive(Clone, Debug)]
pub struct OutgoingFile {
    pub transfer_id: TransferId,
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
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
    })
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
        let directory = local_app_data_path()
            .ok_or(TransferResult::IoFailed)?
            .join("DeskLink")
            .join("Transfers");
        let destination_directory = downloads_path().ok_or(TransferResult::IoFailed)?;
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
        fs::create_dir_all(&directory).map_err(|_| TransferResult::IoFailed)?;
        let temporary_path = directory.join(format!(
            ".incoming-{}.part",
            transfer_id
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        ));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)
            .map_err(|_| TransferResult::IoFailed)?;
        Ok(Self {
            transfer_id,
            name,
            expected_size,
            written: 0,
            hasher: Blake2s256::new(),
            file: Some(file),
            temporary_path,
            destination_directory,
        })
    }

    pub fn transfer_id(&self) -> TransferId {
        self.transfer_id
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

impl Drop for IncomingFile {
    fn drop(&mut self) {
        if self.temporary_path.exists() {
            let _ = fs::remove_file(&self.temporary_path);
        }
    }
}

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
    use super::{IncomingFile, prepare_outgoing_file};
    use blake2::{Blake2s256, Digest};
    use desklink_protocol::TransferResult;
    use std::{fs, path::PathBuf, time::SystemTime};

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
}
