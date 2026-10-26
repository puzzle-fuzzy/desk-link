//! Windows handle-relative filesystem operations. Every child name is one
//! validated component; RootDirectory is an already opened directory object.
//! This avoids re-traversing mutable reparse points through an absolute path.

use desklink_protocol::{TransferResult, is_valid_transfer_path_segment};
use std::{
    cell::UnsafeCell,
    ffi::{OsStr, c_void},
    fs::{File, OpenOptions},
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf},
};
use windows::Win32::{
    Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE},
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO,
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
    },
};

const SYNCHRONIZE: u32 = 0x0010_0000;
const FILE_OPEN: u32 = 1;
const FILE_CREATE: u32 = 2;
const FILE_DIRECTORY_FILE: u32 = 1;
const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
const FILE_SYNCHRONOUS_IO_NONALERT: u32 = 0x20;
const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const OBJ_CASE_INSENSITIVE: u32 = 0x40;
const OBJ_DONT_REPARSE: u32 = 0x1000;
const FILE_RENAME_INFORMATION: u32 = 10;
const FILE_DISPOSITION_INFORMATION: u32 = 13;
const STATUS_PENDING: i32 = 0x103;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}
#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root: HANDLE,
    name: *mut UnicodeString,
    attributes: u32,
    security: *mut c_void,
    quality: *mut c_void,
}
#[repr(C)]
struct IoStatus {
    status: usize,
    information: usize,
}
#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtCreateFile(
        file: *mut HANDLE,
        access: u32,
        attributes: *const ObjectAttributes,
        status: *mut IoStatus,
        allocation: *const i64,
        file_attributes: u32,
        share: u32,
        disposition: u32,
        options: u32,
        ea: *const c_void,
        ea_length: u32,
    ) -> i32;
    fn NtSetInformationFile(
        file: HANDLE,
        status: *mut IoStatus,
        information: *const c_void,
        length: u32,
        class: u32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
    fn NtWaitForSingleObject(file: HANDLE, alertable: u8, timeout: *const i64) -> i32;
}

fn result(status: i32) -> Result<(), TransferResult> {
    if status == 0 {
        return Ok(());
    }
    if status == 0x104 {
        return Err(TransferResult::PermissionDenied);
    }
    let code = unsafe { RtlNtStatusToDosError(status) };
    Err(match code {
        32 | 33 => TransferResult::Busy,
        5 | 681 | 1920 | 1921 | 4393..=4395 => TransferResult::PermissionDenied,
        80 | 183 => TransferResult::Busy,
        _ => TransferResult::IoFailed,
    })
}

fn completed(file: HANDLE, status: &UnsafeCell<IoStatus>, code: i32) -> Result<(), TransferResult> {
    // Returned handles use SYNCHRONOUS_IO_NONALERT. Keep the status block alive
    // even if a filesystem reports pending completion rather than completing
    // the request inside the syscall.
    if code == STATUS_PENDING {
        result(unsafe { NtWaitForSingleObject(file, 0, std::ptr::null()) })?;
        result(
            unsafe { std::ptr::read_volatile(std::ptr::addr_of!((*status.get()).status)) } as u32
                as i32,
        )
    } else {
        result(code)
    }
}

fn name_wide(name: &OsStr) -> Result<Vec<u16>, TransferResult> {
    let text = name.to_str().ok_or(TransferResult::InvalidData)?;
    if !is_valid_transfer_path_segment(text) {
        return Err(TransferResult::InvalidData);
    }
    Ok(name.encode_wide().collect())
}

pub(crate) fn check_directory(file: &File) -> Result<(), TransferResult> {
    let metadata = file.metadata().map_err(super::io_result)?;
    if !metadata.is_dir() || super::is_link_or_reparse(&metadata) {
        Err(TransferResult::PermissionDenied)
    } else {
        Ok(())
    }
}

fn open_relative(
    parent: &File,
    name: &OsStr,
    directory: bool,
    create: bool,
    write: bool,
    check_parent: bool,
) -> Result<File, TransferResult> {
    open_relative_with_hook(parent, name, directory, create, write, check_parent, || {})
}

