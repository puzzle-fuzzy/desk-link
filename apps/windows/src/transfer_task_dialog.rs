//! Owned native file-task dialogs. Call only from the blocking file worker.
//!
//! Each operation owns an STA thread. Its timer runs inside the modal dialog's
//! message loop and closes that thread's dialog; COM objects never cross threads.

use std::{
    cell::RefCell,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use desklink_protocol::TransferResult;
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
            Ole::IOleWindow,
            Threading::GetCurrentThreadId,
        },
        UI::{
            Shell::{
                FOS_ALLOWMULTISELECT, FOS_DONTADDTORECENT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM,
                FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, IShellItem,
                SIGDN_FILESYSPATH,
            },
            WindowsAndMessaging::{
                EndDialog, EnumThreadWindows, GetClassNameW, GetWindowTextW,
                GetWindowThreadProcessId, IDNO, IDYES, IsWindowVisible, KillTimer, MB_DEFBUTTON2,
                MB_ICONQUESTION, MB_YESNO, MessageBoxW, PostMessageW, SetTimer, WM_CLOSE,
            },
        },
    },
    core::{BOOL, HRESULT, HSTRING, Interface},
};

use crate::transfer_task::FileTaskCancellation;

const CANCELLED: HRESULT = HRESULT(0x8007_04c7_u32 as i32);
const POLL_MILLISECONDS: u32 = 50;
// The desktop queue admits at most 20 sources at once. Avoid allocating an
// unbounded list if this helper is reused for multiple selection later.
const MAX_SELECTED_PATHS: u32 = 20;
static MODAL_ACTIVE: Mutex<()> = Mutex::new(());
static MODAL_COUNT: AtomicUsize = AtomicUsize::new(0);

struct ModalPermit;
impl ModalPermit {
    fn acquire() -> Result<Self, TransferResult> {
        MODAL_COUNT
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < 2).then_some(count + 1)
            })
            .map(|_| Self)
            .map_err(|_| TransferResult::Busy)
    }
}
impl Drop for ModalPermit {
    fn drop(&mut self) {
        MODAL_COUNT.fetch_sub(1, Ordering::AcqRel);
    }
}

fn wait_modal(
    token: &FileTaskCancellation,
) -> Result<std::sync::MutexGuard<'static, ()>, TransferResult> {
    loop {
        if token.is_cancelled() {
            return Err(TransferResult::Cancelled);
        }
        match MODAL_ACTIVE.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(_)) => return Err(TransferResult::IoFailed),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(20))
            }
        }
    }
}

#[derive(Clone)]
enum ActiveDialog {
    File(IFileOpenDialog, FileTaskCancellation),
    Confirmation(String, FileTaskCancellation),
}

thread_local! {
    static ACTIVE: RefCell<Option<ActiveDialog>> = const { RefCell::new(None) };
    #[cfg(test)]
    static MODAL_STARTED: RefCell<Option<std::sync::mpsc::Sender<()>>> = const { RefCell::new(None) };
    #[cfg(test)]
    static MODAL_INITIAL_FOLDER: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

struct ModalTimer(usize);
impl ModalTimer {
    fn install(active: ActiveDialog) -> Result<Self, TransferResult> {
        ACTIVE.with(|slot| *slot.borrow_mut() = Some(active));
        let id = unsafe { SetTimer(None, 0, POLL_MILLISECONDS, Some(cancel_modal)) };
        if id == 0 {
            ACTIVE.with(|slot| *slot.borrow_mut() = None);
            return Err(TransferResult::IoFailed);
        }
        Ok(Self(id))
    }
}
impl Drop for ModalTimer {
    fn drop(&mut self) {
        let _ = unsafe { KillTimer(None, self.0) };
        ACTIVE.with(|slot| *slot.borrow_mut() = None);
    }
}

unsafe extern "system" fn cancel_modal(_: HWND, _: u32, timer_id: usize, _: u32) {
    // Release the RefCell borrow before Close/EndDialog can pump more messages.
    let active = ACTIVE.with(|slot| slot.try_borrow().ok().and_then(|value| value.clone()));
    match active {
        Some(ActiveDialog::File(dialog, token)) => {
            // Close() is supported from IFileDialogEvents callbacks, and can
            // return S_OK without closing from an ordinary TimerProc. Close the
            // exact owned window instead, after Show has made it visible.
            // Chromium uses this same HWND close for external callbacks:
            // ui/shell_dialogs/auto_close_dialog_event_handler_win.cc.
            let window = dialog
                .cast::<IOleWindow>()
                .ok()
                .and_then(|value| unsafe { value.GetWindow() }.ok());
            if let Some(window) =
                window.filter(|window| unsafe { IsWindowVisible(*window) }.as_bool())
            {
                if unsafe { GetWindowThreadProcessId(window, None) }
                    != unsafe { GetCurrentThreadId() }
                {
                    return;
                }
                modal_started();
                if token.is_cancelled()
                    && unsafe { PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0)) }.is_ok()
                {
                    let _ = unsafe { KillTimer(None, timer_id) };
                }
            }
        }
        Some(ActiveDialog::Confirmation(_, _)) => {
            let _ = unsafe {
                EnumThreadWindows(
                    GetCurrentThreadId(),
                    Some(close_confirmation),
                    LPARAM(timer_id as isize),
                )
            };
        }
        _ => {}
    }
}

