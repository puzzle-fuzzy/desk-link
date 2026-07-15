use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    slice,
};

use crate::storage::local_app_data_path;

use desklink_crypto::{DeviceIdentity, IdentityStore};
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

const IDENTITY_MAGIC: &[u8; 8] = b"DLIDV1\0\0";
const IDENTITY_PLAINTEXT_BYTES: usize = IDENTITY_MAGIC.len() + 16 + 32;

#[derive(Debug, Error)]
pub enum WindowsIdentityError {
    #[error("identity storage path is unavailable")]
    MissingStoragePath,
    #[error("identity file operation failed: {0}")]
    Io(#[from] io::Error),
    #[error("Windows identity protection failed: {0}")]
    Platform(#[from] windows::core::Error),
    #[error("protected identity data is malformed")]
    CorruptIdentity,
}

#[derive(Clone, Debug)]
pub struct WindowsIdentityStore {
    path: PathBuf,
}

impl WindowsIdentityStore {
    pub fn for_current_user() -> Result<Self, WindowsIdentityError> {
        let local_app_data =
            local_app_data_path().ok_or(WindowsIdentityError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data.join("DeskLink").join("identity.bin"),
        ))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_or_create(
        &self,
        rng: &mut impl CryptoRngCore,
    ) -> Result<DeviceIdentity, WindowsIdentityError> {
        if let Some(identity) = self.load()? {
            return Ok(identity);
        }
        let identity = DeviceIdentity::generate(rng);
        self.save(&identity)?;
        Ok(identity)
    }
}

impl IdentityStore for WindowsIdentityStore {
    type Error = WindowsIdentityError;

    fn load(&self) -> Result<Option<DeviceIdentity>, Self::Error> {
        let protected = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut plaintext = unprotect(&protected)?;
        if plaintext.len() != IDENTITY_PLAINTEXT_BYTES
            || &plaintext[..IDENTITY_MAGIC.len()] != IDENTITY_MAGIC
        {
            plaintext.fill(0);
            return Err(WindowsIdentityError::CorruptIdentity);
        }
        let device_start = IDENTITY_MAGIC.len();
        let secret_start = device_start + 16;
        let device_id: [u8; 16] = plaintext[device_start..secret_start]
            .try_into()
            .map_err(|_| WindowsIdentityError::CorruptIdentity)?;
        let secret: [u8; 32] = plaintext[secret_start..]
            .try_into()
            .map_err(|_| WindowsIdentityError::CorruptIdentity)?;
        let identity = DeviceIdentity::from_secret_key(device_id, &secret);
        plaintext.fill(0);
        Ok(Some(identity))
    }

    fn save(&self, identity: &DeviceIdentity) -> Result<(), Self::Error> {
        let parent = self
            .path
            .parent()
            .ok_or(WindowsIdentityError::MissingStoragePath)?;
        fs::create_dir_all(parent)?;
        let mut plaintext = Vec::with_capacity(IDENTITY_PLAINTEXT_BYTES);
        plaintext.extend_from_slice(IDENTITY_MAGIC);
        plaintext.extend_from_slice(&identity.device_id);
        identity.with_secret_key_bytes(|secret| plaintext.extend_from_slice(secret));
        let protected = protect(&plaintext)?;
        plaintext.fill(0);

        let temporary = self.path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&protected)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, &self.path)?;
        Ok(())
    }
}

fn protect(plaintext: &[u8]) -> Result<Vec<u8>, WindowsIdentityError> {
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

fn unprotect(protected: &[u8]) -> Result<Vec<u8>, WindowsIdentityError> {
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

fn blob_for(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, WindowsIdentityError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| WindowsIdentityError::CorruptIdentity)?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

fn copy_and_free(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, WindowsIdentityError> {
    if blob.pbData.is_null() && blob.cbData != 0 {
        return Err(WindowsIdentityError::CorruptIdentity);
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

fn replace_file(source: &Path, destination: &Path) -> Result<(), WindowsIdentityError> {
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use desklink_crypto::IdentityStore;
    use rand_core::OsRng;

    use super::WindowsIdentityStore;

    #[test]
    fn dpapi_store_persists_the_same_identity_without_plaintext_secret() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("desklink-identity-{}-{unique}", std::process::id()));
        let store = WindowsIdentityStore::new(directory.join("identity.bin"));
        let identity = store.load_or_create(&mut OsRng).unwrap();
        let device_id = identity.device_id;
        let verify_key = identity.verify_key();
        let protected = std::fs::read(store.path()).unwrap();
        identity.with_secret_key_bytes(|secret| {
            assert!(
                !protected
                    .windows(secret.len())
                    .any(|window| window == secret)
            );
        });
        drop(identity);

        let restored = store.load().unwrap().unwrap();
        assert_eq!(restored.device_id, device_id);
        assert_eq!(restored.verify_key(), verify_key);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
