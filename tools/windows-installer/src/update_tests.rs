use super::*;

struct Sandbox {
    root: PathBuf,
    layout: InstallLayout,
    installer: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "desklink-installer-test-{}-{}",
            std::process::id(),
            ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let layout = InstallLayout::from_user_roots(&root.join("local"), &root.join("roaming"));
        fs::create_dir_all(&layout.install_directory).unwrap();
        fs::create_dir_all(&layout.data_directory).unwrap();
        fs::write(&layout.application, b"old application").unwrap();
        fs::write(&layout.uninstaller, b"old installer").unwrap();
        fs::write(
            layout.data_directory.join("fixed-access.bin"),
            b"test-only fixed access sentinel",
        )
        .unwrap();
        fs::write(
            layout.data_directory.join("trusted-controllers.bin"),
            b"test-only trust sentinel",
        )
        .unwrap();
        let installer = root.join("downloaded-installer.exe");
        fs::write(&installer, b"new installer").unwrap();
        Self {
            root,
            layout,
            installer,
        }
    }

    fn operations(&self) -> Operations {
        Operations {
            app: self.layout.application.clone(),
            ..Operations::default()
        }
    }

    fn assert_old_files(&self) {
        assert_eq!(
            fs::read(&self.layout.application).unwrap(),
            b"old application"
        );
        assert_eq!(
            fs::read(&self.layout.uninstaller).unwrap(),
            b"old installer"
        );
    }

    fn assert_data_preserved_and_artifacts_cleaned(&self) {
        assert_eq!(
            fs::read(self.layout.data_directory.join("fixed-access.bin")).unwrap(),
            b"test-only fixed access sentinel"
        );
        assert_eq!(
            fs::read(self.layout.data_directory.join("trusted-controllers.bin")).unwrap(),
            b"test-only trust sentinel"
        );
        assert_eq!(
            fs::read_dir(&self.layout.install_directory)
                .unwrap()
                .count(),
            2
        );
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = fs::canonicalize(&self.root).unwrap();
        assert_eq!(root.parent(), Some(temporary.as_path()));
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("desklink-installer-test-")
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[derive(Default)]
struct Operations {
    app: PathBuf,
    stops: usize,
    launches: Vec<Vec<u8>>,
    registration: bool,
    fail_registration: bool,
    fail_stop: bool,
    fail_new_start: bool,
}

impl UpdateOperations for Operations {
    fn stop_application(&mut self) -> io::Result<()> {
        self.stops += 1;
        if self.fail_stop {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "test shutdown timeout",
            ));
        }
        Ok(())
    }
    fn write_registration(&mut self) -> io::Result<()> {
        self.registration = true;
        if self.fail_registration {
            return Err(io::Error::other(
                "test registry failure after partial write",
            ));
        }
        Ok(())
    }
    fn restore_registration(&mut self) -> io::Result<()> {
        self.registration = false;
        Ok(())
    }
    fn start_application(&mut self) -> io::Result<()> {
        let bytes = fs::read(&self.app)?;
        self.launches.push(bytes.clone());
        if self.fail_new_start && bytes == b"new application" {
            return Err(io::Error::other("test loader failure"));
        }
        Ok(())
    }
}

#[test]
fn successful_update_replaces_both_files_relaunches_and_preserves_data() {
    let sandbox = Sandbox::new();
    let mut operations = sandbox.operations();
    apply_update(
        &sandbox.layout,
        b"new application",
        &sandbox.installer,
        &mut operations,
    )
    .unwrap();
    assert_eq!(
        fs::read(&sandbox.layout.application).unwrap(),
        b"new application"
    );
    assert_eq!(
        fs::read(&sandbox.layout.uninstaller).unwrap(),
        b"new installer"
    );
    assert_eq!(operations.stops, 1);
    assert!(operations.registration);
    assert_eq!(operations.launches, [b"new application".to_vec()]);
    sandbox.assert_data_preserved_and_artifacts_cleaned();
}

#[test]
fn partial_registration_failure_restores_both_files_and_starts_the_old_app() {
    let sandbox = Sandbox::new();
    let mut operations = sandbox.operations();
    operations.fail_registration = true;
    assert!(
        apply_update(
            &sandbox.layout,
            b"new application",
            &sandbox.installer,
            &mut operations
        )
        .unwrap_err()
        .to_string()
        .contains("registry failure")
    );
    sandbox.assert_old_files();
    assert!(!operations.registration);
    assert_eq!(operations.launches, [b"old application".to_vec()]);
    sandbox.assert_data_preserved_and_artifacts_cleaned();
}

