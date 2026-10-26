//! Bounded, durable block proofs for protocol 14 file transfers.
//!
//! These are blocking disk primitives. Callers must keep them on the existing
//! file worker thread, retain its writer permit and any pinned directory handles,
//! and publish a destination only after `verify_all` succeeds. A checkpoint ACK
//! means both the bytes and their journal record have been synced to disk.

#[cfg(any(not(windows), test))]
use std::fs::OpenOptions;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::UNIX_EPOCH,
};

use blake2::{Blake2s256, Digest};
use desklink_protocol::{
    BLOCK_ROOT_DOMAIN, MAX_LARGE_FILE_BYTES, MAX_TRANSFER_CHUNK_BYTES, TransferResult,
};

pub const FILE_BLOCK_BYTES: u32 = MAX_TRANSFER_CHUNK_BYTES as u32;
const INDEX_MAGIC: &[u8; 8] = b"DLIDX14\0";
const LEDGER_MAGIC: &[u8; 8] = b"DLACK14\0";
const INDEX_HEADER_BYTES: u64 = 88;
const LEDGER_HEADER_BYTES: u64 = 56;
const RECORD_BYTES: u64 = 64;

fn io_failed(_: std::io::Error) -> TransferResult {
    TransferResult::IoFailed
}

#[cfg(not(windows))]
fn private_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    options
}

fn create_cache_file(path: &Path) -> Result<File, TransferResult> {
    #[cfg(windows)]
    {
        crate::transfer_directory::native::create_file_path(path)
    }
    #[cfg(not(windows))]
    {
        private_file_options()
            .create_new(true)
            .open(path)
            .map_err(io_failed)
    }
}

fn open_cache_file(path: &Path, write: bool) -> Result<File, TransferResult> {
    #[cfg(windows)]
    {
        crate::transfer_directory::native::open_file_path(path, write)
    }
    #[cfg(not(windows))]
    {
        let metadata = fs::symlink_metadata(path).map_err(io_failed)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(TransferResult::InvalidData);
        }
        OpenOptions::new()
            .read(true)
            .write(write)
            .open(path)
            .map_err(io_failed)
    }
}

fn delete_cache_path(path: &Path) -> Result<(), TransferResult> {
    #[cfg(windows)]
    {
        crate::transfer_directory::native::delete_path(path)
    }
    #[cfg(not(windows))]
    {
        fs::remove_file(path).map_err(io_failed)
    }
}

fn cancelled(flag: &AtomicBool) -> Result<(), TransferResult> {
    if flag.load(Ordering::Acquire) {
        Err(TransferResult::Cancelled)
    } else {
        Ok(())
    }
}

fn root_hasher(size: u64) -> Blake2s256 {
    let mut hash = Blake2s256::new();
    hash.update(BLOCK_ROOT_DOMAIN);
    hash.update(size.to_le_bytes());
    hash.update(FILE_BLOCK_BYTES.to_le_bytes());
    hash
}

fn digest(hash: &Blake2s256) -> [u8; 32] {
    hash.clone().finalize().into()
}

pub fn empty_resume_proof(size: u64) -> [u8; 32] {
    digest(&root_hasher(size))
}

pub fn block_digest(bytes: &[u8]) -> [u8; 32] {
    Blake2s256::digest(bytes).into()
}

fn validate_size(size: u64) -> Result<(), TransferResult> {
    if size > MAX_LARGE_FILE_BYTES {
        Err(TransferResult::TooLarge)
    } else {
        Ok(())
    }
}

fn blocks(size: u64) -> u64 {
    size.div_ceil(u64::from(FILE_BLOCK_BYTES))
}

/// Exact sidecar disk budgets, excluding filesystem allocation overhead. Both
/// can be reserved before starting a scan/receive; no size is narrowed to u32.
pub fn source_index_bytes(size: u64) -> Result<u64, TransferResult> {
    validate_size(size)?;
    record_position(INDEX_HEADER_BYTES, blocks(size))
}

pub fn checkpoint_ledger_bytes(size: u64) -> Result<u64, TransferResult> {
    validate_size(size)?;
    record_position(LEDGER_HEADER_BYTES, blocks(size))
}

fn block_len(size: u64, offset: u64) -> Result<usize, TransferResult> {
    if offset >= size || !offset.is_multiple_of(u64::from(FILE_BLOCK_BYTES)) {
        return Err(TransferResult::InvalidData);
    }
    usize::try_from((size - offset).min(u64::from(FILE_BLOCK_BYTES)))
        .map_err(|_| TransferResult::InvalidData)
}

fn record_position(header: u64, index: u64) -> Result<u64, TransferResult> {
    index
        .checked_mul(RECORD_BYTES)
        .and_then(|offset| offset.checked_add(header))
        .ok_or(TransferResult::InvalidData)
}

fn read_record(
    file: &mut File,
    header: u64,
    index: u64,
) -> Result<([u8; 32], [u8; 32]), TransferResult> {
    file.seek(SeekFrom::Start(record_position(header, index)?))
        .map_err(io_failed)?;
    let mut hash = [0; 32];
    let mut proof = [0; 32];
    file.read_exact(&mut hash).map_err(io_failed)?;
    file.read_exact(&mut proof).map_err(io_failed)?;
    Ok((hash, proof))
}

type ChangeTokenReader = fn(&File) -> Result<Option<i64>, TransferResult>;

/// FSCTL_READ_FILE_USN_DATA returns the file's last journal USN. Timestamp and
/// reason fields are intentionally ignored; Microsoft does not define them for
/// this request. An absent/zero/unsupported token requires full content checking.
fn file_change_token(file: &File) -> Result<Option<i64>, TransferResult> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            System::{
                IO::DeviceIoControl,
                Ioctl::{
                    FSCTL_READ_FILE_USN_DATA, READ_FILE_USN_DATA, USN_RECORD_V2, USN_RECORD_V3,
                },
            },
        };
        let versions = READ_FILE_USN_DATA {
            MinMajorVersion: 2,
            MaxMajorVersion: 3,
        };
        let mut buffer = [0_u64; 128];
        let mut returned = 0_u32;
        let result = unsafe {
            DeviceIoControl(
                HANDLE(file.as_raw_handle()),
                FSCTL_READ_FILE_USN_DATA,
                Some((&versions as *const READ_FILE_USN_DATA).cast()),
                std::mem::size_of::<READ_FILE_USN_DATA>() as u32,
                Some(buffer.as_mut_ptr().cast()),
                std::mem::size_of_val(&buffer) as u32,
                Some(&mut returned),
                None,
            )
        };
        if result.is_err() {
            return Ok(None);
        }
        let returned = returned as usize;
        if returned < 8 || returned > std::mem::size_of_val(&buffer) {
            return Ok(None);
        }
        let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), returned) };
        let record_length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        let version = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
        let offset = match version {
            2 => std::mem::offset_of!(USN_RECORD_V2, Usn),
            3 => std::mem::offset_of!(USN_RECORD_V3, Usn),
            _ => return Ok(None),
        };
        if record_length > returned || record_length < offset + 8 {
            return Ok(None);
        }
        let usn = i64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        Ok((usn > 0).then_some(usn))
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Ok(None)
    }
}

