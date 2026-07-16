use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    slice,
};

use desklink_crypto::PairingCode;
use rand_core::CryptoRngCore;
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

const FIXED_ACCESS_MAGIC: &[u8; 8] = b"DLFAV1\0\0";
const FIXED_ACCESS_BYTES: usize = FIXED_ACCESS_MAGIC.len() + 8;
const MAX_PROTECTED_BYTES: usize = 1_024;

#[derive(Debug, Error)]
pub enum WindowsFixedAccessError {
    #[error("固定密码存储路径不可用")]
    MissingStoragePath,
    #[error("固定密码文件操作失败：{0}")]
    Io(#[from] io::Error),
    #[error("Windows 固定密码保护失败：{0}")]
    Platform(#[from] windows::core::Error),
    #[error("受保护的固定密码已损坏，或属于其他 Windows 用户")]
    CorruptProtectedData,
    #[error("固定密码数据格式无效")]
    CorruptStore,
}

#[derive(Clone, Debug)]
pub struct WindowsFixedAccessStore {
    path: PathBuf,
}

impl WindowsFixedAccessStore {
    pub fn for_current_user() -> Result<Self, WindowsFixedAccessError> {
        let local_app_data =
            local_app_data_path().ok_or(WindowsFixedAccessError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data.join("DeskLink").join("fixed-access.bin"),
        ))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<Option<PairingCode>, WindowsFixedAccessError> {
        let protected = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if protected.len() > MAX_PROTECTED_BYTES {
            return Err(WindowsFixedAccessError::CorruptStore);
        }
        let mut plaintext = unprotect(&protected)?;
        let password = decode(&plaintext);
        plaintext.zeroize();
        password.map(Some)
    }

    pub fn generate_and_save(
        &self,
        rng: &mut impl CryptoRngCore,
    ) -> Result<PairingCode, WindowsFixedAccessError> {
        let password = PairingCode::generate(rng);
        self.save(&password)?;
        Ok(password)
    }

    pub fn save(&self, password: &PairingCode) -> Result<(), WindowsFixedAccessError> {
        let parent = self
            .path
            .parent()
            .ok_or(WindowsFixedAccessError::MissingStoragePath)?;
        fs::create_dir_all(parent)?;
        let mut plaintext = Vec::with_capacity(FIXED_ACCESS_BYTES);
        plaintext.extend_from_slice(FIXED_ACCESS_MAGIC);
        plaintext.extend_from_slice(password.as_bytes());
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

    pub fn clear(&self) -> Result<bool, WindowsFixedAccessError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

fn decode(bytes: &[u8]) -> Result<PairingCode, WindowsFixedAccessError> {
    if bytes.len() != FIXED_ACCESS_BYTES || &bytes[..FIXED_ACCESS_MAGIC.len()] != FIXED_ACCESS_MAGIC
    {
        return Err(WindowsFixedAccessError::CorruptStore);
    }
    let password = bytes[FIXED_ACCESS_MAGIC.len()..]
        .try_into()
        .map_err(|_| WindowsFixedAccessError::CorruptStore)?;
    PairingCode::from_bytes(password).map_err(|_| WindowsFixedAccessError::CorruptStore)
}

fn protect(plaintext: &[u8]) -> Result<Vec<u8>, WindowsFixedAccessError> {
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

fn unprotect(protected: &[u8]) -> Result<Vec<u8>, WindowsFixedAccessError> {
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
        .map_err(|_| WindowsFixedAccessError::CorruptProtectedData)?;
    }
    copy_and_free(output)
}

fn blob_for(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, WindowsFixedAccessError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| WindowsFixedAccessError::CorruptStore)?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

fn copy_and_free(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, WindowsFixedAccessError> {
    if blob.pbData.is_null() && blob.cbData != 0 {
        return Err(WindowsFixedAccessError::CorruptProtectedData);
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

fn replace_file(source: &Path, destination: &Path) -> Result<(), WindowsFixedAccessError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn dpapi_store_round_trips_without_plaintext_password() {
        let directory = std::env::temp_dir().join(format!(
            "desklink-fixed-access-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let path = directory.join("fixed-access.bin");
        let store = WindowsFixedAccessStore::new(&path);
        let password = store.generate_and_save(&mut OsRng).unwrap();
        let protected = fs::read(&path).unwrap();
        assert!(
            !protected
                .windows(8)
                .any(|window| window == password.as_bytes())
        );
        assert_eq!(
            store.load().unwrap().unwrap().as_bytes(),
            password.as_bytes()
        );
        assert!(store.clear().unwrap());
        assert!(store.load().unwrap().is_none());
        let _ = fs::remove_dir_all(directory);
    }
}
