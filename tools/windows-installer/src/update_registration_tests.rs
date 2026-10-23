use super::*;
use crate::update::{UpdateOperations, apply_update};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use windows::Win32::System::Registry::RegDeleteTreeW;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Sandbox {
    root: PathBuf,
    key: String,
    layout: InstallLayout,
    installer: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let id = format!(
            "{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let root = std::env::temp_dir().join(format!("desklink-installer-registry-test-{id}"));
        fs::create_dir(&root).unwrap();
        let layout = InstallLayout::from_user_roots(&root.join("local"), &root.join("roaming"));
        fs::create_dir_all(&layout.install_directory).unwrap();
        fs::create_dir_all(&layout.data_directory).unwrap();
        fs::write(&layout.application, b"old application").unwrap();
        fs::write(&layout.uninstaller, b"old installer").unwrap();
        fs::write(
            layout.data_directory.join("fixed-access.bin"),
            b"isolated fixed code sentinel",
        )
        .unwrap();
        let installer = root.join("downloaded.exe");
        fs::write(&installer, b"new installer").unwrap();
        let key = format!(r"Software\DeskLinkInstallerTests\{id}");
        Self {
            root,
            key,
            layout,
            installer,
        }
    }

    fn set(&self, name: &str, value: &str) {
        write_value(
            &self.key,
            name,
            Some(&RegistryValue {
                kind: REG_SZ,
                bytes: wide_bytes(value),
            }),
        )
        .unwrap();
    }

    fn register(&self) {
        self.set(
            "InstallLocation",
            &self.layout.install_directory.display().to_string(),
        );
        self.set(
            "DisplayIcon",
            &self.layout.application.display().to_string(),
        );
        self.set("UninstallString", &self.layout.uninstall_command());
        self.set("DisplayVersion", "0.1.90");
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        assert!(self.key.starts_with(r"Software\DeskLinkInstallerTests\"));
        let key = wide(&self.key);
        let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr())) };
        assert!(status == ERROR_FILE_NOT_FOUND || status.is_ok());
        let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = fs::canonicalize(&self.root).unwrap();
        assert_eq!(root.parent(), Some(temporary.as_path()));
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("desklink-installer-registry-test-")
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn update_requires_existing_matching_registered_paths_before_writing_anything() {
    let sandbox = Sandbox::new();
    assert!(InstalledRegistration::load(&sandbox.layout, &sandbox.key, "0.1.91").is_err());
    sandbox.register();
    assert!(InstalledRegistration::load(&sandbox.layout, &sandbox.key, "0.1.91").is_ok());
    for (name, wrong) in [
        ("InstallLocation", sandbox.root.display().to_string()),
        (
            "DisplayIcon",
            sandbox.layout.uninstaller.display().to_string(),
        ),
        ("UninstallString", "other.exe --uninstall".to_owned()),
        ("DisplayVersion", "unexpected version".to_owned()),
        ("DisplayVersion", "1.0.0".to_owned()),
    ] {
        sandbox.set(name, &wrong);
        assert!(
            InstalledRegistration::load(&sandbox.layout, &sandbox.key, "0.1.91").is_err(),
            "accepted mismatched {name}"
        );
        sandbox.register();
    }
    fs::remove_file(&sandbox.layout.uninstaller).unwrap();
    assert!(InstalledRegistration::load(&sandbox.layout, &sandbox.key, "0.1.91").is_err());
    assert_eq!(
        fs::read(&sandbox.layout.application).unwrap(),
        b"old application"
    );
}

#[test]
fn actual_registry_version_restore_preserves_missing_size_and_unrelated_values() {
    let sandbox = Sandbox::new();
    sandbox.register();
    sandbox.set("UnrelatedSentinel", "unchanged");
    let registration =
        InstalledRegistration::load(&sandbox.layout, &sandbox.key, "0.1.91").unwrap();
    registration.write_version("0.1.91", 4096).unwrap();
    assert_eq!(
        required_string(&sandbox.key, "DisplayVersion").unwrap(),
        "0.1.91"
    );
    assert!(read_value(&sandbox.key, "EstimatedSize").unwrap().is_some());
    registration.restore().unwrap();
    assert_eq!(
        required_string(&sandbox.key, "DisplayVersion").unwrap(),
        "0.1.90"
    );
    assert!(read_value(&sandbox.key, "EstimatedSize").unwrap().is_none());
    assert_eq!(
        required_string(&sandbox.key, "UnrelatedSentinel").unwrap(),
        "unchanged"
    );
}

struct RegistryOperations<'a> {
    sandbox: &'a Sandbox,
    registration: InstalledRegistration,
    launches: Vec<Vec<u8>>,
    fail_registration: bool,
}

