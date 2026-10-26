//! A bounded source manifest. Durable successes are a stable prefix; scan
//! failures are a replaceable tail and never create destination objects.
use super::{
    DirectoryEntry, DirectoryScanFailure, DirectoryScanFailureScope, File, ManifestEntry, Path,
    TransferEntryKind, TransferResult, cache_create_new, cache_remove, io_result, path_exists,
    read_line, write_line,
};
use serde::{Deserialize, Serialize};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestFailure {
    scan_failure: bool,
    pub item_id: u64,
    pub relative_path: String,
    pub failure: DirectoryScanFailure,
}

impl ManifestFailure {
    pub fn new(item_id: u64, failure: DirectoryScanFailure) -> Self {
        let relative_path = failure
            .relative_path
            .clone()
            .unwrap_or_else(|| format!("DeskLink-scan-error-{item_id}"));
        Self {
            scan_failure: true,
            item_id,
            relative_path,
            failure,
        }
    }

    pub fn validate(&self) -> Result<(), TransferResult> {
        let retry_valid = self
            .failure
            .retry_relative_path
            .as_deref()
            .is_none_or(|path| {
                path.is_empty() || desklink_protocol::is_valid_transfer_relative_path(path)
            });
        let scope_valid = match self.failure.scope {
            DirectoryScanFailureScope::Entry | DirectoryScanFailureScope::Subtree => {
                self.failure
                    .retry_relative_path
                    .as_deref()
                    .is_some_and(|path| {
                        !path.is_empty() && self.failure.relative_path.as_deref() == Some(path)
                    })
                    && !self.failure.terminal
                    && (self.failure.scope != DirectoryScanFailureScope::Subtree
                        || self.failure.kind == Some(TransferEntryKind::Directory))
            }
            DirectoryScanFailureScope::Enumeration => {
                self.failure
                    .retry_relative_path
                    .as_deref()
                    .is_some_and(|prefix| {
                        if prefix.is_empty() {
                            self.failure.relative_path.is_none()
                        } else {
                            self.failure.relative_path.as_deref() == Some(prefix)
                        }
                    })
                    && !self.failure.terminal
                    && self.failure.kind == Some(TransferEntryKind::Directory)
            }
            DirectoryScanFailureScope::Task => {
                self.failure.terminal
                    && self.failure.retry_relative_path.is_none()
                    && self.failure.relative_path.is_none()
                    && self.failure.kind.is_none()
                    && self.failure.size.is_none()
            }
        };
        if !self.scan_failure
            || self.item_id == 0
            || self.item_id > desklink_protocol::MAX_FILE_TASK_ENTRIES
            || !desklink_protocol::is_valid_transfer_relative_path(&self.relative_path)
            || self.failure.result == TransferResult::Completed
            || self.failure.result == TransferResult::Cancelled
            || !retry_valid
            || !scope_valid
            || self
                .failure
                .relative_path
                .as_ref()
                .is_some_and(|path| path != &self.relative_path)
            || (self.failure.kind.is_none() && self.failure.size.is_some())
            || (self.failure.kind == Some(TransferEntryKind::Directory)
                && self.failure.size != Some(0))
        {
            return Err(TransferResult::InvalidData);
        }
        Ok(())
    }

    pub fn manifest(&self) -> ManifestEntry {
        ManifestEntry {
            item_id: self.item_id,
            relative_path: self.relative_path.clone(),
            kind: self.failure.kind.unwrap_or(TransferEntryKind::File),
            size: 0,
            content_root: None,
        }
    }
}

// Keep the original entry representation readable after upgrading an existing
// task. Required entry fields and the explicit failure marker disambiguate it.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub(super) enum ManifestRecord {
    Entry(DirectoryEntry),
    Failure(ManifestFailure),
}
impl<'de> Deserialize<'de> for ManifestRecord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("scan_failure").is_some() {
            serde_json::from_value(value)
                .map(Self::Failure)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Self::Entry)
                .map_err(serde::de::Error::custom)
        }
    }
}
impl ManifestRecord {
    pub fn item_id(&self) -> u64 {
        match self {
            Self::Entry(e) => e.item_id,
            Self::Failure(f) => f.item_id,
        }
    }
    pub fn size(&self) -> u64 {
        match self {
            Self::Entry(e) => e.size,
            Self::Failure(_) => 0,
        }
    }
    pub fn validate(&self, root: &Path, directory: bool) -> Result<(), TransferResult> {
        let root = if root.is_absolute() {
            root.to_owned()
        } else {
            std::env::current_dir().map_err(io_result)?.join(root)
        };
        match self {
            Self::Failure(f) if directory => f.validate(),
            Self::Failure(_) => Err(TransferResult::InvalidData),
            Self::Entry(e) => {
                let expected = if directory {
                    e.relative_path
                        .split('/')
                        .fold(root.clone(), |p, segment| p.join(segment))
                } else {
                    root.to_owned()
                };
                if e.item_id == 0
                    || e.source_path != expected
                    || !desklink_protocol::is_valid_transfer_relative_path(&e.relative_path)
                    || (e.kind == TransferEntryKind::Directory && e.size != 0)
                {
                    Err(TransferResult::InvalidData)
                } else {
                    Ok(())
                }
            }
        }
    }
}

