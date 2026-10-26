//! Disposable disk lookup for a single immutable-prefix manifest spool.
//!
//! The canonical JSONL remains authoritative. This table is create-new, derived
//! from that spool, and discarded after the canonical replacement is published;
//! it is never reopened as recovery state. Memory stays constant at 48 bytes per
//! probe. Every hash match is checked against the complete path read from its
//! recorded spool offset, including collisions between distinct full keys.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

use blake2::{Blake2s256, Digest};
use desklink_protocol::{
    MAX_FILE_TASK_ENTRIES, MAX_TRANSFER_RELATIVE_PATH_BYTES, TransferResult,
    is_valid_transfer_relative_path,
};

const SLOT_BYTES: u64 = 48;
type PathHash = fn(&str) -> [u8; 32];

fn hash_path(path: &str) -> [u8; 32] {
    let mut hash = Blake2s256::new();
    hash.update(b"DeskLink manifest lookup v14\0");
    hash.update(path.as_bytes());
    hash.finalize().into()
}

fn io_result(_: std::io::Error) -> TransferResult {
    TransferResult::IoFailed
}

fn layout(max_entries: u64) -> Result<(u64, u64), TransferResult> {
    if max_entries == 0 || max_entries > MAX_FILE_TASK_ENTRIES {
        return Err(TransferResult::InvalidData);
    }
    slot_layout(max_entries)
}

fn slot_layout(max_entries: u64) -> Result<(u64, u64), TransferResult> {
    if max_entries == 0 || max_entries > MAX_FILE_TASK_ENTRIES * 2 {
        return Err(TransferResult::InvalidData);
    }
    let capacity = max_entries
        .checked_mul(2)
        .and_then(u64::checked_next_power_of_two)
        .ok_or(TransferResult::TooLarge)?;
    let bytes = capacity
        .checked_mul(SLOT_BYTES)
        .ok_or(TransferResult::TooLarge)?;
    Ok((capacity, bytes))
}

pub(crate) struct ManifestPathIndex {
    table: File,
    capacity: u64,
    max_entries: u64,
    entries: u64,
    hash: PathHash,
    max_item_id: u64,
    failure_keys: bool,
    #[cfg(not(windows))]
    path: std::path::PathBuf,
}

impl ManifestPathIndex {
    pub(crate) fn create(path: &Path, max_entries: u64) -> Result<Self, TransferResult> {
        Self::create_mode(path, max_entries, false)
    }

    /// Internal scope/prefix keys are bounded strings, not filesystem paths.
    /// At most N old failure keys and N new discovery outcomes can coexist.
    pub(crate) fn create_for_failures(
        path: &Path,
        max_entries: u64,
    ) -> Result<Self, TransferResult> {
        Self::create_mode(path, max_entries, true)
    }

    fn create_mode(
        path: &Path,
        canonical_maximum: u64,
        failure_keys: bool,
    ) -> Result<Self, TransferResult> {
        let (max_entries, capacity, bytes) = if failure_keys {
            if canonical_maximum == 0 || canonical_maximum > MAX_FILE_TASK_ENTRIES {
                return Err(TransferResult::InvalidData);
            }
            let maximum = canonical_maximum
                .checked_mul(2)
                .ok_or(TransferResult::TooLarge)?;
            let (capacity, bytes) = slot_layout(maximum)?;
            (maximum, capacity, bytes)
        } else {
            let (capacity, bytes) = layout(canonical_maximum)?;
            (canonical_maximum, capacity, bytes)
        };
        crate::transfer::ensure_receive_capacity(
            path.parent().ok_or(TransferResult::InvalidData)?,
            bytes,
        )?;
        #[cfg(windows)]
        let table = crate::transfer_directory::native::create_file_path(path)?;
        #[cfg(not(windows))]
        let table = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(io_result)?;
        if let Err(error) = table.set_len(bytes) {
            #[cfg(windows)]
            let _ = crate::transfer_directory::native::delete_file(&table);
            drop(table);
            #[cfg(not(windows))]
            let _ = std::fs::remove_file(path);
            return Err(io_result(error));
        }
        Ok(Self {
            table,
            capacity,
            max_entries,
            entries: 0,
            hash: hash_path,
            max_item_id: if failure_keys {
                max_entries
            } else {
                MAX_FILE_TASK_ENTRIES
            },
            failure_keys,
            #[cfg(not(windows))]
            path: path.to_owned(),
        })
    }

