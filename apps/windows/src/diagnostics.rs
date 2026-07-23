use std::{
    collections::VecDeque,
    fs,
    fs::OpenOptions,
    io,
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use desklink_protocol::H264Profile;
use serde::Deserialize;

use crate::storage::{downloads_path, local_app_data_path};

use thiserror::Error;

use crate::runtime::HostLifecycleEvent;

const DEFAULT_MAX_BYTES: u64 = 512 * 1024;
const DEFAULT_RETAINED_FILES: usize = 3;
const MAX_REASON_CHARS: usize = 512;
const MAX_EXPORT_LINES: usize = 200;
const MAX_EXPORT_LINE_CHARS: usize = 2_048;
const MAX_REPORT_CHARS: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticOperation {
    TrustedControllersRefresh,
    ControllerRevocation,
    RelayProbe,
    DiagnosticExport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerDiagnosticStage {
    Connecting,
    RelayConnected,
    RelayJoined,
    WaitingForApproval,
    SecureSessionReady,
    Connected,
    RetryScheduled,
    Stopped,
    Cancelled,
}

impl ControllerDiagnosticStage {
    const fn event_name(self) -> &'static str {
        match self {
            Self::Connecting => "controller_connecting",
            Self::RelayConnected => "controller_relay_connected",
            Self::RelayJoined => "controller_relay_joined",
            Self::WaitingForApproval => "controller_waiting_for_approval",
            Self::SecureSessionReady => "controller_secure_session_ready",
            Self::Connected => "controller_connected",
            Self::RetryScheduled => "controller_retry_scheduled",
            Self::Stopped => "controller_stopped",
            Self::Cancelled => "controller_cancelled",
        }
    }

    const fn level(self) -> &'static str {
        match self {
            Self::RetryScheduled => "warning",
            Self::Stopped => "error",
            _ => "info",
        }
    }
}

impl DiagnosticOperation {
    const fn event_name(self, succeeded: bool) -> &'static str {
        match (self, succeeded) {
            (Self::TrustedControllersRefresh, true) => "trusted_controllers_refreshed",
            (Self::TrustedControllersRefresh, false) => "trusted_controllers_refresh_failed",
            (Self::ControllerRevocation, true) => "controller_revoked",
            (Self::ControllerRevocation, false) => "controller_revocation_failed",
            (Self::RelayProbe, true) => "relay_probe_succeeded",
            (Self::RelayProbe, false) => "relay_probe_failed",
            (Self::DiagnosticExport, true) => "diagnostic_report_exported",
            (Self::DiagnosticExport, false) => "diagnostic_report_export_failed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum H264ProfileProbe {
    NotChecked,
    Supported,
    Unsupported,
    Unavailable,
}

impl H264ProfileProbe {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotChecked => "notChecked",
            Self::Supported => "supported",
            Self::Unsupported => "unsupported",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum H264ProfileFallbackReason {
    DecoderUnsupported,
    DecoderError,
    DecoderStall,
}

impl H264ProfileFallbackReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DecoderUnsupported => "decoderUnsupported",
            Self::DecoderError => "decoderError",
            Self::DecoderStall => "decoderStall",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiagnosticEvent {
    ControlSurfaceStarted,
    ApplicationStarted {
        pairing_mode: bool,
    },
    ApplicationStopped,
    PowerResumeMonitoringStarted,
    PowerResumeMonitoringFailed {
        reason: String,
    },
    PowerResumeDetected,
    Lifecycle(HostLifecycleEvent),
    OperationSucceeded(DiagnosticOperation),
    OperationFailed {
        operation: DiagnosticOperation,
        reason: String,
    },
    ControllerConnection {
        stage: ControllerDiagnosticStage,
        attempt: u32,
        retry: Option<u32>,
        delay: Option<Duration>,
        reason: Option<String>,
    },
    ControllerVideoMetrics {
        attempt: u32,
        stream_id: Option<u64>,
        video_path: String,
        video_path_rtt_ms: Option<u32>,
        video_path_loss_basis_points: Option<u16>,
        video_path_fallback_reason: Option<String>,
        received_video_packets: u64,
        dropped_video_packets: u64,
        completed_frames: u64,
        delivered_video_frames: u64,
        video_ipc_overflow_drops: u64,
        video_ipc_keyframe_replacements: u64,
        input_backpressure_count: u64,
    },
    ControllerRenderMetrics {
        stream_id: u64,
        video_width: u16,
        video_height: u16,
        video_path: String,
        received_frames: u64,
        submitted_frames: u64,
        displayed_frames: u64,
        malformed_frames: u64,
        decoder_recoveries: u32,
        video_pull_failures: u32,
        first_frame_ms: Option<u64>,
        displayed_fps_x100: Option<u32>,
        max_frame_gap_ms: Option<u64>,
        coalesced_frame_drops: u64,
        h264_profile: H264Profile,
        profile_probe: H264ProfileProbe,
        profile_probe_ms: Option<u64>,
        profile_fallback_reason: Option<H264ProfileFallbackReason>,
    },
}

#[derive(Debug, Error)]
pub enum DiagnosticLogError {
    #[error("diagnostic log storage path is unavailable")]
    MissingStoragePath,
    #[error("diagnostic log file operation failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub struct DiagnosticLog {
    inner: Arc<DiagnosticLogInner>,
}

#[derive(Debug)]
struct DiagnosticLogInner {
    path: PathBuf,
    max_bytes: u64,
    retained_files: usize,
    write_lock: Mutex<()>,
}

impl DiagnosticLog {
    pub fn for_current_user() -> Result<Self, DiagnosticLogError> {
        let local_app_data = local_app_data_path().ok_or(DiagnosticLogError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data
                .join("DeskLink")
                .join("logs")
                .join("host.jsonl"),
        ))
    }