pub(super) struct ManifestSpool {
    output: Option<File>,
    output_path: std::path::PathBuf,
    failures: File,
    failures_path: std::path::PathBuf,
    states: Option<File>,
    states_path: std::path::PathBuf,
    failure_index: Option<super::manifest_index::ManifestPathIndex>,
    failure_index_path: std::path::PathBuf,
    canonical_limit: u64,
    physical_slots: u64,
    pub successes: u64,
    pub failures_count: u64,
    pub total_bytes: u64,
}
impl ManifestSpool {
    pub fn create(
        output_path: &Path,
        failures_path: &Path,
        maximum: u64,
    ) -> Result<Self, TransferResult> {
        if maximum == 0 || maximum > desklink_protocol::MAX_FILE_TASK_ENTRIES {
            return Err(TransferResult::InvalidData);
        }
        for path in [output_path, failures_path] {
            if path_exists(path)? {
                cache_remove(path)?;
            }
        }
        let output = cache_create_new(output_path)?;
        let failures = match cache_create_new(failures_path) {
            Ok(file) => file,
            Err(error) => {
                remove_owned_temporary(&output, output_path);
                return Err(error);
            }
        };
        Ok(Self {
            output: Some(output),
            output_path: output_path.to_owned(),
            failures,
            failures_path: failures_path.to_owned(),
            states: None,
            states_path: failures_path.with_extension("states.pending"),
            failure_index: None,
            failure_index_path: failures_path.with_extension("keys.pending"),
            canonical_limit: maximum,
            physical_slots: 0,
            successes: 0,
            failures_count: 0,
            total_bytes: 0,
        })
    }

    pub fn entry(&mut self, mut entry: DirectoryEntry) -> Result<u64, TransferResult> {
        if self.count() >= self.canonical_limit {
            return Err(TransferResult::TooLarge);
        }
        let id = self
            .successes
            .checked_add(1)
            .ok_or(TransferResult::TooLarge)?;
        entry.item_id = id;
        let output = self.output.as_mut().ok_or(TransferResult::InvalidData)?;
        let offset = output.stream_position().map_err(io_result)?;
        write_line(output, &ManifestRecord::Entry(entry.clone()))?;
        self.successes = id;
        self.total_bytes = self
            .total_bytes
            .checked_add(entry.size)
            .ok_or(TransferResult::TooLarge)?;
        Ok(offset)
    }

    pub fn failure(&mut self, failure: DirectoryScanFailure) -> Result<u64, TransferResult> {
        let record = ManifestFailure::new(1, failure);
        record.validate()?;
        self.ensure_failure_index()?;
        if let Some((slot, _)) = self.find_failure(&record.failure)? {
            let old = self.state(slot)?;
            if old & ACTIVE == 0 && self.count() >= self.canonical_limit {
                return Err(TransferResult::TooLarge);
            }
            let state = old
                | ACTIVE
                | if record.failure.result == TransferResult::SourceChanged {
                    SOURCE_CHANGED
                } else {
                    0
                };
            self.set_state(slot, state)?;
            if old & ACTIVE == 0 {
                self.failures_count += 1;
            }
            return Ok(slot);
        }
        if self.count() >= self.canonical_limit || self.physical_slots >= self.canonical_limit * 2 {
            return Err(TransferResult::TooLarge);
        }
        let slot = self.physical_slots + 1;
        let offset = self.failures.stream_position().map_err(io_result)?;
        write_line(&mut self.failures, &record)?;
        let key = failure_key(&record.failure);
        let failures = &mut self.failures;
        self.failure_index
            .as_mut()
            .ok_or(TransferResult::InvalidData)?
            .insert(&key, slot, offset, |at| {
                read_failure_at(failures, at).map(|record| failure_key(&record.failure))
            })?;
        self.physical_slots = slot;
        self.set_state(slot, ACTIVE)?;
        self.failures_count += 1;
        Ok(slot)
    }

