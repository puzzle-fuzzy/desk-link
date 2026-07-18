use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use apps_windows::{
    controller_settings::{ControllerConnectionSettings, WindowsControllerConnectionStore},
    diagnostics::{ControllerDiagnosticStage, DiagnosticEvent, DiagnosticLog},
    identity::WindowsIdentityStore,
    recent_access::{RecentAccessEntry, WindowsRecentAccessError, WindowsRecentAccessStore},
};
use desklink_crypto::{PairingCode, PairingInvite};
use desklink_ffi::{ControllerError, ControllerEvent, ControllerRuntime};
use desklink_protocol::{
    AccessDenialReason, ControlMessage, FrameFlags, InputEvent, KeyCode, MAX_POINTER_COORDINATE,
    MAX_WHEEL_DELTA, Modifiers, MouseButton, Platform,
};
use desklink_session::{ReconnectDecision, ReconnectPolicy, ReconnectSchedule};
use desklink_transport::{
    JoinRejectCode, QuicClient, RelayDirectoryLookup, RelayJoin, TransportError,
};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use tauri::ipc::{Channel, Response};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use zeroize::Zeroize;

const COMMAND_CAPACITY: usize = 512;
const FRAME_PREFIX_BYTES: usize = 17;
const MAX_TEXT_INPUT_CHARACTERS: usize = 256;
const MAX_TEXT_INPUT_BYTES: usize = 1_024;
const RECENT_CANCELLATION_WINDOW: Duration = Duration::from_secs(15);
const RECONNECT_BUDGET_RESET_AFTER: Duration = Duration::from_secs(30);
const DIRECTORY_RECOVERY_DELAYS: [Duration; 2] =
    [Duration::from_millis(500), Duration::from_millis(1_250)];
const DIRECTORY_TRANSPORT_RETRY_DELAYS: [Duration; 2] =
    [Duration::from_millis(350), Duration::from_millis(900)];

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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerDisplaySummary {
    id: u32,
    width: u16,
    height: u16,
    primary: bool,
}

enum ControllerCommand {
    Input(InputEvent),
    Text(String),
    RequestKeyframe,
    SelectDisplay(u32),
    Stop,
}

struct ControllerWorker {
    commands: mpsc::Sender<ControllerCommand>,
    task: tauri::async_runtime::JoinHandle<()>,
}

#[derive(Clone)]
pub struct ControllerManager {
    status: Arc<Mutex<ControllerRuntimeSummary>>,
    worker: Arc<Mutex<Option<ControllerWorker>>>,
    operation_lock: Arc<AsyncMutex<()>>,
    operation_generation: Arc<AtomicU64>,
    recent_cancellation: Arc<Mutex<Option<Instant>>>,
}

impl Default for ControllerManager {
    fn default() -> Self {
        Self {
            status: Arc::new(Mutex::new(ControllerRuntimeSummary::idle())),
            worker: Arc::new(Mutex::new(None)),
            operation_lock: Arc::new(AsyncMutex::new(())),
            operation_generation: Arc::new(AtomicU64::new(0)),
            recent_cancellation: Arc::new(Mutex::new(None)),
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
        )
        .await
    }

    pub async fn connect_saved_device(
        &self,
        input: SavedDeviceInput,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
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
        self.connect_device_credentials(device_id, saved.password().clone(), source, signals, video)
            .await
    }

    async fn connect_device_credentials(
        &self,
        device_id: u64,
        password: PairingCode,
        source: DeviceCredentialSource,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
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
        self.start(generation, settings, true, signals, video)
            .await?;
        load_snapshot(self.snapshot())
    }

