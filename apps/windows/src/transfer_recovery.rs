use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    slice,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use desklink_protocol::{
    FileResumeHint, MAX_TRANSFER_FILE_BYTES, TransferId, is_valid_transfer_file_name,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
        Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
    },
    core::PCWSTR,
};
use zeroize::Zeroize;

use crate::{storage::local_app_data_path, transfer::OutgoingFile};

const MAX_PLAINTEXT_BYTES: usize = 1024 * 1024;
const MAX_PROTECTED_BYTES: usize = 2 * 1024 * 1024;
const MAX_RECOVERY_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_FUTURE_SKEW: Duration = Duration::from_secs(5 * 60);
pub const MAX_FILE_QUEUE_RECOVERY_ITEMS: usize = 20;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TransferRecoveryAction {
    Upload {
        host_device_id: [u8; 16],
        transfer_id: TransferId,
        path: PathBuf,
        name: String,
        size: u64,
        #[serde(default)]
        modified_at_unix_ns: u64,
        saved_at_unix_s: u64,
    },
    Download {
        host_device_id: [u8; 16],
        resume: Option<FileResumeHint>,
        saved_at_unix_s: u64,
    },
}

impl TransferRecoveryAction {
    pub fn upload(host_device_id: [u8; 16], file: &OutgoingFile) -> Self {
        Self::Upload {
            host_device_id,
            transfer_id: file.transfer_id,
            path: file.path.clone(),
            name: file.name.clone(),
            size: file.size,
            modified_at_unix_ns: file.modified_at_unix_ns,
            saved_at_unix_s: now_unix_s(),
        }
    }

    pub fn download(host_device_id: [u8; 16], resume: Option<FileResumeHint>) -> Self {
        Self::Download {
            host_device_id,
            resume,
            saved_at_unix_s: now_unix_s(),
        }
    }

    fn saved_at_unix_s(&self) -> u64 {
        match self {
            Self::Upload {
                saved_at_unix_s, ..
            }
            | Self::Download {
                saved_at_unix_s, ..
            } => *saved_at_unix_s,
        }
    }

    fn validate(&self, now_unix_s: u64) -> Result<(), WindowsTransferRecoveryError> {
        let saved_at = self.saved_at_unix_s();
        if saved_at == 0 || saved_at > now_unix_s.saturating_add(MAX_FUTURE_SKEW.as_secs()) {
            return Err(WindowsTransferRecoveryError::CorruptStore);
        }
        match self {
            Self::Upload {
                host_device_id,
                transfer_id,
                path,
                name,
                size,
                ..
            } => {
                if !valid_device_id(host_device_id)
                    || !valid_transfer_id(transfer_id)
                    || !path.is_absolute()
                    || path.as_os_str().encode_wide().count() > 32_767
                    || path.file_name().and_then(|value| value.to_str()) != Some(name)
                    || !is_valid_transfer_file_name(name)
                    || *size > MAX_TRANSFER_FILE_BYTES
                {
                    return Err(WindowsTransferRecoveryError::CorruptStore);
                }
            }
            Self::Download {
                host_device_id,
                resume,
                ..
            } => {
                if !valid_device_id(host_device_id)
                    || resume.as_ref().is_some_and(|resume| {
                        !valid_transfer_id(&resume.transfer_id)
                            || !is_valid_transfer_file_name(&resume.name)
                            || resume.size > MAX_TRANSFER_FILE_BYTES
                    })
                {
                    return Err(WindowsTransferRecoveryError::CorruptStore);
                }
            }
        }
        Ok(())
    }

