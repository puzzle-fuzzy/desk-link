use crate::controller::{ControllerFileTarget, ControllerManager};
use apps_windows::transfer_task::FileTaskCancellation;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

struct PickerLease(Arc<AtomicBool>);
impl Drop for PickerLease {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
struct PickerScope {
    token: FileTaskCancellation,
    monitor: tauri::async_runtime::JoinHandle<()>,
}
impl Drop for PickerScope {
    fn drop(&mut self) {
        self.token.stopped.store(true, Ordering::Release);
        self.monitor.abort();
    }
}

async fn pick_owned(
    manager: ControllerManager,
    target: ControllerFileTarget,
    directory: bool,
    multiple: bool,
) -> Result<Vec<PathBuf>, String> {
    let lease = PickerLease(manager.claim_file_picker(target, directory)?);
    let token = FileTaskCancellation {
        cancelled: Arc::new(AtomicBool::new(false)),
        stopped: Arc::new(AtomicBool::new(false)),
    };
    let monitor_token = token.clone();
    let monitor = tauri::async_runtime::spawn(async move {
        loop {
            if manager.capture_file_target().ok() != Some(target) {
                monitor_token.stopped.store(true, Ordering::Release);
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let _scope = PickerScope {
        token: token.clone(),
        monitor,
    };
    tauri::async_runtime::spawn_blocking(move || {
        // The lease survives cancellation of the awaiting IPC future and is
        // released only after the actual owned native dialog has closed.
        let _lease = lease;
        apps_windows::transfer_task_dialog::pick_paths(
            if directory {
                "选择要发送到远端的文件夹"
            } else {
                "选择要发送到远端的文件"
            },
            multiple,
            directory,
            token,
        )
    })
    .await
    .map_err(|_| "Windows 选择器未能完成操作。".to_owned())?
    .map_err(|_| "Windows 选择器未能完成操作，请检查当前连接后重试。".to_owned())
}

pub async fn pick_files(
    manager: ControllerManager,
    target: ControllerFileTarget,
) -> Result<Vec<PathBuf>, String> {
    pick_owned(manager, target, false, true).await
}
pub async fn pick_directory(
    manager: ControllerManager,
    target: ControllerFileTarget,
) -> Result<Option<PathBuf>, String> {
    pick_owned(manager, target, true, false)
        .await
        .map(|mut paths| paths.pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_ipc_scope_keeps_picker_lease_until_native_worker_returns() {
        let active = Arc::new(AtomicBool::new(true));
        let lease = PickerLease(active.clone());
        let token = FileTaskCancellation {
            cancelled: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
        };
        let scope = PickerScope {
            token: token.clone(),
            monitor: tauri::async_runtime::spawn(std::future::pending()),
        };
        drop(scope);
        assert!(token.is_stopped());
        assert!(active.load(Ordering::Acquire));
        drop(lease);
        assert!(!active.load(Ordering::Acquire));
    }
}
