use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    slice,
};

use desklink_crypto::PairingCode;
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

pub const MAX_RECENT_ACCESS_ENTRIES: usize = 5;
pub const MAX_DEVICE_ALIAS_CHARACTERS: usize = 32;

const RECENT_ACCESS_MAGIC_V1: &[u8; 8] = b"DLRAV1\0\0";
const RECENT_ACCESS_MAGIC_V2: &[u8; 8] = b"DLRAV2\0\0";
const ENTRY_V1_BYTES: usize = 8 + 8 + 1 + 8;
const ENTRY_V2_FIXED_BYTES: usize = ENTRY_V1_BYTES + 1;
const MAX_DEVICE_ALIAS_BYTES: usize = 96;
const MAX_PLAINTEXT_BYTES: usize = RECENT_ACCESS_MAGIC_V2.len()
    + 1
    + (ENTRY_V2_FIXED_BYTES + MAX_DEVICE_ALIAS_BYTES) * MAX_RECENT_ACCESS_ENTRIES;
const MAX_PROTECTED_BYTES: usize = 4_096;

#[derive(Debug, Error)]
pub enum WindowsRecentAccessError {
    #[error("已保存设备的存储路径不可用")]
    MissingStoragePath,
    #[error("已保存设备文件操作失败：{0}")]
    Io(#[from] io::Error),
    #[error("Windows 已保存设备保护失败：{0}")]
    Platform(#[from] windows::core::Error),
    #[error("受保护的已保存设备数据已损坏，或属于其他 Windows 用户")]
    CorruptProtectedData,
    #[error("已保存设备数据格式无效")]
    CorruptStore,
    #[error("设备名称最多 32 个字符，且不能包含控制字符")]
    InvalidAlias,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecentAccessEntry {
    device_id: u64,
    password: PairingCode,
    persistent: bool,
    last_used_unix_s: u64,
    alias: Option<String>,
}

impl RecentAccessEntry {
    pub fn new(
        device_id: u64,
        password: PairingCode,
        persistent: bool,
        last_used_unix_s: u64,
    ) -> Result<Self, WindowsRecentAccessError> {
        if device_id == 0 {
            return Err(WindowsRecentAccessError::CorruptStore);
        }
        Ok(Self {
            device_id,
            password,
            persistent,
            last_used_unix_s,
            alias: None,
        })
    }

    pub const fn device_id(&self) -> u64 {
        self.device_id
    }

    pub fn password(&self) -> &PairingCode {
        &self.password
    }

    pub const fn is_persistent(&self) -> bool {
        self.persistent
    }

    pub const fn last_used_unix_s(&self) -> u64 {
        self.last_used_unix_s
    }

    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
}

#[derive(Clone, Debug)]
pub struct WindowsRecentAccessStore {
    path: PathBuf,
}

impl WindowsRecentAccessStore {
    pub fn for_current_user() -> Result<Self, WindowsRecentAccessError> {
        let local_app_data =
            local_app_data_path().ok_or(WindowsRecentAccessError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data.join("DeskLink").join("recent-access.bin"),
        ))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<Vec<RecentAccessEntry>, WindowsRecentAccessError> {
        let protected = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        if protected.is_empty() || protected.len() > MAX_PROTECTED_BYTES {
            return Err(WindowsRecentAccessError::CorruptStore);
        }
        let mut plaintext = unprotect(&protected)?;
        let entries = decode(&plaintext);
        plaintext.zeroize();
        entries
    }

    pub fn find(
        &self,
        device_id: u64,
    ) -> Result<Option<RecentAccessEntry>, WindowsRecentAccessError> {
        Ok(self
            .load()?
            .into_iter()
            .find(|entry| entry.device_id == device_id))
    }

    pub fn remember(
        &self,
        device_id: u64,
        password: PairingCode,
        persistent: bool,
        last_used_unix_s: u64,
    ) -> Result<(), WindowsRecentAccessError> {
        let mut entries = self.load()?;
        let alias = entries
            .iter()
            .find(|existing| existing.device_id == device_id)
            .and_then(|existing| existing.alias.clone());
        let mut entry = RecentAccessEntry::new(device_id, password, persistent, last_used_unix_s)?;
        entry.alias = alias;
        entries.retain(|existing| existing.device_id != device_id);
        entries.insert(0, entry);
        entries.truncate(MAX_RECENT_ACCESS_ENTRIES);
        self.save_all(&entries)
    }

    pub fn rename(&self, device_id: u64, alias: &str) -> Result<bool, WindowsRecentAccessError> {
        let alias = normalize_alias(alias)?;
        let mut entries = self.load()?;
        let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.device_id == device_id)
        else {
            return Ok(false);
        };
        if entry.alias == alias {
            return Ok(false);
        }
        entry.alias = alias;
        self.save_all(&entries)?;
        Ok(true)
    }

    pub fn remove(&self, device_id: u64) -> Result<bool, WindowsRecentAccessError> {
        let mut entries = self.load()?;
        let original_len = entries.len();
        entries.retain(|entry| entry.device_id != device_id);
        if entries.len() == original_len {
            return Ok(false);
        }
        if entries.is_empty() {
            self.clear()?;
        } else {
            self.save_all(&entries)?;
        }
        Ok(true)
    }

    pub fn clear(&self) -> Result<bool, WindowsRecentAccessError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    fn save_all(&self, entries: &[RecentAccessEntry]) -> Result<(), WindowsRecentAccessError> {
        let parent = self
            .path
            .parent()
            .ok_or(WindowsRecentAccessError::MissingStoragePath)?;
        fs::create_dir_all(parent)?;
        let mut plaintext = encode(entries)?;
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
}

fn encode(entries: &[RecentAccessEntry]) -> Result<Vec<u8>, WindowsRecentAccessError> {
    if entries.len() > MAX_RECENT_ACCESS_ENTRIES {
        return Err(WindowsRecentAccessError::CorruptStore);
    }
    let mut device_ids = HashSet::with_capacity(entries.len());
    let mut bytes = Vec::with_capacity(
        RECENT_ACCESS_MAGIC_V2.len()
            + 1
            + entries.len() * (ENTRY_V2_FIXED_BYTES + MAX_DEVICE_ALIAS_BYTES),
    );
    bytes.extend_from_slice(RECENT_ACCESS_MAGIC_V2);
    bytes.push(u8::try_from(entries.len()).map_err(|_| WindowsRecentAccessError::CorruptStore)?);
    for entry in entries {
        if entry.device_id == 0 || !device_ids.insert(entry.device_id) {
            bytes.zeroize();
            return Err(WindowsRecentAccessError::CorruptStore);
        }
        bytes.extend_from_slice(&entry.device_id.to_be_bytes());
        bytes.extend_from_slice(entry.password.as_bytes());
        bytes.push(u8::from(entry.persistent));
        bytes.extend_from_slice(&entry.last_used_unix_s.to_be_bytes());
        let alias = entry.alias.as_deref().unwrap_or_default();
        let normalized = normalize_alias(alias)?;
        if normalized.as_deref().unwrap_or_default() != alias {
            bytes.zeroize();
            return Err(WindowsRecentAccessError::InvalidAlias);
        }
        bytes.push(u8::try_from(alias.len()).map_err(|_| WindowsRecentAccessError::InvalidAlias)?);
        bytes.extend_from_slice(alias.as_bytes());
    }
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> Result<Vec<RecentAccessEntry>, WindowsRecentAccessError> {
    if bytes.len() < RECENT_ACCESS_MAGIC_V2.len() + 1 || bytes.len() > MAX_PLAINTEXT_BYTES {
        return Err(WindowsRecentAccessError::CorruptStore);
    }
    if &bytes[..RECENT_ACCESS_MAGIC_V1.len()] == RECENT_ACCESS_MAGIC_V1 {
        decode_v1(bytes)
    } else if &bytes[..RECENT_ACCESS_MAGIC_V2.len()] == RECENT_ACCESS_MAGIC_V2 {
        decode_v2(bytes)
    } else {
        Err(WindowsRecentAccessError::CorruptStore)
    }
}

fn decode_v1(bytes: &[u8]) -> Result<Vec<RecentAccessEntry>, WindowsRecentAccessError> {
    let count = usize::from(bytes[RECENT_ACCESS_MAGIC_V1.len()]);
    if count > MAX_RECENT_ACCESS_ENTRIES
        || bytes.len() != RECENT_ACCESS_MAGIC_V1.len() + 1 + count * ENTRY_V1_BYTES
    {
        return Err(WindowsRecentAccessError::CorruptStore);
    }
    let mut entries = Vec::with_capacity(count);
    let mut device_ids = HashSet::with_capacity(count);
    let mut offset = RECENT_ACCESS_MAGIC_V1.len() + 1;
    for _ in 0..count {
        let (device_id, password, persistent, last_used_unix_s) =
            decode_entry_fields(bytes, &mut offset)?;
        if device_id == 0 || !device_ids.insert(device_id) {
            return Err(WindowsRecentAccessError::CorruptStore);
        }
        entries.push(RecentAccessEntry {
            device_id,
            password,
            persistent,
            last_used_unix_s,
            alias: None,
        });
    }
    Ok(entries)
}

fn decode_v2(bytes: &[u8]) -> Result<Vec<RecentAccessEntry>, WindowsRecentAccessError> {
    let count = usize::from(bytes[RECENT_ACCESS_MAGIC_V2.len()]);
    if count > MAX_RECENT_ACCESS_ENTRIES {
        return Err(WindowsRecentAccessError::CorruptStore);
    }
    let mut entries = Vec::with_capacity(count);
    let mut device_ids = HashSet::with_capacity(count);
    let mut offset = RECENT_ACCESS_MAGIC_V2.len() + 1;
    for _ in 0..count {
        let (device_id, password, persistent, last_used_unix_s) =
            decode_entry_fields(bytes, &mut offset)?;
        if device_id == 0 || !device_ids.insert(device_id) {
            return Err(WindowsRecentAccessError::CorruptStore);
        }
        let alias_len = usize::from(read_array::<1>(bytes, &mut offset)?[0]);
        if alias_len > MAX_DEVICE_ALIAS_BYTES {
            return Err(WindowsRecentAccessError::CorruptStore);
        }
        let alias_end = offset
            .checked_add(alias_len)
            .ok_or(WindowsRecentAccessError::CorruptStore)?;
        let alias_bytes = bytes
            .get(offset..alias_end)
            .ok_or(WindowsRecentAccessError::CorruptStore)?;
        offset = alias_end;
        let alias =
            std::str::from_utf8(alias_bytes).map_err(|_| WindowsRecentAccessError::CorruptStore)?;
        let alias = normalize_alias(alias).map_err(|_| WindowsRecentAccessError::CorruptStore)?;
        entries.push(RecentAccessEntry {
            device_id,
            password,
            persistent,
            last_used_unix_s,
            alias,
        });
    }
    if offset != bytes.len() {
        return Err(WindowsRecentAccessError::CorruptStore);
    }
    Ok(entries)
}

fn decode_entry_fields(
    bytes: &[u8],
    offset: &mut usize,
) -> Result<(u64, PairingCode, bool, u64), WindowsRecentAccessError> {
    let device_id = u64::from_be_bytes(read_array(bytes, offset)?);
    let password = PairingCode::from_bytes(read_array(bytes, offset)?)
        .map_err(|_| WindowsRecentAccessError::CorruptStore)?;
    let persistent = match read_array::<1>(bytes, offset)?[0] {
        0 => false,
        1 => true,
        _ => return Err(WindowsRecentAccessError::CorruptStore),
    };
    let last_used_unix_s = u64::from_be_bytes(read_array(bytes, offset)?);
    Ok((device_id, password, persistent, last_used_unix_s))
}

fn read_array<const N: usize>(
    bytes: &[u8],
    offset: &mut usize,
) -> Result<[u8; N], WindowsRecentAccessError> {
    let end = offset
        .checked_add(N)
        .ok_or(WindowsRecentAccessError::CorruptStore)?;
    let value = bytes
        .get(*offset..end)
        .ok_or(WindowsRecentAccessError::CorruptStore)?
        .try_into()
        .map_err(|_| WindowsRecentAccessError::CorruptStore)?;
    *offset = end;
    Ok(value)
}

fn normalize_alias(alias: &str) -> Result<Option<String>, WindowsRecentAccessError> {
    let alias = alias.trim();
    if alias.is_empty() {
        return Ok(None);
    }
    if alias.len() > MAX_DEVICE_ALIAS_BYTES
        || alias.chars().count() > MAX_DEVICE_ALIAS_CHARACTERS
        || alias.chars().any(char::is_control)
    {
        return Err(WindowsRecentAccessError::InvalidAlias);
    }
    Ok(Some(alias.to_owned()))
}

fn protect(plaintext: &[u8]) -> Result<Vec<u8>, WindowsRecentAccessError> {
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

fn unprotect(protected: &[u8]) -> Result<Vec<u8>, WindowsRecentAccessError> {
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
        .map_err(|_| WindowsRecentAccessError::CorruptProtectedData)?;
    }
    copy_and_free(output)
}

fn blob_for(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, WindowsRecentAccessError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len()).map_err(|_| WindowsRecentAccessError::CorruptStore)?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

fn copy_and_free(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, WindowsRecentAccessError> {
    if blob.pbData.is_null() && blob.cbData != 0 {
        return Err(WindowsRecentAccessError::CorruptProtectedData);
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

fn replace_file(source: &Path, destination: &Path) -> Result<(), WindowsRecentAccessError> {
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
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_store() -> (PathBuf, WindowsRecentAccessStore) {
        let directory = std::env::temp_dir().join(format!(
            "desklink-recent-access-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let store = WindowsRecentAccessStore::new(directory.join("recent-access.bin"));
        (directory, store)
    }

    fn password(value: &[u8; 8]) -> PairingCode {
        PairingCode::from_bytes(*value).unwrap()
    }

    #[test]
    fn dpapi_store_round_trips_without_plaintext_passwords() {
        let (directory, store) = test_store();
        store
            .remember(101, password(b"ABCDEFGH"), true, 10)
            .unwrap();
        store
            .remember(202, password(b"23456789"), false, 20)
            .unwrap();
        assert!(store.rename(202, "办公室电脑").unwrap());

        let protected = fs::read(directory.join("recent-access.bin")).unwrap();
        assert!(!protected.windows(8).any(|window| window == b"ABCDEFGH"));
        assert!(!protected.windows(8).any(|window| window == b"23456789"));
        assert!(
            !protected
                .windows("办公室电脑".len())
                .any(|window| window == "办公室电脑".as_bytes())
        );

        let entries = store.load().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].device_id(), 202);
        assert_eq!(entries[0].password().as_bytes(), b"23456789");
        assert!(!entries[0].is_persistent());
        assert_eq!(entries[0].alias(), Some("办公室电脑"));
        assert_eq!(entries[1].device_id(), 101);
        assert!(entries[1].is_persistent());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn remember_deduplicates_and_keeps_five_most_recent_devices() {
        let (directory, store) = test_store();
        for device_id in 1..=7 {
            store
                .remember(device_id, password(b"ABCDEFGH"), true, device_id)
                .unwrap();
        }
        store
            .remember(5, password(b"23456789"), false, 100)
            .unwrap();

        let entries = store.load().unwrap();
        assert_eq!(
            entries
                .iter()
                .map(RecentAccessEntry::device_id)
                .collect::<Vec<_>>(),
            vec![5, 7, 6, 4, 3]
        );
        assert_eq!(entries[0].password().as_bytes(), b"23456789");
        assert!(!entries[0].is_persistent());

        assert!(store.rename(5, "家里电脑").unwrap());
        store.remember(5, password(b"ABCDEFGH"), true, 101).unwrap();
        let renamed = store.find(5).unwrap().unwrap();
        assert_eq!(renamed.alias(), Some("家里电脑"));
        assert!(renamed.is_persistent());
        assert!(matches!(
            store.rename(5, "包含\n换行"),
            Err(WindowsRecentAccessError::InvalidAlias)
        ));

        assert!(store.remove(6).unwrap());
        assert!(!store.remove(999).unwrap());
        assert!(store.find(6).unwrap().is_none());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn decoder_rejects_duplicate_devices_and_unknown_flags() {
        let first = RecentAccessEntry::new(1, password(b"ABCDEFGH"), true, 10).unwrap();
        let second = RecentAccessEntry::new(2, password(b"23456789"), false, 20).unwrap();
        let mut bytes = encode(&[first, second]).unwrap();

        let second_device_offset = RECENT_ACCESS_MAGIC_V2.len() + 1 + ENTRY_V2_FIXED_BYTES;
        bytes[second_device_offset..second_device_offset + 8].copy_from_slice(&1_u64.to_be_bytes());
        assert!(matches!(
            decode(&bytes),
            Err(WindowsRecentAccessError::CorruptStore)
        ));

        let first_flag_offset = RECENT_ACCESS_MAGIC_V2.len() + 1 + 8 + 8;
        bytes[second_device_offset..second_device_offset + 8].copy_from_slice(&2_u64.to_be_bytes());
        bytes[first_flag_offset] = 2;
        assert!(matches!(
            decode(&bytes),
            Err(WindowsRecentAccessError::CorruptStore)
        ));
        bytes.zeroize();
    }

    #[test]
    fn decoder_keeps_v1_records_compatible_without_aliases() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(RECENT_ACCESS_MAGIC_V1);
        bytes.push(1);
        bytes.extend_from_slice(&42_u64.to_be_bytes());
        bytes.extend_from_slice(b"ABCDEFGH");
        bytes.push(1);
        bytes.extend_from_slice(&99_u64.to_be_bytes());

        let entries = decode(&bytes).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].device_id(), 42);
        assert_eq!(entries[0].password().as_bytes(), b"ABCDEFGH");
        assert!(entries[0].is_persistent());
        assert_eq!(entries[0].last_used_unix_s(), 99);
        assert_eq!(entries[0].alias(), None);
        bytes.zeroize();
    }
}