    pub fn controller_for_current_user() -> Result<Self, DiagnosticLogError> {
        let local_app_data = local_app_data_path().ok_or(DiagnosticLogError::MissingStoragePath)?;
        Ok(Self::new(
            local_app_data
                .join("DeskLink")
                .join("logs")
                .join("controller.jsonl"),
        ))
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::with_limits(path, DEFAULT_MAX_BYTES, DEFAULT_RETAINED_FILES)
    }

    pub fn with_limits(path: impl Into<PathBuf>, max_bytes: u64, retained_files: usize) -> Self {
        Self {
            inner: Arc::new(DiagnosticLogInner {
                path: path.into(),
                max_bytes: max_bytes.max(256),
                retained_files: retained_files.min(8),
                write_lock: Mutex::new(()),
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub fn record(&self, event: &DiagnosticEvent) -> Result<(), DiagnosticLogError> {
        let _guard = self
            .inner
            .write_lock
            .lock()
            .map_err(|_| io::Error::other("diagnostic log write lock is poisoned"))?;
        let line = encode_event(event);
        let parent = self
            .inner
            .path
            .parent()
            .ok_or(DiagnosticLogError::MissingStoragePath)?;
        fs::create_dir_all(parent)?;
        self.rotate_if_needed(line.len() as u64)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.inner.path)?;
        file.write_all(line.as_bytes())?;
        file.sync_data()?;
        Ok(())
    }

    pub fn recent_sanitized_lines(&self) -> Result<Vec<String>, DiagnosticLogError> {
        let _guard = self
            .inner
            .write_lock
            .lock()
            .map_err(|_| io::Error::other("diagnostic log read lock is poisoned"))?;
        let mut lines = VecDeque::with_capacity(MAX_EXPORT_LINES);
        let mut paths = (1..=self.inner.retained_files)
            .rev()
            .map(|index| rotated_path(&self.inner.path, index))
            .collect::<Vec<_>>();
        paths.push(self.inner.path.clone());
        for path in paths {
            let contents = match fs::read_to_string(path) {
                Ok(contents) => contents,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            for line in contents.lines() {
                if lines.len() == MAX_EXPORT_LINES {
                    lines.pop_front();
                }
                lines.push_back(bounded_export_line(line));
            }
        }
        Ok(lines.into_iter().collect())
    }

    pub fn export_report(
        &self,
        report_id: &str,
        contents: &str,
    ) -> Result<PathBuf, DiagnosticLogError> {
        let directory = downloads_path()
            .or_else(|| {
                self.inner
                    .path
                    .parent()
                    .map(|parent| parent.join("exports"))
            })
            .ok_or(DiagnosticLogError::MissingStoragePath)?;
        export_report_to_directory(&directory, report_id, contents)
    }

    fn rotate_if_needed(&self, incoming_bytes: u64) -> io::Result<()> {
        let current_bytes = match fs::metadata(&self.inner.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if current_bytes == 0
            || current_bytes.saturating_add(incoming_bytes) <= self.inner.max_bytes
        {
            return Ok(());
        }
        if self.inner.retained_files == 0 {
            return fs::remove_file(&self.inner.path);
        }
        for index in (1..=self.inner.retained_files).rev() {
            let source = if index == 1 {
                self.inner.path.clone()
            } else {
                rotated_path(&self.inner.path, index - 1)
            };
            let destination = rotated_path(&self.inner.path, index);
            if destination.exists() {
                fs::remove_file(&destination)?;
            }
            if source.exists() {
                fs::rename(source, destination)?;
            }
        }
        Ok(())
    }
}

fn encode_event(event: &DiagnosticEvent) -> String {
    let timestamp_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let fields = match event {
        DiagnosticEvent::ControlSurfaceStarted => {
            "\"level\":\"info\",\"event\":\"control_surface_started\"".to_owned()
        }
        DiagnosticEvent::ApplicationStarted { pairing_mode } => format!(
            "\"level\":\"info\",\"event\":\"application_started\",\"pairing_mode\":{pairing_mode}"
        ),
        DiagnosticEvent::ApplicationStopped => {
            "\"level\":\"info\",\"event\":\"application_stopped\"".to_owned()
        }
        DiagnosticEvent::PowerResumeMonitoringStarted => {
            "\"level\":\"info\",\"event\":\"power_resume_monitoring_started\"".to_owned()
        }
        DiagnosticEvent::PowerResumeMonitoringFailed { reason } => format!(
            "\"level\":\"warning\",\"event\":\"power_resume_monitoring_failed\",\"reason\":{}",
            json_string(&bounded_redacted_text(reason))
        ),
        DiagnosticEvent::PowerResumeDetected => {
            "\"level\":\"info\",\"event\":\"power_resume_detected\"".to_owned()
        }
        DiagnosticEvent::Lifecycle(HostLifecycleEvent::Connecting { attempt, stream_id }) => {
            format!(
                "\"level\":\"info\",\"event\":\"host_connecting\",\"attempt\":{attempt},\"stream_id\":{stream_id}"
            )
        }
        DiagnosticEvent::Lifecycle(HostLifecycleEvent::Available { stream_id }) => {
            format!("\"level\":\"info\",\"event\":\"host_available\",\"stream_id\":{stream_id}")
        }
        DiagnosticEvent::Lifecycle(HostLifecycleEvent::Connected { stream_id }) => {
            format!("\"level\":\"info\",\"event\":\"host_connected\",\"stream_id\":{stream_id}")
        }
        DiagnosticEvent::Lifecycle(HostLifecycleEvent::Reconnecting {
            retry,
            maximum_retries,
            delay,
            reason,
        }) => format!(
            "\"level\":\"warning\",\"event\":\"host_reconnecting\",\"retry\":{retry},\"maximum_retries\":{maximum_retries},\"delay_ms\":{},\"reason\":{}",
            delay.as_millis(),
            json_string(&bounded_redacted_text(reason))
        ),
        DiagnosticEvent::Lifecycle(HostLifecycleEvent::Stopped { reason }) => format!(
            "\"level\":\"error\",\"event\":\"host_stopped\",\"reason\":{}",
            json_string(&bounded_redacted_text(reason))
        ),
        DiagnosticEvent::OperationSucceeded(operation) => format!(
            "\"level\":\"info\",\"event\":{}",
            json_string(operation.event_name(true))
        ),
        DiagnosticEvent::OperationFailed { operation, reason } => format!(
            "\"level\":\"error\",\"event\":{},\"reason\":{}",
            json_string(operation.event_name(false)),
            json_string(&bounded_redacted_text(reason))
        ),
        DiagnosticEvent::ControllerConnection {
            stage,
            attempt,
            retry,
            delay,
            reason,
        } => {
            let retry = retry.map_or_else(String::new, |retry| format!(",\"retry\":{retry}"));
            let delay = delay.map_or_else(String::new, |delay| {
                format!(",\"delay_ms\":{}", delay.as_millis())
            });
            let reason = reason.as_ref().map_or_else(String::new, |reason| {
                format!(
                    ",\"reason\":{}",
                    json_string(&bounded_redacted_text(reason))
                )
            });
            format!(
                "\"level\":\"{}\",\"event\":{},\"attempt\":{attempt}{retry}{delay}{reason}",
                stage.level(),
                json_string(stage.event_name())
            )
        }
        DiagnosticEvent::ControllerVideoMetrics {
            attempt,
            stream_id,
            video_path,
            video_path_rtt_ms,
            video_path_loss_basis_points,
            video_path_fallback_reason,
            received_video_packets,
            dropped_video_packets,
            completed_frames,
            delivered_video_frames,
            video_ipc_overflow_drops,
            video_ipc_keyframe_replacements,
            input_backpressure_count,
        } => {
            let stream_id =
                stream_id.map_or_else(String::new, |value| format!(",\"stream_id\":{value}"));
            let video_path = json_string(&bounded_redacted_text(video_path));
            let video_path_rtt_ms = video_path_rtt_ms.map_or_else(String::new, |value| {
                format!(",\"video_path_rtt_ms\":{value}")
            });
            let video_path_loss_basis_points = video_path_loss_basis_points
                .map_or_else(String::new, |value| {
                    format!(",\"video_path_loss_basis_points\":{value}")
                });
            let video_path_fallback_reason =
                video_path_fallback_reason
                    .as_ref()
                    .map_or_else(String::new, |value| {
                        format!(
                            ",\"video_path_fallback_reason\":{}",
                            json_string(&bounded_redacted_text(value))
                        )
                    });
            format!(
                "\"level\":\"info\",\"event\":\"controller_video_metrics\",\"attempt\":{attempt}{stream_id},\"video_path\":{video_path}{video_path_rtt_ms}{video_path_loss_basis_points}{video_path_fallback_reason},\"received_video_packets\":{received_video_packets},\"dropped_video_packets\":{dropped_video_packets},\"completed_frames\":{completed_frames},\"delivered_video_frames\":{delivered_video_frames},\"video_ipc_overflow_drops\":{video_ipc_overflow_drops},\"video_ipc_keyframe_replacements\":{video_ipc_keyframe_replacements},\"input_backpressure_count\":{input_backpressure_count}"
            )
        }
        DiagnosticEvent::ControllerRenderMetrics {
            stream_id,
            video_width,
            video_height,
            video_path,
            received_frames,
            submitted_frames,
            displayed_frames,
            malformed_frames,
            decoder_recoveries,
            video_pull_failures,
            first_frame_ms,
            displayed_fps_x100,
            max_frame_gap_ms,
            coalesced_frame_drops,
            h264_profile,
            profile_probe,
            profile_probe_ms,
            profile_fallback_reason,
        } => {
            let first_frame_ms = first_frame_ms
                .map_or_else(String::new, |value| format!(",\"first_frame_ms\":{value}"));
            let displayed_fps_x100 = displayed_fps_x100.map_or_else(String::new, |value| {
                format!(",\"displayed_fps_x100\":{value}")
            });
            let max_frame_gap_ms = max_frame_gap_ms.map_or_else(String::new, |value| {
                format!(",\"max_frame_gap_ms\":{value}")
            });
            let h264_profile = match h264_profile {
                H264Profile::Main => "main",
                H264Profile::High => "high",
            };
            let profile_probe =
                format!(",\"profile_probe\":{}", json_string(profile_probe.as_str()));
            let profile_probe_ms = profile_probe_ms.map_or_else(String::new, |value| {
                format!(",\"profile_probe_ms\":{value}")
            });
            let profile_fallback_reason =
                profile_fallback_reason
                    .as_ref()
                    .map_or_else(String::new, |reason| {
                        format!(
                            ",\"profile_fallback_reason\":{}",
                            json_string(reason.as_str())
                        )
                    });
            let video_path = json_string(&bounded_redacted_text(video_path));
            format!(
                "\"level\":\"info\",\"event\":\"controller_render_metrics\",\"stream_id\":{stream_id},\"video_width\":{video_width},\"video_height\":{video_height},\"video_path\":{video_path},\"received_frames\":{received_frames},\"submitted_frames\":{submitted_frames},\"displayed_frames\":{displayed_frames},\"malformed_frames\":{malformed_frames},\"decoder_recoveries\":{decoder_recoveries},\"video_pull_failures\":{video_pull_failures},\"coalesced_frame_drops\":{coalesced_frame_drops},\"h264_profile\":\"{h264_profile}\"{profile_probe}{profile_probe_ms}{profile_fallback_reason}{first_frame_ms}{displayed_fps_x100}{max_frame_gap_ms}"
            )
        }
    };
    format!("{{\"schema\":1,\"timestamp_unix_ms\":{timestamp_unix_ms},{fields}}}\n")
}

fn bounded_redacted_text(value: &str) -> String {
    let redacted = redact_sensitive_text(value);
    let mut characters = redacted.chars();
    let mut bounded = characters
        .by_ref()
        .take(MAX_REASON_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        bounded.push_str("...");
    }
    bounded
}

pub fn redact_sensitive_text(value: &str) -> String {
    const NAMES: [&str; 5] = [
        "DESKLINK_AUTH_KEY",
        "DESKLINK_PAIRING_INVITE",
        "DESKLINK_SESSION_ID",
        "DESKLINK_PEER_VERIFY_KEY",
        "DESKLINK_HOST_VERIFY_KEY",
    ];

    let mut redacted = value.to_owned();
    for name in NAMES {
        redacted = redact_named_assignment(&redacted, name);
    }
    redact_long_hex_sequences(&redacted)
}

fn redact_named_assignment(value: &str, name: &str) -> String {
    let pattern = format!("{name}=");
    let mut output = String::with_capacity(value.len());
    let mut remaining = value;
    while let Some(index) = remaining.find(&pattern) {
        let value_start = index + pattern.len();
        output.push_str(&remaining[..value_start]);
        output.push_str("<redacted>");
        let tail = &remaining[value_start..];
        let value_end = tail
            .find(|character: char| {
                character.is_whitespace() || matches!(character, ',' | ';' | '\"' | '\'')
            })
            .unwrap_or(tail.len());
        remaining = &tail[value_end..];
    }
    output.push_str(remaining);
    output
}

fn redact_long_hex_sequences(value: &str) -> String {
    let characters = value.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < characters.len() {
        if characters[index].is_ascii_hexdigit() {
            let start = index;
            while index < characters.len() && characters[index].is_ascii_hexdigit() {
                index += 1;
            }
            if index - start >= 32 {
                output.push_str("<redacted-hex>");
            } else {
                output.extend(&characters[start..index]);
            }
        } else {
            output.push(characters[index]);
            index += 1;
        }
    }
    output
}

fn bounded_export_line(value: &str) -> String {
    let redacted = redact_sensitive_text(value);
    let mut characters = redacted.chars();
    let mut bounded = characters
        .by_ref()
        .take(MAX_EXPORT_LINE_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        bounded.push_str("...");
    }
    bounded
}

fn export_report_to_directory(
    directory: &Path,
    report_id: &str,
    contents: &str,
) -> Result<PathBuf, DiagnosticLogError> {
    fs::create_dir_all(directory)?;
    let safe_report_id = report_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .take(64)
        .collect::<String>();
    let safe_report_id = if safe_report_id.is_empty() {
        "DeskLink".to_owned()
    } else {
        safe_report_id
    };
    let redacted = redact_sensitive_text(contents);
    let mut bounded = redacted.chars().take(MAX_REPORT_CHARS).collect::<String>();
    if redacted.chars().count() > MAX_REPORT_CHARS {
        bounded.push_str("\n[报告内容已达到安全长度上限]\n");
    } else if !bounded.ends_with('\n') {
        bounded.push('\n');
    }

    for collision in 0..=99 {
        let suffix = if collision == 0 {
            String::new()
        } else {
            format!("-{collision}")
        };
        let path = directory.join(format!("DeskLink-Diagnostics-{safe_report_id}{suffix}.txt"));
        let mut file = match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        file.write_all(bounded.as_bytes())?;
        file.sync_all()?;
        return Ok(path);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many diagnostic report files share this identifier",
    )
    .into())
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('\"');
    for character in value.chars() {
        match character {
            '\"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                write!(&mut output, "\\u{:04X}", character as u32)
                    .expect("writing to String cannot fail");
            }
            character => output.push(character),
        }
    }
    output.push('\"');
    output
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("host.jsonl");
    path.with_file_name(format!("{file_name}.{index}"))
}

#[cfg(test)]
mod tests {
    use std::{
        env, fs,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };

    use super::*;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temporary_log_path(name: &str) -> PathBuf {
        env::temp_dir()
            .join(format!(
                "desklink-diagnostics-{}-{}-{}",
                std::process::id(),
                TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
                name
            ))
            .join("host.jsonl")
    }

    #[test]
    fn structured_log_redacts_credentials_and_escapes_reason_text() {
        let path = temporary_log_path("redaction");
        let logger = DiagnosticLog::new(&path);
        let secret = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        logger
            .record(&DiagnosticEvent::Lifecycle(
                HostLifecycleEvent::Reconnecting {
                    retry: 2,
                    maximum_retries: 6,
                    delay: Duration::from_millis(500),
                    reason: format!(
                        "relay said \"no\"\nDESKLINK_AUTH_KEY={secret} DESKLINK_PAIRING_INVITE=plain-secret"
                    ),
                },
            ))
            .unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"event\":\"host_reconnecting\""));
        assert!(contents.contains("\"retry\":2"));
        assert!(contents.contains("\\\"no\\\"\\n"));
        assert!(contents.contains("<redacted>"));
        assert!(!contents.contains(secret));
        assert!(!contents.contains("plain-secret"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn controller_log_keeps_failure_stage_but_redacts_connection_secrets() {
        let path = temporary_log_path("controller-redaction");
        let logger = DiagnosticLog::new(&path);
        let secret = "ef".repeat(32);
        logger
            .record(&DiagnosticEvent::ControllerConnection {
                stage: ControllerDiagnosticStage::RetryScheduled,
                attempt: 3,
                retry: Some(2),
                delay: Some(Duration::from_millis(1_000)),
                reason: Some(format!("peer_replaced DESKLINK_SESSION_ID={secret}")),
            })
            .unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"event\":\"controller_retry_scheduled\""));
        assert!(contents.contains("\"attempt\":3"));
        assert!(contents.contains("\"retry\":2"));
        assert!(contents.contains("\"delay_ms\":1000"));
        assert!(contents.contains("peer_replaced"));
        assert!(contents.contains("<redacted>"));
        assert!(!contents.contains(&secret));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn controller_video_metrics_distinguish_transport_from_ui_rendering() {
        let path = temporary_log_path("controller-video");
        let logger = DiagnosticLog::new(&path);
        logger
            .record(&DiagnosticEvent::ControllerVideoMetrics {
                attempt: 2,
                stream_id: Some(7),
                video_path: "directLan".to_owned(),
                video_path_rtt_ms: Some(4),
                video_path_loss_basis_points: Some(12),
                video_path_fallback_reason: None,
                received_video_packets: 1_200,
                dropped_video_packets: 4,
                completed_frames: 87,
                delivered_video_frames: 84,
                video_ipc_overflow_drops: 3,
                video_ipc_keyframe_replacements: 1,
                input_backpressure_count: 3,
            })
            .unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"event\":\"controller_video_metrics\""));
        assert!(contents.contains("\"received_video_packets\":1200"));
        assert!(contents.contains("\"dropped_video_packets\":4"));
        assert!(contents.contains("\"completed_frames\":87"));
        assert!(contents.contains("\"stream_id\":7"));
        assert!(contents.contains("\"video_path\":\"directLan\""));
        assert!(contents.contains("\"video_path_rtt_ms\":4"));
        assert!(contents.contains("\"video_path_loss_basis_points\":12"));
        assert!(contents.contains("\"delivered_video_frames\":84"));
        assert!(contents.contains("\"video_ipc_overflow_drops\":3"));
        assert!(contents.contains("\"video_ipc_keyframe_replacements\":1"));
        assert!(contents.contains("\"input_backpressure_count\":3"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn controller_render_metrics_separate_received_decoded_and_displayed_frames() {
        let path = temporary_log_path("controller-render");
        let logger = DiagnosticLog::new(&path);
        logger
            .record(&DiagnosticEvent::ControllerRenderMetrics {
                stream_id: 7,
                video_width: 2560,
                video_height: 1440,
                video_path: "relay".to_owned(),
                received_frames: 90,
                submitted_frames: 86,
                displayed_frames: 82,
                malformed_frames: 1,
                decoder_recoveries: 2,
                video_pull_failures: 2,
                first_frame_ms: Some(740),
                displayed_fps_x100: Some(2_970),
                max_frame_gap_ms: Some(167),
                coalesced_frame_drops: 4,
                h264_profile: H264Profile::High,
                profile_probe: H264ProfileProbe::Supported,
                profile_probe_ms: Some(4),
                profile_fallback_reason: None,
            })
            .unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"event\":\"controller_render_metrics\""));
        assert!(contents.contains("\"stream_id\":7"));
        assert!(contents.contains("\"displayed_frames\":82"));
        assert!(contents.contains("\"h264_profile\":\"high\""));
        assert!(contents.contains("\"profile_probe\":\"supported\""));
        assert!(contents.contains("\"decoder_recoveries\":2"));
        assert!(contents.contains("\"video_pull_failures\":2"));
        assert!(contents.contains("\"first_frame_ms\":740"));
        assert!(contents.contains("\"displayed_fps_x100\":2970"));
        assert!(contents.contains("\"max_frame_gap_ms\":167"));
        assert!(contents.contains("\"coalesced_frame_drops\":4"));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn log_rotation_keeps_only_the_configured_history() {
        let path = temporary_log_path("rotation");
        let logger = DiagnosticLog::with_limits(&path, 256, 2);
        for retry in 1..=20 {
            logger
                .record(&DiagnosticEvent::Lifecycle(
                    HostLifecycleEvent::Reconnecting {
                        retry,
                        maximum_retries: 20,
                        delay: Duration::from_millis(250),
                        reason: "the relay connection closed while DeskLink was hosting".to_owned(),
                    },
                ))
                .unwrap();
        }

        assert!(path.exists());
        assert!(rotated_path(&path, 1).exists());
        assert!(rotated_path(&path, 2).exists());
        assert!(!rotated_path(&path, 3).exists());
        for file in [&path, &rotated_path(&path, 1), &rotated_path(&path, 2)] {
            let contents = fs::read_to_string(file).unwrap();
            assert!(
                contents
                    .lines()
                    .all(|line| line.starts_with("{\"schema\":1,"))
            );
        }
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn long_unlabeled_hex_values_are_redacted() {
        assert_eq!(
            redact_sensitive_text("peer=00112233445566778899aabbccddeeff"),
            "peer=<redacted-hex>"
        );
        assert_eq!(
            redact_sensitive_text("error=0x80004005"),
            "error=0x80004005"
        );
    }

    #[test]
    fn recent_export_lines_remain_bounded_and_redacted() {
        let path = temporary_log_path("recent-export");
        let logger = DiagnosticLog::new(&path);
        let secret = "ab".repeat(32);
        logger
            .record(&DiagnosticEvent::OperationFailed {
                operation: DiagnosticOperation::RelayProbe,
                reason: format!("DESKLINK_AUTH_KEY={secret}"),
            })
            .unwrap();

        let lines = logger.recent_sanitized_lines().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("relay_probe_failed"));
        assert!(lines[0].contains("<redacted>"));
        assert!(!lines[0].contains(&secret));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn exported_report_uses_safe_unique_names_and_redacts_again() {
        let root = temporary_log_path("report-export")
            .parent()
            .unwrap()
            .to_path_buf();
        let directory = root.join("Downloads");
        let secret = "cd".repeat(32);
        let first = export_report_to_directory(
            &directory,
            "DL-WIN/../../unsafe",
            &format!("报告\nDESKLINK_AUTH_KEY={secret}"),
        )
        .unwrap();
        let second = export_report_to_directory(
            &directory,
            "DL-WIN/../../unsafe",
            &format!("报告\nDESKLINK_AUTH_KEY={secret}"),
        )
        .unwrap();

        assert_ne!(first, second);
        assert!(first.starts_with(&directory));
        assert!(!first.file_name().unwrap().to_string_lossy().contains('/'));
        let contents = fs::read_to_string(first).unwrap();
        assert!(contents.contains("<redacted>"));
        assert!(!contents.contains(&secret));
        let _ = fs::remove_dir_all(root);
    }
}