    fn is_expired(&self, now_unix_s: u64) -> bool {
        now_unix_s.saturating_sub(self.saved_at_unix_s()) > MAX_RECOVERY_AGE.as_secs()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct QueuedTransferRecoveryFile {
    transfer_id: TransferId,
    path: PathBuf,
    name: String,
    size: u64,
    #[serde(default)]
    modified_at_unix_ns: u64,
}

impl QueuedTransferRecoveryFile {
    fn from_outgoing(file: &OutgoingFile) -> Self {
        Self {
            transfer_id: file.transfer_id,
            path: file.path.clone(),
            name: file.name.clone(),
            size: file.size,
            modified_at_unix_ns: file.modified_at_unix_ns,
        }
    }

    pub fn outgoing_file(&self) -> OutgoingFile {
        OutgoingFile {
            transfer_id: self.transfer_id,
            path: self.path.clone(),
            name: self.name.clone(),
            size: self.size,
            modified_at_unix_ns: self.modified_at_unix_ns,
        }
    }

    fn matches_outgoing(&self, file: &OutgoingFile) -> bool {
        self.transfer_id == file.transfer_id
            && self.path == file.path
            && self.name == file.name
            && self.size == file.size
            && self.modified_at_unix_ns == file.modified_at_unix_ns
    }

    fn validate(&self) -> Result<(), WindowsTransferRecoveryError> {
        if !valid_transfer_id(&self.transfer_id)
            || !self.path.is_absolute()
            || self.path.as_os_str().encode_wide().count() > 32_767
            || self.path.file_name().and_then(|value| value.to_str()) != Some(&self.name)
            || !is_valid_transfer_file_name(&self.name)
            || self.size > MAX_TRANSFER_FILE_BYTES
        {
            return Err(WindowsTransferRecoveryError::CorruptStore);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransferQueueRecovery {
    host_device_id: [u8; 16],
    files: Vec<QueuedTransferRecoveryFile>,
    paused: bool,
    saved_at_unix_s: u64,
}

impl TransferQueueRecovery {
    pub fn new<'a>(
        host_device_id: [u8; 16],
        files: impl IntoIterator<Item = &'a OutgoingFile>,
        paused: bool,
    ) -> Self {
        Self {
            host_device_id,
            files: files
                .into_iter()
                .map(QueuedTransferRecoveryFile::from_outgoing)
                .collect(),
            paused,
            saved_at_unix_s: now_unix_s(),
        }
    }

    pub fn host_device_id(&self) -> [u8; 16] {
        self.host_device_id
    }

    pub fn files(&self) -> &[QueuedTransferRecoveryFile] {
        &self.files
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn matches_queue<'a>(
        &self,
        host_device_id: [u8; 16],
        files: impl IntoIterator<Item = &'a OutgoingFile>,
        paused: bool,
    ) -> bool {
        if self.host_device_id != host_device_id || self.paused != paused {
            return false;
        }
        let mut files = files.into_iter();
        self.files.iter().all(|saved| {
            files
                .next()
                .is_some_and(|file| saved.matches_outgoing(file))
        }) && files.next().is_none()
    }

    fn validate(&self, now_unix_s: u64) -> Result<(), WindowsTransferRecoveryError> {
        if !valid_device_id(&self.host_device_id)
            || self.files.is_empty()
            || self.files.len() > MAX_FILE_QUEUE_RECOVERY_ITEMS
            || self.saved_at_unix_s == 0
            || self.saved_at_unix_s > now_unix_s.saturating_add(MAX_FUTURE_SKEW.as_secs())
        {
            return Err(WindowsTransferRecoveryError::CorruptStore);
        }
        for (index, file) in self.files.iter().enumerate() {
            file.validate()?;
            if self.files[..index]
                .iter()
                .any(|previous| previous.transfer_id == file.transfer_id)
            {
                return Err(WindowsTransferRecoveryError::CorruptStore);
            }
        }
        Ok(())
    }

    fn is_expired(&self, now_unix_s: u64) -> bool {
        now_unix_s.saturating_sub(self.saved_at_unix_s) > MAX_RECOVERY_AGE.as_secs()
    }
}

#[derive(Debug, Error)]
pub enum WindowsTransferRecoveryError {
    #[error("文件恢复存储路径不可用")]
    MissingStoragePath,
    #[error("文件恢复记录操作失败：{0}")]
    Io(#[from] io::Error),
    #[error("Windows 文件恢复记录保护失败：{0}")]
    Platform(#[from] windows::core::Error),
    #[error("受保护的文件恢复记录已损坏，或属于其他 Windows 用户")]
    CorruptProtectedData,
    #[error("文件恢复记录格式无效")]
    CorruptStore,
}

#[derive(Clone, Debug)]
pub struct WindowsTransferRecoveryStore {
    path: PathBuf,
    operation_lock: Arc<Mutex<()>>,
}

impl WindowsTransferRecoveryStore {
    pub fn for_current_user() -> Result<Self, WindowsTransferRecoveryError> {
        let local_app_data =
            local_app_data_path().ok_or(WindowsTransferRecoveryError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data
                .join("DeskLink")
                .join("transfer-recovery.bin"),
        ))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            operation_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn load(&self) -> Result<Option<TransferRecoveryAction>, WindowsTransferRecoveryError> {
        self.load_at(now_unix_s())
    }

    fn load_at(
        &self,
        now_unix_s: u64,
    ) -> Result<Option<TransferRecoveryAction>, WindowsTransferRecoveryError> {
        let Some(mut plaintext) = self.load_plaintext()? else {
            return Ok(None);
        };
        let action = serde_json::from_slice::<TransferRecoveryAction>(&plaintext)
            .map_err(|_| WindowsTransferRecoveryError::CorruptStore);
        plaintext.zeroize();
        let action = action?;
        action.validate(now_unix_s)?;
        if action.is_expired(now_unix_s) {
            let _ = self.clear_locked();
            return Ok(None);
        }
        Ok(Some(action))
    }

    pub fn save(
        &self,
        action: &TransferRecoveryAction,
    ) -> Result<(), WindowsTransferRecoveryError> {
        action.validate(now_unix_s())?;
        let mut plaintext =
            serde_json::to_vec(action).map_err(|_| WindowsTransferRecoveryError::CorruptStore)?;
        let result = self.save_plaintext(&plaintext);
        plaintext.zeroize();
        result
    }

    fn load_plaintext(&self) -> Result<Option<Vec<u8>>, WindowsTransferRecoveryError> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| WindowsTransferRecoveryError::CorruptStore)?;
        let protected = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if protected.is_empty() || protected.len() > MAX_PROTECTED_BYTES {
            return Err(WindowsTransferRecoveryError::CorruptStore);
        }
        let mut plaintext = unprotect(&protected)?;
        if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_BYTES {
            plaintext.zeroize();
            return Err(WindowsTransferRecoveryError::CorruptStore);
        }
        Ok(Some(plaintext))
    }

    fn save_plaintext(&self, plaintext: &[u8]) -> Result<(), WindowsTransferRecoveryError> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| WindowsTransferRecoveryError::CorruptStore)?;
        if plaintext.is_empty() || plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(WindowsTransferRecoveryError::CorruptStore);
        }
        let parent = self
            .path
            .parent()
            .ok_or(WindowsTransferRecoveryError::MissingStoragePath)?;
        fs::create_dir_all(parent)?;
        let protected = protect(plaintext)?;
        if protected.is_empty() || protected.len() > MAX_PROTECTED_BYTES {
            return Err(WindowsTransferRecoveryError::CorruptStore);
        }

        let temporary = self.path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&protected)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, &self.path)
    }

    pub fn clear(&self) -> Result<bool, WindowsTransferRecoveryError> {
        let _operation = self
            .operation_lock
            .lock()
            .map_err(|_| WindowsTransferRecoveryError::CorruptStore)?;
        self.clear_locked()
    }

    fn clear_locked(&self) -> Result<bool, WindowsTransferRecoveryError> {
        let mut removed = remove_if_exists(&self.path)?;
        removed |= remove_if_exists(&self.path.with_extension("tmp"))?;
        Ok(removed)
    }
}

