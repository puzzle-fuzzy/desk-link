use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    slice,
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

use crate::storage::local_app_data_path;

const ACCOUNT_MAGIC: &[u8; 8] = b"DLACV1\0\0";
const MAX_ACCOUNT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccountSession {
    pub user_id: String,
    pub email: String,
    pub device_id: String,
    pub access_token: String,
    pub refresh_token: String,
}

impl Drop for AccountSession {
    fn drop(&mut self) {
        self.access_token.zeroize();
        self.refresh_token.zeroize();
    }
}

#[derive(Debug, Error)]
pub enum AccountSessionError {
    #[error("账号会话存储路径不可用")]
    MissingStoragePath,
    #[error("账号会话文件操作失败：{0}")]
    Io(#[from] io::Error),
    #[error("Windows 账号会话保护失败：{0}")]
    Platform(#[from] windows::core::Error),
    #[error("受保护的账号会话已损坏，或属于其他 Windows 用户")]
    CorruptProtectedData,
    #[error("账号会话数据格式无效")]
    CorruptStore,
}

#[derive(Clone, Debug)]
pub struct WindowsAccountSessionStore {
    path: PathBuf,
}

impl WindowsAccountSessionStore {
    pub fn for_current_user() -> Result<Self, AccountSessionError> {
        let root = local_app_data_path().ok_or(AccountSessionError::MissingStoragePath)?;
        Ok(Self::new(root.join("DeskLink").join("account-session.bin")))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<Option<AccountSession>, AccountSessionError> {
        let protected = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if protected.len() > MAX_ACCOUNT_BYTES {
            return Err(AccountSessionError::CorruptStore);
        }
        let mut plaintext = unprotect(&protected)?;
        if plaintext.len() < ACCOUNT_MAGIC.len()
            || &plaintext[..ACCOUNT_MAGIC.len()] != ACCOUNT_MAGIC
        {
            plaintext.zeroize();
            return Err(AccountSessionError::CorruptProtectedData);
        }
        let session = serde_json::from_slice(&plaintext[ACCOUNT_MAGIC.len()..])
            .map_err(|_| AccountSessionError::CorruptStore);
        plaintext.zeroize();
        session.map(Some)
    }

    pub fn save(&self, session: &AccountSession) -> Result<(), AccountSessionError> {
        let parent = self
            .path
            .parent()
            .ok_or(AccountSessionError::MissingStoragePath)?;
        fs::create_dir_all(parent)?;
        let mut plaintext = ACCOUNT_MAGIC.to_vec();
        plaintext.extend_from_slice(
            &serde_json::to_vec(session).map_err(|_| AccountSessionError::CorruptStore)?,
        );
        let protected = protect(&plaintext)?;
        plaintext.zeroize();

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

    pub fn clear(&self) -> Result<bool, AccountSessionError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

fn protect(plaintext: &[u8]) -> Result<Vec<u8>, AccountSessionError> {
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

fn unprotect(protected: &[u8]) -> Result<Vec<u8>, AccountSessionError> {
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
        )?;
    }
    copy_and_free(output)
}

fn blob_for(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, AccountSessionError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| AccountSessionError::CorruptStore)?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

fn copy_and_free(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, AccountSessionError> {
    if blob.pbData.is_null() && blob.cbData != 0 {
        return Err(AccountSessionError::CorruptProtectedData);
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

fn replace_file(source: &Path, destination: &Path) -> Result<(), AccountSessionError> {
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )?;
    }
    Ok(())
}
