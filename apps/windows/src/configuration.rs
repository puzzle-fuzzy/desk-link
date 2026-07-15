use std::{
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    net::SocketAddr,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    slice,
};

use crate::storage::local_app_data_path;

use desklink_crypto::SessionId;
use thiserror::Error;
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, HINSTANCE, HLOCAL, HWND, LPARAM, LocalFree, WAIT_OBJECT_0, WPARAM,
        },
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
        Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
        System::{
            LibraryLoader::GetModuleHandleW,
            Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        },
        UI::WindowsAndMessaging::{
            DialogBoxParamW, EndDialog, FindWindowW, GWLP_USERDATA, GetDlgItem, GetWindowLongPtrW,
            GetWindowTextLengthW, GetWindowTextW, IDCANCEL, IDOK, RegisterWindowMessageW,
            SMTO_ABORTIFHUNG, SendMessageTimeoutW, SetWindowLongPtrW, SetWindowTextW, WM_CLOSE,
            WM_COMMAND, WM_INITDIALOG,
        },
    },
    core::{Error as WindowsError, PCWSTR},
};
use zeroize::Zeroize;

const CONNECTION_MAGIC: &[u8; 8] = b"DLCNV1\0\0";
const MAX_CONNECTION_BYTES: usize = 4_096;
const WINDOW_CLASS: &str = "DeskLinkHostStatusWindow";
const SHUTDOWN_MESSAGE: &str = "DeskLinkInstallerShutdown";

const DIALOG_CONNECTION_SETTINGS: usize = 201;
const CONTROL_RELAY_ADDRESS: i32 = 2101;
const CONTROL_SERVER_NAME: i32 = 2102;
const CONTROL_SESSION_ID: i32 = 2103;
const CONTROL_AUTHENTICATION: i32 = 2104;
const CONTROL_STREAM_ID: i32 = 2105;
const CONTROL_FEEDBACK: i32 = 2106;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ConnectionSettingsInputError {
    #[error("中继服务器地址必须包含 IP 地址和端口，例如 192.0.2.10:4433。")]
    InvalidRelayAddress,
    #[error("必须填写 TLS 服务器名称，且不能包含空格或控制字符。")]
    InvalidServerName,
    #[error("会话 ID 必须正好包含 32 位十六进制字符。")]
    InvalidSessionId,
    #[error("新连接必须填写中继密钥。")]
    MissingAuthentication,
    #[error("中继密钥必须正好包含 64 位十六进制字符。")]
    InvalidAuthentication,
    #[error("视频流 ID 必须是正整数。")]
    InvalidStreamId,
}

pub struct HostConnectionSettings {
    relay_address: SocketAddr,
    server_name: String,
    session_id: SessionId,
    authentication: [u8; 32],
    stream_id: u64,
}

impl HostConnectionSettings {
    pub fn from_text(
        relay_address: &str,
        server_name: &str,
        session_id: &str,
        authentication: &str,
        existing_authentication: Option<[u8; 32]>,
        stream_id: &str,
    ) -> Result<Self, ConnectionSettingsInputError> {
        let relay_address = relay_address
            .trim()
            .parse::<SocketAddr>()
            .map_err(|_| ConnectionSettingsInputError::InvalidRelayAddress)?;
        let server_name = server_name.trim();
        if server_name.is_empty()
            || server_name.len() > 253
            || !server_name.is_ascii()
            || server_name.chars().any(char::is_whitespace)
            || server_name.chars().any(char::is_control)
        {
            return Err(ConnectionSettingsInputError::InvalidServerName);
        }
        let session_id = SessionId::from_bytes(
            parse_hex::<16>(session_id.trim())
                .map_err(|_| ConnectionSettingsInputError::InvalidSessionId)?,
        );
        let authentication = if authentication.trim().is_empty() {
            existing_authentication.ok_or(ConnectionSettingsInputError::MissingAuthentication)?
        } else {
            parse_hex::<32>(authentication.trim())
                .map_err(|_| ConnectionSettingsInputError::InvalidAuthentication)?
        };
        let stream_id = stream_id
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|stream_id| *stream_id != 0)
            .ok_or(ConnectionSettingsInputError::InvalidStreamId)?;
        Ok(Self {
            relay_address,
            server_name: server_name.to_owned(),
            session_id,
            authentication,
            stream_id,
        })
    }

    pub const fn relay_address(&self) -> SocketAddr {
        self.relay_address
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

    pub const fn stream_id(&self) -> u64 {
        self.stream_id
    }

    pub fn relay_address_text(&self) -> String {
        self.relay_address.to_string()
    }

    pub fn session_id_text(&self) -> String {
        hex(self.session_id.as_bytes())
    }
}