#[derive(Clone, Debug)]
pub struct WindowsFileQueueRecoveryStore {
    inner: WindowsTransferRecoveryStore,
}

impl WindowsFileQueueRecoveryStore {
    pub fn for_current_user() -> Result<Self, WindowsTransferRecoveryError> {
        let local_app_data =
            local_app_data_path().ok_or(WindowsTransferRecoveryError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data
                .join("DeskLink")
                .join("file-queue-recovery.bin"),
        ))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            inner: WindowsTransferRecoveryStore::new(path),
        }
    }

    pub fn load(&self) -> Result<Option<TransferQueueRecovery>, WindowsTransferRecoveryError> {
        self.load_at(now_unix_s())
    }

    fn load_at(
        &self,
        now_unix_s: u64,
    ) -> Result<Option<TransferQueueRecovery>, WindowsTransferRecoveryError> {
        let Some(mut plaintext) = self.inner.load_plaintext()? else {
            return Ok(None);
        };
        let queue = serde_json::from_slice::<TransferQueueRecovery>(&plaintext)
            .map_err(|_| WindowsTransferRecoveryError::CorruptStore);
        plaintext.zeroize();
        let queue = queue?;
        queue.validate(now_unix_s)?;
        if queue.is_expired(now_unix_s) {
            let _ = self.inner.clear();
            return Ok(None);
        }
        Ok(Some(queue))
    }

    pub fn save(&self, queue: &TransferQueueRecovery) -> Result<(), WindowsTransferRecoveryError> {
        queue.validate(now_unix_s())?;
        let mut plaintext =
            serde_json::to_vec(queue).map_err(|_| WindowsTransferRecoveryError::CorruptStore)?;
        let result = self.inner.save_plaintext(&plaintext);
        plaintext.zeroize();
        result
    }

    pub fn clear(&self) -> Result<bool, WindowsTransferRecoveryError> {
        self.inner.clear()
    }
}