/// Stable identity of the opened source, including the OS file identity,
/// change time and nonzero journal USN where available. Without a reliable
/// change token, cached source indices require full content revalidation.
pub(crate) fn source_stamp(file: &File) -> Result<(u64, [u8; 32]), TransferResult> {
    source_stamp_with_reader(file, file_change_token)
}

fn source_stamp_with_reader(
    file: &File,
    token_reader: ChangeTokenReader,
) -> Result<(u64, [u8; 32]), TransferResult> {
    let metadata = file.metadata().map_err(io_failed)?;
    if !metadata.is_file() {
        return Err(TransferResult::InvalidData);
    }
    let mut stamp = Blake2s256::new();
    stamp.update(b"DeskLink source identity v14\0");
    stamp.update(metadata.len().to_le_bytes());
    match token_reader(file)? {
        Some(token) => {
            stamp.update([1]);
            stamp.update(token.to_le_bytes());
        }
        None => {
            stamp.update([0]);
            stamp.update(0_i64.to_le_bytes());
        }
    }
    for timestamp in [metadata.modified(), metadata.created()] {
        match timestamp
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        {
            Some(value) => {
                stamp.update([1]);
                stamp.update(value.as_nanos().to_le_bytes());
            }
            None => {
                stamp.update([0]);
                stamp.update(0_u128.to_le_bytes());
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO,
                FileBasicInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            },
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }
            .map_err(|_| TransferResult::IoFailed)?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(TransferResult::InvalidData);
        }
        stamp.update(info.dwVolumeSerialNumber.to_le_bytes());
        stamp.update(info.nFileIndexHigh.to_le_bytes());
        stamp.update(info.nFileIndexLow.to_le_bytes());
        let mut basic = FILE_BASIC_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                HANDLE(file.as_raw_handle()),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        }
        .map_err(|_| TransferResult::IoFailed)?;
        // ChangeTime changes even if a writer restores the last-write time.
        stamp.update(basic.ChangeTime.to_le_bytes());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        stamp.update(metadata.dev().to_le_bytes());
        stamp.update(metadata.ino().to_le_bytes());
        stamp.update(metadata.ctime().to_le_bytes());
        stamp.update(metadata.ctime_nsec().to_le_bytes());
    }
    Ok((metadata.len(), stamp.finalize().into()))
}

fn open_stable_source(path: &Path) -> Result<File, TransferResult> {
    #[cfg(windows)]
    {
        crate::transfer_directory::native::open_file_path(path, false)
    }
    #[cfg(not(windows))]
    {
        let metadata = fs::symlink_metadata(path).map_err(io_failed)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(TransferResult::InvalidData);
        }
        File::open(path).map_err(io_failed)
    }
}

/// One verified, canonical protocol block. At most 48 KiB is allocated.
pub struct IndexedBlock {
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub hash: [u8; 32],
}

/// A source file and a persistent disk index. The initial scan is O(file bytes),
/// with one 64-byte record per block and constant memory. A reliable nonzero
/// change token permits reopening without source-prefix reads. Other filesystems
/// revalidate the complete source with cancellation/progress before reuse.
/// Resume proof lookup is O(1).
/// Each outgoing block is checked against its indexed digest before it is sent.
pub struct SourceIndex {
    source: File,
    index: File,
    size: u64,
    stamp: [u8; 32],
    root: [u8; 32],
    token_reader: ChangeTokenReader,
}

impl SourceIndex {
    pub fn build(
        source: &Path,
        index_path: &Path,
        cancellation: &AtomicBool,
        progress: impl FnMut(u64),
    ) -> Result<Self, TransferResult> {
        Self::build_from_file(
            open_stable_source(source)?,
            index_path,
            cancellation,
            progress,
        )
    }

    /// The caller retains any ancestor pins associated with `source`. Existing
    /// index files are never overwritten, including an incomplete prior build.
    pub fn build_from_file(
        source: File,
        index_path: &Path,
        cancellation: &AtomicBool,
        progress: impl FnMut(u64),
    ) -> Result<Self, TransferResult> {
        Self::build_with_reader(
            source,
            index_path,
            file_change_token,
            cancellation,
            progress,
        )
    }

    fn build_with_reader(
        mut source: File,
        index_path: &Path,
        token_reader: ChangeTokenReader,
        cancellation: &AtomicBool,
        mut progress: impl FnMut(u64),
    ) -> Result<Self, TransferResult> {
        cancelled(cancellation)?;
        let (size, stamp) = source_stamp_with_reader(&source, token_reader)?;
        validate_size(size)?;
        // The index is disk backed and may itself exceed a gigabyte for a
        // negotiated 1-TiB file. Reserve its complete growth before scanning;
        // the helper includes the normal receive/cache free-space margin.
        crate::transfer::ensure_receive_capacity(
            index_path.parent().ok_or(TransferResult::InvalidData)?,
            source_index_bytes(size)?,
        )?;
        let mut index = create_cache_file(index_path)?;
        let result = (|| {
            index.write_all(INDEX_MAGIC).map_err(io_failed)?;
            index
                .write_all(&FILE_BLOCK_BYTES.to_le_bytes())
                .map_err(io_failed)?;
            index.write_all(&[0; 4]).map_err(io_failed)?;
            index.write_all(&size.to_le_bytes()).map_err(io_failed)?;
            index.write_all(&stamp).map_err(io_failed)?;
            index.write_all(&[0; 32]).map_err(io_failed)?;
            source.seek(SeekFrom::Start(0)).map_err(io_failed)?;
            let mut root = root_hasher(size);
            let mut offset = 0;
            let mut buffer = vec![0; FILE_BLOCK_BYTES as usize];
            while offset < size {
                cancelled(cancellation)?;
                let count = block_len(size, offset)?;
                source.read_exact(&mut buffer[..count]).map_err(|error| {
                    if error.kind() == std::io::ErrorKind::UnexpectedEof {
                        TransferResult::SourceChanged
                    } else {
                        io_failed(error)
                    }
                })?;
                let hash = block_digest(&buffer[..count]);
                root.update(hash);
                index.write_all(&hash).map_err(io_failed)?;
                index.write_all(&digest(&root)).map_err(io_failed)?;
                offset += count as u64;
                progress(offset);
            }
            cancelled(cancellation)?;
            if source_stamp_with_reader(&source, token_reader)? != (size, stamp) {
                return Err(TransferResult::SourceChanged);
            }
            let root = digest(&root);
            index.seek(SeekFrom::Start(56)).map_err(io_failed)?;
            index.write_all(&root).map_err(io_failed)?;
            index.sync_all().map_err(io_failed)?;
            Ok(root)
        })();
        match result {
            Ok(root) => Ok(Self {
                source,
                index,
                size,
                stamp,
                root,
                token_reader,
            }),
            Err(error) => {
                #[cfg(windows)]
                let _ = crate::transfer_directory::native::delete_file(&index);
                drop(index);
                // This path was created by this call with create_new. No other
                // owner is allowed to replace it while the build handle lives.
                #[cfg(not(windows))]
                let _ = fs::remove_file(index_path);
                Err(error)
            }
        }
    }