impl fmt::Debug for HostConnectionSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostConnectionSettings")
            .field("relay_address", &self.relay_address)
            .field("server_name", &self.server_name)
            .field("session_id", &self.session_id)
            .field("authentication", &"[redacted]")
            .field("stream_id", &self.stream_id)
            .finish()
    }
}

impl Drop for HostConnectionSettings {
    fn drop(&mut self) {
        self.authentication.zeroize();
    }
}

#[derive(Debug, Error)]
pub enum WindowsConnectionSettingsError {
    #[error("连接设置存储路径不可用")]
    MissingStoragePath,
    #[error("连接设置文件操作失败：{0}")]
    Io(#[from] io::Error),
    #[error("Windows 连接保护失败：{0}")]
    Platform(#[from] WindowsError),
    #[error("受保护的连接设置已损坏，或属于其他 Windows 用户")]
    CorruptProtectedData,
    #[error("连接设置数据格式无效")]
    CorruptStore,
    #[error("连接配置改变后，正在运行的 DeskLink 主机未能停止")]
    HostDidNotStop,
}

#[derive(Clone, Debug)]
pub struct WindowsConnectionSettingsStore {
    path: PathBuf,
}

impl WindowsConnectionSettingsStore {
    pub fn for_current_user() -> Result<Self, WindowsConnectionSettingsError> {
        let local_app_data =
            local_app_data_path().ok_or(WindowsConnectionSettingsError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data.join("DeskLink").join("connection.bin"),
        ))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<HostConnectionSettings>, WindowsConnectionSettingsError> {
        let protected = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if protected.len() > MAX_CONNECTION_BYTES {
            return Err(WindowsConnectionSettingsError::CorruptStore);
        }
        let mut plaintext = unprotect(&protected)?;
        let settings = decode(&plaintext);
        plaintext.zeroize();
        settings.map(Some)
    }

    pub fn save(
        &self,
        settings: &HostConnectionSettings,
    ) -> Result<(), WindowsConnectionSettingsError> {
        let parent = self
            .path
            .parent()
            .ok_or(WindowsConnectionSettingsError::MissingStoragePath)?;
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
}

pub struct WindowsConnectionSettingsDialog;

impl WindowsConnectionSettingsDialog {
    pub fn show(
        store: &WindowsConnectionSettingsStore,
        existing: Option<&HostConnectionSettings>,
    ) -> Result<bool, WindowsConnectionSettingsError> {
        let module = unsafe { GetModuleHandleW(None)? };
        let mut state = DialogState {
            store,
            existing,
            saved: false,
        };
        let result = unsafe {
            DialogBoxParamW(
                Some(HINSTANCE(module.0)),
                PCWSTR(DIALOG_CONNECTION_SETTINGS as *const u16),
                None,
                Some(connection_dialog_proc),
                LPARAM((&mut state as *mut DialogState<'_>) as isize),
            )
        };
        if result == -1 {
            return Err(WindowsError::from_win32().into());
        }
        Ok(state.saved)
    }
}

struct DialogState<'a> {
    store: &'a WindowsConnectionSettingsStore,
    existing: Option<&'a HostConnectionSettings>,
    saved: bool,
}

impl DialogState<'_> {
    fn initialize(&self, dialog: HWND) -> Result<(), WindowsConnectionSettingsError> {
        let (relay, server, session, stream, feedback) = match self.existing {
            Some(settings) => (
                settings.relay_address_text(),
                settings.server_name().to_owned(),
                settings.session_id_text(),
                settings.stream_id().to_string(),
                "已保存的中继密钥受 Windows 保护，留空即可保留原密钥。",
            ),
            None => (
                "127.0.0.1:4433".to_owned(),
                "localhost".to_owned(),
                String::new(),
                "1".to_owned(),
                "请输入另一台 DeskLink 设备共享的连接信息。",
            ),
        };
        set_control_text(dialog, CONTROL_RELAY_ADDRESS, &relay)?;
        set_control_text(dialog, CONTROL_SERVER_NAME, &server)?;
        set_control_text(dialog, CONTROL_SESSION_ID, &session)?;
        set_control_text(dialog, CONTROL_STREAM_ID, &stream)?;
        set_control_text(dialog, CONTROL_FEEDBACK, feedback)
    }

    fn save_from_dialog(&mut self, dialog: HWND) -> Result<(), String> {
        let relay =
            control_text(dialog, CONTROL_RELAY_ADDRESS).map_err(|error| error.to_string())?;
        let server =
            control_text(dialog, CONTROL_SERVER_NAME).map_err(|error| error.to_string())?;
        let session =
            control_text(dialog, CONTROL_SESSION_ID).map_err(|error| error.to_string())?;
        let authentication =
            control_text(dialog, CONTROL_AUTHENTICATION).map_err(|error| error.to_string())?;
        let stream = control_text(dialog, CONTROL_STREAM_ID).map_err(|error| error.to_string())?;
        let settings = HostConnectionSettings::from_text(
            &relay,
            &server,
            &session,
            &authentication,
            self.existing.map(|settings| *settings.authentication()),
            &stream,
        )
        .map_err(|error| error.to_string())?;
        self.store
            .save(&settings)
            .map_err(|error| error.to_string())?;
        self.saved = true;
        Ok(())
    }
}

unsafe extern "system" fn connection_dialog_proc(
    dialog: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    if message == WM_INITDIALOG {
        let state = lparam.0 as *mut DialogState<'_>;
        unsafe { SetWindowLongPtrW(dialog, GWLP_USERDATA, state as isize) };
        if !state.is_null() {
            if let Err(error) = unsafe { (&*state).initialize(dialog) } {
                let _ = set_control_text(dialog, CONTROL_FEEDBACK, &error.to_string());
            }
        }
        return 1;
    }
    let state = unsafe { GetWindowLongPtrW(dialog, GWLP_USERDATA) as *mut DialogState<'_> };
    if message == WM_COMMAND {
        match wparam.0 & 0xffff {
            identifier if identifier == IDOK.0 as usize => {
                if !state.is_null() {
                    match unsafe { (&mut *state).save_from_dialog(dialog) } {
                        Ok(()) => {
                            let _ = unsafe { EndDialog(dialog, IDOK.0 as isize) };
                        }
                        Err(error) => {
                            let _ = set_control_text(dialog, CONTROL_FEEDBACK, &error);
                        }
                    }
                }
                return 1;
            }
            identifier if identifier == IDCANCEL.0 as usize => {
                let _ = unsafe { EndDialog(dialog, IDCANCEL.0 as isize) };
                return 1;
            }
            _ => {}
        }
    }
    if message == WM_CLOSE {
        let _ = unsafe { EndDialog(dialog, IDCANCEL.0 as isize) };
        return 1;
    }
    0
}

pub fn request_running_host_shutdown() -> Result<(), WindowsConnectionSettingsError> {
    let class = wide(WINDOW_CLASS);
    let Ok(window) = (unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }) else {
        return Ok(());
    };
    let mut process_id = 0;
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
            window,
            Some(&mut process_id),
        )
    };
    let process = if process_id == 0 {
        None
    } else {
        unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, process_id) }.ok()
    };
    let message = wide(SHUTDOWN_MESSAGE);
    let message = unsafe { RegisterWindowMessageW(PCWSTR(message.as_ptr())) };
    if message != 0 {
        unsafe {
            let _ = SendMessageTimeoutW(
                window,
                message,
                WPARAM(0),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                3_000,
                None,
            );
        }
    }
    if let Some(process) = process {
        let result = unsafe { WaitForSingleObject(process, 5_000) };
        unsafe {
            let _ = CloseHandle(process);
        }
        if result != WAIT_OBJECT_0 {
            return Err(WindowsConnectionSettingsError::HostDidNotStop);
        }
    }
    Ok(())
}

