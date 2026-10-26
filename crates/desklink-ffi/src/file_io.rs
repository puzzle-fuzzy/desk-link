//! Local file IO for explicit Apple-controller transfers. A destination directory
//! descriptor pins the user's choice; publication uses a no-replace hard link.
use blake2::{Blake2s256, Digest};
use desklink_protocol::{
    MAX_TRANSFER_CHUNK_BYTES, MAX_TRANSFER_FILE_BYTES, TransferId, TransferResult,
    is_valid_transfer_file_name,
};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(crate) struct OutgoingFile {
    pub file: File,
    pub name: String,
    pub size: u64,
    modified: Option<std::time::SystemTime>,
}
impl OutgoingFile {
    pub fn open(path: &Path) -> Result<Self, TransferResult> {
        if !path.is_absolute() {
            return Err(TransferResult::InvalidData);
        }
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| is_valid_transfer_file_name(s))
            .ok_or(TransferResult::InvalidData)?
            .to_owned();
        let directory = Directory::open(path.parent().ok_or(TransferResult::InvalidData)?)?;
        let file = directory.open_source(&name)?;
        let metadata = file.metadata().map_err(|_| TransferResult::IoFailed)?;
        if !metadata.is_file() {
            return Err(TransferResult::InvalidData);
        }
        if metadata.len() > MAX_TRANSFER_FILE_BYTES {
            return Err(TransferResult::TooLarge);
        }
        Ok(Self {
            file,
            name,
            size: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
    pub fn unchanged(&self) -> bool {
        self.file
            .metadata()
            .is_ok_and(|m| m.len() == self.size && m.modified().ok() == self.modified)
    }
}

pub(crate) struct IncomingFile {
    destination: Directory,
    staging: Directory,
    staging_name: String,
    file: Option<File>,
    pub name: String,
    pub size: u64,
    pub written: u64,
    hasher: Blake2s256,
}
impl IncomingFile {
    pub fn create(
        destination: &Path,
        transfer_id: TransferId,
        name: String,
        size: u64,
    ) -> Result<Self, TransferResult> {
        if !is_valid_transfer_file_name(&name) || transfer_id == [0; 16] {
            return Err(TransferResult::InvalidData);
        }
        if size > MAX_TRANSFER_FILE_BYTES {
            return Err(TransferResult::TooLarge);
        }
        let destination = Directory::open(destination)?;
        if destination.path.join(&name).to_string_lossy().len() > 4096 {
            return Err(TransferResult::InvalidData);
        }
        destination.ensure_capacity(size)?;
        // One private directory per transfer prevents collisions with other app
        // instances and prevents reopening a stale or untrusted partial file.
        let staging_name = format!(".desklink-{}", hex(&transfer_id));
        let staging = destination.create_directory(&staging_name)?;
        let file = match staging.create_file("payload.part") {
            Ok(f) => f,
            Err(e) => {
                let _ = destination.remove_directory(&staging_name);
                return Err(e);
            }
        };
        Ok(Self {
            destination,
            staging,
            staging_name,
            file: Some(file),
            name,
            size,
            written: 0,
            hasher: Blake2s256::new(),
        })
    }
    pub fn write_chunk(&mut self, offset: u64, bytes: &[u8]) -> Result<(), TransferResult> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or(TransferResult::InvalidData)?;
        if offset != self.written
            || bytes.is_empty()
            || bytes.len() > MAX_TRANSFER_CHUNK_BYTES
            || end > self.size
        {
            return Err(TransferResult::InvalidData);
        }
        self.file
            .as_mut()
            .ok_or(TransferResult::InvalidData)?
            .write_all(bytes)
            .map_err(io_result)?;
        self.hasher.update(bytes);
        self.written = end;
        Ok(())
    }
    pub fn finish(
        mut self,
        hash: [u8; 32],
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<PathBuf, TransferResult> {
        use std::sync::atomic::Ordering;
        if cancel.load(Ordering::Acquire) {
            return Err(TransferResult::Cancelled);
        }
        let actual: [u8; 32] = self.hasher.clone().finalize().into();
        if self.written != self.size || actual != hash {
            return Err(TransferResult::InvalidData);
        }
        self.file
            .take()
            .ok_or(TransferResult::InvalidData)?
            .sync_all()
            .map_err(io_result)?;
        for suffix in 0..=9999 {
            if cancel.load(Ordering::Acquire) {
                return Err(TransferResult::Cancelled);
            }
            let candidate = if suffix == 0 {
                self.name.clone()
            } else {
                let path = Path::new(&self.name);
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or(TransferResult::InvalidData)?;
                match path.extension().and_then(|s| s.to_str()) {
                    Some(ext) => format!("{stem} ({suffix}).{ext}"),
                    None => format!("{stem} ({suffix})"),
                }
            };
            if self
                .destination
                .path
                .join(&candidate)
                .to_string_lossy()
                .len()
                > 4096
            {
                return Err(TransferResult::InvalidData);
            }
            match self
                .staging
                .publish("payload.part", &self.destination, &candidate)
            {
                Ok(()) => return Ok(self.destination.path.join(candidate)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io_result(e)),
            }
        }
        Err(TransferResult::IoFailed)
    }
}
impl Drop for IncomingFile {
    fn drop(&mut self) {
        self.file.take();
        let _ = self.staging.remove_file("payload.part");
        let _ = self.destination.remove_directory(&self.staging_name);
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn io_result(error: std::io::Error) -> TransferResult {
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => TransferResult::PermissionDenied,
        std::io::ErrorKind::StorageFull => TransferResult::InsufficientSpace,
        _ => TransferResult::IoFailed,
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct Directory {
    file: File,
    path: PathBuf,
}
#[cfg(unix)]
impl Directory {
    fn open(path: &Path) -> Result<Self, TransferResult> {
        use std::os::unix::fs::MetadataExt;
        use std::{
            ffi::CString,
            os::{fd::FromRawFd, unix::ffi::OsStrExt},
        };
        let selected = std::fs::symlink_metadata(path).map_err(io_result)?;
        if !path.is_absolute() || !selected.is_dir() || selected.file_type().is_symlink() {
            return Err(TransferResult::InvalidData);
        }
        let path = path.canonicalize().map_err(io_result)?;
        let root = unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if root < 0 {
            return Err(io_result(std::io::Error::last_os_error()));
        }
        let mut file = unsafe { File::from_raw_fd(root) };
        for component in path.components() {
            let std::path::Component::Normal(component) = component else {
                continue;
            };
            let name =
                CString::new(component.as_bytes()).map_err(|_| TransferResult::InvalidData)?;
            file = Self::open_at(&file, &name, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
        let opened = file.metadata().map_err(io_result)?;
        if opened.dev() != selected.dev() || opened.ino() != selected.ino() {
            return Err(TransferResult::InvalidData);
        }
        Ok(Self { file, path })
    }
    fn open_at(parent: &File, name: &std::ffi::CStr, flags: i32) -> Result<File, TransferResult> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if fd < 0 {
            Err(io_result(std::io::Error::last_os_error()))
        } else {
            Ok(unsafe { File::from_raw_fd(fd) })
        }
    }
    fn name(name: &str) -> Result<std::ffi::CString, TransferResult> {
        std::ffi::CString::new(name).map_err(|_| TransferResult::InvalidData)
    }
    fn open_source(&self, name: &str) -> Result<File, TransferResult> {
        Self::open_at(
            &self.file,
            &Self::name(name)?,
            libc::O_RDONLY | libc::O_NONBLOCK,
        )
    }
    fn create_directory(&self, name: &str) -> Result<Self, TransferResult> {
        use std::os::fd::AsRawFd;
        let name_c = Self::name(name)?;
        if unsafe { libc::mkdirat(self.file.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
            return Err(io_result(std::io::Error::last_os_error()));
        }
        match Self::open_at(&self.file, &name_c, libc::O_RDONLY | libc::O_DIRECTORY) {
            Ok(file) => Ok(Self {
                file,
                path: self.path.join(name),
            }),
            Err(error) => {
                let _ = self.remove_directory(name);
                Err(error)
            }
        }
    }
    fn create_file(&self, name: &str) -> Result<File, TransferResult> {
        Self::open_at(
            &self.file,
            &Self::name(name)?,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        )
    }
    fn remove_file(&self, name: &str) -> std::io::Result<()> {
        self.unlink(name, 0)
    }
    fn remove_directory(&self, name: &str) -> std::io::Result<()> {
        self.unlink(name, libc::AT_REMOVEDIR)
    }
    fn unlink(&self, name: &str, flags: i32) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let name = std::ffi::CString::new(name)?;
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), flags) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    fn publish(&self, name: &str, destination: &Self, candidate: &str) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let name = std::ffi::CString::new(name)?;
        let candidate = std::ffi::CString::new(candidate)?;
        if unsafe {
            libc::linkat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                destination.file.as_raw_fd(),
                candidate.as_ptr(),
                0,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    fn ensure_capacity(&self, size: u64) -> Result<(), TransferResult> {
        use std::os::fd::AsRawFd;
        let mut info = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::fstatvfs(self.file.as_raw_fd(), info.as_mut_ptr()) } != 0 {
            return Err(io_result(std::io::Error::last_os_error()));
        }
        let info = unsafe { info.assume_init() };
        let available = (info.f_bavail as u128) * (info.f_frsize as u128);
        if size > 0 && available < (size as u128) + (64 * 1024 * 1024) {
            Err(TransferResult::InsufficientSpace)
        } else {
            Ok(())
        }
    }
}

// File actions are an Apple-controller API. Other targets retain the protocol
// but must provide an equally safe local filesystem adapter before enabling it.
#[cfg(not(unix))]
#[derive(Debug)]
struct Directory {
    path: PathBuf,
}
#[cfg(not(unix))]
impl Directory {
    fn open(_: &Path) -> Result<Self, TransferResult> {
        Err(TransferResult::Unsupported)
    }
    fn open_source(&self, _: &str) -> Result<File, TransferResult> {
        Err(TransferResult::Unsupported)
    }
    fn create_directory(&self, _: &str) -> Result<Self, TransferResult> {
        Err(TransferResult::Unsupported)
    }
    fn create_file(&self, _: &str) -> Result<File, TransferResult> {
        Err(TransferResult::Unsupported)
    }
    fn remove_file(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn remove_directory(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn publish(&self, _: &str, _: &Self, _: &str) -> std::io::Result<()> {
        Err(std::io::Error::other("unsupported filesystem"))
    }
    fn ensure_capacity(&self, _: u64) -> Result<(), TransferResult> {
        Err(TransferResult::Unsupported)
    }
}

pub(crate) enum ReadEvent {
    Chunk { offset: u64, bytes: Vec<u8> },
    Complete([u8; 32]),
    Failed(TransferResult),
}
/// Reads and hashes once on a blocking pool thread. At most two chunks remain
/// queued; cancellation or a dropped receiver stops further disk reads.
pub(crate) fn read_outgoing(
    mut file: OutgoingFile,
    resume_offset: u64,
    prefix: Option<[u8; 32]>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    events: tokio::sync::mpsc::Sender<ReadEvent>,
) {
    use std::sync::atomic::Ordering;
    let result = (|| {
        if resume_offset > file.size || (resume_offset == 0) != prefix.is_none() {
            return Err(TransferResult::InvalidData);
        }
        let mut hasher = Blake2s256::new();
        let mut offset = 0_u64;
        let mut prefix_verified = resume_offset == 0;
        loop {
            if cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            // Split at the resume boundary so no new byte is sent before proof.
            let limit = if offset < resume_offset {
                (resume_offset - offset).min(MAX_TRANSFER_CHUNK_BYTES as u64) as usize
            } else {
                MAX_TRANSFER_CHUNK_BYTES
            };
            let mut bytes = vec![0; limit];
            let count = file.file.read(&mut bytes).map_err(io_result)?;
            if count == 0 {
                break;
            }
            bytes.truncate(count);
            hasher.update(&bytes);
            let end = offset
                .checked_add(count as u64)
                .ok_or(TransferResult::InvalidData)?;
            if end > file.size {
                return Err(TransferResult::SourceChanged);
            }
            if !prefix_verified && end == resume_offset {
                let actual: [u8; 32] = hasher.clone().finalize().into();
                if Some(actual) != prefix {
                    return Err(TransferResult::SourceChanged);
                }
                prefix_verified = true;
            }
            if offset >= resume_offset {
                events
                    .blocking_send(ReadEvent::Chunk { offset, bytes })
                    .map_err(|_| TransferResult::Cancelled)?;
            }
            offset = end;
        }
        if offset != file.size || !file.unchanged() || !prefix_verified {
            return Err(TransferResult::SourceChanged);
        }
        events
            .blocking_send(ReadEvent::Complete(hasher.finalize().into()))
            .map_err(|_| TransferResult::Cancelled)?;
        Ok(())
    })();
    if let Err(result) = result {
        let _ = events.blocking_send(ReadEvent::Failed(result));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};
    fn root(label: &str) -> PathBuf {
        let p = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "desklink-apple-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&p).unwrap();
        p
    }
    #[test]
    fn sequential_integrity_and_failure_cleanup() {
        let root = root("integrity");
        let mut file = IncomingFile::create(&root, [1; 16], "报告.txt".into(), 5).unwrap();
        assert_eq!(
            file.write_chunk(1, b"hello"),
            Err(TransferResult::InvalidData)
        );
        assert_eq!(
            file.write_chunk(0, &vec![0; MAX_TRANSFER_CHUNK_BYTES + 1]),
            Err(TransferResult::InvalidData)
        );
        file.write_chunk(0, b"hello").unwrap();
        assert_eq!(
            file.finish([0; 32], &AtomicBool::new(false)),
            Err(TransferResult::InvalidData)
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn hash_validated_atomic_publish_never_overwrites_existing_file_or_symlink() {
        let root = root("publish");
        std::fs::write(root.join("数据.txt"), b"existing").unwrap();
        std::os::unix::fs::symlink(root.join("数据.txt"), root.join("数据 (1).txt")).unwrap();
        let mut file = IncomingFile::create(&root, [2; 16], "数据.txt".into(), 8).unwrap();
        file.write_chunk(0, b"desklink").unwrap();
        let saved = file
            .finish(
                Blake2s256::digest(b"desklink").into(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(saved, root.join("数据 (2).txt"));
        assert_eq!(std::fs::read(root.join("数据.txt")).unwrap(), b"existing");
        assert_eq!(std::fs::read(saved).unwrap(), b"desklink");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 3);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn untrusted_names_sources_and_destinations_are_rejected_before_writing() {
        let root = root("paths");
        for name in ["../outside", "x/y", "x\\y", ".", "..", "a:b"] {
            assert!(IncomingFile::create(&root, [3; 16], name.into(), 0).is_err());
        }
        assert!(matches!(
            IncomingFile::create(
                &root,
                [3; 16],
                "big.bin".into(),
                MAX_TRANSFER_FILE_BYTES + 1
            ),
            Err(TransferResult::TooLarge)
        ));
        std::fs::write(root.join("real.txt"), b"real").unwrap();
        std::os::unix::fs::symlink(root.join("real.txt"), root.join("alias.txt")).unwrap();
        assert!(OutgoingFile::open(&root.join("alias.txt")).is_err());
        let linked = root.with_extension("alias");
        std::os::unix::fs::symlink(&root, &linked).unwrap();
        assert!(IncomingFile::create(&linked, [3; 16], "unsafe.txt".into(), 0).is_err());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
        std::fs::remove_file(linked).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cancellation_and_drop_remove_partial_data_and_do_not_publish() {
        let root = root("cancel");
        let mut file = IncomingFile::create(&root, [4; 16], "cancel.txt".into(), 8).unwrap();
        file.write_chunk(0, b"desk").unwrap();
        drop(file);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let mut file = IncomingFile::create(&root, [4; 16], "cancel.txt".into(), 8).unwrap();
        file.write_chunk(0, b"desklink").unwrap();
        assert_eq!(
            file.finish(
                Blake2s256::digest(b"desklink").into(),
                &AtomicBool::new(true)
            ),
            Err(TransferResult::Cancelled)
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn outgoing_resume_requires_matching_prefix_and_hashes_full_file() {
        let root = root("resume");
        let source = root.join("source.bin");
        let bytes = vec![7u8; MAX_TRANSFER_CHUNK_BYTES + 19];
        std::fs::write(&source, &bytes).unwrap();
        let resume = MAX_TRANSFER_CHUNK_BYTES as u64 - 3;
        let prefix = Blake2s256::digest(&bytes[..resume as usize]).into();
        let (tx, mut rx) = mpsc_for_read();
        let file = OutgoingFile::open(&source).unwrap();
        let thread = std::thread::spawn(move || {
            read_outgoing(
                file,
                resume,
                Some(prefix),
                Arc::new(AtomicBool::new(false)),
                tx,
            )
        });
        let mut output = Vec::new();
        let mut complete = None;
        while let Some(event) = rx.blocking_recv() {
            match event {
                ReadEvent::Chunk { offset, bytes } => {
                    assert_eq!(offset, resume + output.len() as u64);
                    output.extend(bytes);
                }
                ReadEvent::Complete(hash) => complete = Some(hash),
                ReadEvent::Failed(r) => panic!("{r:?}"),
            }
        }
        thread.join().unwrap();
        assert_eq!(output, bytes[resume as usize..]);
        assert_eq!(complete, Some(Blake2s256::digest(&bytes).into()));
        let (tx, mut rx) = mpsc_for_read();
        let file = OutgoingFile::open(&source).unwrap();
        let thread = std::thread::spawn(move || {
            read_outgoing(file, 1, Some([0; 32]), Arc::new(AtomicBool::new(false)), tx)
        });
        assert!(matches!(
            rx.blocking_recv(),
            Some(ReadEvent::Failed(TransferResult::SourceChanged))
        ));
        thread.join().unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn outgoing_source_replacement_and_size_changes_cannot_escape_open_descriptor() {
        let root = root("source");
        let source = root.join("source.txt");
        std::fs::write(&source, b"original").unwrap();
        let mut opened = OutgoingFile::open(&source).unwrap();
        std::fs::rename(&source, root.join("moved.txt")).unwrap();
        std::fs::write(&source, b"new path").unwrap();
        let mut data = String::new();
        opened.file.read_to_string(&mut data).unwrap();
        assert_eq!(data, "original");
        std::fs::write(root.join("moved.txt"), b"changed size").unwrap();
        assert!(!opened.unchanged());
        std::fs::remove_dir_all(root).unwrap();
    }
    fn mpsc_for_read() -> (
        tokio::sync::mpsc::Sender<ReadEvent>,
        tokio::sync::mpsc::Receiver<ReadEvent>,
    ) {
        tokio::sync::mpsc::channel(2)
    }
}
