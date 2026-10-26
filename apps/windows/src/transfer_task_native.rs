//! Windows file-task integration. Native prompts/pickers run on the file actor.
use crate::transfer_task::{FileTaskCancellation, FileTaskOptions, FileTaskService};
use desklink_protocol::{FileTaskMessage, TransferResult};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::{mpsc, watch};

pub fn current_user_service(peer_key: String) -> Result<FileTaskService, TransferResult> {
    let cache_root = crate::storage::local_app_data_path()
        .ok_or(TransferResult::IoFailed)?
        .join("DeskLink")
        .join("file-tasks-v14");
    let receive_parent = crate::storage::downloads_path().ok_or(TransferResult::IoFailed)?;
    FileTaskService::spawn(FileTaskOptions {
        peer_key,
        cache_root,
        receive_parent,
        authorize: Arc::new(confirm_task_offer),
        select: Some(Arc::new(select_source)),
        authorize_source_resume: Some(Arc::new(confirm_source_resume)),
    })
}

#[cfg(windows)]
fn select_source(
    directory: bool,
    token: FileTaskCancellation,
) -> Result<Option<PathBuf>, TransferResult> {
    let title = if directory {
        "选择要发送到远端的文件夹"
    } else {
        "选择要发送到远端的文件"
    };
    crate::transfer_task_dialog::pick_paths(title, false, directory, token)
        .map(|mut paths| paths.pop())
}
#[cfg(not(windows))]
fn select_source(_: bool, _: FileTaskCancellation) -> Result<Option<PathBuf>, TransferResult> {
    Err(TransferResult::Unsupported)
}

#[cfg(windows)]
fn confirm_task_offer(name: &str, directory: bool, token: FileTaskCancellation) -> bool {
    let kind = if directory {
        "文件夹及其所有文件"
    } else {
        "文件"
    };
    let message = format!(
        "已认证的远端希望向此电脑发送{kind}：\n\n{name}\n\n接收后保存到下载目录中的独立 DeskLink 文件夹。现有文件不会被覆盖。中断后再次接收可继续已确认的部分。是否接收？"
    );
    crate::transfer_task_dialog::confirm("DeskLink 接收确认", &message, token)
}
#[cfg(not(windows))]
fn confirm_task_offer(_: &str, _: bool, _: FileTaskCancellation) -> bool {
    false
}

#[cfg(windows)]
fn confirm_source_resume(path: &Path, directory: bool, token: FileTaskCancellation) -> bool {
    let kind = if directory {
        "文件夹及其所有文件"
    } else {
        "文件"
    };
    let message = format!(
        "已认证的远端请求继续接收此前由此电脑发送的{kind}：\n\n{}\n\n允许后将读取并继续发送本地源文件。DeskLink 会核对已有清单和文件内容；未完成的扫描将重新进行。是否允许继续发送？",
        path.display()
    );
    crate::transfer_task_dialog::confirm("DeskLink 恢复发送确认", &message, token)
}
#[cfg(not(windows))]
fn confirm_source_resume(_: &Path, _: bool, _: FileTaskCancellation) -> bool {
    false
}

/// The owner aborts this pump on connection teardown. It never consumes video,
/// audio or input, and prioritizes the latest user control over file blocks.
pub struct FileTaskPump(tokio::task::JoinHandle<()>);
impl Drop for FileTaskPump {
    fn drop(&mut self) {
        self.0.abort();
    }
}
pub fn spawn_pump<F, Fut>(
    mut outgoing: mpsc::Receiver<FileTaskMessage>,
    mut controls: watch::Receiver<Option<([u8; 16], desklink_protocol::FileTaskAction)>>,
    mut send: F,
) -> FileTaskPump
where
    F: FnMut(FileTaskMessage) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), TransferResult>> + Send + 'static,
{
    FileTaskPump(tokio::spawn(async move {
        loop {
            let message = tokio::select! {biased;
                result=controls.changed()=>{if result.is_err(){return}let action=*controls.borrow_and_update();let Some((task_id,action))=action else {continue};FileTaskMessage::Action {task_id,action}},
                message=outgoing.recv()=>{let Some(message)=message else {return};message},
            };
            if !matches!(
                tokio::time::timeout(std::time::Duration::from_secs(30), send(message)).await,
                Ok(Ok(()))
            ) {
                return;
            }
        }
    }))
}