fn remove_if_exists(path: &Path) -> Result<bool, WindowsTransferRecoveryError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

impl Drop for WindowsTransferRecoveryStore {
    fn drop(&mut self) {
        if Arc::strong_count(&self.operation_lock) == 1 {
            let _ = fs::remove_file(self.path.with_extension("tmp"));
        }
    }
}

fn valid_transfer_id(transfer_id: &TransferId) -> bool {
    transfer_id.iter().any(|byte| *byte != 0)
}

fn valid_device_id(device_id: &[u8; 16]) -> bool {
    device_id.iter().any(|byte| *byte != 0)
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn protect(plaintext: &[u8]) -> Result<Vec<u8>, WindowsTransferRecoveryError> {
    let input = blob_for(plaintext)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )?;
    }
    copy_and_free(output)
}

fn unprotect(protected: &[u8]) -> Result<Vec<u8>, WindowsTransferRecoveryError> {
    let input = blob_for(protected)?;
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|_| WindowsTransferRecoveryError::CorruptProtectedData)?;
    }
    copy_and_free(output)
}

fn blob_for(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, WindowsTransferRecoveryError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| WindowsTransferRecoveryError::CorruptStore)?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

fn copy_and_free(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, WindowsTransferRecoveryError> {
    if blob.pbData.is_null() && blob.cbData != 0 {
        return Err(WindowsTransferRecoveryError::CorruptProtectedData);
    }
    let bytes = if blob.cbData == 0 {
        Vec::new()
    } else {
        unsafe { slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec()
    };
    if !blob.pbData.is_null() {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(blob.pbData.cast())));
        }
    }
    Ok(bytes)
}

fn replace_file(source: &Path, destination: &Path) -> Result<(), WindowsTransferRecoveryError> {
    let source = wide_path(source);
    let destination = wide_path(destination);
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}