    pub fn open(source: &Path, index_path: &Path) -> Result<Self, TransferResult> {
        Self::open_from_file(open_stable_source(source)?, index_path)
    }

    /// Rebuild only an incomplete/corrupt index in the caller's private owned
    /// cache. SourceChanged is never erased or silently accepted. The source
    /// handle remains pinned throughout, and regular source files are never
    /// removed. Do not pass an arbitrary user-selected path as `index_path`.
    pub fn recover_or_build_from_file(
        source: File,
        index_path: &Path,
        cancellation: &AtomicBool,
        mut progress: impl FnMut(u64),
    ) -> Result<Self, TransferResult> {
        cancelled(cancellation)?;
        let metadata = match fs::symlink_metadata(index_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::build_from_file(source, index_path, cancellation, progress);
            }
            Err(error) => return Err(io_failed(error)),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(TransferResult::InvalidData);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes()
                & windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT.0
                != 0
            {
                return Err(TransferResult::InvalidData);
            }
        }
        match Self::open_checked(
            source.try_clone().map_err(io_failed)?,
            index_path,
            file_change_token,
            cancellation,
            &mut progress,
        ) {
            Ok(index) => Ok(index),
            Err(TransferResult::InvalidData) => {
                // A live Windows builder denies delete sharing; this removal
                // fails rather than replacing an index owned by another worker.
                delete_cache_path(index_path)?;
                Self::build_from_file(source, index_path, cancellation, progress)
            }
            Err(error) => Err(error),
        }
    }

    pub fn open_from_file(source: File, index_path: &Path) -> Result<Self, TransferResult> {
        Self::open_checked(
            source,
            index_path,
            file_change_token,
            &AtomicBool::new(false),
            &mut |_| {},
        )
    }

    pub fn open_from_file_with_control(
        source: File,
        index_path: &Path,
        cancellation: &AtomicBool,
        mut progress: impl FnMut(u64),
    ) -> Result<Self, TransferResult> {
        Self::open_checked(
            source,
            index_path,
            file_change_token,
            cancellation,
            &mut progress,
        )
    }

    fn open_checked(
        source: File,
        index_path: &Path,
        token_reader: ChangeTokenReader,
        cancellation: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<Self, TransferResult> {
        cancelled(cancellation)?;
        let (actual_size, actual_stamp) = source_stamp_with_reader(&source, token_reader)?;
        let mut index = open_cache_file(index_path, false)?;
        source_stamp(&index)?;
        let mut header = [0; INDEX_HEADER_BYTES as usize];
        index.read_exact(&mut header).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                TransferResult::InvalidData
            } else {
                io_failed(error)
            }
        })?;
        if &header[..8] != INDEX_MAGIC
            || header[8..12] != FILE_BLOCK_BYTES.to_le_bytes()
            || header[12..16] != [0; 4]
        {
            return Err(TransferResult::InvalidData);
        }
        let size = u64::from_le_bytes(header[16..24].try_into().unwrap());
        validate_size(size)?;
        let stamp: [u8; 32] = header[24..56].try_into().unwrap();
        let root: [u8; 32] = header[56..88].try_into().unwrap();
        if size != actual_size || stamp != actual_stamp {
            return Err(TransferResult::SourceChanged);
        }
        if index.metadata().map_err(io_failed)?.len()
            != record_position(INDEX_HEADER_BYTES, blocks(size))?
            || root == [0; 32]
        {
            return Err(TransferResult::InvalidData);
        }
        if size == 0 {
            if root != empty_resume_proof(0) {
                return Err(TransferResult::InvalidData);
            }
        } else if read_record(&mut index, INDEX_HEADER_BYTES, blocks(size) - 1)?.1 != root {
            return Err(TransferResult::InvalidData);
        }
        let mut opened = Self {
            source,
            index,
            size,
            stamp,
            root,
            token_reader,
        };
        if token_reader(&opened.source)?.is_none() {
            opened.verify_entire_source(cancellation, progress)?;
        }
        Ok(opened)
    }

    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn content_root(&self) -> [u8; 32] {
        self.root
    }

    pub fn source_identity(&self) -> [u8; 32] {
        self.stamp
    }

    pub fn verify_source(&self) -> Result<(), TransferResult> {
        if source_stamp_with_reader(&self.source, self.token_reader)? == (self.size, self.stamp) {
            Ok(())
        } else {
            Err(TransferResult::SourceChanged)
        }
    }

    fn verify_entire_source(
        &mut self,
        cancellation: &AtomicBool,
        progress: &mut dyn FnMut(u64),
    ) -> Result<(), TransferResult> {
        self.source.seek(SeekFrom::Start(0)).map_err(io_failed)?;
        let mut root = root_hasher(self.size);
        let mut offset = 0;
        let mut buffer = vec![0; FILE_BLOCK_BYTES as usize];
        while offset < self.size {
            cancelled(cancellation)?;
            let count = block_len(self.size, offset)?;
            self.source
                .read_exact(&mut buffer[..count])
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::UnexpectedEof {
                        TransferResult::SourceChanged
                    } else {
                        io_failed(error)
                    }
                })?;
            let hash = block_digest(&buffer[..count]);
            let (expected, _) = read_record(
                &mut self.index,
                INDEX_HEADER_BYTES,
                offset / u64::from(FILE_BLOCK_BYTES),
            )?;
            if hash != expected {
                return Err(TransferResult::SourceChanged);
            }
            root.update(hash);
            offset += count as u64;
            progress(offset);
        }
        cancelled(cancellation)?;
        if digest(&root) != self.root {
            return Err(TransferResult::SourceChanged);
        }
        self.verify_source()
    }

    pub fn resume_proof(&mut self, offset: u64) -> Result<[u8; 32], TransferResult> {
        if offset > self.size
            || (offset != self.size && !offset.is_multiple_of(u64::from(FILE_BLOCK_BYTES)))
        {
            return Err(TransferResult::InvalidData);
        }
        if offset == 0 {
            return Ok(empty_resume_proof(self.size));
        }
        read_record(&mut self.index, INDEX_HEADER_BYTES, blocks(offset) - 1).map(|record| record.1)
    }

    pub fn read_block(&mut self, offset: u64) -> Result<Option<IndexedBlock>, TransferResult> {
        self.verify_source()?;
        if offset == self.size {
            return Ok(None);
        }
        let count = block_len(self.size, offset)?;
        let (hash, _) = read_record(
            &mut self.index,
            INDEX_HEADER_BYTES,
            offset / u64::from(FILE_BLOCK_BYTES),
        )?;
        let mut bytes = vec![0; count];
        self.source
            .seek(SeekFrom::Start(offset))
            .map_err(io_failed)?;
        self.source.read_exact(&mut bytes).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                TransferResult::SourceChanged
            } else {
                io_failed(error)
            }
        })?;
        if block_digest(&bytes) != hash {
            return Err(TransferResult::SourceChanged);
        }
        Ok(Some(IndexedBlock {
            offset,
            bytes,
            hash,
        }))
    }
}

