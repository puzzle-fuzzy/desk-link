#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(all(windows, feature = "embedded-payload"))]
const APPLICATION_PAYLOAD: &[u8] = include_bytes!(env!("DESKLINK_WINDOWS_UI_PAYLOAD_RESOLVED"));
#[cfg(all(windows, not(feature = "embedded-payload")))]
const APPLICATION_PAYLOAD: &[u8] = &[];
#[cfg(windows)]
mod windows_installer {
    use std::{
        env, fs, io,
        io::Write as _,
        os::windows::process::CommandExt,
        path::{Path, PathBuf},
        process::{self, Command},
        thread,
        time::{Duration, Instant},
    };

    use crate::APPLICATION_PAYLOAD;
    use desklink_delivery_layout::{
        Action, InstallLayout, Options, PRODUCT_NAME, PRODUCT_VERSION, compare_release_versions,
        update::{
            InstallerLock, UpdateOperations, apply_update, check_application_start,
            wait_for_shutdown_helper,
        },
        update_registration::InstalledRegistration,
        webview2_version_is_suitable,
    };
    use windows::{
        Win32::{
            Foundation::{
                CloseHandle, ERROR_FILE_NOT_FOUND, LPARAM, WAIT_ABANDONED, WAIT_OBJECT_0, WPARAM,
            },
            Storage::FileSystem::{
                MOVEFILE_DELAY_UNTIL_REBOOT, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
                MoveFileExW,
            },
            System::{
                Com::{
                    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance,
                    CoInitializeEx, CoUninitialize, IPersistFile,
                },
                Registry::{
                    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_DWORD,
                    REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW,
                    RegDeleteTreeW, RegDeleteValueW, RegGetValueW, RegSetValueExW,
                },
                Threading::{
                    OpenMutexW, OpenProcess, PROCESS_SYNCHRONIZE, ReleaseMutex,
                    SYNCHRONIZATION_ACCESS_RIGHTS, WaitForSingleObject,
                },
            },
            UI::{
                Shell::{IShellLinkW, ShellLink},
                WindowsAndMessaging::{
                    FindWindowW, GetWindowThreadProcessId, IDYES, MB_DEFBUTTON2, MB_ICONERROR,
                    MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO, MESSAGEBOX_RESULT,
                    MessageBoxW, RegisterWindowMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
                },
            },
        },
        core::{Interface, PCWSTR},
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\DeskLink";
    const WINDOW_CLASS: &str = "DeskLinkHostStatusWindow";
    const INSTALLER_SHUTDOWN_MESSAGE: &str = "DeskLinkInstallerShutdown";
    const CONTROL_SURFACE_MUTEX: windows::core::PCWSTR =
        windows::core::w!("Local\\DeskLinkControlSurface");
    const SYNCHRONIZE_ACCESS: SYNCHRONIZATION_ACCESS_RIGHTS =
        SYNCHRONIZATION_ACCESS_RIGHTS(0x0010_0001);

    pub fn run() -> i32 {
        let options = match Options::parse(env::args().skip(1)) {
            Ok(options) => options,
            Err(error) => return report_error(false, &error),
        };
        let layout = match current_user_layout() {
            Ok(layout) => layout,
            Err(error) => return report_error(!options.show_errors(), &error.to_string()),
        };
        let result = (|| {
            // The handoff worker may start before its parent releases this
            // guard. All other concurrent installers fail immediately.
            let timeout = if options.action == Action::UninstallWorker {
                Duration::from_secs(5)
            } else {
                Duration::ZERO
            };
            let _lock = InstallerLock::acquire(r"Local\DeskLinkInstaller", timeout)?;
            match options.action {
                Action::Install => install(&layout, options),
                Action::Update => update(&layout),
                Action::Uninstall => uninstall(&layout, options),
                Action::UninstallWorker => {
                    thread::sleep(Duration::from_millis(600));
                    uninstall_now(&layout, options)
                }
            }
        })();
        // In particular, release the guard before the uninstall parent's
        // informational dialog: its worker must be free to acquire the lock.
        match result {
            Ok(Outcome::Cancelled | Outcome::HandedOff) => 0,
            Ok(Outcome::Completed(message)) => {
                if options.show_success() {
                    show_message(&message, MB_OK | MB_ICONINFORMATION);
                }
                0
            }
            Err(error) => report_error(!options.show_errors(), &error.to_string()),
        }
    }

    enum Outcome {
        Cancelled,
        HandedOff,
        Completed(String),
    }

    fn current_user_layout() -> io::Result<InstallLayout> {
        let local = env::var_os("LOCALAPPDATA")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法读取 LOCALAPPDATA"))?;
        let roaming = env::var_os("APPDATA")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "无法读取 APPDATA"))?;
        Ok(InstallLayout::from_user_roots(
            Path::new(&local),
            Path::new(&roaming),
        ))
    }

    fn ensure_webview2_runtime() -> io::Result<()> {
        const WEBVIEW2_MACHINE_CLIENT: &str = r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
        const WEBVIEW2_USER_CLIENT: &str =
            r"Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

        for (root, path) in [
            (HKEY_LOCAL_MACHINE, WEBVIEW2_MACHINE_CLIENT),
            (HKEY_CURRENT_USER, WEBVIEW2_USER_CLIENT),
        ] {
            if let Some(version) = read_registry_value(root, path, "pv")?
                && webview2_version_is_suitable(&version)
            {
                return Ok(());
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "未检测到 Microsoft Edge WebView2 Runtime。请先安装 Evergreen WebView2 Runtime：\r\nhttps://developer.microsoft.com/microsoft-edge/webview2/\r\n\r\n安装完成后重新运行 DeskLink 安装程序。",
        ))
    }

    fn read_registry_value(root: HKEY, subkey: &str, value: &str) -> io::Result<Option<String>> {
        let subkey = wide(subkey);
        let value = wide(value);
        let mut byte_count = 0u32;
        let status = unsafe {
            RegGetValueW(
                root,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut byte_count),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok().map_err(windows_error_to_io)?;
        if byte_count == 0 || byte_count > 512 || !byte_count.is_multiple_of(2) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "WebView2 Runtime 注册表版本值无效",
            ));
        }
        let mut data = vec![0u16; (byte_count as usize).div_ceil(2)];
        let status = unsafe {
            RegGetValueW(
                root,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(data.as_mut_ptr().cast()),
                Some(&mut byte_count),
            )
        };
        status.ok().map_err(windows_error_to_io)?;
        data.truncate(byte_count as usize / 2);
        if data.last() == Some(&0) {
            data.pop();
        }
        String::from_utf16(&data).map(Some).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "WebView2 Runtime 版本不是有效文本",
            )
        })
    }

    fn install(layout: &InstallLayout, options: Options) -> io::Result<Outcome> {
        if APPLICATION_PAYLOAD.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "此开发版安装器不包含 DeskLink 应用程序文件",
            ));
        }
        ensure_webview2_runtime()?;
        refuse_downgrade()?;
        let upgrading = layout.application.is_file();
        if !options.quiet {
            let verb = if upgrading { "升级" } else { "安装" };
            let prompt = format!(
                "要为当前 Windows 用户{verb} DeskLink {PRODUCT_VERSION} 吗？\r\n\r\n程序位置：{}\r\n用户数据：{}\r\n\r\n设备身份、可信控制端和诊断记录将与程序文件分开保存。",
                layout.install_directory.display(),
                layout.data_directory.display()
            );
            if !confirm(&prompt) {
                return Ok(Outcome::Cancelled);
            }
        }

        stop_running_application(layout)?;
        fs::create_dir_all(&layout.install_directory)?;
        atomic_write_with_retry(&layout.application, APPLICATION_PAYLOAD)?;
        let installer = env::current_exe()?;
        if !same_file_path(&installer, &layout.uninstaller) {
            atomic_copy(&installer, &layout.uninstaller)?;
        }
        create_start_menu_shortcut(layout)?;
        if options.no_autostart {
            delete_registry_value(RUN_KEY, PRODUCT_NAME)?;
        } else if !upgrading {
            set_registry_string(RUN_KEY, PRODUCT_NAME, &layout.startup_command())?;
        }
        write_uninstall_registration(layout, APPLICATION_PAYLOAD.len())?;

        if !options.quiet {
            Command::new(&layout.application).spawn()?;
        }

        let message = if upgrading {
            format!("DeskLink {PRODUCT_VERSION} 已升级，现有设备身份和信任数据均已保留。")
        } else if options.no_autostart {
            format!("DeskLink {PRODUCT_VERSION} 已为当前 Windows 用户安装，开机自动启动未启用。")
        } else {
            format!("DeskLink {PRODUCT_VERSION} 已为当前 Windows 用户安装，并会在登录后自动启动。")
        };
        Ok(Outcome::Completed(message))
    }

    fn update(layout: &InstallLayout) -> io::Result<Outcome> {
        // Validate before staging files, signalling a running process or opening
        // any writable registry key. A portable/debug process cannot create an
        // installation by accidentally invoking this mode.
        let registration = InstalledRegistration::load(layout, UNINSTALL_KEY, PRODUCT_VERSION)?;
        ensure_webview2_runtime()?;
        let installer = env::current_exe()?;
        let mut operations = WindowsUpdateOperations {
            layout,
            registration,
        };
        apply_update(layout, APPLICATION_PAYLOAD, &installer, &mut operations)?;
        Ok(Outcome::Completed(String::new()))
    }

    struct WindowsUpdateOperations<'a> {
        layout: &'a InstallLayout,
        registration: InstalledRegistration,
    }

    impl UpdateOperations for WindowsUpdateOperations<'_> {
        fn stop_application(&mut self) -> io::Result<()> {
            stop_running_application(self.layout)
        }

        fn write_registration(&mut self) -> io::Result<()> {
            self.registration
                .write_version(PRODUCT_VERSION, APPLICATION_PAYLOAD.len())
        }

        fn restore_registration(&mut self) -> io::Result<()> {
            self.registration.restore()
        }

        fn start_application(&mut self) -> io::Result<()> {
            let mut application = Command::new(&self.layout.application).spawn()?;
            check_application_start(&mut application, Duration::from_secs(1))
        }
    }

    fn uninstall(layout: &InstallLayout, options: Options) -> io::Result<Outcome> {
        if !options.quiet {
            let data_copy = if options.remove_data {
                "设备身份、可信控制端记录和诊断数据也会被删除。"
            } else {
                "设备身份、可信控制端记录和诊断数据将保留。"
            };
            if !confirm(&format!(
                "要为当前 Windows 用户卸载 DeskLink 吗？\r\n\r\n{data_copy}"
            )) {
                return Ok(Outcome::Cancelled);
            }
        }
        if same_file_path(&env::current_exe()?, &layout.uninstaller) {
            spawn_uninstall_worker(options)?;
            // Exit without an informational dialog: the worker must be able
            // to remove this executable after it acquires the installer lock.
            return Ok(Outcome::HandedOff);
        }
        uninstall_now(layout, options)
    }

    fn uninstall_now(layout: &InstallLayout, options: Options) -> io::Result<Outcome> {
        stop_running_application(layout)?;
        delete_registry_value(RUN_KEY, PRODUCT_NAME)?;
        delete_registry_tree(UNINSTALL_KEY)?;
        remove_file_if_present(&layout.start_menu_shortcut)?;
        remove_directory_if_present(&layout.install_directory)?;
        if options.remove_data {
            remove_directory_if_present(&layout.data_directory)?;
        }
        if options.action == Action::UninstallWorker {
            schedule_current_executable_for_deletion();
        }
        Ok(Outcome::Completed(if options.remove_data {
            "DeskLink 及当前用户的数据均已移除。".to_owned()
        } else {
            format!(
                "DeskLink 已移除。当前用户的设备身份、信任和诊断数据仍保存在 {}。",
                layout.data_directory.display()
            )
        }))
    }

    fn spawn_uninstall_worker(options: Options) -> io::Result<()> {
        let current = env::current_exe()?;
        let worker = env::temp_dir().join(format!("DeskLinkUninstallWorker-{}.exe", process::id()));
        fs::copy(current, &worker)?;
        let mut command = Command::new(worker);
        command.arg("--uninstall-worker");
        if options.quiet {
            command.arg("--quiet");
        }
        if options.remove_data {
            command.arg("--remove-data");
        }
        command.spawn()?;
        Ok(())
    }

    fn stop_running_application(layout: &InstallLayout) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let control_surface =
            unsafe { OpenMutexW(SYNCHRONIZE_ACCESS, false, CONTROL_SURFACE_MUTEX) }.ok();
        let class = wide(WINDOW_CLASS);
        let window = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }.ok();
        if control_surface.is_none() && window.is_none() {
            return Ok(());
        }
        if let Some(control_surface) = control_surface {
            let stopped = (|| {
                let mut helper = Command::new(&layout.application)
                    .arg("--installer-shutdown")
                    .creation_flags(0x0800_0000) // CREATE_NO_WINDOW; only our helper is terminated on timeout.
                    .spawn()?;
                wait_for_shutdown_helper(&mut helper, Duration::from_secs(5))?;
                let result =
                    unsafe { WaitForSingleObject(control_surface, remaining_ms(deadline)) };
                if matches!(result, WAIT_OBJECT_0 | WAIT_ABANDONED) {
                    unsafe {
                        let _ = ReleaseMutex(control_surface);
                    }
                    Ok(())
                } else {
                    Err(shutdown_timeout())
                }
            })();
            unsafe {
                let _ = CloseHandle(control_surface);
            }
            stopped?;
        }
        let hwnd = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) };
        let Ok(hwnd) = hwnd else {
            return Ok(());
        };
        let mut process_id = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut process_id));
        }
        let process = if process_id == 0 {
            None
        } else {
            unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, process_id) }.ok()
        };
        let message_name = wide(INSTALLER_SHUTDOWN_MESSAGE);
        let message = unsafe { RegisterWindowMessageW(PCWSTR(message_name.as_ptr())) };
        if message != 0 {
            unsafe {
                let _ = SendMessageTimeoutW(
                    hwnd,
                    message,
                    WPARAM(0),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG,
                    remaining_ms(deadline).min(3_000),
                    None,
                );
            }
        }
        while Instant::now() < deadline {
            if unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }.is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(75));
        }
        if let Some(process) = process {
            let stopped = unsafe { WaitForSingleObject(process, remaining_ms(deadline)) };
            unsafe {
                let _ = CloseHandle(process);
            }
            if stopped != WAIT_OBJECT_0 {
                return Err(shutdown_timeout());
            }
        } else if unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }.is_ok() {
            return Err(shutdown_timeout());
        }
        Ok(())
    }

    fn remaining_ms(deadline: Instant) -> u32 {
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(u32::MAX as u128) as u32
    }

    fn shutdown_timeout() -> io::Error {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "DeskLink 尚未退出。请从托盘完全退出后重试安装。",
        )
    }

    fn atomic_write_with_retry(destination: &Path, bytes: &[u8]) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(6);
        loop {
            match atomic_write(destination, bytes) {
                Ok(()) => return Ok(()),
                Err(error)
                    if Instant::now() < deadline
                        && matches!(error.raw_os_error(), Some(5) | Some(32)) =>
                {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn atomic_write(destination: &Path, bytes: &[u8]) -> io::Result<()> {
        let temporary = temporary_sibling(destination)?;
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            atomic_replace(&temporary, destination)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn atomic_copy(source: &Path, destination: &Path) -> io::Result<()> {
        let temporary = temporary_sibling(destination)?;
        let result = (|| {
            fs::copy(source, &temporary)?;
            let file = fs::OpenOptions::new().write(true).open(&temporary)?;
            file.sync_all()?;
            atomic_replace(&temporary, destination)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn temporary_sibling(destination: &Path) -> io::Result<PathBuf> {
        let name = destination
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "目标路径没有文件名"))?;
        Ok(
            destination.with_file_name(format!(
                ".{}.tmp-{}",
                name.to_string_lossy(),
                process::id()
            )),
        )
    }

    fn atomic_replace(source: &Path, destination: &Path) -> io::Result<()> {
        let source = wide_path(source);
        let destination = wide_path(destination);
        unsafe {
            MoveFileExW(
                PCWSTR(source.as_ptr()),
                PCWSTR(destination.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(windows_error_to_io)
    }

    fn create_start_menu_shortcut(layout: &InstallLayout) -> io::Result<()> {
        if let Some(parent) = layout.start_menu_shortcut.parent() {
            fs::create_dir_all(parent)?;
        }
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(windows_error_to_io)?;
        let result = (|| -> windows::core::Result<()> {
            let link: IShellLinkW =
                unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)? };
            let application = wide_path(&layout.application);
            let working_directory = wide_path(&layout.install_directory);
            let shortcut = wide_path(&layout.start_menu_shortcut);
            unsafe {
                link.SetPath(PCWSTR(application.as_ptr()))?;
                link.SetDescription(windows::core::w!("安全控制此 Windows 电脑"))?;
                link.SetWorkingDirectory(PCWSTR(working_directory.as_ptr()))?;
                link.SetIconLocation(PCWSTR(application.as_ptr()), 0)?;
                let persist: IPersistFile = link.cast()?;
                persist.Save(PCWSTR(shortcut.as_ptr()), true)?;
            }
            Ok(())
        })();
        unsafe { CoUninitialize() };
        result.map_err(windows_error_to_io)
    }

    fn write_uninstall_registration(layout: &InstallLayout, payload_size: usize) -> io::Result<()> {
        set_registry_string(UNINSTALL_KEY, "DisplayName", PRODUCT_NAME)?;
        set_registry_string(UNINSTALL_KEY, "DisplayVersion", PRODUCT_VERSION)?;
        set_registry_string(UNINSTALL_KEY, "Publisher", "DeskLink")?;
        set_registry_string(
            UNINSTALL_KEY,
            "InstallLocation",
            &layout.install_directory.display().to_string(),
        )?;
        set_registry_string(
            UNINSTALL_KEY,
            "DisplayIcon",
            &layout.application.display().to_string(),
        )?;
        set_registry_string(
            UNINSTALL_KEY,
            "UninstallString",
            &layout.uninstall_command(),
        )?;
        set_registry_string(
            UNINSTALL_KEY,
            "QuietUninstallString",
            &format!("{} --quiet", layout.uninstall_command()),
        )?;
        set_registry_dword(UNINSTALL_KEY, "NoModify", 1)?;
        set_registry_dword(UNINSTALL_KEY, "NoRepair", 1)?;
        let estimated_kib = payload_size.div_ceil(1024).min(u32::MAX as usize) as u32;
        set_registry_dword(UNINSTALL_KEY, "EstimatedSize", estimated_kib)
    }

    fn refuse_downgrade() -> io::Result<()> {
        let Some(installed_version) = read_registry_string(UNINSTALL_KEY, "DisplayVersion")? else {
            return Ok(());
        };
        if compare_release_versions(&installed_version, PRODUCT_VERSION)
            == Some(std::cmp::Ordering::Greater)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "当前电脑已安装较新的 DeskLink {installed_version}，不能用 {PRODUCT_VERSION} 覆盖。请使用不低于已安装版本的安装包。"
                ),
            ));
        }
        Ok(())
    }

    fn read_registry_string(subkey: &str, name: &str) -> io::Result<Option<String>> {
        let subkey = wide(subkey);
        let name = wide(name);
        let mut byte_count = 0u32;
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                None,
                Some(&mut byte_count),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status.ok().map_err(windows_error_to_io)?;
        if byte_count == 0 || byte_count > 512 || !byte_count.is_multiple_of(2) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "已安装版本信息无效",
            ));
        }
        let mut data = vec![0u16; (byte_count as usize).div_ceil(2)];
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                PCWSTR(name.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(data.as_mut_ptr().cast()),
                Some(&mut byte_count),
            )
        };
        status.ok().map_err(windows_error_to_io)?;
        data.truncate(byte_count as usize / 2);
        if data.last() == Some(&0) {
            data.pop();
        }
        String::from_utf16(&data)
            .map(Some)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "已安装版本信息不是有效文本"))
    }

    fn set_registry_string(subkey: &str, name: &str, value: &str) -> io::Result<()> {
        with_writable_key(subkey, |key| {
            let name = wide(name);
            let data = wide_bytes(value);
            unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&data)).ok() }
                .map_err(windows_error_to_io)
        })
    }

    fn set_registry_dword(subkey: &str, name: &str, value: u32) -> io::Result<()> {
        with_writable_key(subkey, |key| {
            let name = wide(name);
            unsafe {
                RegSetValueExW(
                    key,
                    PCWSTR(name.as_ptr()),
                    None,
                    REG_DWORD,
                    Some(&value.to_le_bytes()),
                )
                .ok()
            }
            .map_err(windows_error_to_io)
        })
    }

    fn delete_registry_value(subkey: &str, name: &str) -> io::Result<()> {
        with_writable_key(subkey, |key| {
            let name = wide(name);
            let status = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };
            if status == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                status.ok().map_err(windows_error_to_io)
            }
        })
    }

    fn delete_registry_tree(subkey: &str) -> io::Result<()> {
        let subkey = wide(subkey);
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(subkey.as_ptr())) };
        if status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            status.ok().map_err(windows_error_to_io)
        }
    }

    fn with_writable_key<T>(
        subkey: &str,
        operation: impl FnOnce(HKEY) -> io::Result<T>,
    ) -> io::Result<T> {
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
            .ok()
        }
        .map_err(windows_error_to_io)?;
        let result = operation(key);
        unsafe {
            let _ = RegCloseKey(key);
        }
        result
    }

    fn remove_file_if_present(path: &Path) -> io::Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn remove_directory_if_present(path: &Path) -> io::Result<()> {
        match fs::remove_dir_all(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn schedule_current_executable_for_deletion() {
        if let Ok(current) = env::current_exe() {
            let current = wide_path(&current);
            unsafe {
                let _ = MoveFileExW(
                    PCWSTR(current.as_ptr()),
                    PCWSTR::null(),
                    MOVEFILE_DELAY_UNTIL_REBOOT,
                );
            }
        }
    }

    fn same_file_path(left: &Path, right: &Path) -> bool {
        let normalize = |path: &Path| {
            fs::canonicalize(path)
                .unwrap_or_else(|_| path.to_path_buf())
                .to_string_lossy()
                .to_ascii_lowercase()
        };
        normalize(left) == normalize(right)
    }

    fn confirm(text: &str) -> bool {
        matches!(
            show_message(text, MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2),
            IDYES
        )
    }

    fn show_message(
        text: &str,
        style: windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE,
    ) -> MESSAGEBOX_RESULT {
        let text = wide(text);
        let title = wide("DeskLink 安装程序");
        unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), style) }
    }

    fn report_error(quiet: bool, error: &str) -> i32 {
        if !quiet {
            show_message(
                &format!("DeskLink 安装程序无法完成操作。\r\n\r\n{error}"),
                MB_OK | MB_ICONERROR,
            );
        }
        1
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        wide(&path.as_os_str().to_string_lossy())
    }

    fn wide_bytes(value: &str) -> Vec<u8> {
        wide(value).into_iter().flat_map(u16::to_le_bytes).collect()
    }

    fn windows_error_to_io(error: windows::core::Error) -> io::Error {
        desklink_delivery_layout::update::windows_error_to_io(error)
    }
}

#[cfg(windows)]
fn main() {
    std::process::exit(windows_installer::run());
}

#[cfg(not(windows))]
fn main() {}