    fn ensure_failure_index(&mut self) -> Result<(), TransferResult> {
        if self.failure_index.is_some() {
            return Ok(());
        }
        for path in [&self.states_path, &self.failure_index_path] {
            if path_exists(path)? {
                cache_remove(path)?;
            }
        }
        let index = super::manifest_index::ManifestPathIndex::create_for_failures(
            &self.failure_index_path,
            self.canonical_limit,
        )?;
        let state = cache_create_new(&self.states_path)?;
        if let Err(error) = state.set_len(self.canonical_limit * 2) {
            remove_owned_temporary(&state, &self.states_path);
            return Err(io_result(error));
        }
        self.states = Some(state);
        self.failure_index = Some(index);
        Ok(())
    }

    fn find_failure(
        &mut self,
        failure: &DirectoryScanFailure,
    ) -> Result<Option<(u64, u64)>, TransferResult> {
        let Some(index) = self.failure_index.as_mut() else {
            return Ok(None);
        };
        let failures = &mut self.failures;
        let found = index.find(&failure_key(failure), |at| {
            read_failure_at(failures, at).map(|record| failure_key(&record.failure))
        })?;
        if found.is_some_and(|(slot, _)| slot == 0 || slot > self.physical_slots) {
            return Err(TransferResult::InvalidData);
        }
        Ok(found)
    }

    pub fn find_failure_slot(
        &mut self,
        failure: &DirectoryScanFailure,
    ) -> Result<Option<u64>, TransferResult> {
        ManifestFailure::new(1, failure.clone()).validate()?;
        self.find_failure(failure)
            .map(|found| found.map(|(slot, _)| slot))
    }

    fn state(&mut self, slot: u64) -> Result<u8, TransferResult> {
        read_state(
            self.states.as_mut().ok_or(TransferResult::InvalidData)?,
            slot,
            self.physical_slots,
        )
    }

    fn set_state(&mut self, slot: u64, state: u8) -> Result<(), TransferResult> {
        if slot == 0 || slot > self.physical_slots || state & !(ACTIVE | SOURCE_CHANGED) != 0 {
            return Err(TransferResult::InvalidData);
        }
        let file = self.states.as_mut().ok_or(TransferResult::InvalidData)?;
        file.seek(SeekFrom::Start(slot - 1)).map_err(io_result)?;
        file.write_all(&[state]).map_err(io_result)
    }

    pub fn failure_active(&mut self, slot: u64) -> Result<bool, TransferResult> {
        self.state(slot).map(|state| state & ACTIVE != 0)
    }

    pub fn deactivate_failure(&mut self, slot: u64) -> Result<(), TransferResult> {
        let state = self.state(slot)?;
        if state & ACTIVE != 0 {
            self.set_state(slot, state & !ACTIVE)?;
            self.failures_count = self
                .failures_count
                .checked_sub(1)
                .ok_or(TransferResult::InvalidData)?;
        }
        Ok(())
    }

    pub fn resolve_entry_failures(
        &mut self,
        path: &str,
        kind: TransferEntryKind,
    ) -> Result<bool, TransferResult> {
        if !desklink_protocol::is_valid_transfer_relative_path(path) {
            return Err(TransferResult::InvalidData);
        }
        let mut compatible = true;
        for scope in [
            DirectoryScanFailureScope::Entry,
            DirectoryScanFailureScope::Subtree,
        ] {
            let key = format!("{scope:?}\0{path}");
            let found = if let Some(index) = self.failure_index.as_mut() {
                let failures = &mut self.failures;
                index.find(&key, |at| {
                    read_failure_at(failures, at).map(|record| failure_key(&record.failure))
                })?
            } else {
                None
            };
            if let Some((slot, offset)) = found {
                let state = self.state(slot)?;
                if state & ACTIVE == 0 {
                    continue;
                }
                let record = read_failure_at(&mut self.failures, offset)?;
                if record.failure.kind.is_some_and(|expected| expected != kind) {
                    self.set_state(slot, state | SOURCE_CHANGED)?;
                    compatible = false;
                } else {
                    self.deactivate_failure(slot)?;
                }
            }
        }
        Ok(compatible)
    }

    pub fn count(&self) -> u64 {
        self.successes + self.failures_count
    }

    pub fn read_path(&mut self, offset: u64) -> Result<String, TransferResult> {
        self.read_entry(offset).map(|entry| entry.relative_path)
    }

