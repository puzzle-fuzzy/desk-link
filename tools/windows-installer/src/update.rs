use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ExitStatus},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use crate::InstallLayout;

static ARTIFACT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Only the installed files and uninstall version are changed by an update.
/// Login-start preferences, shortcuts and user data are deliberately outside
/// this interface.
pub trait UpdateOperations {
    fn stop_application(&mut self) -> io::Result<()>;
    fn write_registration(&mut self) -> io::Result<()>;
    fn restore_registration(&mut self) -> io::Result<()>;
    fn start_application(&mut self) -> io::Result<()>;
}

/// Prepare both replacement files before stopping the app. Keep copies of the
/// old files until registration and relaunch have succeeded, so every error
/// after shutdown can restore and relaunch the previous installation.
pub fn apply_update(
    layout: &InstallLayout,
    payload: &[u8],
    installer: &Path,
    operations: &mut impl UpdateOperations,
) -> io::Result<()> {
    if payload.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "升级包没有应用程序文件",
        ));
    }
    let mut files = vec![
        Replacement::prepare(&layout.application, &mut io::Cursor::new(payload))?,
        Replacement::prepare(&layout.uninstaller, &mut File::open(installer)?)?,
    ];
    let mut registration_attempted = false;
    let result = (|| {
        operations.stop_application()?;
        for file in &mut files {
            file.install()?;
        }
        registration_attempted = true;
        operations.write_registration()?;
        operations.start_application()
    })();
    if let Err(error) = result {
        let mut details = vec![format!("升级未完成：{error}")];
        let mut restored = true;
        for file in files.iter_mut().rev() {
            if let Err(error) = file.rollback() {
                restored = false;
                details.push(format!(
                    "无法恢复 {}：{error}；旧版备份位于 {}",
                    file.destination.display(),
                    file.backup.display()
                ));
            }
        }
        if registration_attempted && let Err(error) = operations.restore_registration() {
            details.push(format!("无法恢复原版本登记：{error}"));
        }
        if restored {
            match operations.start_application() {
                Ok(()) => details
                    .push("已保留原程序并尝试重新打开，用户数据和开机启动设置未更改。".to_owned()),
                Err(error) => details.push(format!(
                    "原程序已保留，但无法重新打开：{error}；请从开始菜单启动 DeskLink。"
                )),
            }
        }
        return Err(io::Error::other(details.join("\r\n")));
    }
    Ok(())
}

struct Replacement {
    destination: PathBuf,
    staged: PathBuf,
    backup: PathBuf,
    installed: bool,
    preserve_backup: bool,
    owns_staged: bool,
    owns_backup: bool,
}

impl Replacement {
    fn prepare(destination: &Path, contents: &mut impl Read) -> io::Result<Self> {
        if !destination.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("未找到已安装文件：{}", destination.display()),
            ));
        }
        let sequence = ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = destination
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "升级目标没有文件名"))?
            .to_string_lossy();
        let suffix = format!("{}-{sequence}", std::process::id());
        let mut replacement = Self {
            destination: destination.to_path_buf(),
            staged: destination.with_file_name(format!(".{name}.update-{suffix}")),
            backup: destination.with_file_name(format!(".{name}.rollback-{suffix}")),
            installed: false,
            preserve_backup: false,
            owns_staged: false,
            owns_backup: false,
        };
        // create_new prevents an existing artifact or concurrent installer from
        // being overwritten. Drop removes only files created successfully here.
        let mut backup = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&replacement.backup)?;
        replacement.owns_backup = true;
        io::copy(&mut File::open(destination)?, &mut backup)?;
        backup.sync_all()?;
        let mut staged = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&replacement.staged)?;
        replacement.owns_staged = true;
        io::copy(contents, &mut staged)?;
        staged.flush()?;
        staged.sync_all()?;
        Ok(replacement)
    }

    fn install(&mut self) -> io::Result<()> {
        replace_file_with_retry(&self.staged, &self.destination)?;
        self.installed = true;
        Ok(())
    }

    fn rollback(&mut self) -> io::Result<()> {
        if self.installed {
            self.preserve_backup = true;
            replace_file_with_retry(&self.backup, &self.destination)?;
            self.installed = false;
            self.preserve_backup = false;
        }
        Ok(())
    }
}

impl Drop for Replacement {
    fn drop(&mut self) {
        if self.owns_staged {
            let _ = fs::remove_file(&self.staged);
        }
        if self.owns_backup && !self.preserve_backup {
            let _ = fs::remove_file(&self.backup);
        }
    }
}

pub fn replace_file_with_retry(source: &Path, destination: &Path) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        match replace_file(source, destination) {
            Err(error)
                if Instant::now() < deadline
                    && matches!(error.raw_os_error(), Some(5) | Some(32)) =>
            {
                thread::sleep(Duration::from_millis(100));
            }
            result => return result,
        }
    }
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        },
        core::PCWSTR,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(windows_error_to_io)
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
pub fn windows_error_to_io(error: windows::core::Error) -> io::Error {
    let code = error.code().0 as u32;
    if code & 0xffff_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xffff) as i32)
    } else {
        io::Error::other(error)
    }
}

/// Wait only for the helper that this installer spawned. A stuck IPC client
/// must not leave an invisible updater waiting forever, and is never permission
/// to terminate the user's original process.
pub fn wait_for_shutdown_helper(child: &mut Child, timeout: Duration) -> io::Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let cleanup_deadline = Instant::now() + Duration::from_secs(1);
            while Instant::now() < cleanup_deadline {
                if child.try_wait()?.is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "等待旧版退出超时。请从托盘退出 DeskLink 后重试升级。",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Surface an immediate loader/startup failure instead of reporting success
/// after merely creating a process. This is a bounded startup observation, not
/// a claim that the application's complete runtime has been accepted.
pub fn check_application_start(child: &mut Child, observation: Duration) -> io::Result<()> {
    let deadline = Instant::now() + observation;
    loop {
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!(
                "DeskLink 启动后立即退出（{status}）。"
            )));
        }
        if Instant::now() >= deadline {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(windows)]
pub struct InstallerLock(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl InstallerLock {
    pub fn acquire(name: &str, timeout: Duration) -> io::Result<Self> {
        use windows::{
            Win32::{
                Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0},
                System::Threading::{CreateMutexW, WaitForSingleObject},
            },
            core::PCWSTR,
        };
        let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(name.as_ptr())) }
            .map_err(windows_error_to_io)?;
        let timeout_ms = timeout.as_millis().min(u32::MAX as u128 - 1) as u32;
        match unsafe { WaitForSingleObject(handle, timeout_ms) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(handle)),
            _ => {
                unsafe {
                    let _ = CloseHandle(handle);
                }
                Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "另一个 DeskLink 安装、升级或卸载程序正在运行，请等待其完成后重试。",
                ))
            }
        }
    }
}

#[cfg(windows)]
impl Drop for InstallerLock {
    fn drop(&mut self) {
        use windows::Win32::{Foundation::CloseHandle, System::Threading::ReleaseMutex};
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;