    /// The callback must read a bounded canonical record at `offset`, return its
    /// exact full relative path, and restore any shared spool cursor afterwards.
    pub(crate) fn find(
        &mut self,
        path: &str,
        mut read_path: impl FnMut(u64) -> Result<String, TransferResult>,
    ) -> Result<Option<(u64, u64)>, TransferResult> {
        let (_, existing) = self.probe(path, &mut read_path)?;
        Ok(existing)
    }

    /// Insert a success already written into the spool. Distinct canonical
    /// records may never claim the same full path; an exact repeat is idempotent.
    pub(crate) fn insert(
        &mut self,
        path: &str,
        item_id: u64,
        offset: u64,
        mut read_path: impl FnMut(u64) -> Result<String, TransferResult>,
    ) -> Result<(), TransferResult> {
        if item_id == 0 || item_id > self.max_item_id {
            return Err(TransferResult::InvalidData);
        }
        let (slot, existing) = self.probe(path, &mut read_path)?;
        if let Some(existing) = existing {
            return if existing == (item_id, offset) {
                Ok(())
            } else {
                Err(TransferResult::InvalidData)
            };
        }
        if self.entries >= self.max_entries {
            return Err(TransferResult::TooLarge);
        }
        let mut record = [0_u8; SLOT_BYTES as usize];
        record[..32].copy_from_slice(&(self.hash)(path));
        record[32..40].copy_from_slice(&item_id.to_le_bytes());
        record[40..48].copy_from_slice(&offset.to_le_bytes());
        self.table
            .seek(SeekFrom::Start(slot * SLOT_BYTES))
            .map_err(io_result)?;
        self.table.write_all(&record).map_err(io_result)?;
        self.entries += 1;
        Ok(())
    }

    fn probe(
        &mut self,
        path: &str,
        read_path: &mut impl FnMut(u64) -> Result<String, TransferResult>,
    ) -> Result<(u64, Option<(u64, u64)>), TransferResult> {
        if !self.valid_key(path) {
            return Err(TransferResult::InvalidData);
        }
        let hash = (self.hash)(path);
        let start = u64::from_le_bytes(hash[..8].try_into().unwrap()) & (self.capacity - 1);
        for distance in 0..self.capacity {
            let slot = (start + distance) & (self.capacity - 1);
            let mut record = [0_u8; SLOT_BYTES as usize];
            self.table
                .seek(SeekFrom::Start(slot * SLOT_BYTES))
                .map_err(io_result)?;
            self.table.read_exact(&mut record).map_err(io_result)?;
            let item = u64::from_le_bytes(record[32..40].try_into().unwrap());
            if item == 0 {
                if record != [0; SLOT_BYTES as usize] {
                    return Err(TransferResult::InvalidData);
                }
                return Ok((slot, None));
            }
            if item > self.max_item_id {
                return Err(TransferResult::InvalidData);
            }
            if record[..32] == hash {
                let offset = u64::from_le_bytes(record[40..48].try_into().unwrap());
                let actual = read_path(offset)?;
                if !self.valid_key(&actual) {
                    return Err(TransferResult::InvalidData);
                }
                if (self.hash)(&actual).as_slice() != &record[..32] {
                    return Err(TransferResult::InvalidData);
                }
                if actual == path {
                    return Ok((slot, Some((item, offset))));
                }
            }
        }
        Err(TransferResult::TooLarge)
    }