fn encode(settings: &HostConnectionSettings) -> Result<Vec<u8>, WindowsConnectionSettingsError> {
    let relay = settings.relay_address.to_string();
    let relay = relay.as_bytes();
    let server = settings.server_name.as_bytes();
    let relay_length =
        u16::try_from(relay.len()).map_err(|_| WindowsConnectionSettingsError::CorruptStore)?;
    let server_length =
        u16::try_from(server.len()).map_err(|_| WindowsConnectionSettingsError::CorruptStore)?;
    let mut output =
        Vec::with_capacity(CONNECTION_MAGIC.len() + 4 + relay.len() + server.len() + 16 + 32 + 8);
    output.extend_from_slice(CONNECTION_MAGIC);
    output.extend_from_slice(&relay_length.to_be_bytes());
    output.extend_from_slice(relay);
    output.extend_from_slice(&server_length.to_be_bytes());
    output.extend_from_slice(server);
    output.extend_from_slice(settings.session_id.as_bytes());
    output.extend_from_slice(&settings.authentication);
    output.extend_from_slice(&settings.stream_id.to_be_bytes());
    Ok(output)
}

fn decode(bytes: &[u8]) -> Result<HostConnectionSettings, WindowsConnectionSettingsError> {
    if bytes.len() < CONNECTION_MAGIC.len() + 4 + 16 + 32 + 8
        || &bytes[..CONNECTION_MAGIC.len()] != CONNECTION_MAGIC
    {
        return Err(WindowsConnectionSettingsError::CorruptStore);
    }
    let mut offset = CONNECTION_MAGIC.len();
    let relay = take_text(bytes, &mut offset)?;
    let server = take_text(bytes, &mut offset)?;
    let session_end = offset + 16;
    let authentication_end = session_end + 32;
    let stream_end = authentication_end + 8;
    if stream_end != bytes.len() {
        return Err(WindowsConnectionSettingsError::CorruptStore);
    }
    let session = hex(&bytes[offset..session_end]);
    let authentication = hex(&bytes[session_end..authentication_end]);
    let stream = u64::from_be_bytes(
        bytes[authentication_end..stream_end]
            .try_into()
            .map_err(|_| WindowsConnectionSettingsError::CorruptStore)?,
    )
    .to_string();
    HostConnectionSettings::from_text(&relay, &server, &session, &authentication, None, &stream)
        .map_err(|_| WindowsConnectionSettingsError::CorruptStore)
}

