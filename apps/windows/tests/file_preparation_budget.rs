#![cfg(windows)]
use apps_windows::{
    transfer::{prepare_outgoing_file, prepare_outgoing_file_with_budget},
    transfer_recovery::{TransferQueueRecovery, WindowsFileQueueRecoveryStore},
};
use desklink_protocol::{MAX_LARGE_FILE_BYTES, TransferResult};
use std::{
    fs::{self, File},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf, PathBuf);
impl Fixture {
    fn new() -> Self {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = parent.join(format!(
            "desklink-large-prepare-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root, parent)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(root) = fs::canonicalize(&self.0)
            && root.parent() == Some(self.1.as_path())
            && root
                .file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("desklink-large-prepare-"))
        {
            fs::remove_dir_all(root).unwrap();
        }
    }
}
fn sparse(file: &File) {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{
        Foundation::HANDLE,
        System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_SPARSE},
    };
    let mut returned = 0;
    unsafe {
        DeviceIoControl(
            HANDLE(file.as_raw_handle()),
            FSCTL_SET_SPARSE,
            None,
            0,
            None,
            0,
            Some(&mut returned),
            None,
        )
    }
    .unwrap();
}

#[test]
fn negotiated_preparation_and_dpapi_waiting_queue_keep_a_real_file_above_four_gib() {
    let f = Fixture::new();
    let path = f.0.join("大文件.bin");
    let file = File::create(&path).unwrap();
    sparse(&file);
    let size = 4 * 1024 * 1024 * 1024 + 123_u64;
    file.set_len(size).unwrap();
    drop(file);
    assert_eq!(
        prepare_outgoing_file(path.clone()).unwrap_err(),
        TransferResult::TooLarge
    );
    let outgoing = prepare_outgoing_file_with_budget(path.clone(), MAX_LARGE_FILE_BYTES).unwrap();
    assert_eq!(outgoing.size, size);
    assert_eq!(outgoing.path, path);
    let store = WindowsFileQueueRecoveryStore::new(f.0.join("queue.bin"));
    store
        .save(&TransferQueueRecovery::new([7; 16], [&outgoing], true))
        .unwrap();
    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.files()[0].outgoing_file().size, size);
    assert_eq!(fs::metadata(&path).unwrap().len(), size);
    assert!(prepare_outgoing_file_with_budget(path, size - 1).is_err());
}

#[test]
fn negotiated_windows_names_keep_utf16_limit_and_reject_invalid_budgets() {
    let f = Fixture::new();
    let path = f.0.join(format!("{}.txt", "汉".repeat(200)));
    fs::write(&path, b"original").unwrap();
    assert_eq!(
        prepare_outgoing_file(path.clone()).unwrap_err(),
        TransferResult::InvalidData
    );
    let outgoing = prepare_outgoing_file_with_budget(path.clone(), MAX_LARGE_FILE_BYTES).unwrap();
    assert_eq!(outgoing.name.encode_utf16().count(), 204);
    assert_eq!(fs::read(&path).unwrap(), b"original");
    assert_eq!(
        prepare_outgoing_file_with_budget(path.clone(), 0).unwrap_err(),
        TransferResult::InvalidData
    );
    assert_eq!(
        prepare_outgoing_file_with_budget(path, MAX_LARGE_FILE_BYTES + 1).unwrap_err(),
        TransferResult::InvalidData
    );
}
