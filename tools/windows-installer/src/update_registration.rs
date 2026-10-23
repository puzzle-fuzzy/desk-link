use std::{fs, io, path::Path};

use windows::{
    Win32::{
        Foundation::ERROR_FILE_NOT_FOUND,
        System::Registry::{
            HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
            REG_VALUE_TYPE, RRF_RT_ANY, RegCloseKey, RegCreateKeyExW, RegDeleteValueW,
            RegGetValueW, RegSetValueExW,
        },
    },
    core::PCWSTR,
};

use crate::{InstallLayout, compare_release_versions, update::windows_error_to_io};

#[derive(Clone, Debug, Eq, PartialEq)]
struct RegistryValue {
    kind: REG_VALUE_TYPE,
    bytes: Vec<u8>,
}

/// The uninstall key is supplied explicitly so tests use their own HKCU subtree.
/// Only DisplayVersion and EstimatedSize can be written; the Run key and data
/// directory are never opened by this adapter.
pub struct InstalledRegistration {
    subkey: String,
    version: RegistryValue,
    size: Option<RegistryValue>,
}

impl InstalledRegistration {
    pub fn load(layout: &InstallLayout, subkey: &str, target_version: &str) -> io::Result<Self> {
        if !layout.application.is_file() || !layout.uninstaller.is_file() {
            return Err(invalid_install());
        }
        let location = required_string(subkey, "InstallLocation")?;
        let icon = required_string(subkey, "DisplayIcon")?;
        let uninstall = required_string(subkey, "UninstallString")?;
        if !same_existing_path(Path::new(&location), &layout.install_directory)?
            || !same_existing_path(Path::new(&icon), &layout.application)?
            || !uninstall.eq_ignore_ascii_case(&layout.uninstall_command())
        {
            return Err(invalid_install());
        }
        let version = read_value(subkey, "DisplayVersion")?.ok_or_else(invalid_install)?;
        let text = string_value(&version)?;
        match compare_release_versions(&text, target_version) {
            Some(std::cmp::Ordering::Greater) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "已安装较新的 DeskLink，不能用旧版升级包覆盖。",
                ));
            }
            None => return Err(invalid_install()),
            _ => {}
        }
        Ok(Self {
            subkey: subkey.to_owned(),
            version,
            size: read_value(subkey, "EstimatedSize")?,
        })
    }

    pub fn write_version(&self, version: &str, payload_size: usize) -> io::Result<()> {
        write_value(
            &self.subkey,
            "DisplayVersion",
            Some(&RegistryValue {
                kind: REG_SZ,
                bytes: wide_bytes(version),
            }),
        )?;
        let kib = payload_size.div_ceil(1024).min(u32::MAX as usize) as u32;
        write_value(
            &self.subkey,
            "EstimatedSize",
            Some(&RegistryValue {
                kind: REG_DWORD,
                bytes: kib.to_le_bytes().to_vec(),
            }),
        )
    }

    pub fn restore(&self) -> io::Result<()> {
        // Attempt both values even if one restoration fails.
        let version = write_value(&self.subkey, "DisplayVersion", Some(&self.version));
        let size = write_value(&self.subkey, "EstimatedSize", self.size.as_ref());
        version.and(size)
    }
}

fn invalid_install() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "找不到路径和登记一致的已安装 DeskLink。请使用普通安装程序修复安装后重试。",
    )
}

fn same_existing_path(left: &Path, right: &Path) -> io::Result<bool> {
    if !left.is_absolute() {
        return Ok(false);
    }
    Ok(fs::canonicalize(left)?
        .to_string_lossy()
        .eq_ignore_ascii_case(&fs::canonicalize(right)?.to_string_lossy()))
}

fn required_string(subkey: &str, name: &str) -> io::Result<String> {
    string_value(&read_value(subkey, name)?.ok_or_else(invalid_install)?)
}

fn string_value(value: &RegistryValue) -> io::Result<String> {
    if value.kind != REG_SZ || !value.bytes.len().is_multiple_of(2) {
        return Err(invalid_install());
    }
    let mut units: Vec<u16> = value
        .bytes
        .chunks_exact(2)
        .map(|part| u16::from_le_bytes([part[0], part[1]]))
        .collect();
    if units.pop() != Some(0) || units.contains(&0) {
        return Err(invalid_install());
    }
    String::from_utf16(&units).map_err(|_| invalid_install())
}

fn read_value(subkey: &str, name: &str) -> io::Result<Option<RegistryValue>> {
    let subkey = wide(subkey);
    let name = wide(name);
    let mut size = 0;
    let mut kind = REG_VALUE_TYPE::default();
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_ANY,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    status.ok().map_err(windows_error_to_io)?;
    if size > 131_072 {
        return Err(invalid_install());
    }
    let mut bytes = vec![0; size as usize];
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_ANY,
            Some(&mut kind),
            Some(bytes.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .map_err(windows_error_to_io)?;
    bytes.truncate(size as usize);
    Ok(Some(RegistryValue { kind, bytes }))
}

fn write_value(subkey: &str, name: &str, value: Option<&RegistryValue>) -> io::Result<()> {
    let subkey = wide(subkey);
    let name = wide(name);
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
    }
    .ok()
    .map_err(windows_error_to_io)?;
    let result = if let Some(value) = value {
        unsafe {
            RegSetValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                value.kind,
                Some(&value.bytes),
            )
        }
        .ok()
        .map_err(windows_error_to_io)
    } else {
        let status = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };
        if status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            status.ok().map_err(windows_error_to_io)
        }
    };
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn wide_bytes(text: &str) -> Vec<u8> {
    wide(text).into_iter().flat_map(u16::to_le_bytes).collect()
}

#[cfg(test)]
#[path = "update_registration_tests.rs"]
mod tests;