fn take_text(bytes: &[u8], offset: &mut usize) -> Result<String, WindowsConnectionSettingsError> {
    let length_end = offset.saturating_add(2);
    let length = u16::from_be_bytes(
        bytes
            .get(*offset..length_end)
            .ok_or(WindowsConnectionSettingsError::CorruptStore)?
            .try_into()
            .map_err(|_| WindowsConnectionSettingsError::CorruptStore)?,
    ) as usize;
    let end = length_end.saturating_add(length);
    let value = std::str::from_utf8(
        bytes
            .get(length_end..end)
            .ok_or(WindowsConnectionSettingsError::CorruptStore)?,
    )
    .map_err(|_| WindowsConnectionSettingsError::CorruptStore)?;
    *offset = end;
    Ok(value.to_owned())
}

fn protect(plaintext: &[u8]) -> Result<Vec<u8>, WindowsConnectionSettingsError> {
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

fn unprotect(protected: &[u8]) -> Result<Vec<u8>, WindowsConnectionSettingsError> {
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
        .map_err(|_| WindowsConnectionSettingsError::CorruptProtectedData)?;
    }
    copy_and_free(output)
}

fn blob_for(bytes: &[u8]) -> Result<CRYPT_INTEGER_BLOB, WindowsConnectionSettingsError> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| WindowsConnectionSettingsError::CorruptStore)?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