    pub async fn connect_saved(
        &self,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
    ) -> Result<ControllerSnapshot, String> {
        let generation = self.begin_operation();
        let store = WindowsControllerConnectionStore::for_current_user()
            .map_err(|_| "当前 Windows 账户无法使用已保存的控制端连接。".to_owned())?;
        let settings = store
            .load()
            .map_err(|_| "无法打开已保存的控制端连接。".to_owned())?
            .ok_or_else(|| "没有可供重新连接的已保存 Windows 电脑。".to_owned())?;
        self.start(generation, settings, false, signals, video)
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
    ) -> Result<(), String> {
        let _operation = self.operation_lock.lock().await;
        self.ensure_current(generation)?;
        self.stop_current().await;
        self.ensure_current(generation)?;
        self.publish_if_current(generation, &signals, ControllerRuntimeSummary::connecting());
        let (commands, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let manager = self.clone();
        let task = tauri::async_runtime::spawn(async move {
            run_controller(
                manager,
                settings,
                save_after_approval,
                receiver,
                signals,
                video,
            )
            .await;
        });
        let mut worker = self
            .worker
            .lock()
            .map_err(|_| "DeskLink 无法启动控制端任务。".to_owned())?;
        *worker = Some(ControllerWorker { commands, task });
        Ok(())
    }

    pub async fn send_input(&self, input: ControllerInput) -> Result<(), String> {
        self.send(ControllerCommand::Input(parse_input(input)?))
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

    async fn send(&self, command: ControllerCommand) -> Result<(), String> {
        let commands = self
            .worker
            .lock()
            .map_err(|_| "DeskLink 无法访问控制端任务。".to_owned())?
            .as_ref()
            .ok_or_else(|| "当前没有正在运行的远程控制会话。".to_owned())?
            .commands
            .clone();
        // Applying backpressure here is important: a full queue must never
        // silently discard a button/key release and leave the remote computer
        // with a logically stuck input state. The WebView dispatcher already
        // permits only one input IPC call in flight.
        commands
            .send(command)
            .await
            .map_err(|_| "远程控制会话已结束，无法继续发送输入。".to_owned())
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
            let _ = worker.commands.try_send(ControllerCommand::Stop);
        }
    }

    async fn stop_current(&self) {
        let worker = self.worker.lock().ok().and_then(|mut worker| worker.take());
        let Some(mut worker) = worker else {
            return;
        };
        let _ = worker.commands.try_send(ControllerCommand::Stop);
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
    signals: Channel<ControllerSignal>,
    video: Channel<Response>,
) {
    let diagnostics = DiagnosticLog::controller_for_current_user().ok();
    let mut schedule = ReconnectSchedule::new(ReconnectPolicy::default(), None);
    let mut attempt = 0_u32;
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
                command = commands.recv() => match command {
                    Some(ControllerCommand::Stop) | None => {
                        record_controller_diagnostic(
                            diagnostics.as_ref(),
                            ControllerDiagnosticStage::Cancelled,
                            attempt,
                            None,
                            None,
                            None,
                        );
                        manager.set_status(ControllerRuntimeSummary::idle());
                        return;
                    }
                    Some(ControllerCommand::Input(_))
                    | Some(ControllerCommand::Text(_))
                    | Some(ControllerCommand::RequestKeyframe)
                    | Some(ControllerCommand::SelectDisplay(_)) => {}
                },
                result = &mut connection => match result {
                    Ok(runtime) => break runtime,
                    Err(failure) => {
                        if !schedule_failure(
                            &manager,
                            &signals,
                            &mut schedule,
                            failure,
                            &mut commands,
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

        let mut stable = false;
        let mut stable_since = None;
        let mut last_metrics = Instant::now();
        let mut last_diagnostic_metrics = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .unwrap_or_else(Instant::now);
        let failure = loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(ControllerCommand::Input(input)) => {
                        if let Err(error) = runtime.send_input(input).await {
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
                    Some(ControllerCommand::Stop) | None => {
                        record_controller_diagnostic(
                            diagnostics.as_ref(),
                            ControllerDiagnosticStage::Cancelled,
                            attempt,
                            None,
                            None,
                            None,
                        );
                        manager.set_status(ControllerRuntimeSummary::idle());
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
                    Ok(ControllerEvent::Control(_)) => {}
                    Ok(ControllerEvent::Closed { reason }) => {
                        break ConnectFailure::retryable(format!("transport closed: {reason}"));
                    }
                    Err(error) => break ConnectFailure::from_controller(error),
                }
            }
            if last_metrics.elapsed() >= Duration::from_secs(1) {
                let metrics = runtime.metrics();
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
                        });
                    }
                    last_diagnostic_metrics = Instant::now();
                }
                last_metrics = Instant::now();
            }
        };
        if stable_since.is_some_and(|started| session_earned_fresh_retry_budget(started.elapsed()))
        {
            schedule.reset();
        }
        if !schedule_failure(
            &manager,
            &signals,
            &mut schedule,
            failure,
            &mut commands,
            diagnostics.as_ref(),
            attempt,
        )
        .await
        {
            return;
        }
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
    commands: &mut mpsc::Receiver<ControllerCommand>,
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
            let sleep = tokio::time::sleep(delay);
            tokio::pin!(sleep);
            loop {
                tokio::select! {
                    command = commands.recv() => match command {
                        Some(ControllerCommand::Stop) | None => {
                            record_controller_diagnostic(
                                diagnostics,
                                ControllerDiagnosticStage::Cancelled,
                                attempt,
                                Some(retry),
                                None,
                                None,
                            );
                            manager.set_status(ControllerRuntimeSummary::idle());
                            return false;
                        }
                        Some(ControllerCommand::Input(_))
                        | Some(ControllerCommand::Text(_))
                        | Some(ControllerCommand::RequestKeyframe)
                        | Some(ControllerCommand::SelectDisplay(_)) => {}
                    },
                    () = &mut sleep => return true,
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
            Ok(InputEvent::Key {
                code: match input.key.as_deref() {
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
                    Some(value) if value.starts_with('f') => {
                        let number = value[1..]
                            .parse::<u8>()
                            .ok()
                            .filter(|number| (1..=12).contains(number))
                            .ok_or_else(|| "不支持此功能键。".to_owned())?;
                        KeyCode::Function(number)
                    }
                    _ => return Err("不支持此键盘按键。".to_owned()),
                },
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
        ConnectFailure, ControllerInput, ControllerManager, ControllerRuntimeSummary,
        directory_transport_error_is_retryable, directory_transport_error_message, parse_input,
        session_earned_fresh_retry_budget, validate_text_input,
    };
    use desklink_ffi::ControllerError;
    use desklink_protocol::{AccessDenialReason, InputEvent, KeyCode, Modifiers};
    use desklink_transport::{JoinRejectCode, TransportError};

    fn empty_input(kind: &str) -> ControllerInput {
        ControllerInput {
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
}