fn modal_started() {
    #[cfg(test)]
    MODAL_STARTED.with(|slot| {
        if let Some(notify) = slot.borrow_mut().take() {
            let _ = notify.send(());
        }
    });
}

unsafe extern "system" fn close_confirmation(window: HWND, timer: LPARAM) -> BOOL {
    if unsafe { GetWindowThreadProcessId(window, None) } != unsafe { GetCurrentThreadId() } {
        return BOOL(1);
    }
    let mut class = [0_u16; 32];
    let length = unsafe { GetClassNameW(window, &mut class) };
    if length <= 0
        || class[..length as usize] != "#32770".encode_utf16().collect::<Vec<_>>()
        || !unsafe { IsWindowVisible(window) }.as_bool()
    {
        return BOOL(1);
    }
    let mut caption = [0_u16; 256];
    let length = unsafe { GetWindowTextW(window, &mut caption) };
    let owned = ACTIVE.with(|slot| {
        let Ok(active) = slot.try_borrow() else {
            return None;
        };
        match active.as_ref() {
            Some(ActiveDialog::Confirmation(expected, token)) => {
                if length >= 0
                    && caption[..length as usize] == expected.encode_utf16().collect::<Vec<_>>()
                {
                    Some(token.is_cancelled())
                } else {
                    None
                }
            }
            _ => None,
        }
    });
    if let Some(cancelled) = owned {
        modal_started();
        if cancelled && unsafe { EndDialog(window, IDNO.0 as isize) }.is_ok() {
            let _ = unsafe { KillTimer(None, timer.0 as usize) };
        }
        return BOOL(0);
    }
    BOOL(1)
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

pub fn pick_paths(
    title: &str,
    multiple: bool,
    directory: bool,
    token: FileTaskCancellation,
) -> Result<Vec<PathBuf>, TransferResult> {
    if token.is_cancelled() {
        return Ok(Vec::new());
    }
    let title = title.to_owned();
    let permit = ModalPermit::acquire()?;
    std::thread::Builder::new()
        .name("desklink-file-picker".into())
        .spawn(move || {
            let _permit = permit;
            let _active = match wait_modal(&token) {
                Err(TransferResult::Cancelled) => return Ok(Vec::new()),
                result => result?,
            };
            pick_paths_sta(&title, multiple, directory, token)
        })
        .map_err(|_| TransferResult::IoFailed)?
        .join()
        .map_err(|_| TransferResult::IoFailed)?
}

fn pick_paths_sta(
    title: &str,
    multiple: bool,
    directory: bool,
    token: FileTaskCancellation,
) -> Result<Vec<PathBuf>, TransferResult> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .map_err(|_| TransferResult::IoFailed)?;
    let _apartment = Apartment;
    let dialog: IFileOpenDialog = unsafe {
        CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|_| TransferResult::IoFailed)?
    };
    let options = unsafe { dialog.GetOptions() }.map_err(|_| TransferResult::IoFailed)?
        | FOS_FORCEFILESYSTEM
        | FOS_FILEMUSTEXIST
        | FOS_PATHMUSTEXIST
        | FOS_DONTADDTORECENT;
    unsafe {
        dialog
            .SetOptions(if directory {
                options | FOS_PICKFOLDERS
            } else if multiple {
                options | FOS_ALLOWMULTISELECT
            } else {
                options
            })
            .map_err(|_| TransferResult::IoFailed)?;
        dialog
            .SetTitle(&HSTRING::from(title))
            .map_err(|_| TransferResult::IoFailed)?;
    }
    #[cfg(test)]
    if let Some(folder) = MODAL_INITIAL_FOLDER.with(|slot| slot.borrow().clone()) {
        use windows::{Win32::UI::Shell::SHCreateItemFromParsingName, core::GUID};
        let folder: IShellItem =
            unsafe { SHCreateItemFromParsingName(&HSTRING::from(folder.as_os_str()), None) }
                .map_err(|_| TransferResult::IoFailed)?;
        unsafe {
            dialog
                .SetClientGuid(&GUID::from_u128(0xbca7555a_e986_4fa0_9eb5_3a65b72a5312))
                .map_err(|_| TransferResult::IoFailed)?;
            dialog
                .SetDefaultFolder(&folder)
                .map_err(|_| TransferResult::IoFailed)?;
            dialog
                .SetFolder(&folder)
                .map_err(|_| TransferResult::IoFailed)?;
        }
    }
    let _timer = ModalTimer::install(ActiveDialog::File(dialog.clone(), token.clone()))?;
    if token.is_cancelled() {
        return Ok(Vec::new());
    }
    if let Err(error) = unsafe { dialog.Show(None) } {
        return if error.code() == CANCELLED {
            Ok(Vec::new())
        } else {
            Err(TransferResult::IoFailed)
        };
    }
    // A simultaneous click and cancellation must never admit selected data.
    if token.is_cancelled() {
        return Ok(Vec::new());
    }
    let item_path = |item: &IShellItem| {
        let path = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
            .map_err(|_| TransferResult::IoFailed)?;
        let result = unsafe { path.to_string() }
            .map(PathBuf::from)
            .map_err(|_| TransferResult::InvalidData);
        unsafe { CoTaskMemFree(Some(path.as_ptr().cast())) };
        result
    };
    let paths = if multiple {
        let items = unsafe { dialog.GetResults() }.map_err(|_| TransferResult::IoFailed)?;
        let count = unsafe { items.GetCount() }.map_err(|_| TransferResult::IoFailed)?;
        if count > MAX_SELECTED_PATHS {
            return Err(TransferResult::TooLarge);
        }
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let item = unsafe { items.GetItemAt(index) }.map_err(|_| TransferResult::IoFailed)?;
            paths.push(item_path(&item)?);
        }
        paths
    } else {
        let item = unsafe { dialog.GetResult() }.map_err(|_| TransferResult::IoFailed)?;
        vec![item_path(&item)?]
    };
    if token.is_cancelled() {
        Ok(Vec::new())
    } else {
        Ok(paths)
    }
}