fn open_relative_with_hook(
    parent: &File,
    name: &OsStr,
    directory: bool,
    create: bool,
    write: bool,
    check_parent: bool,
    before_io: impl FnOnce(),
) -> Result<File, TransferResult> {
    if check_parent {
        check_directory(parent)?;
    }
    let mut name = name_wide(name)?;
    let mut unicode = UnicodeString {
        length: (name.len() * 2) as u16,
        maximum_length: (name.len() * 2) as u16,
        buffer: name.as_mut_ptr(),
    };
    let attributes = ObjectAttributes {
        length: std::mem::size_of::<ObjectAttributes>() as u32,
        root: HANDLE(parent.as_raw_handle()),
        name: &mut unicode,
        attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        security: std::ptr::null_mut(),
        quality: std::ptr::null_mut(),
    };
    let status = UnsafeCell::new(IoStatus {
        status: 0,
        information: 0,
    });
    let mut handle = HANDLE::default();
    let access = if directory {
        FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0 | SYNCHRONIZE
    } else {
        GENERIC_READ.0 | SYNCHRONIZE | if write { GENERIC_WRITE.0 | DELETE.0 } else { 0 }
    };
    let share = if directory {
        FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0
    } else {
        FILE_SHARE_READ.0
    };
    let options = FILE_OPEN_REPARSE_POINT
        | FILE_SYNCHRONOUS_IO_NONALERT
        | if directory {
            FILE_DIRECTORY_FILE
        } else {
            FILE_NON_DIRECTORY_FILE
        };
    before_io();
    let rc = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &attributes,
            status.get(),
            std::ptr::null(),
            0x80,
            share,
            if create { FILE_CREATE } else { FILE_OPEN },
            options,
            std::ptr::null(),
            0,
        )
    };
    completed(handle, &status, rc)?;
    if handle.is_invalid() {
        return Err(TransferResult::IoFailed);
    }
    let file = unsafe { File::from_raw_handle(handle.0) };
    let metadata = file.metadata().map_err(super::io_result)?;
    if metadata.is_dir() != directory || super::is_link_or_reparse(&metadata) {
        return Err(TransferResult::PermissionDenied);
    }
    if write && !directory {
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
            .map_err(|_| TransferResult::IoFailed)?;
        if information.nNumberOfLinks != 1 {
            return Err(TransferResult::PermissionDenied);
        }
    }
    if check_parent && check_directory(parent).is_err() {
        if create && !directory {
            let _ = delete_owned(&file);
        }
        return Err(TransferResult::PermissionDenied);
    }
    Ok(file)
}

pub(crate) fn open_dir(parent: &File, name: &OsStr) -> Result<File, TransferResult> {
    open_relative(parent, name, true, false, false, true)
}
pub(crate) fn create_dir(parent: &File, name: &OsStr) -> Result<File, TransferResult> {
    open_relative(parent, name, true, true, false, true)
}
pub(crate) fn create_file(parent: &File, name: &OsStr) -> Result<File, TransferResult> {
    open_relative(parent, name, false, true, true, true)
}
pub(crate) fn open_file(parent: &File, name: &OsStr, write: bool) -> Result<File, TransferResult> {
    open_relative(parent, name, false, false, write, true)
}

pub(crate) fn pin_chain(path: &Path) -> Result<Vec<File>, TransferResult> {
    pin_chain_inner(path, false)
}

pub(crate) fn prepare_chain(path: &Path) -> Result<Vec<File>, TransferResult> {
    pin_chain_inner(path, true)
}

fn pin_chain_inner(path: &Path, create: bool) -> Result<Vec<File>, TransferResult> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(TransferResult::InvalidData);
    }
    let mut current = PathBuf::new();
    let mut pins = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => current.push(component.as_os_str()),
            Component::RootDir => {
                current.push(component.as_os_str());
                let file = OpenOptions::new()
                    .access_mode(FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0)
                    .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
                    .open(&current)
                    .map_err(super::io_result)?;
                check_directory(&file)?;
                pins.push(file);
            }
            Component::Normal(name) => {
                let parent = pins.last().ok_or(TransferResult::InvalidData)?;
                let file = match open_dir(parent, name) {
                    Ok(file) => file,
                    Err(TransferResult::IoFailed) if create => match create_dir(parent, name) {
                        Ok(file) => file,
                        Err(TransferResult::Busy) => open_dir(parent, name)?,
                        Err(error) => return Err(error),
                    },
                    Err(error) => return Err(error),
                };
                pins.push(file);
                current.push(name);
            }
            Component::CurDir => {}
            Component::ParentDir => return Err(TransferResult::InvalidData),
        }
    }
    if pins.is_empty() {
        Err(TransferResult::InvalidData)
    } else {
        check_chain(&pins)?;
        Ok(pins)
    }
}