impl UpdateOperations for RegistryOperations<'_> {
    fn stop_application(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn write_registration(&mut self) -> io::Result<()> {
        self.registration.write_version("0.1.91", 4096)?;
        if self.fail_registration {
            return Err(io::Error::other("injected failure after registry writes"));
        }
        Ok(())
    }
    fn restore_registration(&mut self) -> io::Result<()> {
        self.registration.restore()
    }
    fn start_application(&mut self) -> io::Result<()> {
        self.launches
            .push(fs::read(&self.sandbox.layout.application)?);
        Ok(())
    }
}

#[test]
fn real_file_transaction_and_isolated_registry_commit_or_rollback_together() {
    for (fail_registration, startup) in [
        (false, None),
        (false, Some("custom --startup")),
        (true, None),
        (true, Some("custom --startup")),
    ] {
        let sandbox = Sandbox::new();
        sandbox.register();
        let size = RegistryValue {
            kind: REG_DWORD,
            bytes: 37u32.to_le_bytes().to_vec(),
        };
        write_value(&sandbox.key, "EstimatedSize", Some(&size)).unwrap();
        let run_key = format!(r"{}\Run", sandbox.key);
        let original_startup = startup.map(|text| RegistryValue {
            kind: REG_SZ,
            bytes: wide_bytes(text),
        });
        if let Some(value) = &original_startup {
            write_value(&run_key, crate::PRODUCT_NAME, Some(value)).unwrap();
        }
        let registration =
            InstalledRegistration::load(&sandbox.layout, &sandbox.key, "0.1.91").unwrap();
        let mut operations = RegistryOperations {
            sandbox: &sandbox,
            registration,
            launches: vec![],
            fail_registration,
        };
        let result = apply_update(
            &sandbox.layout,
            b"new application",
            &sandbox.installer,
            &mut operations,
        );
        if fail_registration {
            assert!(result.is_err());
            assert_eq!(
                fs::read(&sandbox.layout.application).unwrap(),
                b"old application"
            );
            assert_eq!(
                fs::read(&sandbox.layout.uninstaller).unwrap(),
                b"old installer"
            );
            assert_eq!(
                required_string(&sandbox.key, "DisplayVersion").unwrap(),
                "0.1.90"
            );
            assert_eq!(
                read_value(&sandbox.key, "EstimatedSize").unwrap(),
                Some(size)
            );
            assert_eq!(operations.launches, [b"old application".to_vec()]);
        } else {
            result.unwrap();
            assert_eq!(
                fs::read(&sandbox.layout.application).unwrap(),
                b"new application"
            );
            assert_eq!(
                fs::read(&sandbox.layout.uninstaller).unwrap(),
                b"new installer"
            );
            assert_eq!(
                required_string(&sandbox.key, "DisplayVersion").unwrap(),
                "0.1.91"
            );
            assert_eq!(operations.launches, [b"new application".to_vec()]);
        }
        assert_eq!(
            read_value(&run_key, crate::PRODUCT_NAME).unwrap(),
            original_startup
        );
        assert_eq!(
            fs::read(sandbox.layout.data_directory.join("fixed-access.bin")).unwrap(),
            b"isolated fixed code sentinel"
        );
        assert_eq!(
            fs::read_dir(&sandbox.layout.install_directory)
                .unwrap()
                .count(),
            2
        );
    }
}
