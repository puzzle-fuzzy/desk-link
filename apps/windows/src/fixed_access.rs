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
use zeroize::{Zeroize, Zeroizing};

use crate::storage::local_app_data_path;

const FIXED_ACCESS_MAGIC_V1: &[u8; 8] = b"DLFAV1\0\0";
/// Version 2 stores one length byte followed by the verbatim password so
/// user-chosen fixed passwords of 8–64 bytes survive round trips. The
/// directory code is derived from these bytes at registration time.
const FIXED_ACCESS_MAGIC_V2: &[u8; 8] = b"DLFAV2\0\0";
const MAX_STORED_PASSWORD_BYTES: usize = desklink_crypto::MAX_ACCESS_PASSWORD_BYTES;
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

    /// Loads the verbatim password bytes. Generated passwords are the 8
    /// display characters; user-chosen passwords are exactly what was typed.
    pub fn load(&self) -> Result<Option<Zeroizing<Vec<u8>>>, WindowsFixedAccessError> {
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
    ) -> Result<Zeroizing<Vec<u8>>, WindowsFixedAccessError> {
        let password = PairingCode::generate(rng);
        let bytes = Zeroizing::new(password.as_bytes().to_vec());
        self.save_custom(&bytes)?;
        Ok(bytes)
    }

    /// Stores a user-chosen password verbatim after validating its length
    /// against the shared derive rule.
    pub fn save_custom(&self, password: &[u8]) -> Result<(), WindowsFixedAccessError> {
        if desklink_crypto::PairingCode::derive_from_password(password).is_err() {
            return Err(WindowsFixedAccessError::CorruptStore);
        }
        let parent = self
            .path
            .parent()
            .ok_or(WindowsFixedAccessError::MissingStoragePath)?;
        fs::create_dir_all(parent)?;
        let mut plaintext = Vec::with_capacity(FIXED_ACCESS_MAGIC_V2.len() + 1 + password.len());
        plaintext.extend_from_slice(FIXED_ACCESS_MAGIC_V2);
        plaintext
            .push(u8::try_from(password.len()).map_err(|_| WindowsFixedAccessError::CorruptStore)?);
        plaintext.extend_from_slice(password);
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

fn decode(bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>, WindowsFixedAccessError> {
    if bytes.len() < FIXED_ACCESS_MAGIC_V1.len() {
        return Err(WindowsFixedAccessError::CorruptStore);
    }
    // Legacy version 1 stored the generated 8-character code without a
    // length byte; it loads as a verbatim generated password.
    if &bytes[..FIXED_ACCESS_MAGIC_V1.len()] == FIXED_ACCESS_MAGIC_V1 {
        let legacy = bytes[FIXED_ACCESS_MAGIC_V1.len()..]
            .try_into()
            .map_err(|_| WindowsFixedAccessError::CorruptStore)?;
        let code =
            PairingCode::from_bytes(legacy).map_err(|_| WindowsFixedAccessError::CorruptStore)?;
        return Ok(Zeroizing::new(code.as_bytes().to_vec()));
    }
    if &bytes[..FIXED_ACCESS_MAGIC_V2.len()] != FIXED_ACCESS_MAGIC_V2 {
        return Err(WindowsFixedAccessError::CorruptStore);
    }
    let length = usize::from(bytes[FIXED_ACCESS_MAGIC_V2.len()]);
    let start = FIXED_ACCESS_MAGIC_V2.len() + 1;
    if bytes.len() != start + length
        || !(desklink_crypto::MIN_ACCESS_PASSWORD_BYTES..=MAX_STORED_PASSWORD_BYTES)
            .contains(&length)
    {
        return Err(WindowsFixedAccessError::CorruptStore);
    }
    if desklink_crypto::PairingCode::derive_from_password(&bytes[start..]).is_err() {
        return Err(WindowsFixedAccessError::CorruptStore);
    }
    Ok(Zeroizing::new(bytes[start..].to_vec()))
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
                .any(|window| window == password.as_slice())
        );
        assert_eq!(
            store.load().unwrap().unwrap().as_slice(),
            password.as_slice()
        );
        assert!(store.clear().unwrap());
        assert!(store.load().unwrap().is_none());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn custom_password_round_trips_and_legacy_v1_still_loads() {
        let directory = std::env::temp_dir().join(format!(
            "desklink-fixed-access-custom-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        ));
        let path = directory.join("fixed-access.bin");
        let store = WindowsFixedAccessStore::new(&path);
        let custom = "My0Passw0rd-电池!";
        store.save_custom(custom.as_bytes()).unwrap();
        assert_eq!(store.load().unwrap().unwrap().as_slice(), custom.as_bytes());
        // Under-length and over-length passwords are rejected.
        assert!(store.save_custom(b"short").is_err());
        assert!(store.save_custom(&[b'a'; 65]).is_err());
        assert!(store.clear().unwrap());

        // A legacy V1 store with a generated 8-character code still loads.
        let legacy_code = PairingCode::from_bytes(*b"AB2DEF3G").unwrap();
        let mut legacy = FIXED_ACCESS_MAGIC_V1.to_vec();
        legacy.extend_from_slice(legacy_code.as_bytes());
        let mut plaintext = legacy.clone();
        let protected = protect(&plaintext).unwrap();
        plaintext.zeroize();
        fs::create_dir_all(&directory).unwrap();
        fs::write(&path, protected).unwrap();
        assert_eq!(
            store.load().unwrap().unwrap().as_slice(),
            b"AB2DEF3G".to_vec()
        );
        let _ = fs::remove_dir_all(directory);
    }
}
