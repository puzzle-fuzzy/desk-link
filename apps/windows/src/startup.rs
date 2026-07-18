use std::{env, io, path::PathBuf};

use thiserror::Error;
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE,
            REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW,
            RegQueryValueExW, RegSetValueExW,
        },
    },
    core::PCWSTR,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const PRODUCT_NAME: &str = "DeskLink";
const MAX_REGISTRY_VALUE_BYTES: u32 = 32 * 1_024;

#[derive(Debug, Error)]
pub enum WindowsStartupError {
    #[error("无法读取 DeskLink 可执行文件路径：{0}")]
    CurrentExecutable(#[source] io::Error),
    #[error("Windows 登录启动设置不可用：{0}")]
    Platform(#[from] windows::core::Error),
    #[error("Windows 登录启动设置格式无效")]
    InvalidRegistryValue,
}

#[derive(Clone, Debug)]
pub struct WindowsStartupSettings {
    executable: PathBuf,
}

impl WindowsStartupSettings {
    pub fn for_current_executable() -> Result<Self, WindowsStartupError> {
        let executable = env::current_exe().map_err(WindowsStartupError::CurrentExecutable)?;
        Ok(Self { executable })
    }

    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            executable: executable.into(),
        }
    }

    pub fn is_enabled(&self) -> Result<bool, WindowsStartupError> {
        let Some(saved) = read_registry_string(RUN_KEY, PRODUCT_NAME)? else {
            return Ok(false);
        };
        Ok(saved.eq_ignore_ascii_case(&self.startup_command()))
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), WindowsStartupError> {
        if enabled {
            write_registry_string(RUN_KEY, PRODUCT_NAME, &self.startup_command())
        } else {
            delete_registry_value(RUN_KEY, PRODUCT_NAME)
        }
    }

    pub fn startup_command(&self) -> String {
        format!("\"{}\" --startup", self.executable.display())
    }
}

fn read_registry_string(subkey: &str, name: &str) -> Result<Option<String>, WindowsStartupError> {
    let subkey = wide(subkey);
    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &mut key,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    status.ok()?;
    let result = read_open_key_string(key, name);
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

fn read_open_key_string(key: HKEY, name: &str) -> Result<Option<String>, WindowsStartupError> {
    let name = wide(name);
    let mut value_type = REG_VALUE_TYPE::default();
    let mut byte_len = 0_u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut value_type),
            None,
            Some(&mut byte_len),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    status.ok()?;
    if value_type != REG_SZ || byte_len == 0 || byte_len > MAX_REGISTRY_VALUE_BYTES {
        return Err(WindowsStartupError::InvalidRegistryValue);
    }
    let mut bytes = vec![0_u8; byte_len as usize];
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut value_type),
            Some(bytes.as_mut_ptr()),
            Some(&mut byte_len),
        )
    };
    status.ok()?;
    if value_type != REG_SZ || byte_len as usize > bytes.len() || !byte_len.is_multiple_of(2) {
        return Err(WindowsStartupError::InvalidRegistryValue);
    }
    bytes.truncate(byte_len as usize);
    let wide = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let value = String::from_utf16(wide.strip_suffix(&[0]).unwrap_or(&wide))
        .map_err(|_| WindowsStartupError::InvalidRegistryValue)?;
    Ok(Some(value))
}

fn write_registry_string(subkey: &str, name: &str, value: &str) -> Result<(), WindowsStartupError> {
    with_writable_key(subkey, |key| {
        let name = wide(name);
        let data = wide_bytes(value);
        unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&data)).ok()? };
        Ok(())
    })
}

fn delete_registry_value(subkey: &str, name: &str) -> Result<(), WindowsStartupError> {
    with_writable_key(subkey, |key| {
        let name = wide(name);
        let status = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };
        if status == ERROR_FILE_NOT_FOUND || status == ERROR_SUCCESS {
            Ok(())
        } else {
            status.ok().map_err(Into::into)
        }
    })
}

fn with_writable_key<T>(
    subkey: &str,
    operation: impl FnOnce(HKEY) -> Result<T, WindowsStartupError>,
) -> Result<T, WindowsStartupError> {
    let subkey = wide(subkey);
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()?;
    }
    let result = operation(key);
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

fn wide_bytes(value: &str) -> Vec<u8> {
    wide(value).into_iter().flat_map(u16::to_le_bytes).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_command_quotes_paths_and_uses_background_mode() {
        let settings = WindowsStartupSettings::new(r"C:\Program Files\DeskLink\DeskLink.exe");
        assert_eq!(
            settings.startup_command(),
            r#""C:\Program Files\DeskLink\DeskLink.exe" --startup"#
        );
    }
}