fn copy_and_free(blob: CRYPT_INTEGER_BLOB) -> Result<Vec<u8>, WindowsConnectionSettingsError> {
    if blob.pbData.is_null() && blob.cbData != 0 {
        return Err(WindowsConnectionSettingsError::CorruptProtectedData);
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

fn replace_file(source: &Path, destination: &Path) -> Result<(), WindowsConnectionSettingsError> {
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

fn set_control_text(
    dialog: HWND,
    identifier: i32,
    value: &str,
) -> Result<(), WindowsConnectionSettingsError> {
    let control = unsafe { GetDlgItem(Some(dialog), identifier)? };
    let value = wide(value);
    unsafe { SetWindowTextW(control, PCWSTR(value.as_ptr()))? };
    Ok(())
}

fn control_text(dialog: HWND, identifier: i32) -> Result<String, WindowsConnectionSettingsError> {
    let control = unsafe { GetDlgItem(Some(dialog), identifier)? };
    let length = unsafe { GetWindowTextLengthW(control) };
    if !(0..=1_024).contains(&length) {
        return Err(WindowsConnectionSettingsError::CorruptStore);
    }
    let mut value = vec![0_u16; length as usize + 1];
    let copied = unsafe { GetWindowTextW(control, &mut value) };
    String::from_utf16(&value[..copied as usize])
        .map_err(|_| WindowsConnectionSettingsError::CorruptStore)
}

fn parse_hex<const N: usize>(value: &str) -> Result<[u8; N], ()> {
    if value.len() != N * 2 {
        return Err(());
    }
    let mut output = [0_u8; N];
    for (index, byte) in output.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = u8::from_str_radix(&value[offset..offset + 2], 16).map_err(|_| ())?;
    }
    Ok(output)
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    use std::fmt::Write as _;
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn settings(authentication: &str) -> HostConnectionSettings {
        HostConnectionSettings::from_text(
            "127.0.0.1:4433",
            "localhost",
            "00112233445566778899aabbccddeeff",
            authentication,
            None,
            "7",
        )
        .unwrap()
    }

    #[test]
    fn validates_fields_and_reuses_only_an_explicit_existing_secret() {
        let current = settings("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let changed = HostConnectionSettings::from_text(
            "[::1]:4433",
            "relay.example.test",
            "ffeeddccbbaa99887766554433221100",
            "",
            Some(*current.authentication()),
            "9",
        )
        .unwrap();
        assert_eq!(changed.relay_address(), "[::1]:4433".parse().unwrap());
        assert_eq!(changed.authentication(), current.authentication());
        assert_eq!(changed.stream_id(), 9);
        assert!(matches!(
            HostConnectionSettings::from_text("localhost", "localhost", "00", "", None, "0"),
            Err(ConnectionSettingsInputError::InvalidRelayAddress)
        ));
    }

    #[test]
    fn dpapi_round_trip_hides_connection_credentials() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "desklink-connection-{}-{unique}",
            std::process::id()
        ));
        let store = WindowsConnectionSettingsStore::new(directory.join("connection.bin"));
        let original = settings("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        store.save(&original).unwrap();
        let protected = fs::read(store.path()).unwrap();
        assert!(
            !protected
                .windows(original.authentication().len())
                .any(|window| window == original.authentication())
        );
        assert!(
            !protected
                .windows(b"127.0.0.1:4433".len())
                .any(|window| window == b"127.0.0.1:4433")
        );

        let restored = store.load().unwrap().unwrap();
        assert_eq!(restored.relay_address(), original.relay_address());
        assert_eq!(restored.server_name(), original.server_name());
        assert_eq!(restored.session_id(), original.session_id());
        assert_eq!(restored.authentication(), original.authentication());
        assert_eq!(restored.stream_id(), original.stream_id());
        fs::remove_dir_all(directory).unwrap();
    }
}