    pub fn read_entry(&mut self, offset: u64) -> Result<DirectoryEntry, TransferResult> {
        let output = self.output.as_mut().ok_or(TransferResult::InvalidData)?;
        let saved = output.stream_position().map_err(io_result)?;
        let result = (|| {
            output.seek(SeekFrom::Start(offset)).map_err(io_result)?;
            match read_line::<ManifestRecord>(&mut BufReader::new(&mut *output))? {
                Some(ManifestRecord::Entry(entry)) => Ok(entry),
                _ => Err(TransferResult::InvalidData),
            }
        })();
        output.seek(SeekFrom::Start(saved)).map_err(io_result)?;
        result
    }

    pub fn finish(
        mut self,
        mut work: impl FnMut() -> Result<(), TransferResult>,
    ) -> Result<(File, u64, u64), TransferResult> {
        self.failures.flush().map_err(io_result)?;
        self.failures.seek(SeekFrom::Start(0)).map_err(io_result)?;
        let mut reader = BufReader::new(&mut self.failures);
        let mut id = self.successes;
        let mut active_count = 0;
        for slot in 1..=self.physical_slots {
            let mut failure =
                read_line::<ManifestFailure>(&mut reader)?.ok_or(TransferResult::InvalidData)?;
            failure.validate()?;
            let state = read_state(
                self.states.as_mut().ok_or(TransferResult::InvalidData)?,
                slot,
                self.physical_slots,
            )?;
            work()?;
            if state & ACTIVE == 0 {
                continue;
            }
            if state & SOURCE_CHANGED != 0 {
                failure.failure.result = TransferResult::SourceChanged;
            }
            id += 1;
            if id > self.canonical_limit {
                return Err(TransferResult::TooLarge);
            }
            active_count += 1;
            // Recompute synthetic display identities when the failure tail moves.
            failure = ManifestFailure::new(id, failure.failure);
            failure.validate()?;
            write_line(
                self.output.as_mut().ok_or(TransferResult::InvalidData)?,
                &ManifestRecord::Failure(failure),
            )?;
        }
        if active_count != self.failures_count
            || read_line::<ManifestFailure>(&mut reader)?.is_some()
        {
            return Err(TransferResult::InvalidData);
        }
        self.output
            .as_mut()
            .ok_or(TransferResult::InvalidData)?
            .sync_all()
            .map_err(io_result)?;
        drop(reader);
        Ok((
            self.output.take().ok_or(TransferResult::InvalidData)?,
            id,
            self.total_bytes,
        ))
    }
}

const ACTIVE: u8 = 1;
const SOURCE_CHANGED: u8 = 2;

fn failure_key(failure: &DirectoryScanFailure) -> String {
    format!(
        "{:?}\0{}",
        failure.scope,
        failure.retry_relative_path.as_deref().unwrap_or("")
    )
}

fn read_failure_at(file: &mut File, offset: u64) -> Result<ManifestFailure, TransferResult> {
    let saved = file.stream_position().map_err(io_result)?;
    let result = (|| {
        file.seek(SeekFrom::Start(offset)).map_err(io_result)?;
        let record = read_line::<ManifestFailure>(&mut BufReader::new(&mut *file))?
            .ok_or(TransferResult::InvalidData)?;
        record.validate()?;
        Ok(record)
    })();
    file.seek(SeekFrom::Start(saved)).map_err(io_result)?;
    result
}

fn read_state(file: &mut File, slot: u64, slots: u64) -> Result<u8, TransferResult> {
    if slot == 0 || slot > slots {
        return Err(TransferResult::InvalidData);
    }
    file.seek(SeekFrom::Start(slot - 1)).map_err(io_result)?;
    let mut state = [0_u8];
    file.read_exact(&mut state).map_err(io_result)?;
    if state[0] & !(ACTIVE | SOURCE_CHANGED) != 0 {
        return Err(TransferResult::InvalidData);
    }
    Ok(state[0])
}

