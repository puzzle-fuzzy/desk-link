use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    net::SocketAddr,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    slice,
};

use desklink_crypto::{PairingInvite, SessionId};
use ed25519_dalek::VerifyingKey;
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

const CONTROLLER_MAGIC: &[u8; 8] = b"DLCCV1\0\0";
const MAX_CONTROLLER_BYTES: usize = 4_096;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ControllerConnectionInputError {
    #[error("中继服务器地址必须包含 IP 地址和端口，例如 192.0.2.10:4433。")]
    InvalidRelayAddress,
    #[error("必须填写 TLS 服务器名称，且不能包含空格或控制字符。")]
    InvalidServerName,
}

pub struct ControllerConnectionSettings {
    relay_address: SocketAddr,
    server_name: String,
    session_id: SessionId,
    authentication: [u8; 32],
    host_device_id: [u8; 16],
    host_verify_key: VerifyingKey,
}

impl ControllerConnectionSettings {
    pub fn from_invite(
        relay_address: &str,
        server_name: &str,
        invite: &PairingInvite,
    ) -> Result<Self, ControllerConnectionInputError> {
        Self::from_parts(
            relay_address,
            server_name,
            invite.session_id(),
            *invite.relay_authentication(),
            invite.host_device_id(),
            invite.host_verify_key(),
        )
    }

    pub fn from_parts(
        relay_address: &str,
        server_name: &str,
        session_id: SessionId,
        authentication: [u8; 32],
        host_device_id: [u8; 16],
        host_verify_key: VerifyingKey,
    ) -> Result<Self, ControllerConnectionInputError> {
        let relay_address = relay_address
            .trim()
            .parse::<SocketAddr>()
            .map_err(|_| ControllerConnectionInputError::InvalidRelayAddress)?;
        let server_name = server_name.trim();
        if server_name.is_empty()
            || server_name.len() > 253
            || !server_name.is_ascii()
            || server_name.chars().any(char::is_whitespace)
            || server_name.chars().any(char::is_control)
        {
            return Err(ControllerConnectionInputError::InvalidServerName);
        }
        Ok(Self {
            relay_address,
            server_name: server_name.to_owned(),
            session_id,
            authentication,
            host_device_id,
            host_verify_key,
        })
    }

    pub const fn relay_address(&self) -> SocketAddr {
        self.relay_address
    }

    pub fn relay_address_text(&self) -> String {
        self.relay_address.to_string()
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub const fn authentication(&self) -> &[u8; 32] {
        &self.authentication
    }

    pub const fn host_device_id(&self) -> [u8; 16] {
        self.host_device_id
    }

    pub const fn host_verify_key(&self) -> VerifyingKey {
        self.host_verify_key
    }
}

impl fmt::Debug for ControllerConnectionSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControllerConnectionSettings")
            .field("relay_address", &self.relay_address)
            .field("server_name", &self.server_name)
            .field("session_id", &self.session_id)
            .field("authentication", &"[redacted]")
            .field("host_device_id", &self.host_device_id)
            .field("host_verify_key", &self.host_verify_key)
            .finish()
    }
}

impl Drop for ControllerConnectionSettings {
    fn drop(&mut self) {
        self.authentication.zeroize();
    }
}