fn wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_FILE_QUEUE_RECOVERY_ITEMS, MAX_RECOVERY_AGE, TransferQueueRecovery,
        TransferRecoveryAction, WindowsFileQueueRecoveryStore, WindowsTransferRecoveryError,
        WindowsTransferRecoveryStore, now_unix_s,
    };
    use crate::transfer::OutgoingFile;
    use desklink_protocol::FileResumeHint;
    use std::{fs, path::PathBuf, thread, time::SystemTime};

    fn root(test: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "desklink-transfer-recovery-{test}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[test]
    fn dpapi_store_round_trips_both_actions_without_plaintext_paths() {
        let directory = root("roundtrip");
        fs::create_dir_all(&directory).unwrap();
        let store = WindowsTransferRecoveryStore::new(directory.join("recovery.bin"));
        let file = OutgoingFile {
            transfer_id: [7; 16],
            path: directory.join("敏感报告.txt"),
            name: "敏感报告.txt".to_owned(),
            size: 123,
            modified_at_unix_ns: 456,
        };
        let upload = TransferRecoveryAction::upload([6; 16], &file);
        let encoded = serde_json::to_vec(&upload).unwrap();
        let decoded = serde_json::from_slice::<TransferRecoveryAction>(&encoded).unwrap();
        decoded.validate(now_unix_s()).unwrap();
        assert_eq!(decoded, upload);
        store.save(&upload).unwrap();
        assert_eq!(store.load().unwrap(), Some(upload));
        let protected = fs::read(directory.join("recovery.bin")).unwrap();
        assert!(!protected.windows(6).any(|window| window == b"report"));
        assert!(!protected.windows(4).any(|window| window == b"kind"));

        let download = TransferRecoveryAction::download(
            [9; 16],
            Some(FileResumeHint {
                transfer_id: [8; 16],
                name: "下载文件.bin".to_owned(),
                size: 456,
            }),
        );
        store.save(&download).unwrap();
        assert_eq!(store.load().unwrap(), Some(download));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn expired_recovery_is_removed_and_corrupt_data_fails_closed() {
        let directory = root("expiry");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("recovery.bin");
        let store = WindowsTransferRecoveryStore::new(&path);
        let now = now_unix_s();
        let expired = TransferRecoveryAction::Download {
            host_device_id: [7; 16],
            resume: None,
            saved_at_unix_s: now.saturating_sub(MAX_RECOVERY_AGE.as_secs() + 1),
        };
        store
            .save(&TransferRecoveryAction::Download {
                host_device_id: [7; 16],
                resume: None,
                saved_at_unix_s: now,
            })
            .unwrap();
        let mut plaintext = serde_json::to_vec(&expired).unwrap();
        let protected = super::protect(&plaintext).unwrap();
        plaintext.fill(0);
        fs::write(&path, protected).unwrap();
        assert_eq!(store.load_at(now).unwrap(), None);
        assert!(!path.exists());

        fs::write(&path, b"not-dpapi").unwrap();
        assert!(matches!(
            store.load(),
            Err(WindowsTransferRecoveryError::CorruptProtectedData)
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn recovery_rejects_an_unbound_target_device() {
        let directory = root("unbound-target");
        fs::create_dir_all(&directory).unwrap();
        let store = WindowsTransferRecoveryStore::new(directory.join("recovery.bin"));
        assert!(matches!(
            store.save(&TransferRecoveryAction::download([0; 16], None)),
            Err(WindowsTransferRecoveryError::CorruptStore)
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn cloned_store_serializes_concurrent_atomic_replacements() {
        let directory = root("concurrent");
        fs::create_dir_all(&directory).unwrap();
        let store = WindowsTransferRecoveryStore::new(directory.join("recovery.bin"));
        let first = store.clone();
        let second = store.clone();
        let first_worker = thread::spawn(move || {
            for index in 1..=20 {
                first
                    .save(&TransferRecoveryAction::download(
                        [3; 16],
                        Some(FileResumeHint {
                            transfer_id: [1; 16],
                            name: format!("甲-{index}.bin"),
                            size: index,
                        }),
                    ))
                    .unwrap();
            }
        });
        let second_worker = thread::spawn(move || {
            for index in 1..=20 {
                second
                    .save(&TransferRecoveryAction::download(
                        [4; 16],
                        Some(FileResumeHint {
                            transfer_id: [2; 16],
                            name: format!("乙-{index}.bin"),
                            size: index,
                        }),
                    ))
                    .unwrap();
            }
        });
        first_worker.join().unwrap();
        second_worker.join().unwrap();

        assert!(store.load().unwrap().is_some());
        assert!(!directory.join("recovery.tmp").exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn dpapi_queue_store_round_trips_without_plaintext_paths() {
        let directory = root("queue-roundtrip");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("queue.bin");
        let store = WindowsFileQueueRecoveryStore::new(&path);
        let files = [
            OutgoingFile {
                transfer_id: [1; 16],
                path: directory.join("敏感甲.txt"),
                name: "敏感甲.txt".to_owned(),
                size: 123,
                modified_at_unix_ns: 1,
            },
            OutgoingFile {
                transfer_id: [2; 16],
                path: directory.join("敏感乙.bin"),
                name: "敏感乙.bin".to_owned(),
                size: 456,
                modified_at_unix_ns: 2,
            },
        ];
        let queue = TransferQueueRecovery::new([7; 16], &files, true);
        assert!(queue.matches_queue([7; 16], &files, true));
        assert!(!queue.matches_queue([8; 16], &files, true));
        assert!(!queue.matches_queue([7; 16], &files, false));
        assert!(!queue.matches_queue([7; 16], files.iter().rev(), true));
        store.save(&queue).unwrap();
        assert_eq!(store.load().unwrap(), Some(queue));
        let protected = fs::read(path).unwrap();
        assert!(!protected.windows(4).any(|window| window == b"path"));
        assert!(!protected.windows(5).any(|window| window == b"files"));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn queue_recovery_rejects_unsafe_or_ambiguous_records() {
        let directory = root("queue-validation");
        fs::create_dir_all(&directory).unwrap();
        let store = WindowsFileQueueRecoveryStore::new(directory.join("queue.bin"));
        let file = OutgoingFile {
            transfer_id: [3; 16],
            path: directory.join("报告.txt"),
            name: "报告.txt".to_owned(),
            size: 1,
            modified_at_unix_ns: 3,
        };
        assert!(matches!(
            store.save(&TransferQueueRecovery::new([0; 16], [&file], false)),
            Err(WindowsTransferRecoveryError::CorruptStore)
        ));
        let duplicate_ids = [file.clone(), file.clone()];
        assert!(matches!(
            store.save(&TransferQueueRecovery::new([4; 16], &duplicate_ids, false)),
            Err(WindowsTransferRecoveryError::CorruptStore)
        ));
        let too_many = (0..=MAX_FILE_QUEUE_RECOVERY_ITEMS)
            .map(|index| OutgoingFile {
                transfer_id: {
                    let mut id = [0_u8; 16];
                    id[0] = u8::try_from(index + 1).unwrap();
                    id
                },
                path: directory.join(format!("{index}.bin")),
                name: format!("{index}.bin"),
                size: 1,
                modified_at_unix_ns: 4,
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            store.save(&TransferQueueRecovery::new([5; 16], &too_many, false)),
            Err(WindowsTransferRecoveryError::CorruptStore)
        ));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn expired_queue_recovery_is_removed() {
        let directory = root("queue-expiry");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("queue.bin");
        let store = WindowsFileQueueRecoveryStore::new(&path);
        let now = now_unix_s();
        let expired = TransferQueueRecovery {
            host_device_id: [8; 16],
            files: vec![super::QueuedTransferRecoveryFile {
                transfer_id: [9; 16],
                path: directory.join("过期.bin"),
                name: "过期.bin".to_owned(),
                size: 1,
                modified_at_unix_ns: 5,
            }],
            paused: true,
            saved_at_unix_s: now.saturating_sub(MAX_RECOVERY_AGE.as_secs() + 1),
        };
        let mut plaintext = serde_json::to_vec(&expired).unwrap();
        let protected = super::protect(&plaintext).unwrap();
        plaintext.fill(0);
        fs::write(&path, protected).unwrap();
        assert_eq!(store.load_at(now).unwrap(), None);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(directory);
    }
}
