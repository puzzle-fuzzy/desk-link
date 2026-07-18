use std::path::PathBuf;

pub fn pick_files() -> Result<Vec<PathBuf>, String> {
    apps_windows::transfer::pick_outgoing_files("选择要发送到远端的文件", true)
        .map_err(|_| "Windows 文件选择器未能完成操作。".to_owned())
}