pub(crate) fn check_chain(pins: &[File]) -> Result<(), TransferResult> {
    for pin in pins {
        check_directory(pin)?;
    }
    Ok(())
}

pub(crate) fn create_file_path(path: &Path) -> Result<File, TransferResult> {
    open_file_path_inner(path, true, true)
}

pub(crate) fn open_file_path(path: &Path, write: bool) -> Result<File, TransferResult> {
    open_file_path_inner(path, write, false)
}

fn open_file_path_inner(path: &Path, write: bool, create: bool) -> Result<File, TransferResult> {
    let path = absolute(path)?;
    let parent = path.parent().ok_or(TransferResult::InvalidData)?;
    let name = path.file_name().ok_or(TransferResult::InvalidData)?;
    let pins = pin_chain(parent)?;
    check_chain(&pins)?;
    let file = open_relative(
        pins.last().ok_or(TransferResult::InvalidData)?,
        name,
        false,
        create,
        write,
        true,
    )?;
    if let Err(error) = check_chain(&pins) {
        if create {
            let _ = delete_owned(&file);
        }
        return Err(error);
    }
    Ok(file)
}

/// Publish this exact, still-owned writer object. Reopening a temporary path
/// after its writer closes would allow a replacement object to be published.
pub(crate) fn rename_file_path(
    file: &File,
    destination: &Path,
    replace: bool,
) -> Result<(), TransferResult> {
    let metadata = file.metadata().map_err(super::io_result)?;
    if !metadata.is_file() || super::is_link_or_reparse(&metadata) {
        return Err(TransferResult::PermissionDenied);
    }
    let destination = absolute(destination)?;
    let target_pins = pin_chain(destination.parent().ok_or(TransferResult::InvalidData)?)?;
    check_chain(&target_pins)?;
    rename(
        file,
        target_pins.last().ok_or(TransferResult::InvalidData)?,
        destination.file_name().ok_or(TransferResult::InvalidData)?,
        replace,
    )?;
    check_chain(&target_pins)
}

fn absolute(path: &Path) -> Result<PathBuf, TransferResult> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        Ok(std::env::current_dir()
            .map_err(super::io_result)?
            .join(path))
    }
}

pub(crate) fn rename(
    file: &File,
    parent: &File,
    name: &OsStr,
    replace: bool,
) -> Result<(), TransferResult> {
    rename_with_hook(file, parent, name, replace, || {})
}

fn rename_with_hook(
    file: &File,
    parent: &File,
    name: &OsStr,
    replace: bool,
    before_io: impl FnOnce(),
) -> Result<(), TransferResult> {
    check_directory(parent)?;
    before_io();
    rename_unchecked_parent(file, parent, name, replace)?;
    check_directory(parent)
}

fn rename_unchecked_parent(
    file: &File,
    parent: &File,
    name: &OsStr,
    replace: bool,
) -> Result<(), TransferResult> {
    let name = name_wide(name)?;
    let offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let bytes = offset + (name.len() + 1) * 2;
    let mut buffer = vec![0_u64; bytes.div_ceil(8)];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    let status = UnsafeCell::new(IoStatus {
        status: 0,
        information: 0,
    });
    unsafe {
        (*info).Anonymous.ReplaceIfExists = replace;
        (*info).RootDirectory = HANDLE(parent.as_raw_handle());
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            buffer.as_mut_ptr().cast::<u8>().add(offset).cast::<u16>(),
            name.len(),
        );
        let code = NtSetInformationFile(
            HANDLE(file.as_raw_handle()),
            status.get(),
            info.cast(),
            bytes as u32,
            FILE_RENAME_INFORMATION,
        );
        completed(HANDLE(file.as_raw_handle()), &status, code)
    }
}

pub(crate) fn delete_owned(file: &File) -> Result<(), TransferResult> {
    let status = UnsafeCell::new(IoStatus {
        status: 0,
        information: 0,
    });
    let delete: u8 = 1;
    unsafe {
        let code = NtSetInformationFile(
            HANDLE(file.as_raw_handle()),
            status.get(),
            (&delete as *const u8).cast(),
            1,
            FILE_DISPOSITION_INFORMATION,
        );
        completed(HANDLE(file.as_raw_handle()), &status, code)
    }
}

pub(crate) fn delete_file(file: &File) -> Result<(), TransferResult> {
    delete_owned(file)
}

