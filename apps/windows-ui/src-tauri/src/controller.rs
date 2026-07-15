use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use apps_windows::{
    controller_settings::{ControllerConnectionSettings, WindowsControllerConnectionStore},
    identity::WindowsIdentityStore,
};
use desklink_crypto::{PAIRING_INVITE_BYTES, PairingInvite};
use desklink_ffi::{ControllerError, ControllerEvent, ControllerRuntime};
use desklink_protocol::{
    FrameFlags, InputEvent, KeyCode, MAX_POINTER_COORDINATE, MAX_WHEEL_DELTA, Modifiers,
    MouseButton, Platform,
};
use desklink_session::{ReconnectDecision, ReconnectPolicy, ReconnectSchedule};
use desklink_transport::{JoinRejectCode, QuicClient, RelayJoin, TransportError};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use tauri::ipc::{Channel, Response};
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use zeroize::Zeroize;

const COMMAND_CAPACITY: usize = 512;
const FRAME_PREFIX_BYTES: usize = 17;

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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerConnectionInput {
    relay_address: String,
    server_name: String,
    invitation: String,
}

impl Drop for ControllerConnectionInput {
    fn drop(&mut self) {
        self.invitation.zeroize();
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
}

enum ControllerCommand {
    Input(InputEvent),
    RequestKeyframe,
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
}

impl Default for ControllerManager {
    fn default() -> Self {
        Self {
            status: Arc::new(Mutex::new(ControllerRuntimeSummary::idle())),
            worker: Arc::new(Mutex::new(None)),
            operation_lock: Arc::new(AsyncMutex::new(())),
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

    pub async fn connect_invitation(
        &self,
        input: ControllerConnectionInput,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
    ) -> Result<ControllerSnapshot, String> {
        let settings = settings_from_invitation(input)?;
        self.start(settings, true, signals, video).await?;
        load_snapshot(self.snapshot())
    }

    pub async fn connect_saved(
        &self,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
    ) -> Result<ControllerSnapshot, String> {
        let store = WindowsControllerConnectionStore::for_current_user()
            .map_err(|_| "当前 Windows 账户无法使用已保存的控制端连接。".to_owned())?;
        let settings = store
            .load()
            .map_err(|_| "无法打开已保存的控制端连接。".to_owned())?
            .ok_or_else(|| "没有可供重新连接的已保存 Windows 电脑。".to_owned())?;
        self.start(settings, false, signals, video).await?;
        load_snapshot(self.snapshot())
    }

    async fn start(
        &self,
        settings: ControllerConnectionSettings,
        save_after_approval: bool,
        signals: Channel<ControllerSignal>,
        video: Channel<Response>,
    ) -> Result<(), String> {
        let _operation = self.operation_lock.lock().await;
        self.stop_current().await;
        self.publish(&signals, ControllerRuntimeSummary::connecting());
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

    pub fn send_input(&self, input: ControllerInput) -> Result<(), String> {
        self.send(ControllerCommand::Input(parse_input(input)?))
    }

    pub fn request_keyframe(&self) -> Result<(), String> {
        self.send(ControllerCommand::RequestKeyframe)
    }

    fn send(&self, command: ControllerCommand) -> Result<(), String> {
        let worker = self
            .worker
            .lock()
            .map_err(|_| "DeskLink 无法访问控制端任务。".to_owned())?;
        worker
            .as_ref()
            .ok_or_else(|| "当前没有正在运行的远程控制会话。".to_owned())?
            .commands
            .try_send(command)
            .map_err(|_| "控制输入队列暂时不可用。".to_owned())
    }

    pub async fn disconnect(&self) -> Result<ControllerSnapshot, String> {
        let _operation = self.operation_lock.lock().await;
        self.stop_current().await;
        self.set_status(ControllerRuntimeSummary::idle());
        load_snapshot(self.snapshot())
    }

    pub async fn forget_saved(&self) -> Result<ControllerSnapshot, String> {
        let _operation = self.operation_lock.lock().await;
        self.stop_current().await;
        WindowsControllerConnectionStore::for_current_user()
            .map_err(|_| "当前 Windows 账户无法使用已保存的控制端连接。".to_owned())?
            .clear()
            .map_err(|_| "DeskLink 无法移除已保存的控制端连接。".to_owned())?;
        self.set_status(ControllerRuntimeSummary::idle());
        load_snapshot(self.snapshot())
    }

    pub fn request_stop(&self) {
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
}

pub fn load_snapshot(runtime: ControllerRuntimeSummary) -> Result<ControllerSnapshot, String> {
    let store = WindowsControllerConnectionStore::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用已保存的控制端连接。".to_owned())?;
    let (saved_connection, connection_error) = match store.load() {
        Ok(settings) => (
            settings.map(|settings| SavedControllerConnectionSummary {
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
    Ok(ControllerSnapshot {
        runtime,
        saved_connection,
        connection_error,
    })
}

fn settings_from_invitation(
    input: ControllerConnectionInput,
) -> Result<ControllerConnectionSettings, String> {
    let encoded = input.invitation.trim();
    if encoded.len() != PAIRING_INVITE_BYTES * 2
        || !encoded.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("配对邀请必须正好包含 362 位十六进制字符。".to_owned());
    }
    let mut bytes = [0u8; PAIRING_INVITE_BYTES];
    for (index, pair) in encoded.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = (hex_nibble(pair[0]) << 4) | hex_nibble(pair[1]);
    }
    let invite = PairingInvite::decode(&bytes, now_unix_s())
        .map_err(|_| "配对邀请无效、已被修改或已过期。".to_owned())?;
    bytes.zeroize();
    ControllerConnectionSettings::from_invite(&input.relay_address, &input.server_name, &invite)
        .map_err(|error| error.to_string())
}

async fn run_controller(
    manager: ControllerManager,
    settings: ControllerConnectionSettings,
    mut save_after_approval: bool,
    mut commands: mpsc::Receiver<ControllerCommand>,
    signals: Channel<ControllerSignal>,
    video: Channel<Response>,
) {
    let mut schedule = ReconnectSchedule::new(ReconnectPolicy::default(), None);
    'connect: loop {
        let connection = connect_once(&manager, &settings, &signals);
        tokio::pin!(connection);
        let mut runtime = loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(ControllerCommand::Stop) | None => {
                        manager.set_status(ControllerRuntimeSummary::idle());
                        return;
                    }
                    Some(ControllerCommand::Input(_)) | Some(ControllerCommand::RequestKeyframe) => {}
                },
                result = &mut connection => match result {
                    Ok(runtime) => break runtime,
                    Err(failure) => {
                        if !schedule_failure(&manager, &signals, &mut schedule, failure, &mut commands).await {
                            return;
                        }
                        continue 'connect;
                    }
                }
            }
        };

        let mut stable = false;
        let mut last_metrics = Instant::now();
        let failure = loop {
            tokio::select! {
                biased;
                command = commands.recv() => match command {
                    Some(ControllerCommand::Input(input)) => {
                        if let Err(error) = runtime.send_input(input).await {
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
                    Some(ControllerCommand::Stop) | None => {
                        manager.set_status(ControllerRuntimeSummary::idle());
                        return;
                    }
                },
                event = runtime.next_event() => match event {
                    Ok(ControllerEvent::VideoConfig(config)) => {
                        stable = true;
                        schedule.reset();
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
                last_metrics = Instant::now();
            }
        };
        if stable {
            schedule.reset();
        }
        if !schedule_failure(&manager, &signals, &mut schedule, failure, &mut commands).await {
            return;
        }
    }
}

async fn connect_once(
    manager: &ControllerManager,
    settings: &ControllerConnectionSettings,
    signals: &Channel<ControllerSignal>,
) -> Result<ControllerRuntime, ConnectFailure> {
    manager.publish(signals, ControllerRuntimeSummary::connecting());
    let lan = crate::local_relay::is_lan_endpoint(settings.relay_address(), settings.server_name());
    let config =
        crate::local_relay::client_config(settings.relay_address(), settings.server_name())
            .map_err(|error| ConnectFailure::from_transport(error, lan))?;
    let client = QuicClient::connect(config)
        .await
        .map_err(|error| ConnectFailure::from_transport(error, lan))?;
    client
        .join(RelayJoin::controller(
            settings.session_id(),
            *settings.authentication(),
        ))
        .await
        .map_err(|error| ConnectFailure::from_transport(error, lan))?;
    manager.publish(signals, ControllerRuntimeSummary::waiting_for_approval());
    let identity = WindowsIdentityStore::for_current_user()
        .map_err(|_| ConnectFailure::permanent("控制端身份存储不可用"))?
        .load_or_create(&mut OsRng)
        .map_err(|_| ConnectFailure::permanent("无法打开控制端身份"))?;
    ControllerRuntime::connect_for_platform(
        client,
        identity,
        settings.host_verify_key(),
        Platform::Windows,
    )
    .await
    .map_err(ConnectFailure::from_controller)
}

struct ConnectFailure {
    retryable: bool,
    detail: &'static str,
}

impl ConnectFailure {
    fn permanent(_reason: impl Into<String>) -> Self {
        Self {
            retryable: false,
            detail: "已保存的身份或连接与主机不匹配，请重新配对此电脑。",
        }
    }

    fn retryable(_reason: impl Into<String>) -> Self {
        Self {
            retryable: true,
            detail: "中继服务器或主机暂时不可用。",
        }
    }

    fn from_transport(error: TransportError, lan: bool) -> Self {
        match error {
            TransportError::JoinRejected(JoinRejectCode::SessionNotFound) => Self {
                retryable: true,
                detail: "主机配对会话尚未就绪或连接码已经失效，请在主机上重新创建连接码。",
            },
            TransportError::JoinRejected(JoinRejectCode::SessionOccupied) => Self {
                retryable: true,
                detail: "此会话已有控制端连接，请先断开原控制端后再试。",
            },
            TransportError::JoinRejected(
                JoinRejectCode::ConnectionLimit | JoinRejectCode::SessionLimit,
            )
            | TransportError::ConnectionLimit => Self {
                retryable: true,
                detail: "中继服务器当前连接数量已满，请稍后重试。",
            },
            TransportError::JoinRejected(JoinRejectCode::AuthenticationMismatch) => Self {
                retryable: false,
                detail: "连接码与主机的中继会话不匹配，请从主机重新复制完整连接码。",
            },
            TransportError::InvalidConfig(_) => Self {
                retryable: false,
                detail: "中继地址或 TLS 服务器名称无效，请重新复制完整连接码。",
            },
            TransportError::Connection(_) if lan => Self {
                retryable: true,
                detail: "无法到达主机电脑。请确认两台电脑位于同一局域网、主机 DeskLink 正在运行，并允许 Windows 防火墙的专用网络访问。",
            },
            TransportError::Connection(_)
            | TransportError::Stream(_)
            | TransportError::Datagram(_)
            | TransportError::Closed
            | TransportError::JoinRejected(JoinRejectCode::Internal) => {
                Self::retryable("transport unavailable")
            }
            _ => Self::permanent("invalid relay response"),
        }
    }

    fn from_controller(error: ControllerError) -> Self {
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
                )
        );
        if retryable {
            Self::retryable(error.to_string())
        } else {
            Self::permanent(error.to_string())
        }
    }
}

async fn schedule_failure(
    manager: &ControllerManager,
    signals: &Channel<ControllerSignal>,
    schedule: &mut ReconnectSchedule,
    failure: ConnectFailure,
    commands: &mut mpsc::Receiver<ControllerCommand>,
) -> bool {
    if !failure.retryable {
        manager.publish(signals, ControllerRuntimeSummary::stopped(failure.detail));
        return false;
    }
    match schedule.next(now_unix_s()) {
        ReconnectDecision::RetryAfter { retry, delay } => {
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
                            manager.set_status(ControllerRuntimeSummary::idle());
                            return false;
                        }
                        Some(ControllerCommand::Input(_)) | Some(ControllerCommand::RequestKeyframe) => {}
                    },
                    () = &mut sleep => return true,
                }
            }
        }
        ReconnectDecision::Exhausted | ReconnectDecision::SessionExpired => {
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

fn hex_nibble(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => 0,
    }
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
    use super::{ConnectFailure, ControllerInput, parse_input};
    use desklink_protocol::{InputEvent, KeyCode, Modifiers};
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
    fn lan_connection_failure_explains_firewall_and_network_requirements() {
        let failure = ConnectFailure::from_transport(
            TransportError::Connection("timed out".to_owned()),
            true,
        );

        assert!(failure.retryable);
        assert!(failure.detail.contains("同一局域网"));
        assert!(failure.detail.contains("Windows 防火墙"));
    }

    #[test]
    fn expired_and_mismatched_pairing_sessions_have_distinct_recovery_text() {
        let expired = ConnectFailure::from_transport(
            TransportError::JoinRejected(JoinRejectCode::SessionNotFound),
            true,
        );
        let mismatch = ConnectFailure::from_transport(
            TransportError::JoinRejected(JoinRejectCode::AuthenticationMismatch),
            true,
        );

        assert!(expired.retryable);
        assert!(expired.detail.contains("失效"));
        assert!(!mismatch.retryable);
        assert!(mismatch.detail.contains("不匹配"));
    }
}
