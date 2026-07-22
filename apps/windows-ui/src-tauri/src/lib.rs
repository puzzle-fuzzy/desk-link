#![cfg(windows)]

mod controller;
mod device_directory;
mod file_picker;
mod host;
mod local_relay;
mod updates;
mod video_mailbox;

#[cfg(windows)]
mod power;

#[cfg(windows)]
mod instance_guard {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};

    use windows::{Win32::System::Threading::CreateMutexW, core::w};

    pub struct ApplicationInstanceGuard {
        _handle: OwnedHandle,
    }

    impl ApplicationInstanceGuard {
        pub fn create() -> windows::core::Result<Self> {
            let handle = unsafe { CreateMutexW(None, true, w!("Local\\DeskLinkControlSurface"))? };
            let handle = unsafe { OwnedHandle::from_raw_handle(handle.0) };
            Ok(Self { _handle: handle })
        }
    }
}

use std::{
    env,
    fmt::Write as _,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use apps_windows::{
    cloud_diagnostics::{DiagnosticUploadSummary, start_background_uploader, upload_all_once},
    configuration::{HostConnectionSettings, WindowsConnectionSettingsStore},
    diagnostic_sharing::WindowsDiagnosticSharing,
    diagnostics::{DiagnosticEvent, DiagnosticLog, DiagnosticOperation},
    fixed_access::WindowsFixedAccessStore,
    identity::WindowsIdentityStore,
    startup::WindowsStartupSettings,
    transfer,
    trusted::WindowsTrustedControllerStore,
    window::WindowsLocalApprovalDialog,
};
use controller::{
    ControllerDeviceInput, ControllerInput, ControllerManager, ControllerPlaybackPressure,
    ControllerRenderMetrics, ControllerSignal, ControllerSnapshot, ControllerVideoPullInput,
    SavedDeviceInput, SavedDeviceRenameInput,
};
use host::{HostApprovalSummary, HostManager, HostRuntimeSummary, PairingSessionSummary, tray_id};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Manager, RunEvent, State, WindowEvent,
    ipc::{Channel, Response},
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HostSnapshot {
    readiness: &'static str,
    title: String,
    detail: String,
    runtime: HostRuntimeSummary,
    connection: Option<ConnectionSummary>,
    connection_error: Option<String>,
    trusted_controllers: Vec<TrustedControllerSummary>,
    trusted_error: Option<String>,
    relay_status: local_relay::RelayStatusSummary,
    diagnostic_checks: Vec<DiagnosticCheckSummary>,
    pairing_active: bool,
    pending_approval: Option<HostApprovalSummary>,
    fixed_password_enabled: bool,
    fixed_password_error: Option<String>,
    device_id: Option<String>,
    refreshed_at_unix_s: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionSummary {
    relay_address: String,
    server_name: String,
    session_id: String,
    stream_id: u64,
    has_saved_key: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrustedControllerSummary {
    device_id: String,
    verify_key: String,
    fingerprint: String,
    approved_at_unix_s: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticCheckSummary {
    code: &'static str,
    status: &'static str,
    title: String,
    detail: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticExportResult {
    report_id: String,
    file_name: String,
    file_path: String,
    check_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RevocationResult {
    revoked: bool,
    snapshot: HostSnapshot,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FixedAccessSummary {
    device_id: String,
    password: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WindowsPreferencesSummary {
    launch_at_login: bool,
    diagnostics_sharing_enabled: bool,
    close_to_tray: bool,
    interface_language: &'static str,
    version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticUploadResult {
    uploaded_sources: u32,
    uploaded_events: u32,
}

impl From<DiagnosticUploadSummary> for DiagnosticUploadResult {
    fn from(value: DiagnosticUploadSummary) -> Self {
        Self {
            uploaded_sources: value.uploaded_sources,
            uploaded_events: value.uploaded_events,
        }
    }
}

impl Drop for FixedAccessSummary {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

enum RevocationOutcome {
    Revoked,
    Cancelled,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionSettingsInput {
    relay_address: String,
    server_name: String,
    session_id: String,
    relay_key: String,
    stream_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelayProbeInput {
    relay_address: String,
    server_name: String,
}

impl Drop for ConnectionSettingsInput {
    fn drop(&mut self) {
        self.relay_key.zeroize();
    }
}

#[tauri::command]
async fn get_host_snapshot(manager: State<'_, HostManager>) -> Result<HostSnapshot, String> {
    let runtime = manager.snapshot();
    let pairing_active = manager.is_pairing_active();
    let pending_approval = manager.pending_approval();
    tauri::async_runtime::spawn_blocking(move || {
        load_host_snapshot(runtime, pairing_active, pending_approval)
    })
    .await
    .map_err(|_| "DeskLink 无法读取本地状态，请重试。".to_owned())?
}

#[tauri::command]
async fn get_windows_preferences() -> Result<WindowsPreferencesSummary, String> {
    tauri::async_runtime::spawn_blocking(load_windows_preferences)
        .await
        .map_err(|_| "DeskLink 无法读取 Windows 偏好设置，请重试。".to_owned())?
}

#[tauri::command]
async fn set_launch_at_login(enabled: bool) -> Result<WindowsPreferencesSummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let settings = WindowsStartupSettings::for_current_executable()
            .map_err(|_| "DeskLink 无法定位当前安装程序。".to_owned())?;
        settings
            .set_enabled(enabled)
            .map_err(|_| "Windows 未能更新登录启动设置，请检查当前账户权限后重试。".to_owned())?;
        load_windows_preferences()
    })
    .await
    .map_err(|_| "DeskLink 无法更新 Windows 偏好设置，请重试。".to_owned())?
}

#[tauri::command]
async fn set_diagnostics_sharing(enabled: bool) -> Result<WindowsPreferencesSummary, String> {
    tauri::async_runtime::spawn_blocking(move || {
        WindowsDiagnosticSharing::for_current_user()
            .map_err(|_| "DeskLink 无法打开诊断共享设置。".to_owned())?
            .set_enabled(enabled)
            .map_err(|_| "Windows 未能保存诊断共享设置，请检查当前账户权限后重试。".to_owned())?;
        load_windows_preferences()
    })
    .await
    .map_err(|_| "DeskLink 无法更新诊断共享设置，请重试。".to_owned())?
}

#[tauri::command]
async fn upload_diagnostics_now() -> Result<DiagnosticUploadResult, String> {
    tauri::async_runtime::spawn_blocking(|| {
        upload_all_once().map(Into::into).map_err(|_| {
            "暂时无法发送脱敏诊断，请检查网络后重试。DeskLink 会在后台自动补传。".to_owned()
        })
    })
    .await
    .map_err(|_| "DeskLink 无法启动诊断发送任务，请重试。".to_owned())?
}

#[tauri::command]
async fn check_windows_release() -> Result<updates::WindowsReleaseSource, String> {
    updates::check()
        .await
        .map_err(|_| "暂时无法检查 DeskLink 正式版本，请检查网络后在设置中重试。".to_owned())
}

#[tauri::command]
async fn quit_desklink(
    app: AppHandle,
    host_manager: State<'_, HostManager>,
    controller_manager: State<'_, ControllerManager>,
) -> Result<(), String> {
    controller_manager.request_stop();
    host_manager.stop().await;
    app.exit(0);
    Ok(())
}

#[tauri::command]
async fn restart_host(
    app: AppHandle,
    manager: State<'_, HostManager>,
) -> Result<HostSnapshot, String> {
    manager.restart(app).await;
    let runtime = manager.snapshot();
    let pairing_active = manager.is_pairing_active();
    let pending_approval = manager.pending_approval();
    tauri::async_runtime::spawn_blocking(move || {
        load_host_snapshot(runtime, pairing_active, pending_approval)
    })
    .await
    .map_err(|_| "DeskLink 已重新启动主机，但无法刷新本地状态。".to_owned())?
}

#[tauri::command]
fn respond_host_approval(
    manager: State<'_, HostManager>,
    request_id: u64,
    allow: bool,
) -> Result<(), String> {
    manager.respond_approval(request_id, allow)
}

#[tauri::command]
async fn get_controller_snapshot(
    manager: State<'_, ControllerManager>,
) -> Result<ControllerSnapshot, String> {
    controller::load_snapshot(&manager, manager.snapshot())
}

#[tauri::command]
async fn export_diagnostic_report(
    host_manager: State<'_, HostManager>,
    controller_manager: State<'_, ControllerManager>,
) -> Result<DiagnosticExportResult, String> {
    let runtime = host_manager.snapshot();
    let pairing_active = host_manager.is_pairing_active();
    let pending_approval = host_manager.pending_approval();
    let controller_runtime = controller_manager.snapshot();
    tauri::async_runtime::spawn_blocking(move || {
        let snapshot = load_host_snapshot(runtime, pairing_active, pending_approval)?;
        export_snapshot_report(&snapshot, &controller_runtime)
    })
    .await
    .map_err(|_| "DeskLink 无法完成诊断报告导出，请重试。".to_owned())?
}

#[tauri::command]
async fn probe_relay(input: RelayProbeInput) -> Result<local_relay::RelayProbeResult, String> {
    let relay_address = input
        .relay_address
        .trim()
        .parse::<SocketAddr>()
        .map_err(|_| "中继地址无效，请使用 IP 地址和端口，例如 192.168.1.20:4433。".to_owned())?;
    let server_name = input.server_name.trim();
    if server_name.is_empty()
        || server_name.len() > 253
        || server_name.chars().any(char::is_control)
    {
        return Err("TLS 服务器名称无效，请检查中继设置。".to_owned());
    }
    let diagnostics = DiagnosticLog::for_current_user().ok();
    match local_relay::probe(relay_address, server_name).await {
        Ok(result) => {
            if let Some(diagnostics) = diagnostics.as_ref() {
                let _ = diagnostics.record(&DiagnosticEvent::OperationSucceeded(
                    DiagnosticOperation::RelayProbe,
                ));
            }
            Ok(result)
        }
        Err(message) => {
            record_operation_failure(
                diagnostics.as_ref(),
                DiagnosticOperation::RelayProbe,
                &message,
            );
            Err(message)
        }
    }
}

#[tauri::command]
async fn connect_device(
    manager: State<'_, ControllerManager>,
    input: ControllerDeviceInput,
    signals: Channel<ControllerSignal>,
    audio: Channel<Response>,
) -> Result<ControllerSnapshot, String> {
    manager.connect_device(input, signals, audio).await
}

#[tauri::command]
async fn connect_saved_device(
    manager: State<'_, ControllerManager>,
    input: SavedDeviceInput,
    signals: Channel<ControllerSignal>,
    audio: Channel<Response>,
) -> Result<ControllerSnapshot, String> {
    manager.connect_saved_device(input, signals, audio).await
}

#[tauri::command]
async fn reconnect_controller(
    manager: State<'_, ControllerManager>,
    signals: Channel<ControllerSignal>,
    audio: Channel<Response>,
) -> Result<ControllerSnapshot, String> {
    manager.connect_saved(signals, audio).await
}

#[tauri::command]
async fn next_controller_video_frame(
    manager: State<'_, ControllerManager>,
    input: ControllerVideoPullInput,
) -> Result<Response, String> {
    manager.next_video_frame(input).await.map(Response::new)
}

#[tauri::command]
async fn send_controller_input(
    manager: State<'_, ControllerManager>,
    input: ControllerInput,
) -> Result<(), String> {
    manager.send_input(input).await
}

#[tauri::command]
async fn send_controller_text(
    manager: State<'_, ControllerManager>,
    text: String,
) -> Result<(), String> {
    manager.send_text(text).await
}

fn local_clipboard_error(result: desklink_protocol::TransferResult) -> String {
    match result {
        desklink_protocol::TransferResult::TooLarge => {
            "剪贴板文本超过 48 KB，无法发送。".to_owned()
        }
        desklink_protocol::TransferResult::Unsupported => {
            "本机剪贴板当前没有可发送的纯文本。".to_owned()
        }
        _ => "Windows 剪贴板正被其他程序占用，请稍后重试。".to_owned(),
    }
}

async fn read_local_clipboard_text() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(apps_windows::transfer::read_clipboard_text)
        .await
        .map_err(|_| "DeskLink 无法读取本机剪贴板。".to_owned())?
        .map_err(local_clipboard_error)
}

#[tauri::command]
async fn paste_controller_clipboard_text(
    manager: State<'_, ControllerManager>,
) -> Result<(), String> {
    let text = read_local_clipboard_text().await?;
    manager.paste_clipboard(text).await
}

#[tauri::command]
async fn set_controller_audio_enabled(
    manager: State<'_, ControllerManager>,
    enabled: bool,
) -> Result<(), String> {
    manager.set_audio_enabled(enabled).await
}

#[tauri::command]
async fn set_controller_video_quality(
    manager: State<'_, ControllerManager>,
    preference: desklink_protocol::VideoQualityPreference,
) -> Result<(), String> {
    manager.set_video_quality(preference).await
}

#[tauri::command]
async fn set_controller_video_profile(
    manager: State<'_, ControllerManager>,
    profile: desklink_protocol::H264Profile,
) -> Result<(), String> {
    manager.set_video_profile(profile).await
}

#[tauri::command]
async fn send_controller_clipboard(manager: State<'_, ControllerManager>) -> Result<(), String> {
    let text = read_local_clipboard_text().await?;
    manager.send_clipboard(text).await
}

#[tauri::command]
async fn request_controller_clipboard(manager: State<'_, ControllerManager>) -> Result<(), String> {
    manager.request_clipboard().await
}

#[tauri::command]
async fn choose_and_send_controller_file(
    manager: State<'_, ControllerManager>,
) -> Result<(), String> {
    let paths = tauri::async_runtime::spawn_blocking(file_picker::pick_files)
        .await
        .map_err(|_| "DeskLink 无法打开 Windows 文件选择器。".to_owned())??;
    manager.send_files(paths).await
}

#[tauri::command]
async fn queue_controller_files(
    manager: State<'_, ControllerManager>,
    paths: Vec<String>,
) -> Result<(), String> {
    manager
        .send_files(paths.into_iter().map(PathBuf::from).collect())
        .await
}

#[tauri::command]
async fn remove_controller_queued_file(
    manager: State<'_, ControllerManager>,
    transfer_id: String,
) -> Result<(), String> {
    manager.remove_queued_file(&transfer_id).await
}

#[tauri::command]
async fn clear_controller_file_queue(manager: State<'_, ControllerManager>) -> Result<(), String> {
    manager.clear_file_queue().await
}

#[tauri::command]
async fn resume_controller_file_queue(manager: State<'_, ControllerManager>) -> Result<(), String> {
    manager.resume_file_queue().await
}

#[tauri::command]
async fn retry_controller_file_queue_protection(
    manager: State<'_, ControllerManager>,
) -> Result<(), String> {
    manager.retry_file_queue_protection().await
}

#[tauri::command]
async fn request_controller_remote_file(
    manager: State<'_, ControllerManager>,
) -> Result<(), String> {
    manager.request_remote_file().await
}

#[tauri::command]
async fn retry_controller_file(manager: State<'_, ControllerManager>) -> Result<(), String> {
    manager.retry_file().await
}

#[tauri::command]
async fn cancel_controller_file(manager: State<'_, ControllerManager>) -> Result<(), String> {
    manager.cancel_file().await
}

#[tauri::command]
fn discard_controller_file_recovery(
    manager: State<'_, ControllerManager>,
    revision: u64,
) -> Result<(), String> {
    manager.discard_file_recovery(revision)
}

#[tauri::command]
fn discard_controller_file_queue_recovery(
    manager: State<'_, ControllerManager>,
    revision: u64,
) -> Result<(), String> {
    manager.discard_file_queue_recovery(revision)
}

#[tauri::command]
async fn open_controller_downloads_folder() -> Result<(), String> {
    transfer::open_downloads_folder()
        .map_err(|_| "DeskLink 无法打开当前 Windows 账户的下载文件夹。".to_owned())
}

#[tauri::command]
async fn request_controller_keyframe(manager: State<'_, ControllerManager>) -> Result<(), String> {
    manager.request_keyframe().await
}

#[tauri::command]
fn report_controller_render_metrics(
    manager: State<'_, ControllerManager>,
    metrics: ControllerRenderMetrics,
) -> Result<(), String> {
    manager.record_render_metrics(metrics)
}

#[tauri::command]
fn report_controller_playback_pressure(
    manager: State<'_, ControllerManager>,
    pressure: ControllerPlaybackPressure,
) -> Result<(), String> {
    manager.record_playback_pressure(pressure)
}

#[tauri::command]
fn open_github_repository() -> Result<(), String> {
    use windows::{
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        core::w,
    };

    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            w!("https://github.com/puzzle-fuzzy/desk-link"),
            w!(""),
            w!(""),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err("Windows 无法打开 DeskLink 的 GitHub 页面。".to_owned())
    } else {
        Ok(())
    }
}

#[tauri::command]
fn open_windows_releases() -> Result<(), String> {
    use windows::{
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        core::w,
    };

    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            w!("https://github.com/puzzle-fuzzy/desk-link/releases/latest"),
            w!(""),
            w!(""),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err("Windows 无法打开 DeskLink 的正式下载页面。".to_owned())
    } else {
        Ok(())
    }
}

#[tauri::command]
async fn select_controller_display(
    manager: State<'_, ControllerManager>,
    display_id: u32,
) -> Result<(), String> {
    manager.select_display(display_id).await
}

#[tauri::command]
async fn disconnect_controller(
    manager: State<'_, ControllerManager>,
) -> Result<ControllerSnapshot, String> {
    manager.disconnect().await
}

#[tauri::command]
async fn forget_saved_device(
    manager: State<'_, ControllerManager>,
    input: SavedDeviceInput,
) -> Result<ControllerSnapshot, String> {
    manager.forget_saved_device(input).await
}

#[tauri::command]
async fn rename_saved_device(
    manager: State<'_, ControllerManager>,
    input: SavedDeviceRenameInput,
) -> Result<ControllerSnapshot, String> {
    manager.rename_saved_device(input).await
}

#[tauri::command]
async fn clear_saved_devices(
    manager: State<'_, ControllerManager>,
) -> Result<ControllerSnapshot, String> {
    manager.clear_saved_devices().await
}

#[tauri::command]
async fn save_connection_settings(
    app: AppHandle,
    manager: State<'_, HostManager>,
    input: ConnectionSettingsInput,
) -> Result<HostSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || save_connection(input))
        .await
        .map_err(|_| "DeskLink 无法完成连接保存，请重试。".to_owned())??;
    manager.restart(app).await;
    let runtime = manager.snapshot();
    let pairing_active = manager.is_pairing_active();
    let pending_approval = manager.pending_approval();
    tauri::async_runtime::spawn_blocking(move || {
        load_host_snapshot(runtime, pairing_active, pending_approval)
    })
    .await
    .map_err(|_| "DeskLink 无法刷新本地状态，请重试。".to_owned())?
}

#[tauri::command]
async fn setup_managed_connection(
    app: AppHandle,
    manager: State<'_, HostManager>,
) -> Result<HostSnapshot, String> {
    tauri::async_runtime::spawn_blocking(create_managed_connection)
        .await
        .map_err(|_| "DeskLink 无法创建受保护的本机连接，请重试。".to_owned())??;
    manager.restart(app).await;
    let runtime = manager.snapshot();
    let pending_approval = manager.pending_approval();
    tauri::async_runtime::spawn_blocking(move || {
        load_host_snapshot(runtime, false, pending_approval)
    })
    .await
    .map_err(|_| "DeskLink 已保存本机连接，但无法刷新状态，请重试。".to_owned())?
}

#[tauri::command]
async fn start_pairing_session(
    app: AppHandle,
    manager: State<'_, HostManager>,
) -> Result<PairingSessionSummary, String> {
    manager.start_pairing(app).await
}

#[tauri::command]
async fn cancel_pairing_session(
    app: AppHandle,
    manager: State<'_, HostManager>,
) -> Result<HostSnapshot, String> {
    manager.restart(app).await;
    let runtime = manager.snapshot();
    let pairing_active = manager.is_pairing_active();
    let pending_approval = manager.pending_approval();
    tauri::async_runtime::spawn_blocking(move || {
        load_host_snapshot(runtime, pairing_active, pending_approval)
    })
    .await
    .map_err(|_| "DeskLink 已恢复普通主机模式，但无法刷新本地状态。".to_owned())?
}

#[tauri::command]
async fn get_fixed_access_password() -> Result<FixedAccessSummary, String> {
    tauri::async_runtime::spawn_blocking(load_fixed_access_password)
        .await
        .map_err(|_| "DeskLink 无法读取固定密码，请重试。".to_owned())?
}

#[tauri::command]
async fn regenerate_fixed_access_password(
    app: AppHandle,
    manager: State<'_, HostManager>,
) -> Result<FixedAccessSummary, String> {
    let summary = tauri::async_runtime::spawn_blocking(create_fixed_access_password)
        .await
        .map_err(|_| "DeskLink 无法生成固定密码，请重试。".to_owned())??;
    manager.restart(app).await;
    Ok(summary)
}

#[tauri::command]
async fn disable_fixed_access_password(
    app: AppHandle,
    manager: State<'_, HostManager>,
) -> Result<HostSnapshot, String> {
    tauri::async_runtime::spawn_blocking(clear_fixed_access_password)
        .await
        .map_err(|_| "DeskLink 无法关闭固定密码，请重试。".to_owned())??;
    manager.restart(app).await;
    let runtime = manager.snapshot();
    let pending_approval = manager.pending_approval();
    tauri::async_runtime::spawn_blocking(move || {
        load_host_snapshot(runtime, false, pending_approval)
    })
    .await
    .map_err(|_| "固定密码已关闭，但 DeskLink 无法刷新本地状态。".to_owned())?
}

#[tauri::command]
async fn revoke_trusted_controller(
    app: AppHandle,
    manager: State<'_, HostManager>,
    fingerprint: String,
) -> Result<RevocationResult, String> {
    let fingerprint = normalize_fingerprint(&fingerprint)?;
    let outcome = tauri::async_runtime::spawn_blocking(move || revoke_controller(&fingerprint))
        .await
        .map_err(|_| "DeskLink 无法完成本地撤销确认，信任状态没有改变。".to_owned())??;
    let revoked = matches!(outcome, RevocationOutcome::Revoked);
    if revoked {
        manager.restart(app).await;
    }
    let runtime = manager.snapshot();
    let pairing_active = manager.is_pairing_active();
    let pending_approval = manager.pending_approval();
    let snapshot = tauri::async_runtime::spawn_blocking(move || {
        load_host_snapshot(runtime, pairing_active, pending_approval)
    })
    .await
    .map_err(|_| "DeskLink 已完成本地确认，但无法刷新设备状态。".to_owned())??;
    Ok(RevocationResult { revoked, snapshot })
}

fn load_host_snapshot(
    runtime: HostRuntimeSummary,
    pairing_active: bool,
    pending_approval: Option<HostApprovalSummary>,
) -> Result<HostSnapshot, String> {
    let device_id = WindowsIdentityStore::for_current_user()
        .ok()
        .and_then(|store| store.load_or_create(&mut OsRng).ok())
        .map(|identity| {
            device_directory::format_device_id(device_directory::public_device_id(
                identity.device_id,
            ))
        });
    let connection_store = WindowsConnectionSettingsStore::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用连接设置。".to_owned())?;
    let (connection, connection_error) = match connection_store.load() {
        Ok(connection) => (connection, None),
        Err(_) => (
            None,
            Some("无法打开已保存的连接设置，请重新填写。".to_owned()),
        ),
    };

    let trusted_store = WindowsTrustedControllerStore::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用可信设备存储。".to_owned())?;
    let (trusted_controllers, trusted_error) = match trusted_store.list() {
        Ok(mut controllers) => {
            controllers.sort_by_key(|controller| controller.approved_at_unix_s());
            (
                controllers
                    .into_iter()
                    .rev()
                    .map(|controller| TrustedControllerSummary {
                        device_id: hex(&controller.device_id()),
                        verify_key: hex(controller.verify_key().as_bytes()),
                        fingerprint: hex(controller.fingerprint().as_bytes()),
                        approved_at_unix_s: controller.approved_at_unix_s(),
                    })
                    .collect(),
                None,
            )
        }
        Err(_) => (
            Vec::new(),
            Some("无法打开可信设备，主机将保持拒绝连接。".to_owned()),
        ),
    };

    let (fixed_password_enabled, fixed_password_error) =
        match WindowsFixedAccessStore::for_current_user().and_then(|store| store.load()) {
            Ok(password) => (password.is_some(), None),
            Err(_) => (
                false,
                Some("无法打开受保护的固定密码，请重新设置。".to_owned()),
            ),
        };

    let relay_status = local_relay::status(connection.as_ref());
    let connection = connection.map(|settings| ConnectionSummary {
        relay_address: settings.relay_address_text(),
        server_name: settings.server_name().to_owned(),
        session_id: settings.session_id_text(),
        stream_id: settings.stream_id(),
        has_saved_key: true,
    });
    let (readiness, title, detail) = if connection_error.is_some()
        || trusted_error.is_some()
        || fixed_password_error.is_some()
    {
        (
            "attention",
            "检查本地保护状态".to_owned(),
            "DeskLink 发现受保护数据需要处理，完成后才能继续提供主机服务。".to_owned(),
        )
    } else if connection.is_some() {
        (
            if runtime.state == "stopped" {
                "attention"
            } else {
                "configured"
            },
            runtime.title.clone(),
            runtime.detail.clone(),
        )
    } else {
        (
            "setup",
            "完成连接设置".to_owned(),
            "启动主机前，请填写另一台 DeskLink 设备共享的中继服务器信息。".to_owned(),
        )
    };
    let diagnostic_checks = build_diagnostic_checks(
        &runtime,
        connection.as_ref(),
        connection_error.as_deref(),
        trusted_error.as_deref(),
        fixed_password_error.as_deref(),
    );

    Ok(HostSnapshot {
        readiness,
        title,
        detail,
        runtime,
        connection,
        connection_error,
        trusted_controllers,
        trusted_error,
        relay_status,
        diagnostic_checks,
        pairing_active,
        pending_approval,
        fixed_password_enabled,
        fixed_password_error,
        device_id,
        refreshed_at_unix_s: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    })
}

fn build_diagnostic_checks(
    runtime: &HostRuntimeSummary,
    connection: Option<&ConnectionSummary>,
    connection_error: Option<&str>,
    trusted_error: Option<&str>,
    fixed_password_error: Option<&str>,
) -> Vec<DiagnosticCheckSummary> {
    let configuration = if connection_error.is_some() {
        diagnostic_check(
            "DL-CFG-101",
            "failed",
            "连接设置保护",
            "无法打开已保存的连接设置，需要重新填写。",
        )
    } else if connection.is_some() {
        diagnostic_check(
            "DL-CFG-101",
            "passed",
            "连接设置保护",
            "连接设置已由当前 Windows 账户加密保护。",
        )
    } else {
        diagnostic_check(
            "DL-CFG-101",
            "warning",
            "连接设置保护",
            "尚未保存连接设置，主机不能接受控制端。",
        )
    };
    let trust = if trusted_error.is_some() {
        diagnostic_check(
            "DL-SEC-102",
            "failed",
            "可信设备存储",
            "可信设备存储不可用，主机将拒绝新的控制连接。",
        )
    } else {
        diagnostic_check(
            "DL-SEC-102",
            "passed",
            "可信设备存储",
            "可信设备列表可读取，本地批准边界可用。",
        )
    };
    let fixed_access = if fixed_password_error.is_some() {
        diagnostic_check(
            "DL-SEC-103",
            "failed",
            "固定密码保护",
            "受保护的固定密码无法读取；DeskLink 不会发布固定密码入口。",
        )
    } else {
        diagnostic_check(
            "DL-SEC-103",
            "passed",
            "固定密码保护",
            "固定密码状态可读取，启用后由当前 Windows 账户加密保护。",
        )
    };
    let relay_check = match (connection.is_some(), runtime.state) {
        (false, _) => diagnostic_check(
            "DL-NET-201",
            "warning",
            "中继连接状态",
            "尚未保存中继配置，暂时无法建立远程连接。",
        ),
        (true, "connected") => diagnostic_check(
            "DL-NET-201",
            "passed",
            "中继连接状态",
            "中继连接和端到端安全会话均已建立。",
        ),
        (true, "stopped") => diagnostic_check(
            "DL-NET-201",
            "failed",
            "中继连接状态",
            "主机连接已经停止，请根据主机运行状态处理后重试。",
        ),
        (true, "reconnecting") => diagnostic_check(
            "DL-NET-201",
            "warning",
            "中继恢复状态",
            "中继连接暂时中断，DeskLink 正在按退避策略重连。",
        ),
        (true, _) => diagnostic_check(
            "DL-NET-201",
            "warning",
            "中继连接状态",
            "中继配置已保存，主机正在建立或等待安全会话。",
        ),
    };
    let host_check = match runtime.state {
        "connected" => diagnostic_check("DL-HOST-301", "passed", "主机运行状态", &runtime.detail),
        "stopped" => diagnostic_check("DL-HOST-301", "failed", "主机运行状态", &runtime.detail),
        _ => diagnostic_check("DL-HOST-301", "warning", "主机运行状态", &runtime.detail),
    };
    vec![configuration, trust, fixed_access, relay_check, host_check]
}

fn fixed_access_summary(
    password: desklink_crypto::PairingCode,
) -> Result<FixedAccessSummary, String> {
    let identity = WindowsIdentityStore::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用主机身份。".to_owned())?
        .load_or_create(&mut OsRng)
        .map_err(|_| "无法打开当前账户受保护的主机身份。".to_owned())?;
    Ok(FixedAccessSummary {
        device_id: device_directory::format_device_id(device_directory::public_device_id(
            identity.device_id,
        )),
        password: password.to_string(),
    })
}

fn load_fixed_access_password() -> Result<FixedAccessSummary, String> {
    let password = WindowsFixedAccessStore::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用固定密码存储。".to_owned())?
        .load()
        .map_err(|_| "无法打开受保护的固定密码，请重新设置。".to_owned())?
        .ok_or_else(|| "尚未启用固定密码。".to_owned())?;
    fixed_access_summary(password)
}

fn create_fixed_access_password() -> Result<FixedAccessSummary, String> {
    let password = WindowsFixedAccessStore::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用固定密码存储。".to_owned())?
        .generate_and_save(&mut OsRng)
        .map_err(|_| "DeskLink 无法加密保存固定密码。".to_owned())?;
    fixed_access_summary(password)
}

fn clear_fixed_access_password() -> Result<(), String> {
    WindowsFixedAccessStore::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用固定密码存储。".to_owned())?
        .clear()
        .map_err(|_| "DeskLink 无法清除受保护的固定密码。".to_owned())?;
    Ok(())
}

fn diagnostic_check(
    code: &'static str,
    status: &'static str,
    title: &str,
    detail: &str,
) -> DiagnosticCheckSummary {
    DiagnosticCheckSummary {
        code,
        status,
        title: title.to_owned(),
        detail: detail.to_owned(),
    }
}

fn export_snapshot_report(
    snapshot: &HostSnapshot,
    controller_runtime: &controller::ControllerRuntimeSummary,
) -> Result<DiagnosticExportResult, String> {
    let diagnostics = DiagnosticLog::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用诊断报告存储。".to_owned())?;
    let generated_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let report_id = format!("DL-WIN-{generated_at_unix_ms}-{:05}", std::process::id());
    let recent_events = diagnostics
        .recent_sanitized_lines()
        .unwrap_or_else(|_| vec!["{\"event\":\"diagnostic_history_unavailable\"}".to_owned()]);
    let controller_events = DiagnosticLog::controller_for_current_user()
        .ok()
        .and_then(|log| log.recent_sanitized_lines().ok())
        .unwrap_or_default();
    let recent_events = merge_recent_diagnostic_lines(recent_events, controller_events);
    let report = build_diagnostic_report(
        snapshot,
        controller_runtime,
        &report_id,
        generated_at_unix_ms,
        &recent_events,
    );
    let path = match diagnostics.export_report(&report_id, &report) {
        Ok(path) => path,
        Err(error) => {
            record_operation_failure(
                Some(&diagnostics),
                DiagnosticOperation::DiagnosticExport,
                &error.to_string(),
            );
            return Err(
                "无法写入 Windows 下载文件夹，请检查磁盘空间和文件夹权限后重试。".to_owned(),
            );
        }
    };
    let _ = diagnostics.record(&DiagnosticEvent::OperationSucceeded(
        DiagnosticOperation::DiagnosticExport,
    ));
    Ok(DiagnosticExportResult {
        report_id,
        file_name: report_file_name(&path),
        file_path: path.to_string_lossy().into_owned(),
        check_count: snapshot.diagnostic_checks.len(),
    })
}

fn report_file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("DeskLink-Diagnostics.txt")
        .to_owned()
}

fn merge_recent_diagnostic_lines(
    host_events: Vec<String>,
    controller_events: Vec<String>,
) -> Vec<String> {
    let mut events = host_events
        .into_iter()
        .map(|line| (diagnostic_timestamp(&line), "主机", line))
        .chain(
            controller_events
                .into_iter()
                .map(|line| (diagnostic_timestamp(&line), "控制端", line)),
        )
        .collect::<Vec<_>>();
    events.sort_by_key(|(timestamp, source, _)| (*timestamp, *source));
    events
        .into_iter()
        .rev()
        .take(200)
        .rev()
        .map(|(_, source, line)| format!("[{source}] {line}"))
        .collect()
}

fn diagnostic_timestamp(line: &str) -> u128 {
    const MARKER: &str = "\"timestamp_unix_ms\":";
    line.split_once(MARKER)
        .and_then(|(_, remainder)| {
            remainder
                .split(|character: char| !character.is_ascii_digit())
                .next()
        })
        .and_then(|value| value.parse().ok())
        .unwrap_or(u128::MAX)
}

fn build_diagnostic_report(
    snapshot: &HostSnapshot,
    controller_runtime: &controller::ControllerRuntimeSummary,
    report_id: &str,
    generated_at_unix_ms: u128,
    recent_events: &[String],
) -> String {
    let mut report = String::new();
    let _ = writeln!(report, "DeskLink Windows 诊断报告");
    let _ = writeln!(report, "报告编号: {report_id}");
    let _ = writeln!(report, "生成时间 (Unix 毫秒): {generated_at_unix_ms}");
    let _ = writeln!(report, "DeskLink 版本: {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(
        report,
        "隐私说明: 会话 ID、中继密钥、公钥、设备完整身份和长十六进制内容不会写入此报告。"
    );
    let _ = writeln!(report, "\n[检查结论]");
    for check in &snapshot.diagnostic_checks {
        let _ = writeln!(
            report,
            "{} [{}] {}",
            check.code,
            diagnostic_status_label(check.status),
            check.title
        );
        let _ = writeln!(report, "  {}", check.detail);
    }
    let _ = writeln!(report, "\n[主机状态]");
    let _ = writeln!(report, "界面状态: {}", snapshot.readiness);
    let _ = writeln!(report, "运行状态: {}", snapshot.runtime.state);
    let _ = writeln!(report, "运行说明: {}", snapshot.runtime.detail);
    let _ = writeln!(
        report,
        "配对会话: {}",
        if snapshot.pairing_active {
            "正在运行"
        } else {
            "未运行"
        }
    );
    let _ = writeln!(
        report,
        "连接设置: {}",
        if snapshot.connection.is_some() {
            "已保存"
        } else {
            "未保存"
        }
    );
    let _ = writeln!(
        report,
        "可信控制端数量: {}",
        snapshot.trusted_controllers.len()
    );
    if let Some(connection) = snapshot.connection.as_ref() {
        let _ = writeln!(report, "中继地址: {}", connection.relay_address);
        let _ = writeln!(report, "TLS 服务器名称: {}", connection.server_name);
    }
    let _ = writeln!(report, "\n[网络状态]");
    let _ = writeln!(report, "中继模式: {}", snapshot.relay_status.mode);
    let _ = writeln!(report, "中继状态: {}", snapshot.relay_status.state);
    let _ = writeln!(report, "\n[控制端状态]");
    let _ = writeln!(report, "运行状态: {}", controller_runtime.state);
    let _ = writeln!(report, "运行说明: {}", controller_runtime.detail);
    let _ = writeln!(report, "\n[视频性能摘要]");
    for finding in build_video_performance_summary(recent_events) {
        let _ = writeln!(report, "{finding}");
    }
    let _ = writeln!(report, "\n[最近诊断事件，最多 200 条；已区分主机与控制端]");
    if recent_events.is_empty() {
        let _ = writeln!(report, "没有可用的历史事件。");
    } else {
        for event in recent_events {
            let _ = writeln!(report, "{event}");
        }
    }
    apps_windows::diagnostics::redact_sensitive_text(&report)
}

fn build_video_performance_summary(recent_events: &[String]) -> Vec<String> {
    let render = latest_metric_event(recent_events, "controller_render_metrics");
    let Some((render_timestamp, render)) = render else {
        return vec!["控制端尚未捕获足够的视频渲染指标。".to_owned()];
    };
    let transport = latest_metric_event(recent_events, "controller_video_metrics")
        .filter(|(timestamp, value)| {
            let render_stream_id = render.get("stream_id").and_then(serde_json::Value::as_u64);
            let transport_stream_id = value.get("stream_id").and_then(serde_json::Value::as_u64);
            match (render_stream_id, transport_stream_id) {
                (Some(render_stream_id), Some(transport_stream_id)) => {
                    render_stream_id == transport_stream_id
                }
                _ => render_timestamp.abs_diff(*timestamp) <= 120_000,
            }
        })
        .map(|(_, value)| value);

    let mut findings = Vec::new();
    if let Some(fps_x100) = render
        .get("displayed_fps_x100")
        .and_then(serde_json::Value::as_u64)
    {
        findings.push(format!("本地显示帧率: {:.2} FPS", fps_x100 as f64 / 100.0));
    } else {
        findings.push("本地显示帧率: 尚未形成有效采样".to_owned());
    }
    if let Some(gap_ms) = render
        .get("max_frame_gap_ms")
        .and_then(serde_json::Value::as_u64)
    {
        findings.push(format!("本地最大帧间隔: {gap_ms} 毫秒"));
    }

    let received = render
        .get("received_frames")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let submitted = render
        .get("submitted_frames")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let displayed = render
        .get("displayed_frames")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let pull_failures = render
        .get("video_pull_failures")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    findings.push(format!(
        "控制端视频计数: 收到 {received}，提交解码 {submitted}，显示 {displayed}，拉取失败 {pull_failures}"
    ));

    let completed = transport
        .as_ref()
        .and_then(|value| value.get("completed_frames"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let delivered = transport
        .as_ref()
        .and_then(|value| value.get("delivered_video_frames"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    let fps_x100 = render
        .get("displayed_fps_x100")
        .and_then(serde_json::Value::as_u64);
    let gap_ms = render
        .get("max_frame_gap_ms")
        .and_then(serde_json::Value::as_u64);
    let coalesced_frame_drops = render
        .get("coalesced_frame_drops")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    findings.push(format!("本地显示合并帧: {coalesced_frame_drops}"));

    let diagnosis = if submitted > 0 && displayed == 0 {
        "判断线索: 视频已进入解码路径但尚未显示，优先检查 WebView2 解码能力。"
    } else if completed == 0 && delivered == 0 && received == 0 {
        "判断线索: 暂无持续视频交付证据，优先检查网络、中继会话或目标主机编码。"
    } else if fps_x100.is_some_and(|value| value < 1_500)
        || gap_ms.is_some_and(|value| value > 250)
        || (displayed > 0 && coalesced_frame_drops > displayed / 2)
    {
        "判断线索: 视频已交付但本地显示有积压，优先检查解码或 WebView2 渲染。"
    } else if completed > 0 && delivered > 0 {
        "判断线索: 最近一段视频链路未显示明显的显示层卡顿。"
    } else {
        "判断线索: 指标不足以定位单一层级，请在复现卡顿后再次导出报告。"
    };
    findings.push(diagnosis.to_owned());
    findings
}

fn latest_metric_event(
    recent_events: &[String],
    event_name: &str,
) -> Option<(u128, serde_json::Value)> {
    recent_events.iter().rev().find_map(|line| {
        let json = line
            .split_once("] ")
            .map_or(line.as_str(), |(_, value)| value);
        let value = serde_json::from_str::<serde_json::Value>(json).ok()?;
        let timestamp = value
            .get("timestamp_unix_ms")
            .and_then(serde_json::Value::as_u64)
            .map(u128::from)?;
        (value.get("event").and_then(serde_json::Value::as_str) == Some(event_name))
            .then_some((timestamp, value))
    })
}

fn diagnostic_status_label(status: &str) -> &'static str {
    match status {
        "passed" => "通过",
        "failed" => "失败",
        "notApplicable" => "不适用",
        _ => "注意",
    }
}

fn normalize_fingerprint(fingerprint: &str) -> Result<String, String> {
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("可信控制端指纹无效，请刷新设备后重试。".to_owned());
    }
    Ok(fingerprint.to_ascii_lowercase())
}

fn revoke_controller(fingerprint: &str) -> Result<RevocationOutcome, String> {
    let diagnostics = DiagnosticLog::for_current_user().ok();
    let store = WindowsTrustedControllerStore::for_current_user().map_err(|_| {
        record_operation_failure(
            diagnostics.as_ref(),
            DiagnosticOperation::ControllerRevocation,
            "trusted-controller storage path is unavailable",
        );
        "可信设备存储不可用，信任状态没有改变。".to_owned()
    })?;
    let records = store.list().map_err(|error| {
        record_operation_failure(
            diagnostics.as_ref(),
            DiagnosticOperation::ControllerRevocation,
            &error.to_string(),
        );
        "无法打开可信设备，信任状态没有改变。".to_owned()
    })?;
    let record = records
        .into_iter()
        .find(|record| hex(record.fingerprint().as_bytes()) == fingerprint)
        .ok_or_else(|| "该控制端已不再受信任，请刷新设备查看当前状态。".to_owned())?;

    if !WindowsLocalApprovalDialog::confirm_revocation(record.device_id(), record.verify_key()) {
        return Ok(RevocationOutcome::Cancelled);
    }

    let revoked = store.revoke(record.fingerprint()).map_err(|error| {
        record_operation_failure(
            diagnostics.as_ref(),
            DiagnosticOperation::ControllerRevocation,
            &error.to_string(),
        );
        "DeskLink 无法撤销此控制端，其信任状态没有改变。".to_owned()
    })?;
    if !revoked {
        return Err("该控制端已不再受信任，请刷新设备查看当前状态。".to_owned());
    }
    if let Some(diagnostics) = diagnostics.as_ref() {
        let _ = diagnostics.record(&DiagnosticEvent::OperationSucceeded(
            DiagnosticOperation::ControllerRevocation,
        ));
    }
    Ok(RevocationOutcome::Revoked)
}

fn record_operation_failure(
    diagnostics: Option<&DiagnosticLog>,
    operation: DiagnosticOperation,
    reason: &str,
) {
    if let Some(diagnostics) = diagnostics {
        let _ = diagnostics.record(&DiagnosticEvent::OperationFailed {
            operation,
            reason: reason.to_owned(),
        });
    }
}

fn save_connection(input: ConnectionSettingsInput) -> Result<(), String> {
    let store = WindowsConnectionSettingsStore::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用连接设置。".to_owned())?;
    let existing = store
        .load()
        .map_err(|_| "无法打开已保存的连接设置。".to_owned())?;
    let existing_authentication = existing
        .as_ref()
        .map(|settings| Zeroizing::new(*settings.authentication()));
    let settings = HostConnectionSettings::from_text(
        &input.relay_address,
        &input.server_name,
        &input.session_id,
        &input.relay_key,
        existing_authentication
            .as_ref()
            .map(|authentication| **authentication),
        &input.stream_id,
    )
    .map_err(|error| error.to_string())?;
    store
        .save(&settings)
        .map_err(|_| "无法保存连接设置。".to_owned())?;
    Ok(())
}

fn create_managed_connection() -> Result<(), String> {
    let store = WindowsConnectionSettingsStore::for_current_user()
        .map_err(|_| "当前 Windows 账户无法使用连接设置。".to_owned())?;
    if store
        .load()
        .map_err(|_| "无法打开已保存的连接设置。".to_owned())?
        .is_some()
    {
        return Err("本机连接已经存在。如需修改，请打开高级连接设置。".to_owned());
    }

    let mut session_id = [0u8; 16];
    let mut authentication = [0u8; 32];
    OsRng.fill_bytes(&mut session_id);
    OsRng.fill_bytes(&mut authentication);
    let session_id = hex(&session_id);
    let authentication_text = Zeroizing::new(hex(&authentication));
    authentication.zeroize();
    let settings = HostConnectionSettings::from_text(
        local_relay::MANAGED_RELAY_ADDRESS,
        local_relay::MANAGED_RELAY_SERVER_NAME,
        &session_id,
        authentication_text.as_str(),
        None,
        "1",
    )
    .map_err(|_| "DeskLink 无法创建默认公网中继设置。".to_owned())?;
    store
        .save(&settings)
        .map_err(|_| "DeskLink 无法加密保存本机连接。".to_owned())
}

fn load_windows_preferences() -> Result<WindowsPreferencesSummary, String> {
    let launch_at_login = WindowsStartupSettings::for_current_executable()
        .map_err(|_| "DeskLink 无法定位当前安装程序。".to_owned())?
        .is_enabled()
        .map_err(|_| "Windows 登录启动设置当前不可用。".to_owned())?;
    let diagnostics_sharing_enabled = WindowsDiagnosticSharing::for_current_user()
        .map_err(|_| "DeskLink 无法打开诊断共享设置。".to_owned())?
        .is_enabled()
        .map_err(|_| "诊断共享设置当前不可用。".to_owned())?;
    Ok(WindowsPreferencesSummary {
        launch_at_login,
        diagnostics_sharing_enabled,
        close_to_tray: true,
        interface_language: "简体中文",
        version: env!("CARGO_PKG_VERSION"),
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(windows)]
    let instance_guard = instance_guard::ApplicationInstanceGuard::create()
        .expect("DeskLink could not create its installer coordination guard");
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, arguments, _| {
            if arguments
                .iter()
                .any(|argument| argument == "--installer-shutdown")
            {
                app.state::<HostManager>().request_stop();
                app.state::<ControllerManager>().request_stop();
                app.exit(0);
            } else if !arguments.iter().any(|argument| argument == "--startup") {
                show_main_window(app);
            }
        }))
        .manage(HostManager::default())
        .manage(ControllerManager::for_current_user());
    #[cfg(windows)]
    let builder = builder.manage(instance_guard);
    let application = builder
        .setup(|app| {
            if env::args_os().any(|argument| argument == "--installer-shutdown") {
                app.handle().exit(0);
                return Ok(());
            }
            let diagnostics = DiagnosticLog::for_current_user().ok();
            if let Some(diagnostics) = diagnostics.as_ref() {
                let _ = diagnostics.record(&DiagnosticEvent::ControlSurfaceStarted);
            }
            start_background_uploader();
            setup_tray(app)?;
            let manager = app.state::<HostManager>().inner().clone();
            #[cfg(windows)]
            match power::install(app.handle(), manager.clone()) {
                Ok(monitor) => {
                    app.manage(monitor);
                }
                Err(error) => {
                    if let Some(diagnostics) = diagnostics.as_ref() {
                        let _ = diagnostics.record(&DiagnosticEvent::PowerResumeMonitoringFailed {
                            reason: error.to_string(),
                        });
                    }
                }
            }
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                manager.restart(app_handle).await;
            });
            if !env::args_os().any(|argument| argument == "--startup") {
                show_main_window(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_host_snapshot,
            get_windows_preferences,
            set_launch_at_login,
            set_diagnostics_sharing,
            upload_diagnostics_now,
            check_windows_release,
            quit_desklink,
            respond_host_approval,
            restart_host,
            save_connection_settings,
            setup_managed_connection,
            start_pairing_session,
            cancel_pairing_session,
            get_fixed_access_password,
            regenerate_fixed_access_password,
            disable_fixed_access_password,
            revoke_trusted_controller,
            probe_relay,
            export_diagnostic_report,
            get_controller_snapshot,
            connect_device,
            connect_saved_device,
            clear_saved_devices,
            reconnect_controller,
            next_controller_video_frame,
            send_controller_input,
            send_controller_text,
            paste_controller_clipboard_text,
            set_controller_audio_enabled,
            set_controller_video_quality,
            set_controller_video_profile,
            send_controller_clipboard,
            request_controller_clipboard,
            choose_and_send_controller_file,
            queue_controller_files,
            remove_controller_queued_file,
            clear_controller_file_queue,
            resume_controller_file_queue,
            retry_controller_file_queue_protection,
            request_controller_remote_file,
            retry_controller_file,
            cancel_controller_file,
            discard_controller_file_recovery,
            discard_controller_file_queue_recovery,
            open_controller_downloads_folder,
            request_controller_keyframe,
            report_controller_render_metrics,
            report_controller_playback_pressure,
            open_github_repository,
            open_windows_releases,
            select_controller_display,
            disconnect_controller,
            forget_saved_device,
            rename_saved_device
        ])
        .build(tauri::generate_context!())
        .expect("DeskLink could not start its control surface");
    application.run(|app, event| {
        if matches!(event, RunEvent::ExitRequested { .. } | RunEvent::Exit) {
            app.state::<HostManager>().request_stop();
            app.state::<ControllerManager>().request_stop();
        }
    });
}

fn setup_tray(app: &mut tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开 DeskLink", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "退出 DeskLink", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &separator, &quit])?;
    let mut tray = TrayIconBuilder::with_id(tray_id())
        .tooltip("DeskLink：正在启动")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "quit" => {
                let manager = app.state::<HostManager>().inner().clone();
                app.state::<ControllerManager>().request_stop();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    manager.stop().await;
                    app.exit(0);
                });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConnectionSummary, HostSnapshot, TrustedControllerSummary, build_diagnostic_checks,
        build_diagnostic_report, build_video_performance_summary, hex,
        merge_recent_diagnostic_lines, normalize_fingerprint,
    };
    use crate::{
        controller::ControllerRuntimeSummary, host::HostRuntimeSummary,
        local_relay::RelayStatusSummary,
    };

    #[test]
    fn hex_preserves_leading_zeroes() {
        assert_eq!(hex(&[0, 1, 15, 16, 255]), "00010f10ff");
    }

    #[test]
    fn fingerprint_validation_normalizes_ascii_hex() {
        let uppercase = "AB".repeat(32);
        assert_eq!(normalize_fingerprint(&uppercase).unwrap(), "ab".repeat(32));
    }

    #[test]
    fn fingerprint_validation_rejects_wrong_length_and_non_hex() {
        assert!(normalize_fingerprint("ab").is_err());
        assert!(normalize_fingerprint(&"z".repeat(64)).is_err());
    }

    #[test]
    fn diagnostic_report_merges_host_and_controller_events_in_time_order() {
        let merged = merge_recent_diagnostic_lines(
            vec![
                "{\"timestamp_unix_ms\":200,\"event\":\"host_late\"}".to_owned(),
                "{\"timestamp_unix_ms\":100,\"event\":\"host_early\"}".to_owned(),
            ],
            vec!["{\"timestamp_unix_ms\":150,\"event\":\"controller_middle\"}".to_owned()],
        );

        assert_eq!(
            merged,
            vec![
                "[主机] {\"timestamp_unix_ms\":100,\"event\":\"host_early\"}",
                "[控制端] {\"timestamp_unix_ms\":150,\"event\":\"controller_middle\"}",
                "[主机] {\"timestamp_unix_ms\":200,\"event\":\"host_late\"}",
            ]
        );
    }

    #[test]
    fn diagnostic_report_uses_stable_checks_without_identity_or_session_secrets() {
        let runtime = HostRuntimeSummary {
            state: "connected",
            title: "主机已连接".to_owned(),
            detail: "正在等待控制端。".to_owned(),
            tooltip: "DeskLink：已连接".to_owned(),
        };
        let relay_status = RelayStatusSummary {
            mode: "external",
            state: "ready",
            title: "DeskLink 公网中继已配置".to_owned(),
            detail: "支持跨网络连接".to_owned(),
        };
        let connection = ConnectionSummary {
            relay_address: "101.35.246.159:4433".to_owned(),
            server_name: "turn.p2p.yxswy.com".to_owned(),
            session_id: "0123456789abcdef0123456789abcdef".to_owned(),
            stream_id: 9,
            has_saved_key: true,
        };
        let checks = build_diagnostic_checks(&runtime, Some(&connection), None, None, None);
        let verify_key = "ab".repeat(32);
        let snapshot = HostSnapshot {
            readiness: "configured",
            title: "主机已连接".to_owned(),
            detail: "可以接受控制端".to_owned(),
            runtime,
            connection: Some(connection),
            connection_error: None,
            trusted_controllers: vec![TrustedControllerSummary {
                device_id: "cd".repeat(16),
                verify_key: verify_key.clone(),
                fingerprint: "ef".repeat(32),
                approved_at_unix_s: 100,
            }],
            trusted_error: None,
            relay_status,
            diagnostic_checks: checks,
            pairing_active: false,
            pending_approval: None,
            fixed_password_enabled: true,
            fixed_password_error: None,
            device_id: Some("123 456 789 012".to_owned()),
            refreshed_at_unix_s: 200,
        };
        let controller = ControllerRuntimeSummary {
            state: "idle",
            title: "可以控制另一台电脑".to_owned(),
            detail: "尚未连接".to_owned(),
            stream_id: None,
        };
        let secret = "99".repeat(32);
        let report = build_diagnostic_report(
            &snapshot,
            &controller,
            "DL-WIN-TEST-00001",
            123,
            &[format!("DESKLINK_AUTH_KEY={secret}")],
        );

        assert!(report.contains("DL-CFG-101 [通过]"));
        assert!(report.contains("DL-NET-201 [通过]"));
        assert!(report.contains("101.35.246.159:4433"));
        assert!(report.contains("<redacted>"));
        assert!(!report.contains("0123456789abcdef0123456789abcdef"));
        assert!(!report.contains(&verify_key));
        assert!(!report.contains(&secret));
    }

    #[test]
    fn diagnostic_report_adds_video_performance_findings_without_raw_secrets() {
        let events = vec![
            "[控制端] {\"timestamp_unix_ms\":1000,\"event\":\"controller_video_metrics\",\"stream_id\":7,\"completed_frames\":120,\"delivered_video_frames\":118}".to_owned(),
            "[控制端] {\"timestamp_unix_ms\":1100,\"event\":\"controller_render_metrics\",\"received_frames\":116,\"submitted_frames\":114,\"displayed_frames\":110,\"video_pull_failures\":1,\"displayed_fps_x100\":1240,\"max_frame_gap_ms\":420,\"coalesced_frame_drops\":80}".to_owned(),
        ];

        let summary = build_video_performance_summary(&events).join("\n");

        assert!(summary.contains("本地显示帧率: 12.40 FPS"));
        assert!(summary.contains("本地最大帧间隔: 420 毫秒"));
        assert!(summary.contains("本地显示合并帧: 80"));
        assert!(summary.contains("优先检查解码或 WebView2 渲染"));
        assert!(!summary.contains("session"));
    }

    #[test]
    fn video_performance_summary_does_not_mix_an_old_transport_session() {
        let events = vec![
            "[控制端] {\"timestamp_unix_ms\":200000,\"event\":\"controller_video_metrics\",\"stream_id\":6,\"completed_frames\":120,\"delivered_video_frames\":118}".to_owned(),
            "[控制端] {\"timestamp_unix_ms\":200100,\"event\":\"controller_render_metrics\",\"stream_id\":7,\"received_frames\":116,\"submitted_frames\":114,\"displayed_frames\":110,\"video_pull_failures\":1,\"displayed_fps_x100\":3000,\"max_frame_gap_ms\":33}".to_owned(),
        ];

        let summary = build_video_performance_summary(&events).join("\n");

        assert!(summary.contains("指标不足以定位单一层级"));
        assert!(!summary.contains("未显示明显的显示层卡顿"));
    }

    #[test]
    fn video_performance_summary_explains_missing_samples() {
        let summary = build_video_performance_summary(&[]).join("\n");
        assert!(summary.contains("尚未捕获足够的视频渲染指标"));
    }
}
