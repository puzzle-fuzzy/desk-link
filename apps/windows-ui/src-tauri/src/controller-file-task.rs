//! Native IPC binding for one authenticated connection's file-task service.
//! The owner captures and revalidates its target before calling start; this
//! binding cannot choose a peer or extend the service beyond that connection.
use std::path::PathBuf;

use apps_windows::transfer_task::FileTaskHandle;
use desklink_protocol::{FileTaskAction, TransferId, TransferResult};
use rand_core::{OsRng, RngCore};

#[derive(Clone)]
pub struct FileTaskBinding {
    handle: FileTaskHandle,
}

impl FileTaskBinding {
    pub fn new(handle: FileTaskHandle) -> Self {
        Self { handle }
    }

    pub fn start(&self, path: PathBuf) -> Result<String, String> {
        let mut task_id = [0; 16];
        OsRng
            .try_fill_bytes(&mut task_id)
            .map_err(|_| "暂时无法建立文件任务，请稍后重试。".to_owned())?;
        if task_id == [0; 16] {
            return Err("暂时无法建立文件任务，请稍后重试。".to_owned());
        }
        self.handle.start(path, task_id).map_err(task_error)?;
        Ok(format_task_id(task_id))
    }

    pub fn action(&self, task_id: &str, action: FileTaskAction) -> Result<(), String> {
        self.handle
            .action(parse_task_id(task_id)?, action)
            .map_err(task_error)
    }

    pub fn retry(&self, task_id: &str, item_id: Option<&str>) -> Result<(), String> {
        self.handle
            .retry(
                parse_task_id(task_id)?,
                item_id.map(parse_item_id).transpose()?,
            )
            .map_err(task_error)
    }

    pub fn request_remote(&self, request_id: u64, directory: bool) -> Result<(), String> {
        self.handle
            .request_remote(request_id, directory)
            .map_err(task_error)
    }

    pub fn cancel_remote(&self) -> Result<bool, String> {
        self.handle.cancel_remote().map_err(task_error)
    }
}

pub fn parse_task_id(value: &str) -> Result<TransferId, String> {
    if value.len() != 32 || !value.is_ascii() {
        return Err("文件任务标识无效。".to_owned());
    }
    let mut result = [0; 16];
    for (target, pair) in result.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let high = (pair[0] as char).to_digit(16);
        let low = (pair[1] as char).to_digit(16);
        *target = match (high, low) {
            (Some(high), Some(low)) => ((high << 4) | low) as u8,
            _ => return Err("文件任务标识无效。".to_owned()),
        };
    }
    if result == [0; 16] {
        return Err("文件任务标识无效。".to_owned());
    }
    Ok(result)
}

pub fn format_task_id(value: TransferId) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(32);
    for byte in value {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

pub fn parse_item_id(value: &str) -> Result<u64, String> {
    if value.is_empty()
        || value.len() > 20
        || value.starts_with('0')
        || !value.as_bytes().iter().all(u8::is_ascii_digit)
    {
        return Err("文件任务中的项目标识无效。".to_owned());
    }
    value
        .parse()
        .map_err(|_| "文件任务中的项目标识无效。".to_owned())
}

fn task_error(result: TransferResult) -> String {
    match result {
        TransferResult::Busy => "文件任务尚在处理，请稍后重试。",
        TransferResult::Unsupported => "当前连接尚未启用此项文件传输能力。",
        TransferResult::TooLarge => "文件或目录超过本次连接协商的资源上限。",
        TransferResult::PermissionDenied => "没有访问源文件或接收目录的权限。",
        TransferResult::InsufficientSpace => "接收磁盘空间不足，请清理后重试失败项。",
        TransferResult::SourceChanged => "源文件内容已变化，请重新选择后发送。",
        TransferResult::InvalidData => "文件任务已失效或内容校验失败，请重试。",
        TransferResult::Rejected => "接收端拒绝了本次传输。",
        TransferResult::Cancelled => "文件任务已取消。",
        TransferResult::IoFailed => "文件读写失败，请检查磁盘与目录权限后重试。",
        TransferResult::Completed => "文件任务已完成。",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_identity_round_trips_without_numeric_or_unicode_coercion() {
        let id = [0xff; 16];
        assert_eq!(parse_task_id(&format_task_id(id)), Ok(id));
        assert_eq!(parse_task_id(&"AB".repeat(16)), Ok([0xab; 16]));
        for invalid in [
            "",
            "../task",
            "00000000000000000000000000000000",
            "０１２３４５６７８９abcdef",
            "1234567890abcdef1234567890abcdeg",
        ] {
            assert!(parse_task_id(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn item_identity_preserves_full_u64_and_rejects_noncanonical_input() {
        for expected in [1, 4_294_967_297, 9_007_199_254_740_993, u64::MAX] {
            assert_eq!(parse_item_id(&expected.to_string()), Ok(expected));
        }
        for invalid in [
            "",
            "0",
            "01",
            "+1",
            " 1",
            "1.0",
            "-1",
            "18446744073709551616",
        ] {
            assert!(parse_item_id(invalid).is_err(), "{invalid}");
        }
    }
}