pub(crate) fn delete_path(path: &Path) -> Result<(), TransferResult> {
    let file = open_file_path(path, true)?;
    delete_owned(&file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::{OsRng, RngCore};
    use std::{fs, io::Write};

    fn fixture() -> PathBuf {
        let mut id = [0; 16];
        OsRng.fill_bytes(&mut id);
        let root = std::env::temp_dir().join(format!(
            "desklink-native-{}",
            id.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    fn mutate_junction(root: &Path, target: &Path) -> windows::core::Result<()> {
        use windows::Win32::{
            Storage::FileSystem::FILE_SHARE_DELETE,
            System::{IO::DeviceIoControl, Ioctl::FSCTL_SET_REPARSE_POINT},
        };
        let printable = target.to_string_lossy().to_string();
        let substitute: Vec<u16> = format!(r"\??\{printable}").encode_utf16().collect();
        let print: Vec<u16> = printable.encode_utf16().collect();
        let mut data = Vec::new();
        data.extend(0xA0000003_u32.to_le_bytes());
        data.extend((8_u16 + (substitute.len() as u16 + print.len() as u16 + 2) * 2).to_le_bytes());
        data.extend(0_u16.to_le_bytes());
        data.extend(0_u16.to_le_bytes());
        data.extend((substitute.len() as u16 * 2).to_le_bytes());
        data.extend(((substitute.len() as u16 + 1) * 2).to_le_bytes());
        data.extend((print.len() as u16 * 2).to_le_bytes());
        for value in substitute.into_iter().chain([0]).chain(print).chain([0]) {
            data.extend(value.to_le_bytes());
        }
        let writer = OpenOptions::new()
            .access_mode(GENERIC_WRITE.0)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(root)
            .unwrap();
        let mut returned = 0;
        unsafe {
            DeviceIoControl(
                HANDLE(writer.as_raw_handle()),
                FSCTL_SET_REPARSE_POINT,
                Some(data.as_ptr().cast()),
                data.len() as u32,
                None,
                0,
                Some(&mut returned),
                None,
            )
        }
    }

    fn clear_junction(root: &Path) {
        use windows::Win32::{
            Storage::FileSystem::FILE_SHARE_DELETE,
            System::{IO::DeviceIoControl, Ioctl::FSCTL_DELETE_REPARSE_POINT},
        };
        let writer = OpenOptions::new()
            .access_mode(GENERIC_WRITE.0)
            .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0 | FILE_SHARE_DELETE.0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0 | FILE_FLAG_BACKUP_SEMANTICS.0)
            .open(root)
            .unwrap();
        let mut data = Vec::new();
        data.extend(0xA0000003_u32.to_le_bytes());
        data.extend(0_u32.to_le_bytes());
        let mut returned = 0;
        unsafe {
            DeviceIoControl(
                HANDLE(writer.as_raw_handle()),
                FSCTL_DELETE_REPARSE_POINT,
                Some(data.as_ptr().cast()),
                data.len() as u32,
                None,
                0,
                Some(&mut returned),
                None,
            )
        }
        .unwrap();
    }

    #[test]
    fn handle_relative_child_create_never_follows_mutated_parent_junction() {
        let root = fixture();
        let outside = fixture();
        fs::write(outside.join("untouched"), b"outside").unwrap();
        let pins = pin_chain(&root).unwrap();
        let parent = pins.last().unwrap();
        mutate_junction(&root, &outside).unwrap();
        // Simulate mutation immediately after the userspace parent check.
        let file = open_relative(parent, OsStr::new("probe"), false, true, true, false);
        let outside_written = outside.join("probe").exists();
        if let Ok(mut file) = file {
            file.write_all(b"owned original").unwrap();
            delete_owned(&file).unwrap();
        }
        clear_junction(&root);
        assert_eq!(fs::read(outside.join("untouched")).unwrap(), b"outside");
        drop(pins);
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
        assert!(
            !outside_written,
            "relative create followed a mutated parent outside its root"
        );
    }

    #[test]
    fn create_race_after_parent_check_is_rejected_and_leaves_no_outside_or_invisible_file() {
        let root = fixture();
        let outside = fixture();
        let pins = pin_chain(&root).unwrap();
        let parent = pins.last().unwrap();
        let created =
            open_relative_with_hook(parent, OsStr::new("probe"), false, true, true, true, || {
                mutate_junction(&root, &outside).unwrap()
            });
        let diagnostic = format!("{:?}", created.as_ref().map(|_| ()));
        let rejected = matches!(created, Err(TransferResult::PermissionDenied));
        if let Ok(file) = created {
            delete_owned(&file).unwrap();
        }
        let outside_written = outside.join("probe").exists();
        clear_junction(&root);
        let invisible_file_left = root.join("probe").exists();
        drop(pins);
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
        assert!(
            rejected && !outside_written && !invisible_file_left,
            "{diagnostic};outside={outside_written};invisible={invisible_file_left}"
        );
    }

    #[test]
    fn publish_race_to_empty_reparse_target_fails_without_overwriting_outside_file() {
        let source = fixture();
        let target = fixture();
        let outside = fixture();
        fs::write(outside.join("final"), b"outside untouched").unwrap();
        let source_pins = pin_chain(&source).unwrap();
        let target_pins = pin_chain(&target).unwrap();
        let mut file = create_file(source_pins.last().unwrap(), OsStr::new("part")).unwrap();
        file.write_all(b"owned content").unwrap();
        file.sync_all().unwrap();
        let result = rename_with_hook(
            &file,
            target_pins.last().unwrap(),
            OsStr::new("final"),
            true,
            || mutate_junction(&target, &outside).unwrap(),
        );
        let outside_bytes = fs::read(outside.join("final")).unwrap();
        delete_owned(&file).unwrap();
        drop(file);
        clear_junction(&target);
        let invisible_file_left = target.join("final").exists();
        drop(source_pins);
        drop(target_pins);
        fs::remove_dir_all(&source).unwrap();
        fs::remove_dir_all(&target).unwrap();
        fs::remove_dir_all(&outside).unwrap();
        assert_eq!(outside_bytes, b"outside untouched");
        assert_eq!(result, Err(TransferResult::PermissionDenied));
        assert!(!invisible_file_left);
    }

    #[test]
    fn handle_relative_publish_does_not_follow_reparse_mutation_and_keeps_outside_file() {
        let root = fixture();
        let outside = fixture();
        fs::write(outside.join("final"), b"outside untouched").unwrap();
        let pins = pin_chain(&root).unwrap();
        let parent = pins.last().unwrap();
        let mut file = create_file(parent, OsStr::new("part")).unwrap();
        file.write_all(b"inside").unwrap();
        file.sync_all().unwrap();
        let mutation = mutate_junction(&root, &outside);
        let published = rename_unchecked_parent(&file, parent, OsStr::new("final"), true);
        eprintln!("nonempty junction mutation={mutation:?}; anchored publish={published:?}");
        let outside_bytes = fs::read(outside.join("final")).unwrap();
        delete_owned(&file).unwrap();
        drop(file);
        if mutation.is_ok() {
            clear_junction(&root);
        }
        drop(pins);
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&outside).unwrap();
        assert_eq!(
            outside_bytes, b"outside untouched",
            "publish escaped root; mutation={mutation:?},publish={published:?}"
        );
    }

    #[test]
    fn handle_relative_create_publish_and_delete_work_without_parent_path_reopen() {
        let mut id = [0; 16];
        OsRng.fill_bytes(&mut id);
        let root = std::env::temp_dir().join(format!(
            "desklink-native-{}",
            id.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        fs::create_dir(&root).unwrap();
        let pins = pin_chain(&root).unwrap();
        let parent = pins.last().unwrap();
        let mut file = create_file(parent, OsStr::new("part")).unwrap();
        file.write_all(b"relative anchored").unwrap();
        file.sync_all().unwrap();
        rename(&file, parent, OsStr::new("final"), false).unwrap();
        assert_eq!(fs::read(root.join("final")).unwrap(), b"relative anchored");
        delete_owned(&file).unwrap();
        drop(file);
        assert!(!root.join("final").exists());
        drop(pins);
        fs::remove_dir(&root).unwrap();
    }

    #[test]
    fn write_resume_refuses_hardlinked_file_without_changing_other_name() {
        let root = fixture();
        let outside = fixture();
        fs::write(outside.join("original"), b"must stay unchanged").unwrap();
        fs::hard_link(outside.join("original"), root.join("part")).unwrap();
        let opened = open_file_path(&root.join("part"), true);
        let rejected = matches!(opened, Err(TransferResult::PermissionDenied));
        drop(opened);
        assert_eq!(
            fs::read(outside.join("original")).unwrap(),
            b"must stay unchanged"
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
        assert!(rejected);
    }
}
