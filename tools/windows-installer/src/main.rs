#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(all(windows, feature = "embedded-payload"))]
const APPLICATION_PAYLOAD: &[u8] = include_bytes!(env!("DESKLINK_WINDOWS_PAYLOAD"));
#[cfg(all(windows, not(feature = "embedded-payload")))]
const APPLICATION_PAYLOAD: &[u8] = &[];

#[cfg(windows)]
mod windows_installer {
    use std::{
        env, fs, io,
        io::Write as _,
        path::{Path, PathBuf},
        process::{self, Command},
        thread,
        time::{Duration, Instant},
    };

    use crate::APPLICATION_PAYLOAD;
    use desklink_delivery_layout::{InstallLayout, PRODUCT_NAME, PRODUCT_VERSION};
    use windows::{
        Win32::{
            Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, LPARAM, WPARAM},
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
                    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE,
                    REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW,
                    RegSetValueExW,
                },
                Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
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

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Action {
        Install,
        Uninstall,
        UninstallWorker,
    }

    #[derive(Clone, Copy, Debug)]
    struct Options {
        action: Action,
        quiet: bool,
        no_autostart: bool,
        remove_data: bool,
    }

    impl Options {
        fn parse() -> Result<Self, String> {
            let mut options = Self {
                action: Action::Install,
                quiet: false,
                no_autostart: false,
                remove_data: false,
            };
            for argument in env::args().skip(1) {
                match argument.as_str() {
                    "--quiet" => options.quiet = true,
                    "--no-autostart" => options.no_autostart = true,
                    "--remove-data" => options.remove_data = true,
                    "--uninstall" => options.action = Action::Uninstall,
                    "--uninstall-worker" => options.action = Action::UninstallWorker,
                    _ => return Err(format!("unknown installer option: {argument}")),
                }
            }
            if options.action == Action::Install && options.remove_data {
                return Err("--remove-data can only be used with --uninstall".to_owned());
            }
            Ok(options)
        }
    }

    pub fn run() -> i32 {
        let options = match Options::parse() {
            Ok(options) => options,
            Err(error) => return report_error(false, &error),
        };
        let layout = match current_user_layout() {
            Ok(layout) => layout,
            Err(error) => return report_error(options.quiet, &error.to_string()),
        };
        let result = match options.action {
            Action::Install => install(&layout, options),
            Action::Uninstall => uninstall(&layout, options),
            Action::UninstallWorker => {
                thread::sleep(Duration::from_millis(600));
                uninstall_now(&layout, options)
            }
        };
        match result {
            Ok(Outcome::Cancelled) => 0,
            Ok(Outcome::Completed(message)) => {
                if !options.quiet && options.action != Action::UninstallWorker {
                    show_message(&message, MB_OK | MB_ICONINFORMATION);
                }
                0
            }
            Err(error) => report_error(options.quiet, &error.to_string()),
        }
    }

    enum Outcome {
        Cancelled,
        Completed(String),
    }