    fn valid_key(&self, key: &str) -> bool {
        if self.failure_keys {
            !key.is_empty() && key.len() <= MAX_TRANSFER_RELATIVE_PATH_BYTES + 32
        } else {
            is_valid_transfer_relative_path(key)
        }
    }
}

impl Drop for ManifestPathIndex {
    fn drop(&mut self) {
        // Delete the create-new owned object while its exclusive handle lives,
        // never a path another worker might replace after this table closes.
        #[cfg(windows)]
        let _ = crate::transfer_directory::native::delete_file(&self.table);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if let (Ok(owned), Ok(current)) =
                (self.table.metadata(), std::fs::symlink_metadata(&self.path))
                && current.is_file()
                && !current.file_type().is_symlink()
                && (owned.dev(), owned.ino()) == (current.dev(), current.ino())
            {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture {
        path: PathBuf,
        parent: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = parent.join(format!(
                "desklink-manifest-lookup-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self { path, parent }
        }
        fn path(&self) -> PathBuf {
            self.path.join("lookup.derived")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let Ok(metadata) = fs::symlink_metadata(&self.path) else {
                return;
            };
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return;
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes()
                    & windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT.0
                    != 0
                {
                    return;
                }
            }
            if let Ok(resolved) = fs::canonicalize(&self.path)
                && resolved.parent() == Some(self.parent.as_path())
                && resolved
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("desklink-manifest-lookup-"))
            {
                let _ = fs::remove_dir_all(resolved);
            }
        }
    }

    #[test]
    fn collisions_compare_complete_paths_and_preserve_u64_offsets_and_ids() {
        let fixture = Fixture::new();
        let mut index = ManifestPathIndex::create(&fixture.path(), 3).unwrap();
        index.hash = |_| [0x7a; 32];
        let first_offset = (1_u64 << 32) + 123;
        let paths = BTreeMap::from([
            (first_offset, "报告/第一份.txt".to_owned()),
            (first_offset + 500, "报告/第二份.txt".to_owned()),
            (first_offset + 900, "报告/第三份.txt".to_owned()),
        ]);
        let read = |offset| {
            paths
                .get(&offset)
                .cloned()
                .ok_or(TransferResult::InvalidData)
        };
        for (number, (offset, path)) in paths.iter().enumerate() {
            index
                .insert(path, 900_000 + number as u64, *offset, read)
                .unwrap();
        }
        for (number, (offset, path)) in paths.iter().enumerate() {
            assert_eq!(
                index.find(path, read).unwrap(),
                Some((900_000 + number as u64, *offset))
            );
        }
        assert_eq!(index.find("报告/缺失.txt", read).unwrap(), None);
        assert_eq!(
            index.insert("报告/第一份.txt", 900_000, first_offset, read),
            Ok(())
        );
        assert_eq!(
            index.insert("报告/第一份.txt", 1, first_offset, read),
            Err(TransferResult::InvalidData)
        );
        assert_eq!(
            index.insert("第四份.txt", 4, 10, read),
            Err(TransferResult::TooLarge)
        );
    }

    #[test]
    fn corrupt_or_unreadable_canonical_record_fails_closed() {
        let fixture = Fixture::new();
        let mut index = ManifestPathIndex::create(&fixture.path(), 2).unwrap();
        index.insert("safe.txt", 1, 42, |_| unreachable!()).unwrap();
        assert_eq!(
            index.find("safe.txt", |_| Err(TransferResult::IoFailed)),
            Err(TransferResult::IoFailed)
        );
        assert_eq!(
            index.find("safe.txt", |_| Ok("../outside".to_owned())),
            Err(TransferResult::InvalidData)
        );
        assert_eq!(
            index.find("../outside", |_| unreachable!()),
            Err(TransferResult::InvalidData)
        );
        let slot = u64::from_le_bytes(hash_path("safe.txt")[..8].try_into().unwrap())
            & (index.capacity - 1);
        index
            .table
            .seek(SeekFrom::Start(slot * SLOT_BYTES + 32))
            .unwrap();
        index.table.write_all(&0_u64.to_le_bytes()).unwrap();
        assert_eq!(
            index.find("safe.txt", |_| unreachable!()),
            Err(TransferResult::InvalidData)
        );
    }

    #[test]
    fn bounded_capacity_budget_and_create_new_do_not_replace_existing_file() {
        assert_eq!(
            layout(MAX_FILE_TASK_ENTRIES).unwrap(),
            (2_097_152, 100_663_296)
        );
        assert_eq!(layout(0), Err(TransferResult::InvalidData));
        assert_eq!(
            layout(MAX_FILE_TASK_ENTRIES + 1),
            Err(TransferResult::InvalidData)
        );
        let fixture = Fixture::new();
        fs::write(fixture.path(), b"existing-owned-evidence").unwrap();
        assert!(ManifestPathIndex::create(&fixture.path(), 1).is_err());
        assert_eq!(
            fs::read(fixture.path()).unwrap(),
            b"existing-owned-evidence"
        );
    }

    #[test]
    fn derived_table_drop_cleans_success_and_error_paths_only() {
        let fixture = Fixture::new();
        let manifest = fixture.path.join("canonical.jsonl");
        fs::write(&manifest, b"canonical success and failure records\n").unwrap();
        {
            let mut table = ManifestPathIndex::create(&fixture.path(), 2).unwrap();
            table.insert("data.txt", 1, 0, |_| unreachable!()).unwrap();
            assert_eq!(
                table.find("data.txt", |_| Err(TransferResult::InvalidData)),
                Err(TransferResult::InvalidData)
            );
            #[cfg(windows)]
            assert!(
                crate::transfer_directory::native::delete_path(&fixture.path()).is_err(),
                "another owner must not replace a live lookup table"
            );
        }
        assert!(!fixture.path().exists());
        assert_eq!(
            fs::read(manifest).unwrap(),
            b"canonical success and failure records\n"
        );
    }

    #[test]
    fn failure_keys_compare_full_scope_and_prefix_under_forced_hash_collisions() {
        let fixture = Fixture::new();
        let mut index = ManifestPathIndex::create_for_failures(&fixture.path(), 2).unwrap();
        index.hash = |_| [0x3b; 32];
        let keys = BTreeMap::from([
            (11, "Entry\0same/path".to_owned()),
            (22, "Subtree\0same/path".to_owned()),
            (33, "Enumeration\0".to_owned()),
            (44, "Task\0".to_owned()),
        ]);
        let read = |offset| {
            keys.get(&offset)
                .cloned()
                .ok_or(TransferResult::InvalidData)
        };
        for (number, (offset, key)) in keys.iter().enumerate() {
            index.insert(key, number as u64 + 1, *offset, read).unwrap();
        }
        for (number, (offset, key)) in keys.iter().enumerate() {
            assert_eq!(
                index.find(key, read).unwrap(),
                Some((number as u64 + 1, *offset))
            );
        }
        assert_eq!(index.find("Entry\0other/path", read).unwrap(), None);
        assert_eq!(
            index.insert("Entry\0fifth", 5, 55, read),
            Err(TransferResult::InvalidData)
        );
        assert_eq!(
            slot_layout(MAX_FILE_TASK_ENTRIES * 2).unwrap(),
            (4_194_304, 201_326_592)
        );
        assert_eq!(
            slot_layout(MAX_FILE_TASK_ENTRIES * 2 + 1),
            Err(TransferResult::InvalidData)
        );
    }

    #[test]
    fn corrupt_full_key_reference_is_rejected_instead_of_hiding_existing_failure() {
        let fixture = Fixture::new();
        let mut index = ManifestPathIndex::create_for_failures(&fixture.path(), 1).unwrap();
        index.insert("Entry\0a", 1, 17, |_| unreachable!()).unwrap();
        assert_eq!(
            index.find("Entry\0a", |_| Ok("Subtree\0a".into())),
            Err(TransferResult::InvalidData)
        );
    }
}