#[test]
fn failed_new_application_launch_rolls_back_files_and_registration() {
    let sandbox = Sandbox::new();
    let mut operations = sandbox.operations();
    operations.fail_new_start = true;
    assert!(
        apply_update(
            &sandbox.layout,
            b"new application",
            &sandbox.installer,
            &mut operations
        )
        .is_err()
    );
    sandbox.assert_old_files();
    assert!(!operations.registration);
    assert_eq!(
        operations.launches,
        [b"new application".to_vec(), b"old application".to_vec()]
    );
    sandbox.assert_data_preserved_and_artifacts_cleaned();
}

#[test]
fn shutdown_timeout_keeps_old_files_and_still_attempts_to_reopen_them() {
    let sandbox = Sandbox::new();
    let mut operations = sandbox.operations();
    operations.fail_stop = true;
    assert!(
        apply_update(
            &sandbox.layout,
            b"new application",
            &sandbox.installer,
            &mut operations
        )
        .is_err()
    );
    sandbox.assert_old_files();
    assert_eq!(operations.launches, [b"old application".to_vec()]);
    sandbox.assert_data_preserved_and_artifacts_cleaned();
}

#[test]
fn missing_payload_or_installer_never_stops_the_installed_app() {
    let sandbox = Sandbox::new();
    let mut operations = sandbox.operations();
    assert!(apply_update(&sandbox.layout, b"", &sandbox.installer, &mut operations).is_err());
    assert!(
        apply_update(
            &sandbox.layout,
            b"new application",
            &sandbox.root.join("missing.exe"),
            &mut operations
        )
        .is_err()
    );
    assert_eq!(operations.stops, 0);
    sandbox.assert_old_files();
    sandbox.assert_data_preserved_and_artifacts_cleaned();
}

#[cfg(windows)]
#[test]
fn a_locked_destination_leaves_old_bytes_and_returns_a_visible_failure() {
    use std::os::windows::fs::OpenOptionsExt;
    let sandbox = Sandbox::new();
    // Allow reads for staging, but deny FILE_SHARE_DELETE, forcing the actual
    // MoveFileEx replacement to fail. No installed executable is opened.
    let _locked = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&sandbox.layout.uninstaller)
        .unwrap();
    let mut operations = sandbox.operations();
    let started = Instant::now();
    let error = apply_update(
        &sandbox.layout,
        b"new application",
        &sandbox.installer,
        &mut operations,
    )
    .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(9));
    assert!(error.to_string().contains("升级未完成"));
    sandbox.assert_old_files();
    assert_eq!(operations.launches, [b"old application".to_vec()]);
    sandbox.assert_data_preserved_and_artifacts_cleaned();
}

#[cfg(windows)]
#[test]
fn hung_shutdown_helper_is_terminated_within_a_bounded_wait() {
    use std::{
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };
    let mut helper = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 30",
        ])
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    assert_eq!(
        wait_for_shutdown_helper(&mut helper, Duration::from_millis(100))
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(helper.try_wait().unwrap().is_some());
}

#[cfg(windows)]
#[test]
fn an_immediate_application_exit_is_not_reported_as_success() {
    use std::{
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };
    let mut application = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "exit 7"])
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(
        check_application_start(&mut application, Duration::from_secs(5))
            .unwrap_err()
            .to_string()
            .contains("立即退出")
    );
}

#[cfg(windows)]
#[test]
fn concurrent_installer_actions_are_bounded_and_the_lock_is_released_afterwards() {
    let name = format!(
        r"Local\DeskLinkInstallerTest-{}-{}",
        std::process::id(),
        ARTIFACT_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let guard = InstallerLock::acquire(&name, Duration::ZERO).unwrap();
    let contender_name = name.clone();
    let started = Instant::now();
    let blocked = thread::spawn(move || {
        InstallerLock::acquire(&contender_name, Duration::from_millis(100))
            .err()
            .unwrap()
            .kind()
    })
    .join()
    .unwrap();
    assert_eq!(blocked, io::ErrorKind::WouldBlock);
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(guard);
    thread::spawn(move || {
        let _next = InstallerLock::acquire(&name, Duration::ZERO).unwrap();
    })
    .join()
    .unwrap();
}
