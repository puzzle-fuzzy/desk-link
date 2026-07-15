mod controller;
mod host;
mod local_relay;

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
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};

use apps_windows::{
    configuration::{HostConnectionSettings, WindowsConnectionSettingsStore},
    diagnostics::{DiagnosticEvent, DiagnosticLog, DiagnosticOperation},
    trusted::WindowsTrustedControllerStore,
    window::WindowsLocalApprovalDialog,
};
use controller::{
    ControllerConnectionInput, ControllerInput, ControllerManager, ControllerSignal,
    ControllerSnapshot,
};
use host::{HostManager, HostRuntimeSummary, PairingSessionSummary, tray_id};
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
    pairing_active: bool,
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RevocationResult {
    revoked: bool,
    snapshot: HostSnapshot,
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
    tauri::async_runtime::spawn_blocking(move || load_host_snapshot(runtime, pairing_active))
        .await
        .map_err(|_| "DeskLink 无法读取本地状态，请重试。".to_owned())?
}

#[tauri::command]
async fn get_controller_snapshot(
    manager: State<'_, ControllerManager>,
) -> Result<ControllerSnapshot, String> {
    controller::load_snapshot(manager.snapshot())
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
        return Err("TLS 服务器名称无效，请重新复制完整连接码。".to_owned());
    }
    local_relay::probe(relay_address, server_name).await
}

#[tauri::command]
async fn connect_controller(
    manager: State<'_, ControllerManager>,
    input: ControllerConnectionInput,
    signals: Channel<ControllerSignal>,
    video: Channel<Response>,
) -> Result<ControllerSnapshot, String> {
    manager.connect_invitation(input, signals, video).await
}

#[tauri::command]
async fn reconnect_controller(
    manager: State<'_, ControllerManager>,
    signals: Channel<ControllerSignal>,
    video: Channel<Response>,
) -> Result<ControllerSnapshot, String> {
    manager.connect_saved(signals, video).await
}

#[tauri::command]
fn send_controller_input(
    manager: State<'_, ControllerManager>,
    input: ControllerInput,
) -> Result<(), String> {
    manager.send_input(input)
}

#[tauri::command]
fn request_controller_keyframe(manager: State<'_, ControllerManager>) -> Result<(), String> {
    manager.request_keyframe()
}

#[tauri::command]
async fn disconnect_controller(
    manager: State<'_, ControllerManager>,
) -> Result<ControllerSnapshot, String> {
    manager.disconnect().await
}

#[tauri::command]
async fn forget_controller(
    manager: State<'_, ControllerManager>,
) -> Result<ControllerSnapshot, String> {
    manager.forget_saved().await
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
    local_relay::start_if_configured();
    manager.restart(app).await;
    let runtime = manager.snapshot();
    let pairing_active = manager.is_pairing_active();
    tauri::async_runtime::spawn_blocking(move || load_host_snapshot(runtime, pairing_active))
        .await
        .map_err(|_| "DeskLink 无法刷新本地状态，请重试。".to_owned())?
}

#[tauri::command]
async fn start_pairing_session(
    app: AppHandle,
    manager: State<'_, HostManager>,
) -> Result<PairingSessionSummary, String> {
    local_relay::start_if_configured();
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
    tauri::async_runtime::spawn_blocking(move || load_host_snapshot(runtime, pairing_active))
        .await
        .map_err(|_| "DeskLink 已恢复普通主机模式，但无法刷新本地状态。".to_owned())?
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
    let snapshot =
        tauri::async_runtime::spawn_blocking(move || load_host_snapshot(runtime, pairing_active))
            .await
            .map_err(|_| "DeskLink 已完成本地确认，但无法刷新设备状态。".to_owned())??;
    Ok(RevocationResult { revoked, snapshot })
}

fn load_host_snapshot(
    runtime: HostRuntimeSummary,
    pairing_active: bool,
) -> Result<HostSnapshot, String> {
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

    let relay_status = local_relay::status(connection.as_ref());
    let connection = connection.map(|settings| ConnectionSummary {
        relay_address: settings.relay_address_text(),
        server_name: settings.server_name().to_owned(),
        session_id: settings.session_id_text(),
        stream_id: settings.stream_id(),
        has_saved_key: true,
    });
    let (readiness, title, detail) = if connection_error.is_some() || trusted_error.is_some() {
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
        pairing_active,
        refreshed_at_unix_s: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    })
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
        .manage(ControllerManager::default());
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
            setup_tray(app)?;
            local_relay::start_if_configured();
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
            save_connection_settings,
            start_pairing_session,
            cancel_pairing_session,
            revoke_trusted_controller,
            probe_relay,
            get_controller_snapshot,
            connect_controller,
            reconnect_controller,
            send_controller_input,
            request_controller_keyframe,
            disconnect_controller,
            forget_controller
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
    use super::{hex, normalize_fingerprint};

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
}