/// Durable ordered receive proofs. The part file and writer permit belong to
/// the worker, so dropping this value cannot delete another worker's file.
/// A restart reads only the small proof records. The full part is always read
/// and compared with every block proof again before final publication.
pub struct CheckpointLedger {
    journal: File,
    size: u64,
    root: [u8; 32],
    offset: u64,
    proof: Blake2s256,
}

impl CheckpointLedger {
    pub fn open_or_create(
        journal_path: &Path,
        part: &mut File,
        size: u64,
        root: [u8; 32],
    ) -> Result<Self, TransferResult> {
        validate_size(size)?;
        let mut journal = match fs::symlink_metadata(journal_path) {
            Ok(_) => open_cache_file(journal_path, true)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut journal = create_cache_file(journal_path)?;
                // A prefix without a proof journal cannot be acknowledged.
                part.set_len(0).map_err(io_failed)?;
                part.sync_data().map_err(io_failed)?;
                journal.write_all(LEDGER_MAGIC).map_err(io_failed)?;
                journal
                    .write_all(&FILE_BLOCK_BYTES.to_le_bytes())
                    .map_err(io_failed)?;
                journal.write_all(&[0; 4]).map_err(io_failed)?;
                journal.write_all(&size.to_le_bytes()).map_err(io_failed)?;
                journal.write_all(&root).map_err(io_failed)?;
                journal.sync_all().map_err(io_failed)?;
                journal
            }
            Err(error) => return Err(io_failed(error)),
        };
        let mut header = [0; LEDGER_HEADER_BYTES as usize];
        source_stamp(&journal)?;
        journal.seek(SeekFrom::Start(0)).map_err(io_failed)?;
        journal.read_exact(&mut header).map_err(io_failed)?;
        if &header[..8] != LEDGER_MAGIC
            || header[8..12] != FILE_BLOCK_BYTES.to_le_bytes()
            || header[12..16] != [0; 4]
            || header[16..24] != size.to_le_bytes()
            || header[24..56] != root
        {
            return Err(TransferResult::SourceChanged);
        }
        let length = journal.metadata().map_err(io_failed)?.len();
        let records = length.saturating_sub(LEDGER_HEADER_BYTES) / RECORD_BYTES;
        if records > blocks(size) {
            return Err(TransferResult::InvalidData);
        }
        let mut proof = root_hasher(size);
        for index in 0..records {
            let (hash, expected) = read_record(&mut journal, LEDGER_HEADER_BYTES, index)?;
            proof.update(hash);
            if digest(&proof) != expected {
                return Err(TransferResult::InvalidData);
            }
        }
        let offset = records
            .checked_mul(u64::from(FILE_BLOCK_BYTES))
            .ok_or(TransferResult::InvalidData)?
            .min(size);
        if part.metadata().map_err(io_failed)?.len() < offset {
            return Err(TransferResult::InvalidData);
        }
        // A crash can leave bytes or a torn record after the last durable ACK.
        // Never advertise them as resumed bytes.
        let durable_length = record_position(LEDGER_HEADER_BYTES, records)?;
        if length != durable_length {
            journal.set_len(durable_length).map_err(io_failed)?;
            journal.sync_data().map_err(io_failed)?;
        }
        if part.metadata().map_err(io_failed)?.len() != offset {
            part.set_len(offset).map_err(io_failed)?;
            part.sync_data().map_err(io_failed)?;
        }
        // A process can exit after writing a complete journal record but
        // before its sync returns. Re-adopting that valid record promotes it
        // to a durable checkpoint only after both handles are synced here.
        // This reads no prefix data and therefore remains cheap on reconnect.
        part.sync_data().map_err(io_failed)?;
        journal.sync_data().map_err(io_failed)?;
        part.seek(SeekFrom::Start(offset)).map_err(io_failed)?;
        journal.seek(SeekFrom::End(0)).map_err(io_failed)?;
        Ok(Self {
            journal,
            size,
            root,
            offset,
            proof,
        })
    }

    pub fn resume_offset(&self) -> u64 {
        self.offset
    }
    pub fn resume_proof(&self) -> [u8; 32] {
        digest(&self.proof)
    }
    pub fn expected_size(&self) -> u64 {
        self.size
    }
    pub fn content_root(&self) -> [u8; 32] {
        self.root
    }

    pub fn remaining_journal_bytes(&self) -> Result<u64, TransferResult> {
        Ok(checkpoint_ledger_bytes(self.size)?
            .saturating_sub(self.journal.metadata().map_err(io_failed)?.len()))
    }

    pub fn write_block(
        &mut self,
        part: &mut File,
        offset: u64,
        bytes: &[u8],
        hash: [u8; 32],
    ) -> Result<(), TransferResult> {
        self.write_block_unless_cancelled(part, offset, bytes, hash, &AtomicBool::new(false))
    }

    pub fn write_block_unless_cancelled(
        &mut self,
        part: &mut File,
        offset: u64,
        bytes: &[u8],
        hash: [u8; 32],
        cancellation: &AtomicBool,
    ) -> Result<(), TransferResult> {
        self.write_block_with_sync_hook(part, offset, bytes, hash, cancellation, || {})
    }

    fn write_block_with_sync_hook(
        &mut self,
        part: &mut File,
        offset: u64,
        bytes: &[u8],
        hash: [u8; 32],
        cancellation: &AtomicBool,
        after_data_sync: impl FnOnce(),
    ) -> Result<(), TransferResult> {
        cancelled(cancellation)?;
        if offset != self.offset
            || bytes.len() != block_len(self.size, offset)?
            || block_digest(bytes) != hash
        {
            return Err(TransferResult::InvalidData);
        }
        part.seek(SeekFrom::Start(offset)).map_err(io_failed)?;
        part.write_all(bytes).map_err(io_failed)?;
        part.sync_data().map_err(io_failed)?;
        after_data_sync();
        cancelled(cancellation)?;
        let mut proof = self.proof.clone();
        proof.update(hash);
        let record_offset = record_position(LEDGER_HEADER_BYTES, blocks(offset))?;
        self.journal
            .seek(SeekFrom::Start(record_offset))
            .map_err(io_failed)?;
        self.journal.write_all(&hash).map_err(io_failed)?;
        self.journal.write_all(&digest(&proof)).map_err(io_failed)?;
        self.journal.sync_data().map_err(io_failed)?;
        self.proof = proof;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    /// Re-read the actual disk content with bounded memory. This deliberately
    /// happens once at finalization, rather than on every short reconnect.
    pub fn verify_all(
        &mut self,
        part: &mut File,
        cancellation: &AtomicBool,
        mut progress: impl FnMut(u64),
    ) -> Result<(), TransferResult> {
        cancelled(cancellation)?;
        if self.offset != self.size || part.metadata().map_err(io_failed)?.len() != self.size {
            return Err(TransferResult::InvalidData);
        }
        part.seek(SeekFrom::Start(0)).map_err(io_failed)?;
        let mut proof = root_hasher(self.size);
        let mut offset = 0;
        let mut bytes = vec![0; FILE_BLOCK_BYTES as usize];
        while offset < self.size {
            cancelled(cancellation)?;
            let count = block_len(self.size, offset)?;
            part.read_exact(&mut bytes[..count]).map_err(io_failed)?;
            let hash = block_digest(&bytes[..count]);
            let (expected, _) = read_record(
                &mut self.journal,
                LEDGER_HEADER_BYTES,
                offset / u64::from(FILE_BLOCK_BYTES),
            )?;
            if hash != expected {
                return Err(TransferResult::InvalidData);
            }
            proof.update(hash);
            offset += count as u64;
            progress(offset);
        }
        if digest(&proof) != self.root || digest(&self.proof) != self.root {
            return Err(TransferResult::InvalidData);
        }
        cancelled(cancellation)?;
        part.sync_all().map_err(io_failed)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::FileTimes,
        path::PathBuf,
        sync::{Arc, atomic::AtomicUsize},
    };

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "desklink-block-checkpoint-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn part(&self) -> File {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(self.path("receive.part"))
                .unwrap()
        }
        fn source(&self, bytes: &[u8]) -> PathBuf {
            let path = self.path("source.bin");
            fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample() -> Vec<u8> {
        (0..FILE_BLOCK_BYTES as usize * 3 + 17)
            .map(|index| (index % 251) as u8)
            .collect()
    }

    #[test]
    fn block_checkpoint_roundtrip_restarts_with_only_durable_block_proofs() {
        let fixture = Fixture::new();
        let bytes = sample();
        let source_path = fixture.source(&bytes);
        let cancellation = AtomicBool::new(false);
        let mut scanned = 0;
        let mut source = SourceIndex::build(
            &source_path,
            &fixture.path("source.index"),
            &cancellation,
            |count| scanned = count,
        )
        .unwrap();
        assert_eq!(scanned, bytes.len() as u64);
        assert_eq!(source.size(), bytes.len() as u64);
        let root = source.content_root();
        let mut part = fixture.part();
        let mut receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            source.size(),
            root,
        )
        .unwrap();
        assert_eq!(receive.resume_proof(), source.resume_proof(0).unwrap());
        let block = source.read_block(0).unwrap().unwrap();
        receive
            .write_block(&mut part, block.offset, &block.bytes, block.hash)
            .unwrap();
        assert_eq!(receive.resume_offset(), u64::from(FILE_BLOCK_BYTES));
        let saved_proof = receive.resume_proof();
        assert_eq!(
            saved_proof,
            source.resume_proof(receive.resume_offset()).unwrap()
        );
        drop(receive);
        drop(source);
        let mut source = SourceIndex::open(&source_path, &fixture.path("source.index")).unwrap();
        let mut receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            source.size(),
            root,
        )
        .unwrap();
        assert_eq!(receive.resume_proof(), saved_proof);
        while let Some(block) = source.read_block(receive.resume_offset()).unwrap() {
            receive
                .write_block(&mut part, block.offset, &block.bytes, block.hash)
                .unwrap();
        }
        let mut verified = 0;
        receive
            .verify_all(&mut part, &cancellation, |count| verified = count)
            .unwrap();
        assert_eq!(verified, bytes.len() as u64);
        assert_eq!(fs::read(fixture.path("receive.part")).unwrap(), bytes);
    }

    #[test]
    fn torn_record_and_unacknowledged_disk_tail_are_truncated_on_restart() {
        let fixture = Fixture::new();
        let bytes = sample();
        let source_path = fixture.source(&bytes);
        let mut source = SourceIndex::build(
            &source_path,
            &fixture.path("source.index"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let mut part = fixture.part();
        let mut receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            source.size(),
            source.content_root(),
        )
        .unwrap();
        let block = source.read_block(0).unwrap().unwrap();
        receive
            .write_block(&mut part, block.offset, &block.bytes, block.hash)
            .unwrap();
        drop(receive);
        part.write_all(b"unacknowledged bytes").unwrap();
        part.sync_data().unwrap();
        OpenOptions::new()
            .append(true)
            .open(fixture.path("receive.ledger"))
            .unwrap()
            .write_all(&[1; 19])
            .unwrap();
        let receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            source.size(),
            source.content_root(),
        )
        .unwrap();
        assert_eq!(receive.resume_offset(), u64::from(FILE_BLOCK_BYTES));
        assert_eq!(part.metadata().unwrap().len(), u64::from(FILE_BLOCK_BYTES));
        assert_eq!(
            fs::metadata(fixture.path("receive.ledger")).unwrap().len(),
            LEDGER_HEADER_BYTES + RECORD_BYTES
        );
    }

    #[test]
    fn complete_corrupt_record_is_refused_instead_of_advertised_as_resume() {
        let fixture = Fixture::new();
        let bytes = vec![7; FILE_BLOCK_BYTES as usize];
        let mut root = root_hasher(bytes.len() as u64);
        root.update(block_digest(&bytes));
        let root = digest(&root);
        let mut part = fixture.part();
        let mut receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            bytes.len() as u64,
            root,
        )
        .unwrap();
        receive
            .write_block(&mut part, 0, &bytes, block_digest(&bytes))
            .unwrap();
        drop(receive);
        let mut journal = OpenOptions::new()
            .write(true)
            .open(fixture.path("receive.ledger"))
            .unwrap();
        journal.seek(SeekFrom::Start(LEDGER_HEADER_BYTES)).unwrap();
        journal.write_all(&[0xff]).unwrap();
        drop(journal);
        assert!(matches!(
            CheckpointLedger::open_or_create(
                &fixture.path("receive.ledger"),
                &mut part,
                bytes.len() as u64,
                root
            ),
            Err(TransferResult::InvalidData)
        ));
    }

    #[test]
    fn final_verification_detects_an_offline_part_change_before_publication() {
        let fixture = Fixture::new();
        let bytes = sample();
        let source_path = fixture.source(&bytes);
        let mut source = SourceIndex::build(
            &source_path,
            &fixture.path("source.index"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let mut part = fixture.part();
        let mut receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            source.size(),
            source.content_root(),
        )
        .unwrap();
        while let Some(block) = source.read_block(receive.resume_offset()).unwrap() {
            receive
                .write_block(&mut part, block.offset, &block.bytes, block.hash)
                .unwrap();
        }
        part.seek(SeekFrom::Start(1)).unwrap();
        part.write_all(&[0xff]).unwrap();
        part.sync_data().unwrap();
        assert_eq!(
            receive.verify_all(&mut part, &AtomicBool::new(false), |_| {}),
            Err(TransferResult::InvalidData)
        );
    }

    #[test]
    fn cancellation_after_data_sync_does_not_create_a_durable_ack() {
        let fixture = Fixture::new();
        let bytes = vec![9; FILE_BLOCK_BYTES as usize];
        let mut root = root_hasher(bytes.len() as u64);
        root.update(block_digest(&bytes));
        let root = digest(&root);
        let mut part = fixture.part();
        let mut receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            bytes.len() as u64,
            root,
        )
        .unwrap();
        let cancellation = AtomicBool::new(false);
        assert_eq!(
            receive.write_block_with_sync_hook(
                &mut part,
                0,
                &bytes,
                block_digest(&bytes),
                &cancellation,
                || cancellation.store(true, Ordering::Release)
            ),
            Err(TransferResult::Cancelled)
        );
        assert_eq!(receive.resume_offset(), 0);
        drop(receive);
        let receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            bytes.len() as u64,
            root,
        )
        .unwrap();
        assert_eq!(receive.resume_offset(), 0);
        assert_eq!(part.metadata().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn blocked_checkpoint_disk_thread_keeps_async_timers_live_and_cancel_creates_no_ack() {
        let fixture = Fixture::new();
        let bytes = vec![0x59; FILE_BLOCK_BYTES as usize];
        let mut hash = root_hasher(bytes.len() as u64);
        hash.update(block_digest(&bytes));
        let root = digest(&hash);
        let journal_path = fixture.path("receive.ledger");
        let part_path = fixture.path("receive.part");
        let mut part = fixture.part();
        let mut ledger =
            CheckpointLedger::open_or_create(&journal_path, &mut part, bytes.len() as u64, root)
                .unwrap();
        let cancellation = Arc::new(AtomicBool::new(false));
        let worker_cancellation = cancellation.clone();
        let (ready, entered) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = ledger.write_block_with_sync_hook(
                &mut part,
                0,
                &bytes,
                block_digest(&bytes),
                &worker_cancellation,
                || {
                    // The fixture reaches this gate only after the real OS
                    // data sync. Holding this disk thread models a slow sync
                    // boundary, without blocking Tokio's event consumption.
                    ready.send(()).unwrap();
                    released
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                },
            );
            assert_eq!(ledger.resume_offset(), 0);
            result
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), entered)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            tokio::time::sleep(std::time::Duration::from_millis(10)),
        )
        .await
        .unwrap();
        cancellation.store(true, Ordering::Release);
        assert_eq!(
            fs::metadata(&journal_path).unwrap().len(),
            LEDGER_HEADER_BYTES
        );
        release.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), Err(TransferResult::Cancelled));
        let mut part = OpenOptions::new()
            .read(true)
            .write(true)
            .open(part_path)
            .unwrap();
        let ledger = CheckpointLedger::open_or_create(
            &journal_path,
            &mut part,
            FILE_BLOCK_BYTES as u64,
            root,
        )
        .unwrap();
        assert_eq!(ledger.resume_offset(), 0);
        assert_eq!(part.metadata().unwrap().len(), 0);
    }

    #[test]
    fn changed_source_is_detected_even_when_writer_restores_size_and_modified_time() {
        let fixture = Fixture::new();
        let bytes = sample();
        let source_path = fixture.source(&bytes);
        let modified = fs::metadata(&source_path).unwrap().modified().unwrap();
        let index = SourceIndex::build(
            &source_path,
            &fixture.path("source.index"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        drop(index);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut source = OpenOptions::new().write(true).open(&source_path).unwrap();
        source.write_all(&[0xff]).unwrap();
        source
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        source.sync_all().unwrap();
        drop(source);
        assert!(matches!(
            SourceIndex::open(&source_path, &fixture.path("source.index")),
            Err(TransferResult::SourceChanged)
        ));
    }

    #[cfg(windows)]
    fn basic_info(file: &File) -> windows::Win32::Storage::FileSystem::FILE_BASIC_INFO {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx},
        };
        let mut basic = FILE_BASIC_INFO::default();
        unsafe {
            GetFileInformationByHandleEx(
                HANDLE(file.as_raw_handle()),
                FileBasicInfo,
                (&mut basic as *mut FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        }
        .unwrap();
        basic
    }

    #[cfg(windows)]
    fn overwrite_prefix_and_restore_all_basic_info(
        path: &Path,
        original: windows::Win32::Storage::FileSystem::FILE_BASIC_INFO,
    ) {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{
                FILE_BASIC_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
                FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FileBasicInfo, SetFileInformationByHandle,
            },
        };
        {
            let mut data = OpenOptions::new().write(true).open(path).unwrap();
            data.write_all(&[0xff]).unwrap();
            data.sync_all().unwrap();
        }
        // Restore after closing the data writer, using a separate attribute-only
        // handle, so closing the original writer cannot rewrite the timestamps.
        {
            let metadata = OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES.0 | FILE_WRITE_ATTRIBUTES.0)
                .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
                .open(path)
                .unwrap();
            unsafe {
                SetFileInformationByHandle(
                    HANDLE(metadata.as_raw_handle()),
                    FileBasicInfo,
                    (&original as *const FILE_BASIC_INFO).cast(),
                    std::mem::size_of::<FILE_BASIC_INFO>() as u32,
                )
            }
            .unwrap();
        }
        let actual = basic_info(&File::open(path).unwrap());
        assert_eq!(
            (
                actual.CreationTime,
                actual.LastAccessTime,
                actual.LastWriteTime,
                actual.ChangeTime,
                actual.FileAttributes
            ),
            (
                original.CreationTime,
                original.LastAccessTime,
                original.LastWriteTime,
                original.ChangeTime,
                original.FileAttributes
            ),
            "fixture did not restore every FILE_BASIC_INFO member"
        );
    }

    #[cfg(windows)]
    #[test]
    fn ntfs_usn_rejects_prefix_changes_after_all_basic_file_metadata_is_restored_without_rescanning()
     {
        let fixture = Fixture::new();
        let source_path = fixture.source(&sample());
        let source = open_stable_source(&source_path).unwrap();
        let original = basic_info(&source);
        let original_usn = file_change_token(&source)
            .unwrap()
            .expect("NTFS fixture requires a nonzero per-file journal USN");
        let index_path = fixture.path("source.index");
        let index =
            SourceIndex::build_from_file(source, &index_path, &AtomicBool::new(false), |_| {})
                .unwrap();
        drop(index);
        // The reliable NTFS token path must not read the prefix on an unchanged
        // restart. The fallback callback would fire once per real data block.
        let unchanged = SourceIndex::open_from_file_with_control(
            open_stable_source(&source_path).unwrap(),
            &index_path,
            &AtomicBool::new(false),
            |_| panic!("unchanged NTFS cache rehashed source data"),
        )
        .unwrap();
        drop(unchanged);
        overwrite_prefix_and_restore_all_basic_info(&source_path, original);
        let changed = open_stable_source(&source_path).unwrap();
        let changed_usn = file_change_token(&changed)
            .unwrap()
            .expect("NTFS USN disappeared");
        assert_ne!(original_usn, changed_usn);
        let mut scanned = 0;
        assert!(matches!(
            SourceIndex::open_from_file_with_control(
                changed,
                &index_path,
                &AtomicBool::new(false),
                |bytes| scanned = bytes
            ),
            Err(TransferResult::SourceChanged)
        ));
        assert_eq!(
            scanned, 0,
            "USN change was not detected before reading cached prefix data"
        );
        eprintln!(
            "{}",
            serde_json::json!({"ntfsChangeTokenAvailable":true,"beforeUsn":original_usn.to_string(),"afterUsn":changed_usn.to_string(),"unchangedSourceRevalidationBytes":0,"changedSourceRevalidationBytes":scanned,"allBasicInfoRestored":true,"changedSourceRejected":true})
        );
    }

    #[cfg(windows)]
    #[test]
    fn unsupported_change_tokens_force_cancellable_full_content_revalidation_of_cached_sources() {
        fn unavailable(_: &File) -> Result<Option<i64>, TransferResult> {
            Ok(None)
        }
        let fixture = Fixture::new();
        let data = sample();
        let source_path = fixture.source(&data);
        let source = open_stable_source(&source_path).unwrap();
        let original = basic_info(&source);
        let original_stamp = source_stamp_with_reader(&source, unavailable).unwrap();
        let index_path = fixture.path("source.index");
        let index = SourceIndex::build_with_reader(
            source,
            &index_path,
            unavailable,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        drop(index);
        let mut scanned = 0;
        let unchanged = SourceIndex::open_checked(
            open_stable_source(&source_path).unwrap(),
            &index_path,
            unavailable,
            &AtomicBool::new(false),
            &mut |bytes| scanned = bytes,
        )
        .unwrap();
        assert_eq!(scanned, data.len() as u64);
        drop(unchanged);
        let cancelled = AtomicBool::new(false);
        assert!(matches!(
            SourceIndex::open_checked(
                open_stable_source(&source_path).unwrap(),
                &index_path,
                unavailable,
                &cancelled,
                &mut |_| cancelled.store(true, Ordering::Release)
            ),
            Err(TransferResult::Cancelled)
        ));
        overwrite_prefix_and_restore_all_basic_info(&source_path, original);
        let changed = open_stable_source(&source_path).unwrap();
        assert_eq!(
            source_stamp_with_reader(&changed, unavailable).unwrap(),
            original_stamp,
            "fixture did not eliminate all metadata-only change signals"
        );
        assert!(matches!(
            SourceIndex::open_checked(
                changed,
                &index_path,
                unavailable,
                &AtomicBool::new(false),
                &mut |_| {}
            ),
            Err(TransferResult::SourceChanged)
        ));
        eprintln!(
            "{}",
            serde_json::json!({"changeTokenProvider":"forced-unavailable","unchangedFullRevalidationBytes":scanned,"cancellationObserved":true,"metadataOnlyStampUnchanged":true,"changedSourceRejected":true})
        );
    }

    #[test]
    fn scan_and_final_verification_cancel_between_blocks() {
        let fixture = Fixture::new();
        let source_path = fixture.source(&sample());
        let flag = Arc::new(AtomicBool::new(false));
        let progress_flag = flag.clone();
        assert!(matches!(
            SourceIndex::build(
                &source_path,
                &fixture.path("cancelled.index"),
                &flag,
                move |_| progress_flag.store(true, Ordering::Release)
            ),
            Err(TransferResult::Cancelled)
        ));
        assert!(!fixture.path("cancelled.index").exists());
        let mut source = SourceIndex::build(
            &source_path,
            &fixture.path("source.index"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let mut part = fixture.part();
        let mut receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            source.size(),
            source.content_root(),
        )
        .unwrap();
        while let Some(block) = source.read_block(receive.resume_offset()).unwrap() {
            receive
                .write_block(&mut part, block.offset, &block.bytes, block.hash)
                .unwrap();
        }
        flag.store(false, Ordering::Release);
        assert_eq!(
            receive.verify_all(&mut part, &flag, |_| flag.store(true, Ordering::Release)),
            Err(TransferResult::Cancelled)
        );
    }

    #[test]
    fn empty_files_have_a_real_root_and_no_blocks() {
        let fixture = Fixture::new();
        let source_path = fixture.source(&[]);
        let mut source = SourceIndex::build(
            &source_path,
            &fixture.path("source.index"),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!(source.content_root(), empty_resume_proof(0));
        assert!(source.read_block(0).unwrap().is_none());
        let mut part = fixture.part();
        let mut receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            0,
            source.content_root(),
        )
        .unwrap();
        receive
            .verify_all(&mut part, &AtomicBool::new(false), |_| {})
            .unwrap();
    }

    #[test]
    fn interrupted_source_scan_index_is_rebuilt_but_changed_source_is_not_accepted() {
        let fixture = Fixture::new();
        let source_path = fixture.source(&sample());
        let index_path = fixture.path("source.index");
        fs::write(&index_path, b"DLIDX14\0").unwrap();
        let recovered = SourceIndex::recover_or_build_from_file(
            open_stable_source(&source_path).unwrap(),
            &index_path,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let stamp = recovered.source_identity();
        drop(recovered);
        let original_index = fs::read(&index_path).unwrap();
        let mut source = OpenOptions::new().write(true).open(&source_path).unwrap();
        source.write_all(&[0xff]).unwrap();
        source.sync_all().unwrap();
        drop(source);
        assert!(matches!(
            SourceIndex::recover_or_build_from_file(
                open_stable_source(&source_path).unwrap(),
                &index_path,
                &AtomicBool::new(false),
                |_| {},
            ),
            Err(TransferResult::SourceChanged)
        ));
        assert_eq!(fs::read(&index_path).unwrap(), original_index);
        assert_ne!(
            source_stamp(&File::open(source_path).unwrap()).unwrap().1,
            stamp
        );
    }

    #[test]
    fn disk_budget_includes_proof_growth_and_reduces_only_after_durable_ack() {
        let fixture = Fixture::new();
        let bytes = vec![1; FILE_BLOCK_BYTES as usize * 2];
        let mut root = root_hasher(bytes.len() as u64);
        let hash = block_digest(&bytes[..FILE_BLOCK_BYTES as usize]);
        root.update(hash);
        root.update(hash);
        let mut part = fixture.part();
        let mut receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            bytes.len() as u64,
            digest(&root),
        )
        .unwrap();
        assert_eq!(
            source_index_bytes(bytes.len() as u64).unwrap(),
            INDEX_HEADER_BYTES + 2 * RECORD_BYTES
        );
        assert_eq!(receive.remaining_journal_bytes().unwrap(), 2 * RECORD_BYTES);
        receive
            .write_block(&mut part, 0, &bytes[..FILE_BLOCK_BYTES as usize], hash)
            .unwrap();
        assert_eq!(receive.remaining_journal_bytes().unwrap(), RECORD_BYTES);
        assert_eq!(
            checkpoint_ledger_bytes(1_u64 << 40).unwrap(),
            LEDGER_HEADER_BYTES + blocks(1_u64 << 40) * RECORD_BYTES
        );
        assert_eq!(
            source_index_bytes(MAX_LARGE_FILE_BYTES + 1),
            Err(TransferResult::TooLarge)
        );
    }

    #[test]
    fn noncanonical_blocks_and_changed_content_identity_are_refused() {
        let fixture = Fixture::new();
        let bytes = vec![1; FILE_BLOCK_BYTES as usize];
        let mut root = root_hasher(bytes.len() as u64);
        root.update(block_digest(&bytes));
        let root = digest(&root);
        let mut part = fixture.part();
        let mut receive = CheckpointLedger::open_or_create(
            &fixture.path("receive.ledger"),
            &mut part,
            bytes.len() as u64,
            root,
        )
        .unwrap();
        assert_eq!(
            receive.write_block(&mut part, 0, &bytes[..1], block_digest(&bytes[..1])),
            Err(TransferResult::InvalidData)
        );
        assert_eq!(
            receive.write_block(&mut part, 0, &bytes, [0; 32]),
            Err(TransferResult::InvalidData)
        );
        assert_eq!(receive.resume_offset(), 0);
        drop(receive);
        assert!(matches!(
            CheckpointLedger::open_or_create(
                &fixture.path("receive.ledger"),
                &mut part,
                bytes.len() as u64,
                [1; 32]
            ),
            Err(TransferResult::SourceChanged)
        ));
    }

    #[test]
    fn source_index_reads_a_real_sparse_block_beyond_four_gib_without_truncation() {
        let fixture = Fixture::new();
        let offset =
            (1_u64 << 32).div_ceil(u64::from(FILE_BLOCK_BYTES)) * u64::from(FILE_BLOCK_BYTES);
        let payload = vec![0x87; FILE_BLOCK_BYTES as usize];
        let size = offset + payload.len() as u64;
        let source_path = fixture.path("sparse-source.bin");
        let mut source = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&source_path)
            .unwrap();
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::{
                Foundation::HANDLE, System::IO::DeviceIoControl, System::Ioctl::FSCTL_SET_SPARSE,
            };
            let mut returned = 0;
            unsafe {
                DeviceIoControl(
                    HANDLE(source.as_raw_handle()),
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
        source.set_len(size).unwrap();
        source.seek(SeekFrom::Start(offset)).unwrap();
        source.write_all(&payload).unwrap();
        source.sync_all().unwrap();
        // A writable NTFS handle emits a final USN on close. Production source
        // indices are built from a stable read-only handle after that boundary.
        drop(source);
        let source = open_stable_source(&source_path).unwrap();
        let (_, stamp) = source_stamp(&source).unwrap();
        let mut index = File::create(fixture.path("sparse-source.index")).unwrap();
        index.write_all(INDEX_MAGIC).unwrap();
        index.write_all(&FILE_BLOCK_BYTES.to_le_bytes()).unwrap();
        index.write_all(&[0; 4]).unwrap();
        index.write_all(&size.to_le_bytes()).unwrap();
        index.write_all(&stamp).unwrap();
        index.write_all(&[0; 32]).unwrap();
        let zero_hash = block_digest(&vec![0; FILE_BLOCK_BYTES as usize]);
        let mut root = root_hasher(size);
        // Sparse zero blocks have a known digest. Building this disk index from
        // those proofs exercises u64 positioning without a 4-GiB unit scan;
        // full SourceIndex::build is covered by the heavy integration fixture.
        for _ in 0..offset / u64::from(FILE_BLOCK_BYTES) {
            root.update(zero_hash);
            index.write_all(&zero_hash).unwrap();
            index.write_all(&digest(&root)).unwrap();
        }
        let hash = block_digest(&payload);
        root.update(hash);
        index.write_all(&hash).unwrap();
        index.write_all(&digest(&root)).unwrap();
        index.seek(SeekFrom::Start(56)).unwrap();
        index.write_all(&digest(&root)).unwrap();
        index.sync_all().unwrap();
        drop(index);
        drop(source);
        let mut indexed =
            SourceIndex::open(&source_path, &fixture.path("sparse-source.index")).unwrap();
        assert_eq!(indexed.size(), size);
        let block = indexed.read_block(offset).unwrap().unwrap();
        assert_eq!(block.offset, offset);
        assert!(block.offset > u64::from(u32::MAX));
        assert_eq!(block.bytes, payload);
        assert_eq!(block.hash, hash);
        assert_eq!(indexed.resume_proof(size).unwrap(), indexed.content_root());
        assert!(indexed.read_block(size).unwrap().is_none());
    }
}