#[derive(Debug, Error)]
pub enum WindowsControllerConnectionError {
    #[error("控制端连接存储路径不可用")]
    MissingStoragePath,
    #[error("控制端连接文件操作失败：{0}")]
    Io(#[from] io::Error),
    #[error("Windows 控制端连接保护失败：{0}")]
    Platform(#[from] windows::core::Error),
    #[error("受保护的控制端连接已损坏，或属于其他 Windows 用户")]
    CorruptProtectedData,
    #[error("控制端连接数据格式无效")]
    CorruptStore,
}

#[derive(Clone, Debug)]
pub struct WindowsControllerConnectionStore {
    path: PathBuf,
}

impl WindowsControllerConnectionStore {
    pub fn for_current_user() -> Result<Self, WindowsControllerConnectionError> {
        let local_app_data =
            local_app_data_path().ok_or(WindowsControllerConnectionError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data
                .join("DeskLink")
                .join("controller-connection.bin"),
        ))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(
        &self,
    ) -> Result<Option<ControllerConnectionSettings>, WindowsControllerConnectionError> {
        let protected = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if protected.len() > MAX_CONTROLLER_BYTES {
            return Err(WindowsControllerConnectionError::CorruptStore);
        }
        let mut plaintext = unprotect(&protected)?;
        let settings = decode(&plaintext);
        plaintext.zeroize();
        settings.map(Some)
    }

    pub fn save(
        &self,
        settings: &ControllerConnectionSettings,
    ) -> Result<(), WindowsControllerConnectionError> {
        let parent = self
            .path
            .parent()
            .ok_or(WindowsControllerConnectionError::MissingStoragePath)?;
        fs::create_dir_all(parent)?;
        let mut plaintext = encode(settings)?;
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

    pub fn clear(&self) -> Result<bool, WindowsControllerConnectionError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

fn encode(
    settings: &ControllerConnectionSettings,
) -> Result<Vec<u8>, WindowsControllerConnectionError> {
    let relay = settings.relay_address.to_string();
    let relay = relay.as_bytes();
    let server = settings.server_name.as_bytes();
    let relay_length =
        u16::try_from(relay.len()).map_err(|_| WindowsControllerConnectionError::CorruptStore)?;
    let server_length =
        u16::try_from(server.len()).map_err(|_| WindowsControllerConnectionError::CorruptStore)?;
    let mut output = Vec::with_capacity(
        CONTROLLER_MAGIC.len() + 4 + relay.len() + server.len() + 16 + 32 + 16 + 32,
    );
    output.extend_from_slice(CONTROLLER_MAGIC);
    output.extend_from_slice(&relay_length.to_be_bytes());
    output.extend_from_slice(relay);
    output.extend_from_slice(&server_length.to_be_bytes());
    output.extend_from_slice(server);
    output.extend_from_slice(settings.session_id.as_bytes());
    output.extend_from_slice(&settings.authentication);
    output.extend_from_slice(&settings.host_device_id);
    output.extend_from_slice(settings.host_verify_key.as_bytes());
    Ok(output)
}

fn decode(bytes: &[u8]) -> Result<ControllerConnectionSettings, WindowsControllerConnectionError> {
    if bytes.len() < CONTROLLER_MAGIC.len() + 4 + 16 + 32 + 16 + 32
        || &bytes[..CONTROLLER_MAGIC.len()] != CONTROLLER_MAGIC
    {
        return Err(WindowsControllerConnectionError::CorruptStore);
    }
    let mut offset = CONTROLLER_MAGIC.len();
    let relay = take_text(bytes, &mut offset)?;
    let server = take_text(bytes, &mut offset)?;
    let session_end = offset.saturating_add(16);
    let authentication_end = session_end.saturating_add(32);
    let device_end = authentication_end.saturating_add(16);
    let key_end = device_end.saturating_add(32);
    if key_end != bytes.len() {
        return Err(WindowsControllerConnectionError::CorruptStore);
    }
    let session_id = SessionId::from_bytes(
        bytes[offset..session_end]
            .try_into()
            .map_err(|_| WindowsControllerConnectionError::CorruptStore)?,
    );
    let authentication = bytes[session_end..authentication_end]
        .try_into()
        .map_err(|_| WindowsControllerConnectionError::CorruptStore)?;
    let host_device_id = bytes[authentication_end..device_end]
        .try_into()
        .map_err(|_| WindowsControllerConnectionError::CorruptStore)?;
    let host_verify_key = VerifyingKey::from_bytes(
        bytes[device_end..key_end]
            .try_into()
            .map_err(|_| WindowsControllerConnectionError::CorruptStore)?,
    )
    .map_err(|_| WindowsControllerConnectionError::CorruptStore)?;
    ControllerConnectionSettings::from_parts(
        &relay,
        &server,
        session_id,
        authentication,
        host_device_id,
        host_verify_key,
    )
    .map_err(|_| WindowsControllerConnectionError::CorruptStore)
}

fn take_text(bytes: &[u8], offset: &mut usize) -> Result<String, WindowsControllerConnectionError> {
    let length_end = offset.saturating_add(2);
    let length = u16::from_be_bytes(
        bytes
            .get(*offset..length_end)
            .ok_or(WindowsControllerConnectionError::CorruptStore)?
            .try_into()
            .map_err(|_| WindowsControllerConnectionError::CorruptStore)?,
    ) as usize;
    let end = length_end.saturating_add(length);
    let value = std::str::from_utf8(
        bytes
            .get(length_end..end)
            .ok_or(WindowsControllerConnectionError::CorruptStore)?,
    )
    .map_err(|_| WindowsControllerConnectionError::CorruptStore)?;
    *offset = end;
    Ok(value.to_owned())
}

fn protect(plaintext: &[u8]) -> Result<Vec<u8>, WindowsControllerConnectionError> {
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

fn unprotect(protected: &[u8]) -> Result<Vec<u8>, WindowsControllerConnectionError> {
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
        .map_err(|_| WindowsControllerConnectionError::CorruptProtectedData)?;
    }
    copy_and_free(output)
}

fn blob_for(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, WindowsControllerConnectionError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| WindowsControllerConnectionError::CorruptStore)?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

fn copy_and_free(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, WindowsControllerConnectionError> {
    if blob.pbData.is_null() && blob.cbData != 0 {
        return Err(WindowsControllerConnectionError::CorruptProtectedData);
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

fn replace_file(source: &Path, destination: &Path) -> Result<(), WindowsControllerConnectionError> {
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
    use desklink_crypto::DeviceIdentity;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn approved_invitation_preserves_the_hosts_normal_reconnect_credentials() {
        let host = DeviceIdentity::from_secret_key([0x21; 16], &[0x22; 32]);
        let session_id = SessionId::from_bytes([0x23; 16]);
        let authentication = [0x24; 32];
        let invite =
            PairingInvite::for_connection(&host, session_id, authentication, 1_000, 300).unwrap();
        let settings =
            ControllerConnectionSettings::from_invite("127.0.0.1:4433", "localhost", &invite)
                .unwrap();

        assert_eq!(settings.session_id(), session_id);
        assert_eq!(settings.authentication(), &authentication);
        assert_eq!(settings.host_device_id(), host.device_id);
        assert_eq!(settings.host_verify_key(), host.verify_key());
    }

    #[test]
    fn dpapi_store_round_trips_controller_connection_without_plaintext_key() {
        let directory = std::env::temp_dir().join(format!(
            "desklink-controller-settings-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let path = directory.join("controller.bin");
        let store = WindowsControllerConnectionStore::new(&path);
        let authentication = [0xa5; 32];
        let settings = ControllerConnectionSettings::from_parts(
            "127.0.0.1:4433",
            "localhost",
            SessionId::from_bytes([0x33; 16]),
            authentication,
            [0x44; 16],
            VerifyingKey::from_bytes(&[
                0x58, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
                0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x66,
                0x66, 0x66, 0x66, 0x66,
            ])
            .unwrap(),
        )
        .unwrap();
        store.save(&settings).unwrap();
        let protected = fs::read(&path).unwrap();
        assert!(
            !protected
                .windows(authentication.len())
                .any(|window| window == authentication)
        );

        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.relay_address(), settings.relay_address());
        assert_eq!(loaded.server_name(), settings.server_name());
        assert_eq!(loaded.session_id(), settings.session_id());
        assert_eq!(loaded.authentication(), settings.authentication());
        assert_eq!(loaded.host_device_id(), settings.host_device_id());
        assert_eq!(loaded.host_verify_key(), settings.host_verify_key());
        assert!(store.clear().unwrap());
        assert!(store.load().unwrap().is_none());
        let _ = fs::remove_dir_all(directory);
    }
}