fn remove_owned_temporary(file: &File, path: &Path) {
    #[cfg(windows)]
    {
        let _ = path;
        let _ = crate::transfer_directory::native::delete_file(file);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(owned), Ok(current)) = (file.metadata(), std::fs::symlink_metadata(path))
            && current.is_file()
            && !current.file_type().is_symlink()
            && (owned.dev(), owned.ino()) == (current.dev(), current.ino())
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for ManifestSpool {
    fn drop(&mut self) {
        if let Some(file) = self.output.as_ref() {
            remove_owned_temporary(file, &self.output_path);
        }
        remove_owned_temporary(&self.failures, &self.failures_path);
        if let Some(file) = self.states.as_ref() {
            remove_owned_temporary(file, &self.states_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
        parent: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
            let root = parent.join(format!(
                "desklink-manifest-spool-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self { root, parent }
        }
        fn file(&self, relative: &str, bytes: &[u8]) -> PathBuf {
            let path = self.root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let Ok(metadata) = fs::symlink_metadata(&self.root) else {
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
            if let Ok(resolved) = fs::canonicalize(&self.root)
                && resolved.parent() == Some(self.parent.as_path())
                && resolved
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("desklink-manifest-spool-"))
            {
                let _ = fs::remove_dir_all(resolved);
            }
        }
    }

    fn entry_failure(path: &str) -> DirectoryScanFailure {
        DirectoryScanFailure {
            relative_path: Some(path.into()),
            result: TransferResult::PermissionDenied,
            scope: DirectoryScanFailureScope::Entry,
            kind: Some(TransferEntryKind::File),
            size: Some((1_u64 << 32) + 17),
            terminal: false,
            retry_relative_path: Some(path.into()),
        }
    }

    #[test]
    fn scope_path_kind_and_terminal_invariants_reject_malformed_cache_records() {
        let base = ManifestFailure::new(2, entry_failure("blocked.bin"));
        base.validate().unwrap();
        let mut invalid = Vec::new();
        let mut changed = base.clone();
        changed.scan_failure = false;
        invalid.push(changed);
        let mut changed = base.clone();
        changed.item_id = 0;
        invalid.push(changed);
        let mut changed = base.clone();
        changed.item_id = desklink_protocol::MAX_FILE_TASK_ENTRIES + 1;
        invalid.push(changed);
        let mut changed = base.clone();
        changed.relative_path = "../outside".into();
        invalid.push(changed);
        let mut changed = base.clone();
        changed.failure.retry_relative_path = Some("C:/outside".into());
        invalid.push(changed);
        let mut changed = base.clone();
        changed.failure.retry_relative_path = Some("other.bin".into());
        invalid.push(changed);
        let mut changed = base.clone();
        changed.failure.terminal = true;
        invalid.push(changed);
        let mut changed = base.clone();
        changed.failure.kind = None;
        invalid.push(changed);
        let mut changed = base.clone();
        changed.failure.kind = Some(TransferEntryKind::Directory);
        invalid.push(changed);
        let mut changed = base.clone();
        changed.failure.scope = DirectoryScanFailureScope::Subtree;
        invalid.push(changed);
        let mut changed = base.clone();
        changed.failure.scope = DirectoryScanFailureScope::Enumeration;
        invalid.push(changed);
        let mut changed = base.clone();
        changed.failure.scope = DirectoryScanFailureScope::Task;
        invalid.push(changed);
        let mut changed = base.clone();
        changed.failure.result = TransferResult::Completed;
        invalid.push(changed);
        let mut changed = base.clone();
        changed.failure.result = TransferResult::Cancelled;
        invalid.push(changed);
        for failure in invalid {
            assert_eq!(
                failure.validate(),
                Err(TransferResult::InvalidData),
                "accepted malformed failure: {failure:?}"
            );
        }
        let root_enumeration = DirectoryScanFailure {
            relative_path: None,
            result: TransferResult::IoFailed,
            scope: DirectoryScanFailureScope::Enumeration,
            kind: Some(TransferEntryKind::Directory),
            size: Some(0),
            terminal: false,
            retry_relative_path: Some(String::new()),
        };
        ManifestFailure::new(3, root_enumeration)
            .validate()
            .unwrap();
        let task = DirectoryScanFailure {
            relative_path: None,
            result: TransferResult::TooLarge,
            scope: DirectoryScanFailureScope::Task,
            kind: None,
            size: None,
            terminal: true,
            retry_relative_path: None,
        };
        ManifestFailure::new(4, task).validate().unwrap();
    }

    #[test]
    fn explicit_failure_marker_cannot_be_reinterpreted_as_a_success_entry() {
        let fixture = Fixture::new();
        let entry = DirectoryEntry::single_file(&fixture.file("blocked.bin", b"source")).unwrap();
        let mut hybrid =
            serde_json::to_value(ManifestFailure::new(1, entry_failure("blocked.bin"))).unwrap();
        for (key, value) in serde_json::to_value(entry).unwrap().as_object().unwrap() {
            hybrid
                .as_object_mut()
                .unwrap()
                .insert(key.clone(), value.clone());
        }
        match serde_json::from_value::<ManifestRecord>(hybrid) {
            Ok(ManifestRecord::Failure(failure)) => failure.validate().unwrap(),
            Err(_) => {}
            Ok(ManifestRecord::Entry(_)) => {
                panic!("an explicit failure marker became a successful entry")
            }
        }
    }

    #[test]
    fn raw_entry_compatibility_and_large_metadata_preserve_u64_without_admitting_failed_bytes() {
        let fixture = Fixture::new();
        let source = fixture.file("source.bin", b"source");
        let entry = DirectoryEntry::single_file(&source).unwrap();
        let raw = serde_json::to_value(&entry).unwrap();
        let record: ManifestRecord = serde_json::from_value(raw.clone()).unwrap();
        assert!(matches!(record, ManifestRecord::Entry(_)));
        record.validate(&source, false).unwrap();
        let mut large = raw;
        let boundary = (1_u64 << 32) + 17;
        large["size"] = boundary.into();
        large["identity"]["size"] = boundary.into();
        let record: ManifestRecord = serde_json::from_value(large).unwrap();
        assert_eq!(record.size(), boundary);
        let failure = ManifestFailure::new(2, entry_failure("blocked.bin"));
        failure.validate().unwrap();
        assert_eq!(failure.failure.size, Some(boundary));
        assert_eq!(failure.manifest().size, 0);
        assert_eq!(ManifestRecord::Failure(failure).size(), 0);
    }

    #[test]
    fn spool_keeps_success_ids_and_reassigns_only_failure_tail_before_owned_publication() {
        use super::super::{DirectoryScanLimits, DirectoryScanner, cache_publish_file};
        use std::sync::{Arc, atomic::AtomicBool};
        let fixture = Fixture::new();
        let source_root = fixture.root.join("source");
        let good = DirectoryEntry::single_file(&fixture.file("source/good.bin", b"good")).unwrap();
        fixture.file("source/locked/recovered.bin", b"recovered");
        let old_output = fixture.root.join("old.pending");
        let mut old =
            ManifestSpool::create(&old_output, &fixture.root.join("old-failures.pending"), 10)
                .unwrap();
        old.entry(good.clone()).unwrap();
        let blocked = DirectoryScanFailure {
            relative_path: Some("locked".into()),
            result: TransferResult::PermissionDenied,
            scope: DirectoryScanFailureScope::Subtree,
            kind: Some(TransferEntryKind::Directory),
            size: Some(0),
            terminal: false,
            retry_relative_path: Some("locked".into()),
        };
        old.failure(blocked).unwrap();
        old.failure(entry_failure("later.bin")).unwrap();
        let (mut output, count, bytes) = old.finish(|| Ok(())).unwrap();
        assert_eq!((count, bytes), (3, 4));
        output.seek(SeekFrom::Start(0)).unwrap();
        let mut reader = BufReader::new(output);
        let first = read_line::<ManifestRecord>(&mut reader).unwrap().unwrap();
        assert_eq!(first.item_id(), 1);
        assert!(matches!(
            read_line::<ManifestRecord>(&mut reader).unwrap().unwrap(),
            ManifestRecord::Failure(_)
        ));
        let remaining = match read_line::<ManifestRecord>(&mut reader).unwrap().unwrap() {
            ManifestRecord::Failure(f) => f.failure,
            _ => unreachable!(),
        };
        drop(reader);
        let new_output = fixture.root.join("new.pending");
        let mut updated =
            ManifestSpool::create(&new_output, &fixture.root.join("new-failures.pending"), 10)
                .unwrap();
        updated.entry(good).unwrap();
        let scanner = DirectoryScanner::open_subtree_with_limits(
            &source_root,
            "locked",
            true,
            Arc::new(AtomicBool::new(false)),
            DirectoryScanLimits {
                max_entries: 10,
                max_total_bytes: 1024,
                max_file_bytes: 1024,
                max_depth: 10,
                max_relative_path_bytes: 4096,
            },
        )
        .unwrap();
        for entry in scanner {
            updated.entry(entry.unwrap()).unwrap();
        }
        updated.failure(remaining).unwrap();
        let (mut output, count, bytes) = updated.finish(|| Ok(())).unwrap();
        assert_eq!((count, bytes), (4, 13));
        output.seek(SeekFrom::Start(0)).unwrap();
        {
            let mut reader = BufReader::new(&mut output);
            for expected in 1..=4 {
                let record = read_line::<ManifestRecord>(&mut reader).unwrap().unwrap();
                assert_eq!(record.item_id(), expected);
                record.validate(&source_root, true).unwrap();
                if expected == 4 {
                    assert!(matches!(record, ManifestRecord::Failure(_)));
                }
            }
            assert!(read_line::<ManifestRecord>(&mut reader).unwrap().is_none());
        }
        let canonical = fixture.root.join("manifest.jsonl");
        cache_publish_file(&output, &new_output, &canonical, false).unwrap();
        fs::write(&new_output, b"replacement must remain untouched").unwrap();
        let final_path = fixture.root.join("manifest-again.jsonl");
        cache_publish_file(&output, &canonical, &final_path, false).unwrap();
        drop(output);
        assert_eq!(
            fs::read(&new_output).unwrap(),
            b"replacement must remain untouched"
        );
        assert!(final_path.is_file());
    }

    fn root_enumeration_failure() -> DirectoryScanFailure {
        DirectoryScanFailure {
            relative_path: None,
            result: TransferResult::IoFailed,
            scope: DirectoryScanFailureScope::Enumeration,
            kind: Some(TransferEntryKind::Directory),
            size: Some(0),
            terminal: false,
            retry_relative_path: Some(String::new()),
        }
    }

    fn task_failure() -> DirectoryScanFailure {
        DirectoryScanFailure {
            relative_path: None,
            result: TransferResult::TooLarge,
            scope: DirectoryScanFailureScope::Task,
            kind: None,
            size: None,
            terminal: true,
            retry_relative_path: None,
        }
    }

    #[test]
    fn overlapping_recovery_removes_active_entry_and_keeps_root_markers_once() {
        let fixture = Fixture::new();
        let output_path = fixture.root.join("out.pending");
        let failures_path = fixture.root.join("failures.pending");
        let mut spool = ManifestSpool::create(&output_path, &failures_path, 4).unwrap();
        let source = fixture.root.join("source");
        let good = DirectoryEntry::single_file(&fixture.file("source/good.bin", b"good")).unwrap();
        spool.entry(good).unwrap();
        let failure = entry_failure("blocked.bin");
        let blocked = spool.failure(failure.clone()).unwrap();
        let enumeration = spool.failure(root_enumeration_failure()).unwrap();
        let task = spool.failure(task_failure()).unwrap();
        assert_ne!(enumeration, task);
        assert_eq!(
            spool.failure(root_enumeration_failure()).unwrap(),
            enumeration
        );
        assert_eq!(spool.failure(task_failure()).unwrap(), task);
        assert_eq!(spool.count(), 4);
        assert!(
            spool
                .resolve_entry_failures("blocked.bin", TransferEntryKind::File)
                .unwrap()
        );
        assert!(!spool.failure_active(blocked).unwrap());
        let repaired =
            DirectoryEntry::single_file(&fixture.file("source/blocked.bin", b"fixed")).unwrap();
        spool.entry(repaired).unwrap();
        assert_eq!(spool.find_failure_slot(&failure).unwrap(), Some(blocked));
        assert!(
            !spool.failure_active(blocked).unwrap(),
            "lookup must not reactivate a resolved scope"
        );
        assert_eq!(spool.count(), 4);
        let mut visits = 0;
        let (mut output, count, bytes) = spool
            .finish(|| {
                visits += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!((visits, count, bytes), (3, 4, 9));
        output.seek(SeekFrom::Start(0)).unwrap();
        let mut reader = BufReader::new(output);
        for id in 1..=4 {
            let record = read_line::<ManifestRecord>(&mut reader).unwrap().unwrap();
            assert_eq!(record.item_id(), id);
            record.validate(&source, true).unwrap();
            if id <= 2 {
                assert!(matches!(record, ManifestRecord::Entry(_)));
            } else {
                assert!(matches!(record, ManifestRecord::Failure(_)));
            }
        }
        assert!(read_line::<ManifestRecord>(&mut reader).unwrap().is_none());
        assert!(!failures_path.exists());
        assert!(!failures_path.with_extension("states.pending").exists());
        assert!(!failures_path.with_extension("keys.pending").exists());
    }

    #[test]
    fn known_kind_mismatch_retains_source_changed_in_its_distinct_scope_slot() {
        let fixture = Fixture::new();
        let mut spool =
            ManifestSpool::create(&fixture.root.join("out"), &fixture.root.join("failures"), 2)
                .unwrap();
        let file = spool.failure(entry_failure("same")).unwrap();
        let directory = spool
            .failure(DirectoryScanFailure {
                relative_path: Some("same".into()),
                result: TransferResult::PermissionDenied,
                scope: DirectoryScanFailureScope::Subtree,
                kind: Some(TransferEntryKind::Directory),
                size: Some(0),
                terminal: false,
                retry_relative_path: Some("same".into()),
            })
            .unwrap();
        assert_ne!(file, directory);
        assert!(
            !spool
                .resolve_entry_failures("same", TransferEntryKind::File)
                .unwrap()
        );
        assert!(!spool.failure_active(file).unwrap());
        assert!(spool.failure_active(directory).unwrap());
        assert_eq!(
            spool.resolve_entry_failures("../outside", TransferEntryKind::File),
            Err(TransferResult::InvalidData)
        );
        let mut visits = 0;
        let (mut output, count, _) = spool
            .finish(|| {
                visits += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!((visits, count), (2, 1));
        output.seek(SeekFrom::Start(0)).unwrap();
        match read_line::<ManifestRecord>(&mut BufReader::new(output))
            .unwrap()
            .unwrap()
        {
            ManifestRecord::Failure(failure) => {
                assert_eq!(failure.failure.scope, DirectoryScanFailureScope::Subtree);
                assert_eq!(failure.failure.result, TransferResult::SourceChanged);
                assert_eq!(failure.failure.kind, Some(TransferEntryKind::Directory));
            }
            _ => panic!("kind replacement must remain a failure"),
        }
    }

    #[test]
    fn physical_slot_budget_is_twice_canonical_and_reactivation_reuses_original_slot() {
        let fixture = Fixture::new();
        let mut spool =
            ManifestSpool::create(&fixture.root.join("out"), &fixture.root.join("failures"), 1)
                .unwrap();
        let a = spool.failure(entry_failure("a")).unwrap();
        assert_eq!(
            spool.failure(entry_failure("b")),
            Err(TransferResult::TooLarge)
        );
        spool.deactivate_failure(a).unwrap();
        spool.deactivate_failure(a).unwrap();
        let b = spool.failure(entry_failure("b")).unwrap();
        spool.deactivate_failure(b).unwrap();
        assert_eq!(
            spool.failure(entry_failure("c")),
            Err(TransferResult::TooLarge)
        );
        assert_eq!(spool.physical_slots, 2);
        assert_eq!(spool.failure(entry_failure("a")).unwrap(), a);
        assert_eq!(spool.count(), 1);
        let mut visits = 0;
        let (_, count, _) = spool
            .finish(|| {
                visits += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!((visits, count), (2, 1));
    }

    #[test]
    fn success_only_spool_has_no_failure_index_and_drop_cleans_owned_temporaries() {
        let fixture = Fixture::new();
        let output_path = fixture.root.join("out");
        let failures_path = fixture.root.join("failures");
        let canonical = fixture.file("canonical", b"preserve authoritative manifest");
        {
            let mut spool = ManifestSpool::create(
                &output_path,
                &failures_path,
                desklink_protocol::MAX_FILE_TASK_ENTRIES,
            )
            .unwrap();
            spool
                .entry(DirectoryEntry::single_file(&fixture.file("source", b"data")).unwrap())
                .unwrap();
            assert!(spool.failure_index.is_none());
            assert!(spool.states.is_none());
            assert!(!spool.failure_index_path.exists());
            assert!(!spool.states_path.exists());
        }
        assert!(!output_path.exists());
        assert!(!failures_path.exists());
        assert_eq!(
            fs::read(canonical).unwrap(),
            b"preserve authoritative manifest"
        );
    }

    #[test]
    fn finish_cancellation_counts_inactive_slots_and_cleans_without_touching_canonical() {
        let fixture = Fixture::new();
        let output_path = fixture.root.join("out");
        let failures_path = fixture.root.join("failures");
        let canonical = fixture.file("canonical", b"old manifest");
        let mut spool = ManifestSpool::create(&output_path, &failures_path, 2).unwrap();
        let first = spool.failure(entry_failure("a")).unwrap();
        let second = spool.failure(entry_failure("b")).unwrap();
        spool.deactivate_failure(first).unwrap();
        spool.deactivate_failure(second).unwrap();
        let mut visits = 0;
        assert!(matches!(
            spool.finish(|| {
                visits += 1;
                if visits == 2 {
                    Err(TransferResult::Cancelled)
                } else {
                    Ok(())
                }
            }),
            Err(TransferResult::Cancelled)
        ));
        assert_eq!(visits, 2);
        assert_eq!(fs::read(canonical).unwrap(), b"old manifest");
        for path in [
            output_path,
            failures_path.clone(),
            failures_path.with_extension("states.pending"),
            failures_path.with_extension("keys.pending"),
        ] {
            assert!(!path.exists(), "left derived temporary {}", path.display());
        }
    }
}
