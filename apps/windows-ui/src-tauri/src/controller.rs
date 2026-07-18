use std::{
    collections::VecDeque,
    fs::File,
    io::Read,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use apps_windows::{
    audio::RemoteAudioDecoder,
    cloud_diagnostics::{DiagnosticSource, set_session_correlation},
    controller_settings::{ControllerConnectionSettings, WindowsControllerConnectionStore},
    diagnostics::{ControllerDiagnosticStage, DiagnosticEvent, DiagnosticLog},
    identity::WindowsIdentityStore,
    recent_access::{RecentAccessEntry, WindowsRecentAccessError, WindowsRecentAccessStore},
    transfer::{IncomingFile, OutgoingFile, prepare_outgoing_file},
};
use blake2::{Blake2s256, Digest};
use desklink_crypto::{PairingCode, PairingInvite};
use desklink_ffi::{ControllerError, ControllerEvent, ControllerRuntime, ControllerTransferSender};
use desklink_protocol::{
    AccessDenialReason, AudioCodec, AudioPacket, ControlMessage, FrameFlags, InputEvent, KeyCode,
    MAX_CLIPBOARD_TEXT_BYTES, MAX_POINTER_COORDINATE, MAX_TRANSFER_CHUNK_BYTES, MAX_WHEEL_DELTA,
    Modifiers, MouseButton, Platform, TransferId, TransferMessage, TransferResult,
    VideoQualityPreference, VideoQualityPreset,
};
use desklink_session::{ReconnectDecision, ReconnectPolicy, ReconnectSchedule};
use desklink_transport::{
    JoinRejectCode, QuicClient, RelayDirectoryLookup, RelayJoin, TransportError,
};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use tauri::ipc::{Channel, Response};
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};
use zeroize::Zeroize;

const COMMAND_CAPACITY: usize = 512;
const MAX_BUFFERED_POINTER_MOVES: usize = 8;
const FRAME_PREFIX_BYTES: usize = 17;
const MAX_TEXT_INPUT_CHARACTERS: usize = 256;
const MAX_TEXT_INPUT_BYTES: usize = 1_024;
const RECENT_CANCELLATION_WINDOW: Duration = Duration::from_secs(15);
const RECONNECT_BUDGET_RESET_AFTER: Duration = Duration::from_secs(30);
const DIRECTORY_RECOVERY_DELAYS: [Duration; 2] =
    [Duration::from_millis(500), Duration::from_millis(1_250)];
const DIRECTORY_TRANSPORT_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_millis(350), Duration::from_millis(900)];
const AUDIO_FRAME_DURATION_US: u64 = 10_000;
const MAX_FILE_QUEUE_ITEMS: usize = 20;

struct DecodedControllerAudio {
    stream_id: u64,
    sequence: u64,
    capture_timestamp_us: u64,
    sample_rate: u32,
    payload: Vec<u8>,
}

struct ControllerAudioDecoder {
    opus: Option<RemoteAudioDecoder>,
    stream_id: Option<u64>,
    next_sequence: Option<u64>,
    codec: Option<AudioCodec>,
}

impl ControllerAudioDecoder {
    fn new() -> Self {
        Self {
            opus: RemoteAudioDecoder::new().ok(),
            stream_id: None,
            next_sequence: None,
            codec: None,
        }
    }

    fn decode(&mut self, packet: AudioPacket) -> Vec<DecodedControllerAudio> {
        if packet.codec == AudioCodec::PcmS16Le {
            self.stream_id = Some(packet.stream_id);
            self.next_sequence = Some(packet.sequence.saturating_add(1));
            self.codec = Some(packet.codec);
            return vec![DecodedControllerAudio {
                stream_id: packet.stream_id,
                sequence: packet.sequence,
                capture_timestamp_us: packet.capture_timestamp_us,
                sample_rate: packet.sample_rate,
                payload: packet.payload,
            }];
        }

        let stream_changed =
            self.stream_id != Some(packet.stream_id) || self.codec != Some(AudioCodec::Opus);
        if stream_changed {
            self.reset_opus();
            self.next_sequence = None;
        }

        let expected = self.next_sequence;
        if expected.is_some_and(|expected| packet.sequence < expected) {
            return Vec::new();
        }
        if expected.is_some_and(|expected| packet.sequence > expected.saturating_add(1)) {
            self.reset_opus();
        }

        let mut decoded = Vec::with_capacity(2);
        if expected.is_some_and(|expected| packet.sequence == expected.saturating_add(1)) {
            if let Some(payload) = self.decode_opus(&packet.payload, true) {
                decoded.push(DecodedControllerAudio {
                    stream_id: packet.stream_id,
                    sequence: packet.sequence.saturating_sub(1),
                    capture_timestamp_us: packet
                        .capture_timestamp_us
                        .saturating_sub(AUDIO_FRAME_DURATION_US)
                        .max(1),
                    sample_rate: packet.sample_rate,
                    payload,
                });
            } else {
                self.reset_opus();
            }
        }

        let Some(payload) = self.decode_opus(&packet.payload, false) else {
            self.reset_opus();
            self.stream_id = None;
            self.next_sequence = None;
            self.codec = None;
            return Vec::new();
        };
        decoded.push(DecodedControllerAudio {
            stream_id: packet.stream_id,
            sequence: packet.sequence,
            capture_timestamp_us: packet.capture_timestamp_us,
            sample_rate: packet.sample_rate,
            payload,
        });
        self.stream_id = Some(packet.stream_id);
        self.next_sequence = Some(packet.sequence.saturating_add(1));
        self.codec = Some(AudioCodec::Opus);
        decoded
    }

    fn decode_opus(&mut self, payload: &[u8], fec: bool) -> Option<Vec<u8>> {
        if self.opus.is_none() {
            self.opus = RemoteAudioDecoder::new().ok();
        }
        self.opus.as_mut()?.decode(payload, fec).ok()
    }