pub(crate) fn confirm(title: &str, message: &str, token: FileTaskCancellation) -> bool {
    if token.is_cancelled() {
        return false;
    }
    let (title, message) = (title.to_owned(), message.to_owned());
    let Ok(permit) = ModalPermit::acquire() else {
        return false;
    };
    std::thread::Builder::new()
        .name("desklink-file-confirm".into())
        .spawn(move || {
            let _permit = permit;
            let Ok(_active) = wait_modal(&token) else {
                return false;
            };
            confirm_sta(&title, &message, token)
        })
        .ok()
        .and_then(|thread| thread.join().ok())
        .unwrap_or(false)
}

fn confirm_sta(title: &str, message: &str, token: FileTaskCancellation) -> bool {
    if unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_err() {
        return false;
    }
    let _apartment = Apartment;
    let Ok(_timer) =
        ModalTimer::install(ActiveDialog::Confirmation(title.to_owned(), token.clone()))
    else {
        return false;
    };
    if token.is_cancelled() {
        return false;
    }
    let result = unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            &HSTRING::from(title),
            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
        )
    };
    result == IDYES && !token.is_cancelled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };

    fn token() -> FileTaskCancellation {
        FileTaskCancellation {
            cancelled: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    struct TestFolder(PathBuf);
    impl TestFolder {
        fn new() -> Self {
            let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap();
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = workspace
                .join(".build")
                .join(format!("native-dialog-{}-{stamp}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestFolder {
        fn drop(&mut self) {
            std::fs::remove_dir(&self.0).unwrap();
        }
    }

    // Actual modal Windows dialogs, closed through their own timer/message loop.
    // No SendInput, keyboard/mouse synthesis, or cross-thread COM is involved.
    #[test]
    fn native_dialogs_release_on_cancel_and_stop_without_selected_data() {
        for (directory, stopped) in [(false, false), (true, true)] {
            let folder = TestFolder::new();
            let initial_folder = folder.0.clone();
            let token = token();
            let signal = if stopped {
                token.stopped.clone()
            } else {
                token.cancelled.clone()
            };
            let (send, receive) = mpsc::channel();
            let (opened, modal) = mpsc::channel();
            let started = Instant::now();
            let worker = std::thread::spawn(move || {
                MODAL_STARTED.with(|slot| *slot.borrow_mut() = Some(opened));
                MODAL_INITIAL_FOLDER.with(|slot| *slot.borrow_mut() = Some(initial_folder));
                let result =
                    pick_paths_sta("DeskLink 取消测试：不需要选择", false, directory, token);
                let _ = send.send(result);
            });
            modal
                .recv_timeout(Duration::from_secs(8))
                .expect("actual native chooser must start its modal message loop");
            std::thread::sleep(Duration::from_millis(100));
            signal.store(true, Ordering::Release);
            assert_eq!(
                receive
                    .recv_timeout(Duration::from_secs(8))
                    .expect("native chooser must close within eight seconds"),
                Ok(Vec::new())
            );
            worker.join().unwrap();
            assert!(started.elapsed() < Duration::from_secs(8));
        }
        for stopped in [false, true] {
            let token = token();
            let signal = if stopped {
                token.stopped.clone()
            } else {
                token.cancelled.clone()
            };
            let (send, receive) = mpsc::channel();
            let (opened, modal) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                MODAL_STARTED.with(|slot| *slot.borrow_mut() = Some(opened));
                let approved = confirm_sta(
                    "DeskLink 取消测试确认",
                    "此测试不会发送或接收文件。无需操作，窗口会自动关闭。",
                    token,
                );
                let _ = send.send(approved);
            });
            modal
                .recv_timeout(Duration::from_secs(8))
                .expect("actual confirmation must start its modal message loop");
            std::thread::sleep(Duration::from_millis(100));
            signal.store(true, Ordering::Release);
            assert!(
                !receive
                    .recv_timeout(Duration::from_secs(8))
                    .expect("native confirmation must close within eight seconds")
            );
            worker.join().unwrap();
        }
    }

    #[test]
    fn already_cancelled_dialog_never_admits_source_or_consent() {
        let token = token();
        token.cancelled.store(true, Ordering::Release);
        assert_eq!(
            pick_paths("DeskLink", false, false, token.clone()),
            Ok(Vec::new())
        );
        assert!(!confirm("DeskLink", "", token));
    }

    #[test]
    fn modal_gate_bounds_pending_and_cancels_waiter() {
        let active_permit = ModalPermit::acquire().unwrap();
        let active = MODAL_ACTIVE.lock().unwrap();
        let queued_permit = ModalPermit::acquire().unwrap();
        assert!(matches!(ModalPermit::acquire(), Err(TransferResult::Busy)));
        let token = token();
        let stop = token.stopped.clone();
        let (send, receive) = mpsc::channel();
        let queued = std::thread::spawn(move || {
            let _permit = queued_permit;
            let result = wait_modal(&token).map(|_| ());
            send.send(result).unwrap();
        });
        assert!(receive.recv_timeout(Duration::from_millis(50)).is_err());
        stop.store(true, Ordering::Release);
        assert_eq!(
            receive.recv_timeout(Duration::from_secs(1)).unwrap(),
            Err(TransferResult::Cancelled)
        );
        queued.join().unwrap();
        drop(active);
        drop(active_permit);
        assert_eq!(MODAL_COUNT.load(Ordering::Acquire), 0);
    }
}