    fn current_user_layout() -> io::Result<InstallLayout> {
        let local = env::var_os("LOCALAPPDATA").ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not available")
        })?;
        let roaming = env::var_os("APPDATA")
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "APPDATA is not available"))?;
        Ok(InstallLayout::from_user_roots(
            Path::new(&local),
            Path::new(&roaming),
        ))
    }

    fn install(layout: &InstallLayout, options: Options) -> io::Result<Outcome> {
        if APPLICATION_PAYLOAD.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "this development installer does not contain the DeskLink application payload",
            ));
        }
        let upgrading = layout.application.is_file();
        if !options.quiet {
            let verb = if upgrading { "Upgrade" } else { "Install" };
            let prompt = format!(
                "{verb} DeskLink {PRODUCT_VERSION} for the current Windows user?\r\n\r\nProgram: {}\r\nUser data: {}\r\n\r\nYour identity, trusted controllers, and diagnostic history stay separate from program files.",
                layout.install_directory.display(),
                layout.data_directory.display()
            );
            if !confirm(&prompt) {
                return Ok(Outcome::Cancelled);
            }
        }

        stop_running_application();
        fs::create_dir_all(&layout.install_directory)?;
        atomic_write(&layout.application, APPLICATION_PAYLOAD)?;
        let installer = env::current_exe()?;
        if !same_file_path(&installer, &layout.uninstaller) {
            atomic_copy(&installer, &layout.uninstaller)?;
        }
        create_start_menu_shortcut(layout)?;
        if options.no_autostart {
            delete_registry_value(RUN_KEY, PRODUCT_NAME)?;
        } else {
            set_registry_string(RUN_KEY, PRODUCT_NAME, &layout.startup_command())?;
        }
        write_uninstall_registration(layout, APPLICATION_PAYLOAD.len())?;

        let message = if upgrading {
            format!(
                "DeskLink {PRODUCT_VERSION} was upgraded. Existing identity and trust data were preserved."
            )
        } else if options.no_autostart {
            format!(
                "DeskLink {PRODUCT_VERSION} was installed for this Windows user. Automatic startup is disabled."
            )
        } else {
            format!(
                "DeskLink {PRODUCT_VERSION} was installed for this Windows user. It will start automatically at sign-in."
            )
        };
        Ok(Outcome::Completed(message))
    }

    fn uninstall(layout: &InstallLayout, options: Options) -> io::Result<Outcome> {
        if !options.quiet {
            let data_copy = if options.remove_data {
                "Identity, trusted-controller records, and diagnostics will also be deleted."
            } else {
                "Identity, trusted-controller records, and diagnostics will be preserved."
            };
            if !confirm(&format!(
                "Uninstall DeskLink for this Windows user?\r\n\r\n{data_copy}"
            )) {
                return Ok(Outcome::Cancelled);
            }
        }
        if same_file_path(&env::current_exe()?, &layout.uninstaller) {
            spawn_uninstall_worker(options)?;
            return Ok(Outcome::Completed(
                "DeskLink uninstall has started.".to_owned(),
            ));
        }
        uninstall_now(layout, options)
    }

    fn uninstall_now(layout: &InstallLayout, options: Options) -> io::Result<Outcome> {
        stop_running_application();
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
            "DeskLink and its current-user data were removed.".to_owned()
        } else {
            format!(
                "DeskLink was removed. Current-user identity, trust, and diagnostics remain in {}.",
                layout.data_directory.display()
            )
        }))
    }

    fn spawn_uninstall_worker(options: Options) -> io::Result<()> {
        let current = env::current_exe()?;
        let worker = env::temp_dir().join(format!("DeskLinkUninstallWorker-{}.exe", process::id()));
        fs::copy(current, &worker)?;
        let mut command = Command::new(worker);
        command.arg("--uninstall-worker").arg("--quiet");
        if options.remove_data {
            command.arg("--remove-data");
        }
        command.spawn()?;
        Ok(())
    }

    fn stop_running_application() {
        let class = wide(WINDOW_CLASS);
        let hwnd = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) };
        let Ok(hwnd) = hwnd else {
            return;
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
                    3_000,
                    None,
                );
            }
        }
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            if unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }.is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(75));
        }
        if let Some(process) = process {
            unsafe {
                let _ = WaitForSingleObject(process, 4_000);
                let _ = CloseHandle(process);
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
        let name = destination.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
        })?;
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
                link.SetDescription(windows::core::w!("Securely control this Windows PC"))?;
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
        let title = wide("DeskLink Setup");
        unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), style) }
    }

    fn report_error(quiet: bool, error: &str) -> i32 {
        if !quiet {
            show_message(
                &format!("DeskLink Setup could not complete.\r\n\r\n{error}"),
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
        io::Error::other(error)
    }
}

#[cfg(windows)]
fn main() {
    std::process::exit(windows_installer::run());
}

#[cfg(not(windows))]
fn main() {}