    fn reset_opus(&mut self) {
        let reset = self.opus.as_mut().map(RemoteAudioDecoder::reset);
        if reset.is_some_and(|result| result.is_err()) {
            self.opus = RemoteAudioDecoder::new().ok();
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerRuntimeSummary {
    pub state: &'static str,
    pub title: String,
    pub detail: String,
    pub stream_id: Option<u64>,
}

impl ControllerRuntimeSummary {
    fn idle() -> Self {
        Self {
            state: "idle",
            title: "可以控制另一台电脑".to_owned(),
            detail: "粘贴另一台 Windows 电脑生成的邀请，或重新连接已保存的电脑。".to_owned(),
            stream_id: None,
        }
    }

    fn connecting() -> Self {
        Self {
            state: "connecting",
            title: "正在连接中继服务器".to_owned(),
            detail: "DeskLink 正在打开另一台电脑共享的私有会话。".to_owned(),
            stream_id: None,
        }
    }

    fn finding() -> Self {
        Self {
            state: "finding",
            title: "正在查找设备".to_owned(),
            detail: "DeskLink 正在通过安全中继确认设备 ID 和访问密码。".to_owned(),
            stream_id: None,
        }
    }

    fn finding_after_cancel(retry: usize, delay: Duration) -> Self {
        Self {
            state: "finding",
            title: "正在恢复上次连接".to_owned(),
            detail: format!(
                "主机可能正在重新上线，DeskLink 将在 {} 毫秒后自动重试（{retry}/{}）。",
                delay.as_millis(),
                DIRECTORY_RECOVERY_DELAYS.len()
            ),
            stream_id: None,
        }
    }

    fn finding_after_interruption(retry: usize, delay: Duration) -> Self {
        Self {
            state: "finding",
            title: "正在恢复设备查询".to_owned(),
            detail: format!(
                "中继查询刚刚中断，DeskLink 将在 {} 毫秒后使用新连接重试（{retry}/{}）。",
                delay.as_millis(),
                DIRECTORY_TRANSPORT_RETRY_DELAYS.len()
            ),
            stream_id: None,
        }
    }

    fn waiting_for_approval() -> Self {
        Self {
            state: "waitingApproval",
            title: "请在主机上批准此电脑".to_owned(),
            detail: "另一台电脑正在显示此控制端身份，批准后即可继续。".to_owned(),
            stream_id: None,
        }
    }

    fn connected(stream_id: u64) -> Self {
        Self {
            state: "connected",
            title: "远程控制已连接".to_owned(),
            detail: "画面和输入通道均已端到端加密。".to_owned(),
            stream_id: Some(stream_id),
        }
    }

    fn reconnecting(retry: u32, maximum: u32, delay: Duration) -> Self {
        Self {
            state: "reconnecting",
            title: "正在重新连接主机".to_owned(),
            detail: format!(
                "第 {retry}/{maximum} 次重试将在 {} 毫秒后开始。",
                delay.as_millis()
            ),
            stream_id: None,
        }
    }

    fn stopped(detail: impl Into<String>) -> Self {
        Self {
            state: "stopped",
            title: "远程控制已停止".to_owned(),
            detail: detail.into(),
            stream_id: None,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedControllerConnectionSummary {
    pub device_id: String,
    pub relay_address: String,
    pub server_name: String,
    pub host_device_id: String,
    pub host_verify_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerSnapshot {
    pub runtime: ControllerRuntimeSummary,
    pub saved_connection: Option<SavedControllerConnectionSummary>,
    pub connection_error: Option<String>,
    pub saved_devices: Vec<SavedDeviceCredentialSummary>,
    pub saved_devices_error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerDeviceInput {
    device_id: String,
    temporary_password: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedDeviceInput {
    device_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedDeviceRenameInput {
    device_id: String,
    alias: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedDeviceCredentialSummary {
    pub device_id: String,
    pub alias: Option<String>,
    pub persistent: bool,
    pub last_used_unix_s: u64,
}

impl Drop for ControllerDeviceInput {
    fn drop(&mut self) {
        self.temporary_password.zeroize();
    }
}

#[derive(Clone, Copy)]
enum DeviceCredentialSource {
    Entered,
    Saved { persistent: bool },
}

impl DeviceCredentialSource {
    fn not_found_message(self, recover_after_cancel: bool) -> &'static str {
        match self {
            Self::Entered if recover_after_cancel => {
                "主机仍未恢复在线。如果使用临时密码，请在主机上重新生成；如果使用固定密码，请确认主机已打开后再试。"
            }
            Self::Entered => "找不到在线设备或访问密码不正确，请确认主机在线并检查密码后重试。",
            Self::Saved { persistent: true } => {
                "找不到在线设备。请确认主机已打开；如果主机更换过固定密码，请移除此记录并输入新密码。"
            }
            Self::Saved { persistent: false } => {
                "保存的临时密码可能已经过期。请在主机上重新生成临时密码，再输入新密码连接。"
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerInput {
    stream_id: u64,
    kind: String,
    x: Option<i32>,
    y: Option<i32>,
    delta_x: Option<i32>,
    delta_y: Option<i32>,
    button: Option<String>,
    key: Option<String>,
    character: Option<String>,
    pressed: Option<bool>,
    modifiers: Option<u8>,
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ControllerSignal {
    Status {
        runtime: ControllerRuntimeSummary,
    },
    VideoConfig {
        stream_id: u64,
        config_version: u32,
        width: u16,
        height: u16,
        sequence_header: Vec<u8>,
    },
    Cursor {
        stream_id: u64,
        sequence: u64,
        x_millionths: i32,
        y_millionths: i32,
        visible: bool,
    },
    Metrics {
        received_video_packets: u64,
        dropped_video_packets: u64,
        completed_frames: u64,
    },
    Displays {
        displays: Vec<ControllerDisplaySummary>,
        active_display_id: u32,
    },
    Clipboard {
        state: &'static str,
        message: String,
    },
    FileTransfer {
        state: &'static str,
        direction: &'static str,
        name: String,
        transferred: u64,
        total: u64,
        message: String,
    },
    FileQueue {
        queued: Vec<QueuedFileSummary>,
        paused: bool,
    },
    Audio {
        state: &'static str,
        enabled: bool,
        message: String,
    },
    VideoQuality {
        preference: VideoQualityPreference,
        preset: VideoQualityPreset,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerDisplaySummary {
    id: u32,
    width: u16,
    height: u16,
    primary: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedFileSummary {
    id: String,
    name: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerRenderMetrics {
    stream_id: u64,
    received_frames: u64,
    submitted_frames: u64,
    displayed_frames: u64,
    malformed_frames: u64,
    decoder_recoveries: u32,
    first_frame_ms: Option<u64>,
}

enum ControllerCommand {
    Input { stream_id: u64, event: InputEvent },
    Text(String),
    RequestKeyframe,
    SelectDisplay(u32),
    SetAudioEnabled(bool),
    SetVideoQuality(VideoQualityPreference),
    SendClipboard(String),
    RequestClipboard,
    SendFiles(Vec<OutgoingFile>),
    RetryFile(OutgoingFile),
    RemoveQueuedFile(TransferId),
    ClearFileQueue,
    ResumeFileQueue,
    RequestRemoteFile,
    CancelFile,
}

fn should_drop_buffered_pointer_move(command: &ControllerCommand, capacity: usize) -> bool {
    matches!(
        command,
        ControllerCommand::Input {
            event: InputEvent::MouseMove { .. },
            ..
        }
    ) && capacity <= COMMAND_CAPACITY.saturating_sub(MAX_BUFFERED_POINTER_MOVES)
}

struct OutgoingTransfer {
    file: OutgoingFile,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
}

struct IncomingTransfer {
    file: IncomingFile,
    name: String,
    size: u64,
    transferred: u64,
}

#[derive(Clone)]
enum LastFileAction {
    Upload(PathBuf),
    Download,
}

struct OutgoingFileFailure {
    transfer_id: TransferId,
    message: String,
}

struct ControllerWorker {
    commands: mpsc::Sender<ControllerCommand>,
    cancellation: watch::Sender<bool>,
    task: tauri::async_runtime::JoinHandle<()>,
}

struct ControllerOutputChannels {
    signals: Channel<ControllerSignal>,
    video: Channel<Response>,
    audio: Channel<Response>,
}

#[derive(Clone)]
pub struct ControllerManager {
    status: Arc<Mutex<ControllerRuntimeSummary>>,
    worker: Arc<Mutex<Option<ControllerWorker>>>,
    last_file_action: Arc<Mutex<Option<LastFileAction>>>,
    operation_lock: Arc<AsyncMutex<()>>,
    operation_generation: Arc<AtomicU64>,
    recent_cancellation: Arc<Mutex<Option<Instant>>>,
    input_backpressure_count: Arc<AtomicU64>,
}

impl Default for ControllerManager {
    fn default() -> Self {
        Self {
            status: Arc::new(Mutex::new(ControllerRuntimeSummary::idle())),
            worker: Arc::new(Mutex::new(None)),
            last_file_action: Arc::new(Mutex::new(None)),
            operation_lock: Arc::new(AsyncMutex::new(())),
            operation_generation: Arc::new(AtomicU64::new(0)),
            recent_cancellation: Arc::new(Mutex::new(None)),
            input_backpressure_count: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl ControllerManager {
    pub fn snapshot(&self) -> ControllerRuntimeSummary {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| {
                ControllerRuntimeSummary::stopped("DeskLink 无法读取控制端状态，请重新启动应用。")
            })
    }

    pub async fn connect_device(
        &self,
        input: ControllerDeviceInput,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
        audio: Channel<Response>,
    ) -> Result<ControllerSnapshot, String> {
        let device_id =
            crate::device_directory::parse_device_id(&input.device_id).map_err(str::to_owned)?;
        let access_code = crate::device_directory::parse_access_code(&input.temporary_password)
            .map_err(str::to_owned)?;
        let password = PairingCode::from_bytes(access_code)
            .map_err(|_| "设备 ID 或访问密码格式无效。".to_owned())?;
        self.connect_device_credentials(
            device_id,
            password,
            DeviceCredentialSource::Entered,
            signals,
            video,
            audio,
        )
        .await
    }

    pub async fn connect_saved_device(
        &self,
        input: SavedDeviceInput,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
        audio: Channel<Response>,
    ) -> Result<ControllerSnapshot, String> {
        let device_id =
            crate::device_directory::parse_device_id(&input.device_id).map_err(str::to_owned)?;
        let saved = WindowsRecentAccessStore::for_current_user()
            .map_err(|_| "当前 Windows 账户无法使用已保存的设备密码。".to_owned())?
            .find(device_id)
            .map_err(|_| "无法解密已保存的设备密码，请移除此记录后重新输入。".to_owned())?
            .ok_or_else(|| "这台设备没有已保存的访问密码，请重新输入密码。".to_owned())?;
        let source = DeviceCredentialSource::Saved {
            persistent: saved.is_persistent(),
        };
        self.connect_device_credentials(
            device_id,
            saved.password().clone(),
            source,
            signals,
            video,
            audio,
        )
        .await
    }

    async fn connect_device_credentials(
        &self,
        device_id: u64,
        password: PairingCode,
        source: DeviceCredentialSource,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
        audio: Channel<Response>,
    ) -> Result<ControllerSnapshot, String> {
        let recover_after_cancel = self.take_recent_cancellation();
        let generation = self.begin_operation();
        self.publish_if_current(generation, &signals, ControllerRuntimeSummary::finding());
        let settings = async {
            let config = crate::local_relay::client_config(
                crate::local_relay::MANAGED_RELAY_ADDRESS
                    .parse()
                    .map_err(|_| "DeskLink 内置中继地址无效，请重新安装应用。".to_owned())?,
                crate::local_relay::MANAGED_RELAY_SERVER_NAME,
            )
            .map_err(|_| "DeskLink 无法准备安全中继连接。".to_owned())?;
            let lookup = RelayDirectoryLookup::new(device_id, *password.as_bytes())
                .map_err(|_| "设备 ID 或访问密码格式无效。".to_owned())?;
            let mut availability_retry = 0;
            let mut transport_retry = 0;
            let mut invitation = loop {
                self.ensure_current(generation)?;
                let client = match QuicClient::connect(config.clone()).await {
                    Ok(client) => client,
                    Err(error)
                        if directory_transport_error_is_retryable(&error)
                            && transport_retry < DIRECTORY_TRANSPORT_RETRY_DELAYS.len() =>
                    {
                        let delay = DIRECTORY_TRANSPORT_RETRY_DELAYS[transport_retry];
                        transport_retry += 1;
                        self.publish_if_current(
                            generation,
                            &signals,
                            ControllerRuntimeSummary::finding_after_interruption(
                                transport_retry,
                                delay,
                            ),
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    Err(error) => return Err(directory_transport_error_message(&error).to_owned()),
                };
                let result = client.lookup_directory(lookup.clone()).await;
                drop(client);
                match result {
                    Ok(invitation) => break invitation,
                    Err(TransportError::DirectoryNotFound)
                        if recover_after_cancel
                            && availability_retry < DIRECTORY_RECOVERY_DELAYS.len() =>
                    {
                        let delay = DIRECTORY_RECOVERY_DELAYS[availability_retry];
                        availability_retry += 1;
                        self.publish_if_current(
                            generation,
                            &signals,
                            ControllerRuntimeSummary::finding_after_cancel(
                                availability_retry,
                                delay,
                            ),
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Err(TransportError::DirectoryNotFound) => {
                        return Err(source.not_found_message(recover_after_cancel).to_owned());
                    }
                    Err(TransportError::DirectoryRateLimited) => {
                        return Err("尝试次数过多，请等待一分钟后再试。".to_owned());
                    }
                    Err(error)
                        if directory_transport_error_is_retryable(&error)
                            && transport_retry < DIRECTORY_TRANSPORT_RETRY_DELAYS.len() =>
                    {
                        let delay = DIRECTORY_TRANSPORT_RETRY_DELAYS[transport_retry];
                        transport_retry += 1;
                        self.publish_if_current(
                            generation,
                            &signals,
                            ControllerRuntimeSummary::finding_after_interruption(
                                transport_retry,
                                delay,
                            ),
                        );
                        tokio::time::sleep(delay).await;
                    }
                    Err(error) => {
                        return Err(directory_transport_error_message(&error).to_owned());
                    }
                }
            };
            let invite = match PairingInvite::decode(&invitation, now_unix_s()) {
                Ok(invite) => invite,
                Err(_) => {
                    invitation.zeroize();
                    return Err(
                        "主机返回的连接信息无效或已经过期，请检查或刷新访问密码。".to_owned()
                    );
                }
            };
            invitation.zeroize();
            let persistent = invite.is_persistent();
            let settings = ControllerConnectionSettings::from_invite(
                crate::local_relay::MANAGED_RELAY_ADDRESS,
                crate::local_relay::MANAGED_RELAY_SERVER_NAME,
                &invite,
            )
            .map_err(|error| error.to_string())?;
            WindowsRecentAccessStore::for_current_user()
                .map_err(|_| "当前 Windows 账户无法安全保存设备密码。".to_owned())?
                .remember(device_id, password, persistent, now_unix_s())
                .map_err(|_| {
                    "设备验证成功，但 Windows 无法加密保存密码。请检查当前账户的数据目录后重试。"
                        .to_owned()
                })?;
            Ok(settings)
        }
        .await;
        let settings = match settings {
            Ok(settings) => settings,
            Err(error) => {
                if !self.is_current(generation) {
                    return Err("连接已取消。".to_owned());
                }
                self.publish_if_current(generation, &signals, ControllerRuntimeSummary::idle());
                return Err(error);
            }
        };
        self.ensure_current(generation)?;
        self.start(generation, settings, true, signals, video, audio)
            .await?;
        load_snapshot(self.snapshot())
    }

    pub async fn connect_saved(
        &self,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
        audio: Channel<Response>,
    ) -> Result<ControllerSnapshot, String> {
        let generation = self.begin_operation();
        let store = WindowsControllerConnectionStore::for_current_user()
            .map_err(|_| "当前 Windows 账户无法使用已保存的控制端连接。".to_owned())?;
        let settings = store
            .load()
            .map_err(|_| "无法打开已保存的控制端连接。".to_owned())?
            .ok_or_else(|| "没有可供重新连接的已保存 Windows 电脑。".to_owned())?;
        self.start(generation, settings, false, signals, video, audio)
            .await?;
        load_snapshot(self.snapshot())
    }

    async fn start(
        &self,
        generation: u64,
        settings: ControllerConnectionSettings,
        save_after_approval: bool,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
        audio: Channel<Response>,
    ) -> Result<(), String> {
        let _operation = self.operation_lock.lock().await;
        self.ensure_current(generation)?;
        self.stop_current().await;
        self.ensure_current(generation)?;
        self.input_backpressure_count.store(0, Ordering::Relaxed);
        self.publish_if_current(generation, &signals, ControllerRuntimeSummary::connecting());
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let (cancellation, cancellation_receiver) = watch::channel(false);
        let manager = self.clone();
        let task = tauri::async_runtime::spawn(async move {
            run_controller(
                manager,
                settings,
                save_after_approval,
                receiver,
                cancellation_receiver,
                ControllerOutputChannels {
                    signals,
                    video,
                    audio,
                },
            )
            .await;
        });
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| "DeskLink 无法启动控制端任务。".to_owned())?;
        *worker = Some(ControllerWorker {
            commands,
            cancellation,
            task,
        });
        Ok(())
    }

    pub async fn send_input(&self, input: ControllerInput) -> Result<(), String> {
        if input.stream_id == 0 {
            return Err("远程输入缺少有效的视频流标识。".to_owned());
        }
        let stream_id = input.stream_id;
        let event = parse_input(input)?;
        if self.active_stream_id() != Some(stream_id) {
            return Ok(());
        }
        self.send(ControllerCommand::Input { stream_id, event })
            .await
    }

    pub async fn send_text(&self, text: String) -> Result<(), String> {
        validate_text_input(&text)?;
        self.send(ControllerCommand::Text(text)).await
    }

    pub async fn request_keyframe(&self) -> Result<(), String> {
        self.send(ControllerCommand::RequestKeyframe).await
    }

    pub async fn select_display(&self, display_id: u32) -> Result<(), String> {
        self.send(ControllerCommand::SelectDisplay(display_id))
            .await
    }

    pub async fn set_audio_enabled(&self, enabled: bool) -> Result<(), String> {
        self.send(ControllerCommand::SetAudioEnabled(enabled)).await
    }

    pub async fn set_video_quality(
        &self,
        preference: VideoQualityPreference,
    ) -> Result<(), String> {
        self.send(ControllerCommand::SetVideoQuality(preference))
            .await
    }

    pub async fn send_clipboard(&self, text: String) -> Result<(), String> {
        if text.len() > MAX_CLIPBOARD_TEXT_BYTES {
            return Err("剪贴板文本超过 48 KB，无法发送。".to_owned());
        }
        self.send(ControllerCommand::SendClipboard(text)).await
    }

    pub async fn request_clipboard(&self) -> Result<(), String> {
        self.send(ControllerCommand::RequestClipboard).await
    }

    pub async fn send_files(&self, paths: Vec<PathBuf>) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        if paths.len() > MAX_FILE_QUEUE_ITEMS {
            return Err(format!("一次最多添加 {MAX_FILE_QUEUE_ITEMS} 个文件。"));
        }
        let prepared = tauri::async_runtime::spawn_blocking(move || {
            let mut files = Vec::with_capacity(paths.len());
            for path in paths {
                if files.iter().any(|file: &OutgoingFile| file.path == path) {
                    continue;
                }
                files.push(prepare_outgoing_file(path)?);
            }
            Ok::<_, TransferResult>(files)
        })
        .await
        .map_err(|_| "DeskLink 无法读取所选文件。".to_owned())?
        .map_err(|result| prepare_file_error_message(result).to_owned())?;
        self.send(ControllerCommand::SendFiles(prepared)).await
    }

    pub async fn request_remote_file(&self) -> Result<(), String> {
        *self
            .last_file_action
            .lock()
            .map_err(|_| "DeskLink 无法保留最近的文件操作。".to_owned())? =
            Some(LastFileAction::Download);
        self.send(ControllerCommand::RequestRemoteFile).await
    }

    pub async fn retry_file(&self) -> Result<(), String> {
        let action = self
            .last_file_action
            .lock()
            .map_err(|_| "DeskLink 无法读取最近的文件操作。".to_owned())?
            .clone()
            .ok_or_else(|| "没有可以重试的文件任务。".to_owned())?;
        match action {
            LastFileAction::Upload(path) => {
                let prepared =
                    tauri::async_runtime::spawn_blocking(move || prepare_outgoing_file(path))
                        .await
                        .map_err(|_| "DeskLink 无法重新读取所选文件。".to_owned())?
                        .map_err(|result| prepare_file_error_message(result).to_owned())?;
                self.send(ControllerCommand::RetryFile(prepared)).await
            }
            LastFileAction::Download => self.send(ControllerCommand::RequestRemoteFile).await,
        }
    }

    pub async fn cancel_file(&self) -> Result<(), String> {
        self.send(ControllerCommand::CancelFile).await
    }

    pub async fn remove_queued_file(&self, transfer_id: &str) -> Result<(), String> {
        self.send(ControllerCommand::RemoveQueuedFile(parse_transfer_id(
            transfer_id,
        )?))
        .await
    }

    pub async fn clear_file_queue(&self) -> Result<(), String> {
        self.send(ControllerCommand::ClearFileQueue).await
    }

    pub async fn resume_file_queue(&self) -> Result<(), String> {
        self.send(ControllerCommand::ResumeFileQueue).await
    }

    async fn send(&self, command: ControllerCommand) -> Result<(), String> {
        let commands = self
            .worker
            .lock()
            .map_err(|_| "DeskLink 无法访问控制端任务。".to_owned())?
            .as_ref()
            .ok_or_else(|| "当前没有正在运行的远程控制会话。".to_owned())?
            .commands
            .clone();
        // Pointer motion is replaceable, while button/key releases are not. Keep
        // the shared command queue almost empty of stale motion so discrete
        // releases and recovery controls always retain bounded headroom.
        if should_drop_buffered_pointer_move(&command, commands.capacity()) {
            self.input_backpressure_count
                .fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        // A full queue must never silently discard a button/key release and
        // leave the remote computer with a logically stuck input state.
        if matches!(&command, ControllerCommand::Input { .. }) && commands.capacity() == 0 {
            self.input_backpressure_count
                .fetch_add(1, Ordering::Relaxed);
        }
        commands
            .send(command)
            .await
            .map_err(|_| "远程控制会话已结束，无法继续发送输入。".to_owned())
    }

    fn active_stream_id(&self) -> Option<u64> {
        self.status.lock().ok().and_then(|status| status.stream_id)
    }

    pub fn record_render_metrics(&self, metrics: ControllerRenderMetrics) -> Result<(), String> {
        let active_stream = self
            .status
            .lock()
            .map_err(|_| "DeskLink 无法读取当前远程画面状态。".to_owned())?
            .stream_id;
        if active_stream != Some(metrics.stream_id) {
            return Ok(());
        }
        if metrics.submitted_frames > metrics.received_frames
            || metrics.displayed_frames > metrics.submitted_frames
            || metrics
                .first_frame_ms
                .is_some_and(|value| value > 10 * 60 * 1_000)
        {
            return Err("远程画面指标无效。".to_owned());
        }
        DiagnosticLog::controller_for_current_user()
            .map_err(|_| "DeskLink 无法打开控制端诊断记录。".to_owned())?
            .record(&DiagnosticEvent::ControllerRenderMetrics {
                stream_id: metrics.stream_id,
                received_frames: metrics.received_frames,
                submitted_frames: metrics.submitted_frames,
                displayed_frames: metrics.displayed_frames,
                malformed_frames: metrics.malformed_frames,
                decoder_recoveries: metrics.decoder_recoveries,
                first_frame_ms: metrics.first_frame_ms,
            })
            .map_err(|_| "DeskLink 无法记录远程画面指标。".to_owned())
    }

    pub async fn disconnect(&self) -> Result<ControllerSnapshot, String> {
        self.remember_active_cancellation();
        self.cancel_operations();
        let _operation = self.operation_lock.lock().await;
        self.stop_current().await;
        self.set_status(ControllerRuntimeSummary::idle());
        load_snapshot(self.snapshot())
    }

    pub async fn forget_saved_device(
        &self,
        input: SavedDeviceInput,
    ) -> Result<ControllerSnapshot, String> {
        let device_id =
            crate::device_directory::parse_device_id(&input.device_id).map_err(str::to_owned)?;
        self.cancel_operations();
        let _operation = self.operation_lock.lock().await;
        self.stop_current().await;
        WindowsRecentAccessStore::for_current_user()
            .map_err(|_| "当前 Windows 账户无法使用已保存的设备密码。".to_owned())?
            .remove(device_id)
            .map_err(|_| "DeskLink 无法移除已保存的设备密码。".to_owned())?;
        let connection_store = WindowsControllerConnectionStore::for_current_user()
            .map_err(|_| "当前 Windows 账户无法使用已保存的控制端连接。".to_owned())?;
        let matches_saved_connection = connection_store
            .load()
            .map_err(|_| "无法打开已保存的控制端连接。".to_owned())?
            .is_some_and(|settings| {
                crate::device_directory::public_device_id(settings.host_device_id()) == device_id
            });
        if matches_saved_connection {
            connection_store
                .clear()
                .map_err(|_| "DeskLink 无法移除已保存的控制端连接。".to_owned())?;
        }
        self.set_status(ControllerRuntimeSummary::idle());
        load_snapshot(self.snapshot())
    }

    pub async fn rename_saved_device(
        &self,
        input: SavedDeviceRenameInput,
    ) -> Result<ControllerSnapshot, String> {
        let device_id =
            crate::device_directory::parse_device_id(&input.device_id).map_err(str::to_owned)?;
        let _operation = self.operation_lock.lock().await;
        WindowsRecentAccessStore::for_current_user()
            .map_err(|_| "当前 Windows 账户无法使用已保存的设备信息。".to_owned())?
            .rename(device_id, &input.alias)
            .map_err(|error| match error {
                WindowsRecentAccessError::InvalidAlias => error.to_string(),
                _ => "DeskLink 无法保存设备名称，请检查当前账户的数据目录。".to_owned(),
            })?;
        load_snapshot(self.snapshot())
    }

    pub async fn clear_saved_devices(&self) -> Result<ControllerSnapshot, String> {
        WindowsRecentAccessStore::for_current_user()
            .map_err(|_| "当前 Windows 账户无法使用已保存的设备密码。".to_owned())?
            .clear()
            .map_err(|_| "DeskLink 无法清除已保存的设备密码。".to_owned())?;
        load_snapshot(self.snapshot())
    }

    pub fn request_stop(&self) {
        self.cancel_operations();
        if let Ok(worker) = self.worker.lock()
            && let Some(worker) = worker.as_ref()
        {
            let _ = worker.cancellation.send(true);
        }
    }

    async fn stop_current(&self) {
        let worker = self.worker.lock().ok().and_then(|mut worker| worker.take());
        let Some(mut worker) = worker else {
            return;
        };
        let _ = worker.cancellation.send(true);
        if tokio::time::timeout(Duration::from_secs(5), &mut worker.task)
            .await
            .is_err()
        {
            worker.task.abort();
            let _ = worker.task.await;
        }
    }

    fn publish(&self, signals: &Channel<ControllerSignal>, status: ControllerRuntimeSummary) {
        self.set_status(status.clone());
        let _ = signals.send(ControllerSignal::Status { runtime: status });
    }

    fn set_status(&self, status: ControllerRuntimeSummary) {
        if let Ok(mut current) = self.status.lock() {
            *current = status;
        }
    }

    fn begin_operation(&self) -> u64 {
        self.operation_generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn cancel_operations(&self) {
        self.operation_generation.fetch_add(1, Ordering::SeqCst);
    }

    fn remember_active_cancellation(&self) {
        let active = self
            .status
            .lock()
            .map(|status| {
                matches!(
                    status.state,
                    "finding" | "connecting" | "waitingApproval" | "reconnecting"
                )
            })
            .unwrap_or(false);
        if active && let Ok(mut cancelled_at) = self.recent_cancellation.lock() {
            *cancelled_at = Some(Instant::now());
        }
    }

    fn take_recent_cancellation(&self) -> bool {
        self.recent_cancellation
            .lock()
            .ok()
            .and_then(|mut cancelled_at| cancelled_at.take())
            .is_some_and(|cancelled_at| cancelled_at.elapsed() <= RECENT_CANCELLATION_WINDOW)
    }

    fn is_current(&self, generation: u64) -> bool {
        self.operation_generation.load(Ordering::SeqCst) == generation
    }

    fn ensure_current(&self, generation: u64) -> Result<(), String> {
        if self.is_current(generation) {
            Ok(())
        } else {
            Err("连接已取消。".to_owned())
        }
    }

    fn publish_if_current(
        &self,
        generation: u64,
        signals: &Channel<ControllerSignal>,
        status: ControllerRuntimeSummary,
    ) {
        if self.is_current(generation) {
            self.publish(signals, status);
        }
    }
}

fn directory_transport_error_is_retryable(error: &TransportError) -> bool {
    matches!(
        error,
        TransportError::Connection(_)
            | TransportError::ConnectionLimit
            | TransportError::Stream(_)
            | TransportError::Closed
            | TransportError::JoinRejected(
                JoinRejectCode::Internal
                    | JoinRejectCode::ConnectionLimit
                    | JoinRejectCode::SessionLimit
            )
    )
}

fn directory_transport_error_message(error: &TransportError) -> &'static str {
    match error {
        TransportError::ConnectionLimit
        | TransportError::JoinRejected(
            JoinRejectCode::Internal
            | JoinRejectCode::ConnectionLimit
            | JoinRejectCode::SessionLimit,
        ) => "中继服务器当前繁忙，DeskLink 已自动重试，请稍后再试。",
        TransportError::Malformed => "中继服务器返回了不兼容的设备查询响应，请更新两台电脑后重试。",
        TransportError::InvalidConfig(_) => "DeskLink 内置中继配置无效，请重新安装最新版本。",
        TransportError::Connection(_) | TransportError::Stream(_) | TransportError::Closed => {
            "设备查询连接连续中断。请确认两台电脑均为最新版本，并保持目标电脑上的 DeskLink 在线。"
        }
        _ => "设备查询未能完成，请确认目标电脑在线并重新检查设备 ID 和访问密码。",
    }
}

pub fn load_snapshot(runtime: ControllerRuntimeSummary) -> Result<ControllerSnapshot, String> {
    let store = WindowsControllerConnectionStore::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用已保存的控制端连接。".to_owned())?;
    let (saved_connection, connection_error) = match store.load() {
        Ok(settings) => (
            settings.map(|settings| SavedControllerConnectionSummary {
                device_id: crate::device_directory::format_device_id(
                    crate::device_directory::public_device_id(settings.host_device_id()),
                ),
                relay_address: settings.relay_address_text(),
                server_name: settings.server_name().to_owned(),
                host_device_id: hex(&settings.host_device_id()),
                host_verify_key: hex(settings.host_verify_key().as_bytes()),
            }),
            None,
        ),
        Err(_) => (
            None,
            Some("无法打开已保存的控制端连接，请先移除记录再重新配对。".to_owned()),
        ),
    };
    let (saved_devices, saved_devices_error) = match WindowsRecentAccessStore::for_current_user() {
        Ok(store) => match store.load() {
            Ok(entries) => (
                entries.iter().map(saved_device_summary).collect::<Vec<_>>(),
                None,
            ),
            Err(_) => (
                Vec::new(),
                Some("无法解密已保存的设备密码，请移除异常记录后重新输入。".to_owned()),
            ),
        },
        Err(_) => (
            Vec::new(),
            Some("当前 Windows 账户无法使用加密的设备密码存储。".to_owned()),
        ),
    };
    Ok(ControllerSnapshot {
        runtime,
        saved_connection,
        connection_error,
        saved_devices,
        saved_devices_error,
    })
}

fn saved_device_summary(entry: &RecentAccessEntry) -> SavedDeviceCredentialSummary {
    SavedDeviceCredentialSummary {
        device_id: crate::device_directory::format_device_id(entry.device_id()),
        alias: entry.alias().map(str::to_owned),
        persistent: entry.is_persistent(),
        last_used_unix_s: entry.last_used_unix_s(),
    }
}

async fn run_controller(
    manager: ControllerManager,
    settings: ControllerConnectionSettings,
    mut save_after_approval: bool,
    mut commands: mpsc::Receiver<ControllerCommand>,
    mut cancellation: watch::Receiver<bool>,
    outputs: ControllerOutputChannels,
) {
    let ControllerOutputChannels {
        signals,
        video,
        audio,
    } = outputs;
    let diagnostics = DiagnosticLog::controller_for_current_user().ok();
    let _ = set_session_correlation(DiagnosticSource::Controller, settings.session_id());
    let mut schedule = ReconnectSchedule::new(ReconnectPolicy::default(), None);
    let mut attempt = 0_u32;
    let mut audio_enabled = true;
    let mut video_quality = VideoQualityPreference::Sharp;
    let mut queued_files = VecDeque::<OutgoingFile>::new();
    let mut file_queue_paused = false;
    'connect: loop {
        attempt = attempt.saturating_add(1);
        record_controller_diagnostic(
            diagnostics.as_ref(),
            ControllerDiagnosticStage::Connecting,
            attempt,
            None,
            None,
            None,
        );
        let connection = connect_once(&manager, &settings, &signals, diagnostics.as_ref(), attempt);
        tokio::pin!(connection);
        let mut runtime = loop {
            tokio::select! {
                changed = cancellation.changed() => {
                    if cancellation_requested(changed, &cancellation) {
                        finish_cancelled(&manager, &signals, diagnostics.as_ref(), attempt, None);
                        return;
                    }
                }
                command = commands.recv() => match command {
                    None => {
                        finish_cancelled(&manager, &signals, diagnostics.as_ref(), attempt, None);
                        return;
                    }
                    Some(ControllerCommand::SetAudioEnabled(enabled)) => {
                        audio_enabled = enabled;
                    }
                    Some(ControllerCommand::SetVideoQuality(preference)) => {
                        video_quality = preference;
                    }
                    Some(ControllerCommand::Input { .. })
                    | Some(ControllerCommand::Text(_))
                    | Some(ControllerCommand::RequestKeyframe)
                    | Some(ControllerCommand::SelectDisplay(_))
                    | Some(ControllerCommand::SendClipboard(_))
                    | Some(ControllerCommand::RequestClipboard)
                    | Some(ControllerCommand::SendFiles(_))
                    | Some(ControllerCommand::RetryFile(_))
                    | Some(ControllerCommand::RemoveQueuedFile(_))
                    | Some(ControllerCommand::ClearFileQueue)
                    | Some(ControllerCommand::ResumeFileQueue)
                    | Some(ControllerCommand::RequestRemoteFile)
                    | Some(ControllerCommand::CancelFile) => {}
                },
                result = &mut connection => match result {
                    Ok(runtime) => break runtime,
                    Err(failure) => {
                        if !schedule_failure(
                            &manager,
                            &signals,
                            &mut schedule,
                            failure,
                            ControllerWaitChannels {
                                commands: &mut commands,
                                cancellation: &mut cancellation,
                            },
                            diagnostics.as_ref(),
                            attempt,
                        ).await {
                            return;
                        }
                        continue 'connect;
                    }
                }
            }
        };
        if let Err(error) = runtime.set_audio_enabled(audio_enabled).await {
            let failure = ConnectFailure::from_controller(error);
            if !schedule_failure(
                &manager,
                &signals,
                &mut schedule,
                failure,
                ControllerWaitChannels {
                    commands: &mut commands,
                    cancellation: &mut cancellation,
                },
                diagnostics.as_ref(),
                attempt,
            )
            .await
            {
                return;
            }
            continue 'connect;
        }
        if let Err(error) = runtime.set_video_quality(video_quality).await {
            let failure = ConnectFailure::from_controller(error);
            if !schedule_failure(
                &manager,
                &signals,
                &mut schedule,
                failure,
                ControllerWaitChannels {
                    commands: &mut commands,
                    cancellation: &mut cancellation,
                },
                diagnostics.as_ref(),
                attempt,
            )
            .await
            {
                return;
            }
            continue 'connect;
        }

        let mut stable = false;
        let mut stable_since = None;
        let mut video_quality_ack_pending = true;
        let mut last_metrics = Instant::now();
        let mut last_feedback_metrics = runtime.metrics();
        let mut last_diagnostic_metrics = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        let mut next_transfer_request_id = 1_u64;
        let mut outgoing: Option<OutgoingTransfer> = None;
        let mut incoming: Option<IncomingTransfer> = None;
        let mut pending_remote_file_request: Option<u64> = None;
        let mut audio_decoder = ControllerAudioDecoder::new();
        let (file_failures, mut pending_file_failures) = mpsc::unbounded_channel();
        publish_file_queue(&signals, &queued_files, file_queue_paused);
        let failure = loop {
            if outgoing.is_none()
                && incoming.is_none()
                && pending_remote_file_request.is_none()
                && !file_queue_paused
                && let Some(file) = queued_files.pop_front()
            {
                if let Ok(mut action) = manager.last_file_action.lock() {
                    *action = Some(LastFileAction::Upload(file.path.clone()));
                }
                let offer = TransferMessage::FileOffer {
                    transfer_id: file.transfer_id,
                    name: file.name.clone(),
                    size: file.size,
                };
                if let Err(error) = runtime.send_transfer(offer).await {
                    queued_files.push_front(file);
                    break ConnectFailure::from_controller(error);
                }
                let _ = signals.send(ControllerSignal::FileTransfer {
                    state: "waiting",
                    direction: "upload",
                    name: file.name.clone(),
                    transferred: 0,
                    total: file.size,
                    message: "等待远端确认接收…".to_owned(),
                });
                outgoing = Some(OutgoingTransfer {
                    file,
                    cancellation: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                });
                publish_file_queue(&signals, &queued_files, file_queue_paused);
            }
            tokio::select! {
                changed = cancellation.changed() => {
                    if cancellation_requested(changed, &cancellation) {
                        finish_cancelled(&manager, &signals, diagnostics.as_ref(), attempt, None);
                        return;
                    }
                }
                command = commands.recv() => match command {
                    Some(ControllerCommand::Input { stream_id, event }) => {
                        if manager.active_stream_id() != Some(stream_id) {
                            continue;
                        }
                        if let Err(error) = runtime.send_input(event).await {
                            break ConnectFailure::from_controller(error);
                        }
                    }
                    Some(ControllerCommand::Text(text)) => {
                        if let Err(error) = send_text_input(&runtime, &text).await {
                            break ConnectFailure::from_controller(error);
                        }
                    }
                    Some(ControllerCommand::RequestKeyframe) => {
                        if let Err(error) = runtime.request_keyframe().await
                            && !matches!(error, ControllerError::NoActiveStream)
                        {
                            break ConnectFailure::from_controller(error);
                        }
                    }
                    Some(ControllerCommand::SelectDisplay(display_id)) => {
                        if let Err(error) = runtime.select_display(display_id).await {
                            break ConnectFailure::from_controller(error);
                        }
                    }
                    Some(ControllerCommand::SetAudioEnabled(enabled)) => {
                        audio_enabled = enabled;
                        if let Err(error) = runtime.set_audio_enabled(enabled).await {
                            break ConnectFailure::from_controller(error);
                        }
                    }
                    Some(ControllerCommand::SetVideoQuality(preference)) => {
                        video_quality = preference;
                        video_quality_ack_pending = true;
                        if let Err(error) = runtime.set_video_quality(preference).await {
                            break ConnectFailure::from_controller(error);
                        }
                    }
                    Some(ControllerCommand::SendClipboard(text)) => {
                        let request_id = next_transfer_request_id;
                        next_transfer_request_id = next_transfer_request_id.wrapping_add(1).max(1);
                        let _ = signals.send(ControllerSignal::Clipboard {
                            state: "sending",
                            message: "正在发送本机剪贴板…".to_owned(),
                        });
                        if let Err(error) = runtime.send_transfer(TransferMessage::ClipboardSet {
                            request_id,
                            text,
                        }).await {
                            break ConnectFailure::from_controller(error);
                        }
                    }
                    Some(ControllerCommand::RequestClipboard) => {
                        let request_id = next_transfer_request_id;
                        next_transfer_request_id = next_transfer_request_id.wrapping_add(1).max(1);
                        let _ = signals.send(ControllerSignal::Clipboard {
                            state: "receiving",
                            message: "正在读取远端剪贴板…".to_owned(),
                        });
                        if let Err(error) = runtime.send_transfer(TransferMessage::ClipboardRequest {
                            request_id,
                        }).await {
                            break ConnectFailure::from_controller(error);
                        }
                    }
                    Some(ControllerCommand::SendFiles(files)) => {
                        let occupied = queued_files.len() + usize::from(outgoing.is_some());
                        if files.is_empty() {
                            continue;
                        }
                        if occupied.saturating_add(files.len()) > MAX_FILE_QUEUE_ITEMS {
                            let _ = signals.send(ControllerSignal::FileTransfer {
                                state: "failed",
                                direction: "upload",
                                name: "文件队列".to_owned(),
                                transferred: 0,
                                total: 0,
                                message: format!("发送队列最多保留 {MAX_FILE_QUEUE_ITEMS} 个文件。"),
                            });
                            continue;
                        }
                        queued_files.extend(files);
                        publish_file_queue(&signals, &queued_files, file_queue_paused);
                    }
                    Some(ControllerCommand::RetryFile(file)) => {
                        if queued_files.len() + usize::from(outgoing.is_some())
                            >= MAX_FILE_QUEUE_ITEMS
                        {
                            let _ = signals.send(ControllerSignal::FileTransfer {
                                state: "failed",
                                direction: "upload",
                                name: file.name,
                                transferred: 0,
                                total: file.size,
                                message: "发送队列已满，请先移除一个等待文件。".to_owned(),
                            });
                            continue;
                        }
                        queued_files.push_front(file);
                        file_queue_paused = false;
                        publish_file_queue(&signals, &queued_files, file_queue_paused);
                    }
                    Some(ControllerCommand::RemoveQueuedFile(transfer_id)) => {
                        queued_files.retain(|file| file.transfer_id != transfer_id);
                        if queued_files.is_empty() {
                            file_queue_paused = false;
                        }
                        publish_file_queue(&signals, &queued_files, file_queue_paused);
                    }
                    Some(ControllerCommand::ClearFileQueue) => {
                        queued_files.clear();
                        file_queue_paused = false;
                        publish_file_queue(&signals, &queued_files, file_queue_paused);
                    }
                    Some(ControllerCommand::ResumeFileQueue) => {
                        file_queue_paused = false;
                        publish_file_queue(&signals, &queued_files, file_queue_paused);
                    }
                    Some(ControllerCommand::RequestRemoteFile) => {
                        if outgoing.is_some()
                            || incoming.is_some()
                            || pending_remote_file_request.is_some()
                            || !queued_files.is_empty()
                        {
                            let _ = signals.send(ControllerSignal::FileTransfer {
                                state: "failed",
                                direction: "download",
                                name: "远端文件".to_owned(),
                                transferred: 0,
                                total: 0,
                                message: "请等待发送队列完成，或先清空等待中的文件。".to_owned(),
                            });
                            continue;
                        }
                        let request_id = next_transfer_request_id;
                        next_transfer_request_id = next_transfer_request_id.wrapping_add(1).max(1);
                        if let Err(error) = runtime.send_transfer(
                            TransferMessage::FileSelectionRequest { request_id },
                        ).await {
                            break ConnectFailure::from_controller(error);
                        }
                        pending_remote_file_request = Some(request_id);
                        let _ = signals.send(ControllerSignal::FileTransfer {
                            state: "waiting",
                            direction: "download",
                            name: "等待远端选择文件".to_owned(),
                            transferred: 0,
                            total: 0,
                            message: "请在远端电脑上选择要发送的文件…".to_owned(),
                        });
                    }
                    Some(ControllerCommand::CancelFile) => {
                        if let Some(transfer) = outgoing.take() {
                            transfer.cancellation.store(true, Ordering::Release);
                            let _ = runtime.send_transfer(TransferMessage::Cancel {
                                transfer_id: transfer.file.transfer_id,
                            }).await;
                            let _ = signals.send(ControllerSignal::FileTransfer {
                                state: "cancelled",
                                direction: "upload",
                                name: transfer.file.name,
                                transferred: 0,
                                total: transfer.file.size,
                                message: "文件传输已取消。".to_owned(),
                            });
                            if !queued_files.is_empty() {
                                file_queue_paused = true;
                                publish_file_queue(&signals, &queued_files, file_queue_paused);
                            }
                        } else if let Some(transfer) = incoming.take() {
                            let _ = runtime.send_transfer(TransferMessage::Cancel {
                                transfer_id: transfer.file.transfer_id(),
                            }).await;
                            let _ = signals.send(ControllerSignal::FileTransfer {
                                state: "cancelled",
                                direction: "download",
                                name: transfer.name,
                                transferred: transfer.transferred,
                                total: transfer.size,
                                message: "文件接收已取消，未完成的临时文件已删除。".to_owned(),
                            });
                        } else if let Some(request_id) = pending_remote_file_request.take() {
                            let _ = runtime.send_transfer(
                                TransferMessage::FileSelectionCancel { request_id },
                            ).await;
                            let _ = signals.send(ControllerSignal::FileTransfer {
                                state: "cancelled",
                                direction: "download",
                                name: "远端文件".to_owned(),
                                transferred: 0,
                                total: 0,
                                message: "已取消从远端获取文件。".to_owned(),
                            });
                        }
                    }
                    None => {
                        finish_cancelled(&manager, &signals, diagnostics.as_ref(), attempt, None);
                        return;
                    }
                },
                event = runtime.next_event() => match event {
                    Ok(ControllerEvent::VideoConfig(config)) => {
                        if !stable {
                            record_controller_diagnostic(
                                diagnostics.as_ref(),
                                ControllerDiagnosticStage::Connected,
                                attempt,
                                None,
                                None,
                                None,
                            );
                        }
                        stable = true;
                        stable_since.get_or_insert_with(Instant::now);
                        if save_after_approval {
                            if WindowsControllerConnectionStore::for_current_user()
                                .and_then(|store| store.save(&settings))
                                .is_err()
                            {
                                manager.publish(
                                    &signals,
                                    ControllerRuntimeSummary::stopped(
                                        "主机已批准此电脑，但 Windows 无法加密保护已保存的连接。",
                                    ),
                                );
                                return;
                            }
                            save_after_approval = false;
                        }
                        manager.publish(&signals, ControllerRuntimeSummary::connected(config.stream_id));
                        let _ = signals.send(ControllerSignal::VideoConfig {
                            stream_id: config.stream_id,
                            config_version: config.config_version,
                            width: config.width,
                            height: config.height,
                            sequence_header: config.sequence_header,
                        });
                    }
                    Ok(ControllerEvent::H264AccessUnit(frame)) => {
                        let mut payload = Vec::with_capacity(FRAME_PREFIX_BYTES + frame.data.len());
                        payload.push(u8::from(frame.flags.0 & FrameFlags::KEYFRAME.0 != 0));
                        payload.extend_from_slice(&frame.capture_timestamp_us.to_le_bytes());
                        payload.extend_from_slice(&frame.frame_id.to_le_bytes());
                        payload.extend_from_slice(&frame.data);
                        if video.send(Response::new(payload)).is_err() {
                            manager.set_status(ControllerRuntimeSummary::idle());
                            return;
                        }
                    }
                    Ok(ControllerEvent::Cursor(cursor)) => {
                        let _ = signals.send(ControllerSignal::Cursor {
                            stream_id: cursor.stream_id,
                            sequence: cursor.sequence,
                            x_millionths: cursor.x_millionths,
                            y_millionths: cursor.y_millionths,
                            visible: cursor.visible,
                        });
                    }
                    Ok(ControllerEvent::Audio(packet)) => {
                        const AUDIO_PREFIX_BYTES: usize = 28;
                        for packet in audio_decoder.decode(packet) {
                            let mut payload =
                                Vec::with_capacity(AUDIO_PREFIX_BYTES + packet.payload.len());
                            payload.extend_from_slice(&packet.stream_id.to_le_bytes());
                            payload.extend_from_slice(&packet.sequence.to_le_bytes());
                            payload.extend_from_slice(&packet.capture_timestamp_us.to_le_bytes());
                            payload.extend_from_slice(&packet.sample_rate.to_le_bytes());
                            payload.extend_from_slice(&packet.payload);
                            let _ = audio.send(Response::new(payload));
                        }
                    }
                    Ok(ControllerEvent::Control(ControlMessage::DisplayList {
                        displays,
                        active_display_id,
                    })) => {
                        let _ = signals.send(ControllerSignal::Displays {
                            displays: displays
                                .into_iter()
                                .map(|display| ControllerDisplaySummary {
                                    id: display.id,
                                    width: display.width,
                                    height: display.height,
                                    primary: display.primary,
                                })
                                .collect(),
                            active_display_id,
                        });
                    }
                    Ok(ControllerEvent::Control(ControlMessage::AudioState {
                        available,
                        enabled,
                    })) => {
                        let (state, message) = if !available {
                            (
                                "unavailable",
                                "远端系统声音当前不可用，画面和控制不受影响。",
                            )
                        } else if enabled {
                            ("enabled", "正在播放远端系统声音。")
                        } else {
                            ("muted", "远端系统声音已静音。")
                        };
                        let _ = signals.send(ControllerSignal::Audio {
                            state,
                            enabled,
                            message: message.to_owned(),
                        });
                    }
                    Ok(ControllerEvent::Control(ControlMessage::VideoQualityState {
                        preference,
                        preset,
                    })) => {
                        if video_quality_ack_pending && preference != video_quality {
                            continue;
                        }
                        video_quality_ack_pending = false;
                        video_quality = preference;
                        let _ = signals.send(ControllerSignal::VideoQuality {
                            preference,
                            preset,
                        });
                    }
                    Ok(ControllerEvent::Control(_)) => {}
                    Ok(ControllerEvent::Transfer(message)) => match message {
                        TransferMessage::ClipboardData { text, .. } => {
                            let write = tauri::async_runtime::spawn_blocking(move || {
                                apps_windows::transfer::write_clipboard_text(&text)
                            }).await.unwrap_or(Err(TransferResult::IoFailed));
                            let (state, message) = if write.is_ok() {
                                ("completed", "远端文本已复制到本机剪贴板。")
                            } else {
                                ("failed", "无法写入本机剪贴板，请稍后重试。")
                            };
                            let _ = signals.send(ControllerSignal::Clipboard {
                                state,
                                message: message.to_owned(),
                            });
                        }
                        TransferMessage::ClipboardResult { result, .. } => {
                            let completed = result == TransferResult::Completed;
                            let _ = signals.send(ControllerSignal::Clipboard {
                                state: if completed { "completed" } else { "failed" },
                                message: if completed {
                                    "本机文本已写入远端剪贴板。"
                                } else {
                                    transfer_result_message(result)
                                }.to_owned(),
                            });
                        }
                        TransferMessage::FileSelectionResult { request_id, result } => {
                            if pending_remote_file_request != Some(request_id) {
                                continue;
                            }
                            pending_remote_file_request = None;
                            let state = match result {
                                TransferResult::Cancelled => "cancelled",
                                TransferResult::Rejected => "rejected",
                                _ => "failed",
                            };
                            let _ = signals.send(ControllerSignal::FileTransfer {
                                state,
                                direction: "download",
                                name: "远端文件".to_owned(),
                                transferred: 0,
                                total: 0,
                                message: transfer_result_message(result).to_owned(),
                            });
                        }
                        TransferMessage::FileOffer { transfer_id, name, size } => {
                            let requested = pending_remote_file_request.take().is_some();
                            if !requested || outgoing.is_some() || incoming.is_some() {
                                if let Err(error) = runtime.send_transfer(
                                    TransferMessage::FileDecision { transfer_id, accepted: false },
                                ).await {
                                    break ConnectFailure::from_controller(error);
                                }
                                continue;
                            }
                            let incoming_file = tauri::async_runtime::spawn_blocking({
                                let name = name.clone();
                                move || IncomingFile::create(transfer_id, name, size)
                            }).await.unwrap_or(Err(TransferResult::IoFailed));
                            let accepted = incoming_file.is_ok();
                            if let Err(error) = runtime.send_transfer(
                                TransferMessage::FileDecision { transfer_id, accepted },
                            ).await {
                                break ConnectFailure::from_controller(error);
                            }
                            match incoming_file {
                                Ok(file) => {
                                    let _ = signals.send(ControllerSignal::FileTransfer {
                                        state: "receiving",
                                        direction: "download",
                                        name: name.clone(),
                                        transferred: 0,
                                        total: size,
                                        message: "正在接收远端文件… 0%".to_owned(),
                                    });
                                    incoming = Some(IncomingTransfer {
                                        file,
                                        name,
                                        size,
                                        transferred: 0,
                                    });
                                }
                                Err(result) => {
                                    let _ = signals.send(ControllerSignal::FileTransfer {
                                        state: "failed",
                                        direction: "download",
                                        name,
                                        transferred: 0,
                                        total: size,
                                        message: transfer_result_message(result).to_owned(),
                                    });
                                }
                            }
                        }
                        TransferMessage::FileChunk { transfer_id, offset, bytes } => {
                            let result = incoming
                                .as_mut()
                                .filter(|transfer| transfer.file.transfer_id() == transfer_id)
                                .ok_or(TransferResult::InvalidData)
                                .and_then(|transfer| {
                                    transfer.file.write_chunk(offset, &bytes)?;
                                    transfer.transferred = offset.saturating_add(bytes.len() as u64);
                                    Ok((
                                        transfer.name.clone(),
                                        transfer.transferred,
                                        transfer.size,
                                    ))
                                });
                            match result {
                                Ok((name, transferred, total)) => {
                                    let _ = signals.send(ControllerSignal::FileTransfer {
                                        state: "receiving",
                                        direction: "download",
                                        name,
                                        transferred,
                                        total,
                                        message: format!(
                                            "正在接收… {}%",
                                            transfer_percent(transferred, total)
                                        ),
                                    });
                                }
                                Err(result) => {
                                    let failed = incoming.take();
                                    if let Err(error) = runtime.send_transfer(
                                        TransferMessage::FileResult { transfer_id, result },
                                    ).await {
                                        break ConnectFailure::from_controller(error);
                                    }
                                    let _ = signals.send(ControllerSignal::FileTransfer {
                                        state: "failed",
                                        direction: "download",
                                        name: failed.as_ref().map(|item| item.name.clone())
                                            .unwrap_or_else(|| "远端文件".to_owned()),
                                        transferred: failed.as_ref().map_or(0, |item| item.transferred),
                                        total: failed.as_ref().map_or(0, |item| item.size),
                                        message: transfer_result_message(result).to_owned(),
                                    });
                                }
                            }
                        }
                        TransferMessage::FileComplete { transfer_id, content_hash } => {
                            let transfer = incoming.take();
                            let (name, size, result) = match transfer {
                                Some(transfer) if transfer.file.transfer_id() == transfer_id => {
                                    let name = transfer.name;
                                    let size = transfer.size;
                                    let result = tauri::async_runtime::spawn_blocking(move || {
                                        transfer.file.finish(content_hash)
                                    }).await.unwrap_or(Err(TransferResult::IoFailed));
                                    (name, size, result)
                                }
                                Some(transfer) => {
                                    incoming = Some(transfer);
                                    ("远端文件".to_owned(), 0, Err(TransferResult::InvalidData))
                                }
                                None => ("远端文件".to_owned(), 0, Err(TransferResult::InvalidData)),
                            };
                            let transfer_result = match result {
                                Ok(path) => {
                                    tauri::async_runtime::spawn_blocking(move || {
                                        apps_windows::transfer::notify_file_received(&path)
                                    });
                                    TransferResult::Completed
                                }
                                Err(result) => result,
                            };
                            if let Err(error) = runtime.send_transfer(TransferMessage::FileResult {
                                transfer_id,
                                result: transfer_result,
                            }).await {
                                break ConnectFailure::from_controller(error);
                            }
                            let completed = transfer_result == TransferResult::Completed;
                            let _ = signals.send(ControllerSignal::FileTransfer {
                                state: if completed { "completed" } else { "failed" },
                                direction: "download",
                                name,
                                transferred: if completed { size } else { 0 },
                                total: size,
                                message: if completed {
                                    "文件已保存到本机的“下载”文件夹。"
                                } else {
                                    transfer_result_message(transfer_result)
                                }.to_owned(),
                            });
                        }
                        TransferMessage::FileDecision { transfer_id, accepted } => {
                            let matches = outgoing.as_ref().is_some_and(|transfer| {
                                transfer.file.transfer_id == transfer_id
                            });
                            if !matches {
                                continue;
                            }
                            if !accepted {
                                let transfer = outgoing.take().expect("checked outgoing transfer");
                                let _ = signals.send(ControllerSignal::FileTransfer {
                                    state: "rejected",
                                    direction: "upload",
                                    name: transfer.file.name,
                                    transferred: 0,
                                    total: transfer.file.size,
                                    message: "远端已拒绝接收此文件。".to_owned(),
                                });
                                if !queued_files.is_empty() {
                                    file_queue_paused = true;
                                    publish_file_queue(
                                        &signals,
                                        &queued_files,
                                        file_queue_paused,
                                    );
                                }
                                continue;
                            }
                            let transfer = outgoing.as_ref().expect("checked outgoing transfer");
                            let sender = runtime.transfer_sender();
                            let signals = signals.clone();
                            let cancellation = transfer.cancellation.clone();
                            let path = transfer.file.path.clone();
                            let name = transfer.file.name.clone();
                            let size = transfer.file.size;
                            let file_failures = file_failures.clone();
                            tauri::async_runtime::spawn(async move {
                                if let Err(message) = send_outgoing_file(
                                    sender,
                                    transfer_id,
                                    path,
                                    name.clone(),
                                    size,
                                    cancellation,
                                    signals.clone(),
                                ).await {
                                    let _ = file_failures.send(OutgoingFileFailure {
                                        transfer_id,
                                        message,
                                    });
                                }
                            });
                        }
                        TransferMessage::FileResult { transfer_id, result } => {
                            if outgoing.as_ref().is_some_and(|transfer| {
                                transfer.file.transfer_id == transfer_id
                            }) {
                                let transfer = outgoing.take().expect("checked outgoing transfer");
                                transfer.cancellation.store(true, Ordering::Release);
                                let completed = result == TransferResult::Completed;
                                let _ = signals.send(ControllerSignal::FileTransfer {
                                    state: if completed { "completed" } else { "failed" },
                                    direction: "upload",
                                    name: transfer.file.name,
                                    transferred: if completed { transfer.file.size } else { 0 },
                                    total: transfer.file.size,
                                    message: if completed {
                                        "文件已保存到远端的“下载”文件夹。"
                                    } else {
                                        transfer_result_message(result)
                                    }.to_owned(),
                                });
                                if !completed && !queued_files.is_empty() {
                                    file_queue_paused = true;
                                    publish_file_queue(
                                        &signals,
                                        &queued_files,
                                        file_queue_paused,
                                    );
                                }
                            }
                        }
                        TransferMessage::Cancel { transfer_id } => {
                            if outgoing.as_ref().is_some_and(|transfer| {
                                transfer.file.transfer_id == transfer_id
                            }) {
                                let transfer = outgoing.take().expect("checked outgoing transfer");
                                transfer.cancellation.store(true, Ordering::Release);
                                let _ = signals.send(ControllerSignal::FileTransfer {
                                    state: "cancelled",
                                    direction: "upload",
                                    name: transfer.file.name,
                                    transferred: 0,
                                    total: transfer.file.size,
                                    message: "远端已取消文件接收。".to_owned(),
                                });
                                if !queued_files.is_empty() {
                                    file_queue_paused = true;
                                    publish_file_queue(
                                        &signals,
                                        &queued_files,
                                        file_queue_paused,
                                    );
                                }
                            } else if incoming.as_ref().is_some_and(|transfer| {
                                transfer.file.transfer_id() == transfer_id
                            }) {
                                let transfer = incoming.take().expect("checked incoming transfer");
                                let _ = signals.send(ControllerSignal::FileTransfer {
                                    state: "cancelled",
                                    direction: "download",
                                    name: transfer.name,
                                    transferred: transfer.transferred,
                                    total: transfer.size,
                                    message: "远端已取消文件发送。".to_owned(),
                                });
                            }
                        }
                        TransferMessage::ClipboardSet { .. }
                        | TransferMessage::ClipboardRequest { .. }
                        | TransferMessage::FileSelectionRequest { .. }
                        | TransferMessage::FileSelectionCancel { .. } => {}
                    },
                    Ok(ControllerEvent::Closed { reason }) => {
                        break ConnectFailure::retryable(format!("transport closed: {reason}"));
                    }
                    Err(error) => break ConnectFailure::from_controller(error),
                },
                Some(failed) = pending_file_failures.recv() => {
                    let matches = outgoing.as_ref().is_some_and(|transfer| {
                        transfer.file.transfer_id == failed.transfer_id
                    });
                    if !matches {
                        continue;
                    }
                    let transfer = outgoing.take().expect("checked outgoing transfer");
                    transfer.cancellation.store(true, Ordering::Release);
                    let _ = runtime.send_transfer(TransferMessage::Cancel {
                        transfer_id: transfer.file.transfer_id,
                    }).await;
                    let _ = signals.send(ControllerSignal::FileTransfer {
                        state: "failed",
                        direction: "upload",
                        name: transfer.file.name,
                        transferred: 0,
                        total: transfer.file.size,
                        message: failed.message,
                    });
                    if !queued_files.is_empty() {
                        file_queue_paused = true;
                        publish_file_queue(&signals, &queued_files, file_queue_paused);
                    }
                }
            }
            if last_metrics.elapsed() >= Duration::from_secs(1) {
                let metrics = runtime.metrics();
                let received_packets = metrics
                    .received_video_packets
                    .saturating_sub(last_feedback_metrics.received_video_packets);
                let dropped_packets = metrics
                    .dropped_video_packets
                    .saturating_sub(last_feedback_metrics.dropped_video_packets);
                last_feedback_metrics = metrics;
                if video_quality == VideoQualityPreference::Automatic
                    && received_packets.saturating_add(dropped_packets) > 0
                    && let Err(error) = runtime
                        .report_video_network_feedback(
                            u32::try_from(received_packets).unwrap_or(u32::MAX),
                            u32::try_from(dropped_packets).unwrap_or(u32::MAX),
                        )
                        .await
                {
                    break ConnectFailure::from_controller(error);
                }
                let _ = signals.send(ControllerSignal::Metrics {
                    received_video_packets: metrics.received_video_packets,
                    dropped_video_packets: metrics.dropped_video_packets,
                    completed_frames: metrics.completed_frames,
                });
                if last_diagnostic_metrics.elapsed() >= Duration::from_secs(10) {
                    if let Some(diagnostics) = diagnostics.as_ref() {
                        let _ = diagnostics.record(&DiagnosticEvent::ControllerVideoMetrics {
                            attempt,
                            received_video_packets: metrics.received_video_packets,
                            dropped_video_packets: metrics.dropped_video_packets,
                            completed_frames: metrics.completed_frames,
                            input_backpressure_count: manager
                                .input_backpressure_count
                                .load(Ordering::Relaxed),
                        });
                    }
                    last_diagnostic_metrics = Instant::now();
                }
                last_metrics = Instant::now();
            }
        };
        if let Some(transfer) = outgoing.take() {
            transfer.cancellation.store(true, Ordering::Release);
            let _ = signals.send(ControllerSignal::FileTransfer {
                state: "failed",
                direction: "upload",
                name: transfer.file.name,
                transferred: 0,
                total: transfer.file.size,
                message: "连接中断，文件传输未完成。".to_owned(),
            });
            if !queued_files.is_empty() {
                file_queue_paused = true;
                publish_file_queue(&signals, &queued_files, file_queue_paused);
            }
        }
        if let Some(transfer) = incoming.take() {
            let _ = signals.send(ControllerSignal::FileTransfer {
                state: "failed",
                direction: "download",
                name: transfer.name,
                transferred: transfer.transferred,
                total: transfer.size,
                message: "连接中断，文件接收未完成；临时文件已删除。".to_owned(),
            });
        } else if pending_remote_file_request.take().is_some() {
            let _ = signals.send(ControllerSignal::FileTransfer {
                state: "failed",
                direction: "download",
                name: "远端文件".to_owned(),
                transferred: 0,
                total: 0,
                message: "连接中断，远端文件选择未完成。".to_owned(),
            });
        }
        if stable_since.is_some_and(|started| session_earned_fresh_retry_budget(started.elapsed()))
        {
            schedule.reset();
        }
        if !schedule_failure(
            &manager,
            &signals,
            &mut schedule,
            failure,
            ControllerWaitChannels {
                commands: &mut commands,
                cancellation: &mut cancellation,
            },
            diagnostics.as_ref(),
            attempt,
        )
        .await
        {
            return;
        }
    }
}

enum FileReadEvent {
    Chunk { offset: u64, bytes: Vec<u8> },
    Complete([u8; 32]),
    Failed,
}

async fn send_outgoing_file(
    sender: ControllerTransferSender,
    transfer_id: TransferId,
    path: PathBuf,
    name: String,
    size: u64,
    cancellation: Arc<std::sync::atomic::AtomicBool>,
    signals: Channel<ControllerSignal>,
) -> Result<(), String> {
    let (events, mut receiver) = mpsc::channel(2);
    let producer_cancellation = cancellation.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = (|| {
            let mut file = File::open(path).map_err(|_| ())?;
            let mut hasher = Blake2s256::new();
            let mut offset = 0_u64;
            loop {
                if producer_cancellation.load(Ordering::Acquire) {
                    return Ok(());
                }
                let mut bytes = vec![0_u8; MAX_TRANSFER_CHUNK_BYTES];
                let read = file.read(&mut bytes).map_err(|_| ())?;
                if read == 0 {
                    break;
                }
                bytes.truncate(read);
                hasher.update(&bytes);
                events
                    .blocking_send(FileReadEvent::Chunk { offset, bytes })
                    .map_err(|_| ())?;
                offset = offset.checked_add(read as u64).ok_or(())?;
            }
            if offset != size {
                return Err(());
            }
            let hash: [u8; 32] = hasher.finalize().into();
            events
                .blocking_send(FileReadEvent::Complete(hash))
                .map_err(|_| ())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = events.blocking_send(FileReadEvent::Failed);
        }
    });

    while let Some(event) = receiver.recv().await {
        if cancellation.load(Ordering::Acquire) {
            return Ok(());
        }
        match event {
            FileReadEvent::Chunk { offset, bytes } => {
                let transferred = offset.saturating_add(bytes.len() as u64);
                sender
                    .send(TransferMessage::FileChunk {
                        transfer_id,
                        offset,
                        bytes,
                    })
                    .await
                    .map_err(|_| "发送文件时连接中断。".to_owned())?;
                let _ = signals.send(ControllerSignal::FileTransfer {
                    state: "sending",
                    direction: "upload",
                    name: name.clone(),
                    transferred,
                    total: size,
                    message: format!("正在发送… {}%", transfer_percent(transferred, size)),
                });
            }
            FileReadEvent::Complete(content_hash) => {
                sender
                    .send(TransferMessage::FileComplete {
                        transfer_id,
                        content_hash,
                    })
                    .await
                    .map_err(|_| "发送完成确认时连接中断。".to_owned())?;
                let _ = signals.send(ControllerSignal::FileTransfer {
                    state: "verifying",
                    direction: "upload",
                    name,
                    transferred: size,
                    total: size,
                    message: "发送完成，等待远端校验文件…".to_owned(),
                });
                return Ok(());
            }
            FileReadEvent::Failed => return Err("无法继续读取所选文件。".to_owned()),
        }
    }
    Err("文件读取任务意外停止。".to_owned())
}

fn transfer_percent(transferred: u64, total: u64) -> u64 {
    if total == 0 {
        100
    } else {
        transferred
            .saturating_mul(100)
            .saturating_div(total)
            .min(100)
    }
}

fn transfer_result_message(result: TransferResult) -> &'static str {
    match result {
        TransferResult::Completed => "操作已完成。",
        TransferResult::Rejected => "远端已拒绝此操作。",
        TransferResult::Cancelled => "操作已取消。",
        TransferResult::TooLarge => "内容超过 DeskLink 当前允许的大小。",
        TransferResult::InvalidData => "内容校验失败，未保存文件。",
        TransferResult::PermissionDenied => "Windows 拒绝访问剪贴板或下载文件夹。",
        TransferResult::IoFailed => "读写内容失败，请检查 Windows 权限和磁盘空间。",
        TransferResult::Unsupported => "远端暂不支持此操作。",
        TransferResult::Busy => "另一项文件传输正在进行，请稍后重试。",
    }
}

fn prepare_file_error_message(result: TransferResult) -> &'static str {
    match result {
        TransferResult::TooLarge => "单个文件不能超过 256 MB。",
        TransferResult::InvalidData => "请选择文件名有效的普通文件，而不是文件夹。",
        _ => "无法读取所选文件，请检查文件权限后重试。",
    }
}

fn session_earned_fresh_retry_budget(connected_for: Duration) -> bool {
    connected_for >= RECONNECT_BUDGET_RESET_AFTER
}

async fn connect_once(
    manager: &ControllerManager,
    settings: &ControllerConnectionSettings,
    signals: &Channel<ControllerSignal>,
    diagnostics: Option<&DiagnosticLog>,
    attempt: u32,
) -> Result<ControllerRuntime, ConnectFailure> {
    manager.publish(signals, ControllerRuntimeSummary::connecting());
    let config =
        crate::local_relay::client_config(settings.relay_address(), settings.server_name())
            .map_err(ConnectFailure::from_transport)?;
    let client = QuicClient::connect(config)
        .await
        .map_err(ConnectFailure::from_transport)?;
    record_controller_diagnostic(
        diagnostics,
        ControllerDiagnosticStage::RelayConnected,
        attempt,
        None,
        None,
        None,
    );
    let identity = WindowsIdentityStore::for_current_user()
        .map_err(|_| ConnectFailure::permanent("控制端身份存储不可用"))?
        .load_or_create(&mut OsRng)
        .map_err(|_| ConnectFailure::permanent("无法打开控制端身份"))?;
    client
        .join(RelayJoin::controller_with_participant(
            settings.session_id(),
            *settings.authentication(),
            identity.device_id,
        ))
        .await
        .map_err(ConnectFailure::from_transport)?;
    record_controller_diagnostic(
        diagnostics,
        ControllerDiagnosticStage::RelayJoined,
        attempt,
        None,
        None,
        None,
    );
    let runtime = ControllerRuntime::connect_for_platform_with_observer(
        client,
        identity,
        settings.host_verify_key(),
        Platform::Windows,
        || {
            record_controller_diagnostic(
                diagnostics,
                ControllerDiagnosticStage::WaitingForApproval,
                attempt,
                None,
                None,
                None,
            );
            manager.publish(signals, ControllerRuntimeSummary::waiting_for_approval());
        },
    )
    .await
    .map_err(ConnectFailure::from_controller)?;
    record_controller_diagnostic(
        diagnostics,
        ControllerDiagnosticStage::SecureSessionReady,
        attempt,
        None,
        None,
        None,
    );
    Ok(runtime)
}

struct ConnectFailure {
    retryable: bool,
    detail: &'static str,
    kind: &'static str,
    technical_reason: String,
}

impl ConnectFailure {
    fn with_reason(
        retryable: bool,
        detail: &'static str,
        kind: &'static str,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            retryable,
            detail,
            kind,
            technical_reason: reason.into(),
        }
    }

    fn permanent(reason: impl Into<String>) -> Self {
        Self::with_reason(
            false,
            "已保存的身份或连接与主机不匹配，请重新配对此电脑。",
            "permanent",
            reason,
        )
    }

    fn retryable(reason: impl Into<String>) -> Self {
        Self::with_reason(
            true,
            "中继服务器或主机暂时不可用。",
            "transport_unavailable",
            reason,
        )
    }

    fn from_transport(error: TransportError) -> Self {
        let technical_reason = error.to_string();
        match error {
            TransportError::JoinRejected(JoinRejectCode::SessionNotFound) => Self::with_reason(
                true,
                "主机连接窗口尚未就绪或临时密码已经失效，请在主机上重新生成临时密码。",
                "session_not_found",
                technical_reason,
            ),
            TransportError::JoinRejected(JoinRejectCode::SessionOccupied) => Self::with_reason(
                true,
                "此会话已有控制端连接，请先断开原控制端后再试。",
                "session_occupied",
                technical_reason,
            ),
            TransportError::JoinRejected(
                JoinRejectCode::ConnectionLimit | JoinRejectCode::SessionLimit,
            )
            | TransportError::ConnectionLimit => Self::with_reason(
                true,
                "中继服务器当前连接数量已满，请稍后重试。",
                "relay_capacity",
                technical_reason,
            ),
            TransportError::JoinRejected(JoinRejectCode::AuthenticationMismatch) => {
                Self::with_reason(
                    false,
                    "连接请求与主机的中继会话不匹配，请在主机上重新生成临时密码。",
                    "authentication_mismatch",
                    technical_reason,
                )
            }
            TransportError::InvalidConfig(_) => Self::with_reason(
                false,
                "目标主机的中继设置无效，请在主机上重新生成临时密码。",
                "invalid_relay_config",
                technical_reason,
            ),
            TransportError::Connection(_)
            | TransportError::Stream(_)
            | TransportError::Datagram(_)
            | TransportError::Closed
            | TransportError::PeerDisconnected
            | TransportError::PeerReplaced
            | TransportError::JoinRejected(JoinRejectCode::Internal) => {
                Self::retryable(technical_reason)
            }
            _ => Self::permanent(technical_reason),
        }
    }

    fn from_controller(error: ControllerError) -> Self {
        let technical_reason = error.to_string();
        if let ControllerError::AccessDenied(reason) = error {
            return match reason {
                AccessDenialReason::ApprovalRejected => Self::with_reason(
                    false,
                    "主机已拒绝本次控制请求。需要连接时，请重新发起并在主机上允许。",
                    "approval_rejected",
                    technical_reason,
                ),
                AccessDenialReason::ApprovalExpired => Self::with_reason(
                    false,
                    "主机确认请求已过期，请重新连接并及时在主机上允许。",
                    "approval_expired",
                    technical_reason,
                ),
                AccessDenialReason::ControllerNotTrusted => Self::with_reason(
                    false,
                    "主机不再信任此控制端，请在主机上重新配对或启用固定密码后再试。",
                    "controller_not_trusted",
                    technical_reason,
                ),
                AccessDenialReason::ControllerIdentityChanged => Self::with_reason(
                    false,
                    "此电脑的安全身份已变化，主机拒绝了旧信任。请在主机上核对警告并重新允许。",
                    "controller_identity_changed",
                    technical_reason,
                ),
                AccessDenialReason::HostUnavailable => Self::with_reason(
                    false,
                    "主机已批准连接，但当前无法启动屏幕采集。请解锁主机屏幕、退出远程桌面或兼容模式后重试。",
                    "host_unavailable",
                    technical_reason,
                ),
                AccessDenialReason::HostCaptureFailed => Self::with_reason(
                    false,
                    "主机已批准连接，但屏幕采集在首帧阶段失败。请解锁主机、退出 Windows 远程桌面，并更新显示驱动后重试。",
                    "host_capture_failed",
                    technical_reason,
                ),
                AccessDenialReason::HostEncoderFailed => Self::with_reason(
                    false,
                    "主机已批准连接，但 Windows 视频编码器启动失败。请更新 Windows、媒体组件和显示驱动后重试。",
                    "host_encoder_failed",
                    technical_reason,
                ),
                AccessDenialReason::HostInputFailed => Self::with_reason(
                    false,
                    "主机已建立画面连接，但 Windows 无法注入鼠标或键盘输入。请在主机上重新启动 DeskLink 后重试。",
                    "host_input_failed",
                    technical_reason,
                ),
            };
        }
        let retryable = matches!(
            error,
            ControllerError::HandshakeTimeout
                | ControllerError::NegotiationTimeout
                | ControllerError::Transport(
                    TransportError::Connection(_)
                        | TransportError::ConnectionLimit
                        | TransportError::Stream(_)
                        | TransportError::Datagram(_)
                        | TransportError::Closed
                        | TransportError::PeerDisconnected
                        | TransportError::PeerReplaced
                )
        );
        if retryable {
            Self::retryable(technical_reason)
        } else {
            Self::permanent(technical_reason)
        }
    }
}

async fn schedule_failure(
    manager: &ControllerManager,
    signals: &Channel<ControllerSignal>,
    schedule: &mut ReconnectSchedule,
    failure: ConnectFailure,
    wait_channels: ControllerWaitChannels<'_>,
    diagnostics: Option<&DiagnosticLog>,
    attempt: u32,
) -> bool {
    if !failure.retryable {
        record_controller_diagnostic(
            diagnostics,
            ControllerDiagnosticStage::Stopped,
            attempt,
            None,
            None,
            Some(&format!("{}: {}", failure.kind, failure.technical_reason)),
        );
        manager.publish(signals, ControllerRuntimeSummary::stopped(failure.detail));
        return false;
    }
    match schedule.next(now_unix_s()) {
        ReconnectDecision::RetryAfter { retry, delay } => {
            record_controller_diagnostic(
                diagnostics,
                ControllerDiagnosticStage::RetryScheduled,
                attempt,
                Some(retry),
                Some(delay),
                Some(&format!("{}: {}", failure.kind, failure.technical_reason)),
            );
            manager.publish(
                signals,
                ControllerRuntimeSummary::reconnecting(retry, schedule.max_retries(), delay),
            );
            match wait_for_retry_deadline(wait_channels.commands, wait_channels.cancellation, delay)
                .await
            {
                RetryWaitOutcome::Retry => true,
                RetryWaitOutcome::Cancelled => {
                    finish_cancelled(manager, signals, diagnostics, attempt, Some(retry));
                    false
                }
            }
        }
        ReconnectDecision::Exhausted | ReconnectDecision::SessionExpired => {
            record_controller_diagnostic(
                diagnostics,
                ControllerDiagnosticStage::Stopped,
                attempt,
                None,
                None,
                Some(&format!("{}: {}", failure.kind, failure.technical_reason)),
            );
            manager.publish(
                signals,
                ControllerRuntimeSummary::stopped(
                    "多次尝试后仍无法连接主机，请检查两台电脑后重试。",
                ),
            );
            false
        }
    }
}

struct ControllerWaitChannels<'a> {
    commands: &'a mut mpsc::Receiver<ControllerCommand>,
    cancellation: &'a mut watch::Receiver<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetryWaitOutcome {
    Retry,
    Cancelled,
}

async fn wait_for_retry_deadline(
    commands: &mut mpsc::Receiver<ControllerCommand>,
    cancellation: &mut watch::Receiver<bool>,
    delay: Duration,
) -> RetryWaitOutcome {
    let sleep = tokio::time::sleep(delay);
    tokio::pin!(sleep);
    loop {
        tokio::select! {
            biased;
            changed = cancellation.changed() => {
                if cancellation_requested(changed, cancellation) {
                    return RetryWaitOutcome::Cancelled;
                }
            }
            () = &mut sleep => return RetryWaitOutcome::Retry,
            command = commands.recv() => {
                if command.is_none() {
                    return RetryWaitOutcome::Cancelled;
                }
            }
        }
    }
}

fn cancellation_requested(
    changed: Result<(), watch::error::RecvError>,
    cancellation: &watch::Receiver<bool>,
) -> bool {
    changed.is_err() || *cancellation.borrow()
}

fn finish_cancelled(
    manager: &ControllerManager,
    signals: &Channel<ControllerSignal>,
    diagnostics: Option<&DiagnosticLog>,
    attempt: u32,
    retry: Option<u32>,
) {
    record_controller_diagnostic(
        diagnostics,
        ControllerDiagnosticStage::Cancelled,
        attempt,
        retry,
        None,
        None,
    );
    manager.publish(signals, ControllerRuntimeSummary::idle());
}

fn record_controller_diagnostic(
    diagnostics: Option<&DiagnosticLog>,
    stage: ControllerDiagnosticStage,
    attempt: u32,
    retry: Option<u32>,
    delay: Option<Duration>,
    reason: Option<&str>,
) {
    let Some(diagnostics) = diagnostics else {
        return;
    };
    let _ = diagnostics.record(&DiagnosticEvent::ControllerConnection {
        stage,
        attempt,
        retry,
        delay,
        reason: reason.map(str::to_owned),
    });
}

async fn send_text_input(runtime: &ControllerRuntime, text: &str) -> Result<(), ControllerError> {
    for character in text.chars() {
        let code = match character {
            '\n' => KeyCode::Enter,
            '\t' => KeyCode::Tab,
            character => KeyCode::Character(character),
        };
        runtime
            .send_input(InputEvent::Key {
                code,
                pressed: true,
                modifiers: Modifiers(0),
            })
            .await?;
        runtime
            .send_input(InputEvent::Key {
                code,
                pressed: false,
                modifiers: Modifiers(0),
            })
            .await?;
    }
    Ok(())
}

fn validate_text_input(text: &str) -> Result<(), String> {
    let character_count = text.chars().count();
    if character_count == 0 {
        return Err("请输入要发送到远程电脑的文字。".to_owned());
    }
    if character_count > MAX_TEXT_INPUT_CHARACTERS || text.len() > MAX_TEXT_INPUT_BYTES {
        return Err("一次最多发送 256 个字符。".to_owned());
    }
    if text
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        return Err("文字包含不支持的控制字符。".to_owned());
    }
    Ok(())
}

fn parse_input(input: ControllerInput) -> Result<InputEvent, String> {
    match input.kind.as_str() {
        "mouseMove" => {
            let (x, y) = required_point(input.x, input.y)?;
            Ok(InputEvent::MouseMove { x, y })
        }
        "mouseButton" => Ok(InputEvent::MouseButton {
            button: match input.button.as_deref() {
                Some("left") => MouseButton::Left,
                Some("right") => MouseButton::Right,
                Some("middle") => MouseButton::Middle,
                _ => return Err("不支持此鼠标按键。".to_owned()),
            },
            pressed: input
                .pressed
                .ok_or_else(|| "缺少鼠标按键状态。".to_owned())?,
        }),
        "wheel" => {
            let delta_x = input.delta_x.unwrap_or(0);
            let delta_y = input.delta_y.unwrap_or(0);
            if (delta_x == 0 && delta_y == 0)
                || !(-MAX_WHEEL_DELTA..=MAX_WHEEL_DELTA).contains(&delta_x)
                || !(-MAX_WHEEL_DELTA..=MAX_WHEEL_DELTA).contains(&delta_y)
            {
                return Err("滚轮移动超出支持范围。".to_owned());
            }
            Ok(InputEvent::MouseWheel { delta_x, delta_y })
        }
        "key" => {
            let modifiers = Modifiers(input.modifiers.unwrap_or(0));
            if !modifiers.is_valid() {
                return Err("键盘修饰键状态无效。".to_owned());
            }
            let code = match input.key.as_deref() {
                Some("character") => {
                    let value = input
                        .character
                        .as_deref()
                        .and_then(|value| {
                            let mut chars = value.chars();
                            let character = chars.next()?;
                            chars.next().is_none().then_some(character)
                        })
                        .ok_or_else(|| "键盘字符无效。".to_owned())?;
                    KeyCode::Character(value)
                }
                Some("enter") => KeyCode::Enter,
                Some("escape") => KeyCode::Escape,
                Some("backspace") => KeyCode::Backspace,
                Some("tab") => KeyCode::Tab,
                Some("arrowUp") => KeyCode::ArrowUp,
                Some("arrowDown") => KeyCode::ArrowDown,
                Some("arrowLeft") => KeyCode::ArrowLeft,
                Some("arrowRight") => KeyCode::ArrowRight,
                Some("delete") => KeyCode::Delete,
                Some("insert") => KeyCode::Insert,
                Some("home") => KeyCode::Home,
                Some("end") => KeyCode::End,
                Some("pageUp") => KeyCode::PageUp,
                Some("pageDown") => KeyCode::PageDown,
                Some("capsLock") => KeyCode::CapsLock,
                Some("control") => KeyCode::Control,
                Some("alt") => KeyCode::Alt,
                Some("shift") => KeyCode::Shift,
                Some("meta") => KeyCode::Meta,
                Some(value) if value.starts_with('f') => {
                    let number = value[1..]
                        .parse::<u8>()
                        .ok()
                        .filter(|number| (1..=12).contains(number))
                        .ok_or_else(|| "不支持此功能键。".to_owned())?;
                    KeyCode::Function(number)
                }
                _ => return Err("不支持此键盘按键。".to_owned()),
            };
            if code
                .modifier_mask()
                .is_some_and(|own_modifier| modifiers.contains(own_modifier))
            {
                return Err("修饰键不能重复包含自身状态。".to_owned());
            }
            Ok(InputEvent::Key {
                code,
                pressed: input
                    .pressed
                    .ok_or_else(|| "缺少键盘按键状态。".to_owned())?,
                modifiers,
            })
        }
        _ => Err("不支持此控制输入类型。".to_owned()),
    }
}

fn required_point(x: Option<i32>, y: Option<i32>) -> Result<(i32, i32), String> {
    let x = x.ok_or_else(|| "缺少指针横坐标。".to_owned())?;
    let y = y.ok_or_else(|| "缺少指针纵坐标。".to_owned())?;
    if !(0..=MAX_POINTER_COORDINATE).contains(&x) || !(0..=MAX_POINTER_COORDINATE).contains(&y) {
        return Err("指针位置超出远程桌面范围。".to_owned());
    }
    Ok((x, y))
}

fn publish_file_queue(
    signals: &Channel<ControllerSignal>,
    queued_files: &VecDeque<OutgoingFile>,
    paused: bool,
) {
    let _ = signals.send(ControllerSignal::FileQueue {
        queued: queued_files
            .iter()
            .map(|file| QueuedFileSummary {
                id: hex(&file.transfer_id),
                name: file.name.clone(),
                size: file.size,
            })
            .collect(),
        paused,
    });
}

fn parse_transfer_id(value: &str) -> Result<TransferId, String> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("文件队列项目无效，请刷新传输面板。".to_owned());
    }
    let mut transfer_id = [0_u8; 16];
    for (index, byte) in transfer_id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "文件队列项目无效，请刷新传输面板。".to_owned())?;
    }
    if transfer_id.iter().all(|byte| *byte == 0) {
        return Err("文件队列项目无效，请刷新传输面板。".to_owned());
    }
    Ok(transfer_id)
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        COMMAND_CAPACITY, ConnectFailure, ControllerAudioDecoder, ControllerCommand,
        ControllerInput, ControllerManager, ControllerRuntimeSummary, MAX_BUFFERED_POINTER_MOVES,
        RetryWaitOutcome, directory_transport_error_is_retryable,
        directory_transport_error_message, parse_input, parse_transfer_id,
        session_earned_fresh_retry_budget, should_drop_buffered_pointer_move, validate_text_input,
        wait_for_retry_deadline,
    };
    use apps_windows::audio::RemoteAudioEncoder;
    use desklink_ffi::ControllerError;
    use desklink_protocol::{
        AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, AccessDenialReason, AudioCodec, AudioPacket, InputEvent,
        KeyCode, MAX_AUDIO_PAYLOAD_BYTES, Modifiers, MouseButton, PROTOCOL_VERSION,
    };
    use desklink_transport::{JoinRejectCode, TransportError};

    #[test]
    fn queued_file_ids_require_exact_nonzero_hex() {
        assert_eq!(
            parse_transfer_id("0102030405060708090a0b0c0d0e0f10").unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert!(parse_transfer_id("00").is_err());
        assert!(parse_transfer_id("00000000000000000000000000000000").is_err());
        assert!(parse_transfer_id("zz02030405060708090a0b0c0d0e0f10").is_err());
    }
    use tokio::sync::{mpsc, watch};

    fn empty_input(kind: &str) -> ControllerInput {
        ControllerInput {
            stream_id: 1,
            kind: kind.to_owned(),
            x: None,
            y: None,
            delta_x: None,
            delta_y: None,
            button: None,
            key: None,
            character: None,
            pressed: None,
            modifiers: None,
        }
    }

    #[test]
    fn pointer_backlog_is_bounded_without_dropping_discrete_input() {
        let pointer = ControllerCommand::Input {
            stream_id: 1,
            event: InputEvent::MouseMove { x: 1, y: 1 },
        };
        let release = ControllerCommand::Input {
            stream_id: 1,
            event: InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            },
        };

        assert!(!should_drop_buffered_pointer_move(
            &pointer,
            COMMAND_CAPACITY - MAX_BUFFERED_POINTER_MOVES + 1
        ));
        assert!(should_drop_buffered_pointer_move(
            &pointer,
            COMMAND_CAPACITY - MAX_BUFFERED_POINTER_MOVES
        ));
        assert!(!should_drop_buffered_pointer_move(&release, 0));
    }

    #[tokio::test]
    async fn stale_or_missing_stream_ids_never_enter_the_active_command_queue() {
        let manager = ControllerManager::default();
        manager.set_status(ControllerRuntimeSummary::connected(42));

        let mut stale = empty_input("mouseMove");
        stale.stream_id = 41;
        stale.x = Some(1);
        stale.y = Some(1);
        assert!(manager.send_input(stale).await.is_ok());

        let mut missing = empty_input("mouseMove");
        missing.stream_id = 0;
        missing.x = Some(1);
        missing.y = Some(1);
        assert!(manager.send_input(missing).await.is_err());
    }

    #[test]
    fn browser_input_maps_to_bounded_protocol_events() {
        let mut pointer = empty_input("mouseMove");
        pointer.x = Some(250_000);
        pointer.y = Some(750_000);
        assert_eq!(
            parse_input(pointer).unwrap(),
            InputEvent::MouseMove {
                x: 250_000,
                y: 750_000,
            }
        );

        let mut key = empty_input("key");
        key.key = Some("character".to_owned());
        key.character = Some("a".to_owned());
        key.pressed = Some(true);
        key.modifiers = Some(Modifiers::CONTROL.0);
        assert_eq!(
            parse_input(key).unwrap(),
            InputEvent::Key {
                code: KeyCode::Character('a'),
                pressed: true,
                modifiers: Modifiers::CONTROL,
            }
        );
    }

    #[test]
    fn browser_input_supports_desktop_navigation_and_function_keys() {
        let mut delete = empty_input("key");
        delete.key = Some("delete".to_owned());
        delete.pressed = Some(true);
        delete.modifiers = Some(0);
        assert_eq!(
            parse_input(delete).unwrap(),
            InputEvent::Key {
                code: KeyCode::Delete,
                pressed: true,
                modifiers: Modifiers(0),
            }
        );

        let mut function = empty_input("key");
        function.key = Some("f12".to_owned());
        function.pressed = Some(true);
        function.modifiers = Some(Modifiers::SHIFT.0);
        assert_eq!(
            parse_input(function).unwrap(),
            InputEvent::Key {
                code: KeyCode::Function(12),
                pressed: true,
                modifiers: Modifiers::SHIFT,
            }
        );

        let mut windows_key = empty_input("key");
        windows_key.key = Some("meta".to_owned());
        windows_key.pressed = Some(true);
        windows_key.modifiers = Some(0);
        assert_eq!(
            parse_input(windows_key).unwrap(),
            InputEvent::Key {
                code: KeyCode::Meta,
                pressed: true,
                modifiers: Modifiers(0),
            }
        );

        let mut duplicate_modifier = empty_input("key");
        duplicate_modifier.key = Some("control".to_owned());
        duplicate_modifier.pressed = Some(true);
        duplicate_modifier.modifiers = Some(Modifiers::CONTROL.0);
        assert!(parse_input(duplicate_modifier).is_err());

        let mut unsupported = empty_input("key");
        unsupported.key = Some("f13".to_owned());
        unsupported.pressed = Some(true);
        unsupported.modifiers = Some(0);
        assert!(parse_input(unsupported).is_err());
    }

    #[test]
    fn remote_text_input_accepts_unicode_and_rejects_oversized_or_control_text() {
        assert!(validate_text_input("中文输入 ✓").is_ok());
        assert!(validate_text_input("\n\t").is_ok());
        assert!(validate_text_input("").is_err());
        assert!(validate_text_input("\0").is_err());
        assert!(validate_text_input(&"字".repeat(257)).is_err());
    }

    #[test]
    fn relay_connection_failure_explains_temporary_unavailability() {
        let failure =
            ConnectFailure::from_transport(TransportError::Connection("timed out".to_owned()));

        assert!(failure.retryable);
        assert!(failure.detail.contains("中继服务器或主机"));
        assert_eq!(failure.kind, "transport_unavailable");
        assert!(failure.technical_reason.contains("timed out"));
    }

    #[test]
    fn host_capture_failure_stops_the_retry_loop_with_actionable_copy() {
        let failure = ConnectFailure::from_controller(ControllerError::AccessDenied(
            AccessDenialReason::HostUnavailable,
        ));

        assert!(!failure.retryable);
        assert!(failure.detail.contains("屏幕采集"));
        assert!(failure.detail.contains("解锁"));
    }

    #[test]
    fn post_approval_backend_failures_stop_blind_retries_with_exact_copy() {
        let capture = ConnectFailure::from_controller(ControllerError::AccessDenied(
            AccessDenialReason::HostCaptureFailed,
        ));
        let encoder = ConnectFailure::from_controller(ControllerError::AccessDenied(
            AccessDenialReason::HostEncoderFailed,
        ));

        assert!(!capture.retryable);
        assert_eq!(capture.kind, "host_capture_failed");
        assert!(capture.detail.contains("首帧"));
        assert!(!encoder.retryable);
        assert_eq!(encoder.kind, "host_encoder_failed");
        assert!(encoder.detail.contains("视频编码器"));
    }

    #[test]
    fn directory_query_retries_interruptions_but_reports_protocol_incompatibility() {
        let interrupted = TransportError::Connection("stream finished early".to_owned());
        assert!(directory_transport_error_is_retryable(&interrupted));
        assert!(directory_transport_error_message(&interrupted).contains("连续中断"));

        let incompatible = TransportError::Malformed;
        assert!(!directory_transport_error_is_retryable(&incompatible));
        assert!(directory_transport_error_message(&incompatible).contains("不兼容"));
    }

    #[test]
    fn expired_and_mismatched_pairing_sessions_have_distinct_recovery_text() {
        let expired = ConnectFailure::from_transport(TransportError::JoinRejected(
            JoinRejectCode::SessionNotFound,
        ));
        let mismatch = ConnectFailure::from_transport(TransportError::JoinRejected(
            JoinRejectCode::AuthenticationMismatch,
        ));

        assert!(expired.retryable);
        assert!(expired.detail.contains("失效"));
        assert!(!mismatch.retryable);
        assert!(mismatch.detail.contains("不匹配"));
    }

    #[test]
    fn cancelling_invalidates_every_in_flight_connection_generation() {
        let manager = ControllerManager::default();
        let first = manager.begin_operation();
        assert!(manager.ensure_current(first).is_ok());

        manager.cancel_operations();
        assert_eq!(
            manager.ensure_current(first),
            Err("连接已取消。".to_owned())
        );

        let retry = manager.begin_operation();
        assert!(retry > first);
        assert!(manager.ensure_current(retry).is_ok());
    }

    #[tokio::test]
    async fn cancellation_is_independent_from_a_saturated_input_queue() {
        let (commands, mut receiver) = mpsc::channel(1);
        commands
            .send(ControllerCommand::RequestKeyframe)
            .await
            .unwrap();
        let (cancellation, mut cancellation_receiver) = watch::channel(false);
        cancellation.send(true).unwrap();

        let outcome = tokio::time::timeout(
            Duration::from_millis(100),
            wait_for_retry_deadline(
                &mut receiver,
                &mut cancellation_receiver,
                Duration::from_secs(10),
            ),
        )
        .await
        .unwrap();
        assert_eq!(outcome, RetryWaitOutcome::Cancelled);
    }

    #[tokio::test]
    async fn retry_deadline_is_not_starved_by_continuous_input() {
        let (commands, mut receiver) = mpsc::channel(8);
        let producer = tokio::spawn(async move {
            while commands
                .send(ControllerCommand::RequestKeyframe)
                .await
                .is_ok()
            {}
        });
        let (_cancellation, mut cancellation_receiver) = watch::channel(false);

        let outcome = tokio::time::timeout(
            Duration::from_millis(200),
            wait_for_retry_deadline(
                &mut receiver,
                &mut cancellation_receiver,
                Duration::from_millis(10),
            ),
        )
        .await
        .unwrap();
        producer.abort();
        assert_eq!(outcome, RetryWaitOutcome::Retry);
    }

    #[test]
    fn only_an_active_recent_cancellation_enables_one_recovery_lookup() {
        let manager = ControllerManager::default();
        manager.set_status(ControllerRuntimeSummary::finding());
        manager.remember_active_cancellation();

        assert!(manager.take_recent_cancellation());
        assert!(!manager.take_recent_cancellation());

        manager.set_status(ControllerRuntimeSummary::idle());
        manager.remember_active_cancellation();
        assert!(!manager.take_recent_cancellation());
    }

    #[test]
    fn brief_connections_do_not_restore_an_exhausted_retry_budget() {
        assert!(!session_earned_fresh_retry_budget(Duration::from_secs(1)));
        assert!(!session_earned_fresh_retry_budget(Duration::from_secs(29)));
        assert!(session_earned_fresh_retry_budget(Duration::from_secs(30)));
    }

    #[test]
    fn controller_audio_recovers_one_gap_and_drops_duplicate_datagrams() {
        let mut encoder = RemoteAudioEncoder::new().expect("create encoder");
        let mut decoder = ControllerAudioDecoder::new();
        let pcm = vec![0_u8; MAX_AUDIO_PAYLOAD_BYTES];
        let packets = (1_u64..=3)
            .map(|sequence| AudioPacket {
                protocol_version: PROTOCOL_VERSION,
                stream_id: 17,
                sequence,
                capture_timestamp_us: 1_000_000 + sequence * 10_000,
                codec: AudioCodec::Opus,
                sample_rate: AUDIO_SAMPLE_RATE,
                channels: AUDIO_CHANNELS,
                payload: encoder.encode_pcm_s16_le(&pcm).expect("encode frame"),
            })
            .collect::<Vec<_>>();

        let first = decoder.decode(packets[0].clone());
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].sequence, 1);

        // Packet 2 is intentionally dropped. Packet 3 carries enough Opus
        // redundancy to reconstruct at most that one missing 10 ms frame.
        let recovered = decoder.decode(packets[2].clone());
        assert_eq!(
            recovered
                .iter()
                .map(|packet| packet.sequence)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(
            recovered
                .iter()
                .all(|packet| packet.payload.len() == MAX_AUDIO_PAYLOAD_BYTES)
        );

        assert!(decoder.decode(packets[2].clone()).is_empty());
    }
}
