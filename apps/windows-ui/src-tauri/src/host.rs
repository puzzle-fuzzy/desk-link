use std::{
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use apps_windows::{
    configuration::WindowsConnectionSettingsStore,
    diagnostics::{DiagnosticEvent, DiagnosticLog},
    identity::WindowsIdentityStore,
    runtime::{HostLifecycleEvent, HostLifecycleObserver, HostSupervisor},
    tray::HostStatusViewModel,
    trusted::{
        WindowsControllerAuthorizer, WindowsPairingAuthorizer, WindowsTrustedControllerStore,
    },
    window::WindowsLocalApprovalDialog,
};
use desklink_crypto::{MAX_PAIRING_TTL_S, PairingCode, PairingInvite};
use desklink_transport::RelayDirectoryRegistration;
use rand_core::OsRng;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use zeroize::Zeroize;

const TRAY_ID: &str = "desklink-tray";
const HOST_EVENT: &str = "host-runtime-changed";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostRuntimeSummary {
    pub state: &'static str,
    pub title: String,
    pub detail: String,
    pub tooltip: String,
}

impl HostRuntimeSummary {
    fn starting() -> Self {
        let view = HostStatusViewModel::starting();
        Self {
            state: "starting",
            title: view.title,
            detail: view.detail,
            tooltip: view.tooltip,
        }
    }

    fn not_configured() -> Self {
        Self {
            state: "notConfigured",
            title: "完成连接设置".to_owned(),
            detail: "启动主机前，请填写另一台 DeskLink 设备共享的中继服务器信息。".to_owned(),
            tooltip: "DeskLink：需要完成设置".to_owned(),
        }
    }

    fn unavailable(detail: &'static str) -> Self {
        Self {
            state: "stopped",
            title: "主机服务不可用".to_owned(),
            detail: detail.to_owned(),
            tooltip: "DeskLink：主机服务不可用".to_owned(),
        }
    }

    fn stopped_normally() -> Self {
        Self {
            state: "stopped",
            title: "主机服务已停止".to_owned(),
            detail: "主机服务已停止。DeskLink 会保持打开，便于你检查连接。".to_owned(),
            tooltip: "DeskLink：主机服务已停止".to_owned(),
        }
    }

    fn pairing() -> Self {
        Self {
            state: "pairing",
            title: "配对邀请已生效".to_owned(),
            detail: "正在等待控制端。保存信任前，Windows 会要求你确认对方的完整身份。".to_owned(),
            tooltip: "DeskLink：配对邀请已生效".to_owned(),
        }
    }

    fn pairing_finished() -> Self {
        Self {
            state: "stopped",
            title: "配对会话已结束".to_owned(),
            detail: "一次性邀请已失效。准备好后可恢复普通主机模式。".to_owned(),
            tooltip: "DeskLink：配对会话已结束".to_owned(),
        }
    }

    fn from_event(event: &HostLifecycleEvent) -> Self {
        let state = match event {
            HostLifecycleEvent::Connecting { .. } => "connecting",
            HostLifecycleEvent::Available { .. } => "available",
            HostLifecycleEvent::Connected { .. } => "connected",
            HostLifecycleEvent::Reconnecting { .. } => "reconnecting",
            HostLifecycleEvent::Stopped { .. } => "stopped",
        };
        let mut view = HostStatusViewModel::starting();
        view.apply(event);
        Self {
            state,
            title: view.title,
            detail: view.detail.replace("\r\n", " "),
            tooltip: view.tooltip,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingSessionSummary {
    pub device_id: String,
    pub temporary_password: String,
    pub invitation: String,
    pub expires_at_unix_s: u64,
}

impl Drop for PairingSessionSummary {
    fn drop(&mut self) {
        self.temporary_password.zeroize();
        self.invitation.zeroize();
    }
}

struct HostWorker {
    shutdown: oneshot::Sender<()>,
    task: tauri::async_runtime::JoinHandle<()>,
}

#[derive(Clone)]
pub struct HostManager {
    status: Arc<Mutex<HostRuntimeSummary>>,
    worker: Arc<Mutex<Option<HostWorker>>>,
    pairing_active: Arc<Mutex<bool>>,
    restart_lock: Arc<AsyncMutex<()>>,
}

impl Default for HostManager {
    fn default() -> Self {
        Self {
            status: Arc::new(Mutex::new(HostRuntimeSummary::starting())),
            worker: Arc::new(Mutex::new(None)),
            pairing_active: Arc::new(Mutex::new(false)),
            restart_lock: Arc::new(AsyncMutex::new(())),
        }
    }
}

impl HostManager {
    pub fn snapshot(&self) -> HostRuntimeSummary {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_else(|_| {
                HostRuntimeSummary::unavailable("DeskLink 无法读取本地主机状态，请重新启动应用。")
            })
    }

    pub async fn restart(&self, app: AppHandle) {
        let _restart = self.restart_lock.lock().await;
        self.stop_current().await;
        self.set_pairing_active(false);
        self.publish(&app, HostRuntimeSummary::starting());

        self.start_normal_worker(app);
    }

    pub async fn start_pairing(&self, app: AppHandle) -> Result<PairingSessionSummary, String> {
        let _restart = self.restart_lock.lock().await;
        self.stop_current().await;
        self.set_pairing_active(false);
        self.publish(&app, HostRuntimeSummary::pairing());

        let manager = self.clone();
        let observer_app = app.clone();
        let observer: Arc<dyn HostLifecycleObserver> = Arc::new(move |event| {
            manager.publish_event(&observer_app, event);
        });
        let prepared = tauri::async_runtime::spawn_blocking(move || prepare_pairing(observer))
            .await
            .map_err(|_| "DeskLink 无法启动配对任务，已恢复普通主机模式。".to_owned());

        let prepared = match prepared {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(failure)) => {
                let message = failure.pairing_message().to_owned();
                self.publish(&app, failure.summary());
                self.start_normal_worker(app);
                return Err(message);
            }
            Err(message) => {
                self.publish(
                    &app,
                    HostRuntimeSummary::unavailable("DeskLink 无法启动受保护的配对会话。"),
                );
                self.start_normal_worker(app);
                return Err(message);
            }
        };

        let PreparedPairing {
            supervisor,
            diagnostics,
            session,
        } = prepared;
        self.set_pairing_active(true);
        self.start_pairing_worker(app, supervisor, diagnostics);
        Ok(session)
    }

    pub fn is_pairing_active(&self) -> bool {
        self.pairing_active
            .lock()
            .map(|active| *active)
            .unwrap_or(false)
    }

    fn start_normal_worker(&self, app: AppHandle) {
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let manager = self.clone();
        let worker_app = app.clone();
        let task = tauri::async_runtime::spawn(async move {
            manager.run_worker(worker_app, shutdown_receiver).await;
        });
        if let Ok(mut worker) = self.worker.lock() {
            *worker = Some(HostWorker { shutdown, task });
        } else {
            let _ = shutdown.send(());
            task.abort();
        }
    }

    fn start_pairing_worker(
        &self,
        app: AppHandle,
        supervisor: Box<HostSupervisor>,
        diagnostics: DiagnosticLog,
    ) {
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let manager = self.clone();
        let task = tauri::async_runtime::spawn(async move {
            let finished = tokio::select! {
                _ = (*supervisor).run() => true,
                _ = shutdown_receiver => false,
            };
            manager.set_pairing_active(false);
            record_diagnostic(&diagnostics, &DiagnosticEvent::ApplicationStopped);
            if finished {
                manager.publish(&app, HostRuntimeSummary::pairing_finished());
                manager.start_normal_worker(app);
            }
        });
        if let Ok(mut worker) = self.worker.lock() {
            *worker = Some(HostWorker { shutdown, task });
        } else {
            let _ = shutdown.send(());
            task.abort();
            self.set_pairing_active(false);
        }
    }

    pub async fn stop(&self) {
        let _restart = self.restart_lock.lock().await;
        self.stop_current().await;
    }

    pub fn request_stop(&self) {
        if let Some(worker) = self.take_worker() {
            let _ = worker.shutdown.send(());
        }
    }

    async fn stop_current(&self) {
        let Some(mut worker) = self.take_worker() else {
            return;
        };
        let _ = worker.shutdown.send(());
        if tokio::time::timeout(Duration::from_secs(5), &mut worker.task)
            .await
            .is_err()
        {
            worker.task.abort();
            let _ = worker.task.await;
        }
    }

    fn take_worker(&self) -> Option<HostWorker> {
        self.worker.lock().ok()?.take()
    }

    fn set_pairing_active(&self, active: bool) {
        if let Ok(mut pairing_active) = self.pairing_active.lock() {
            *pairing_active = active;
        }
    }

    async fn run_worker(&self, app: AppHandle, shutdown: oneshot::Receiver<()>) {
        let manager = self.clone();
        let observer_app = app.clone();
        let observer: Arc<dyn HostLifecycleObserver> = Arc::new(move |event| {
            manager.publish_event(&observer_app, event);
        });
        let prepared = tauri::async_runtime::spawn_blocking(move || prepare_host(observer)).await;

        match prepared {
            Err(_) => self.publish(
                &app,
                HostRuntimeSummary::unavailable("DeskLink 无法启动本地主机任务，请重新启动应用。"),
            ),
            Ok(Err(failure)) => self.publish(&app, failure.summary()),
            Ok(Ok(PreparedHost::Unconfigured { diagnostics })) => {
                self.publish(&app, HostRuntimeSummary::not_configured());
                let _ = shutdown.await;
                record_diagnostic(&diagnostics, &DiagnosticEvent::ApplicationStopped);
            }
            Ok(Ok(PreparedHost::Ready {
                supervisor,
                diagnostics,
            })) => {
                tokio::select! {
                    result = (*supervisor).run() => {
                        if result.is_ok() {
                            self.publish(&app, HostRuntimeSummary::stopped_normally());
                        }
                    }
                    _ = shutdown => {}
                }
                record_diagnostic(&diagnostics, &DiagnosticEvent::ApplicationStopped);
            }
        }
    }

    fn publish_event(&self, app: &AppHandle, event: HostLifecycleEvent) {
        self.publish(app, HostRuntimeSummary::from_event(&event));
    }

    fn publish(&self, app: &AppHandle, summary: HostRuntimeSummary) {
        if let Ok(mut status) = self.status.lock() {
            *status = summary.clone();
        }
        if let Some(tray) = app.tray_by_id(TRAY_ID) {
            let _ = tray.set_tooltip(Some(&summary.tooltip));
        }
        let _ = app.emit(HOST_EVENT, summary);
    }
}

enum PreparedHost {
    Ready {
        supervisor: Box<HostSupervisor>,
        diagnostics: DiagnosticLog,
    },
    Unconfigured {
        diagnostics: DiagnosticLog,
    },
}

struct PreparedPairing {
    supervisor: Box<HostSupervisor>,
    diagnostics: DiagnosticLog,
    session: PairingSessionSummary,
}

enum HostPreparationFailure {
    Diagnostics,
    ConnectionStorage,
    ConnectionProtection,
    Identity,
    TrustStorage,
    RelayConfiguration,
    Runtime,
    Clock,
    Pairing,
}

impl HostPreparationFailure {
    fn summary(&self) -> HostRuntimeSummary {
        let detail = match self {
            Self::Diagnostics => "DeskLink 无法打开当前 Windows 账户的受保护诊断存储。",
            Self::ConnectionStorage => "当前 Windows 账户无法使用连接设置。",
            Self::ConnectionProtection => "无法打开已保存的连接设置，请重新填写。",
            Self::Identity => "无法打开当前账户受保护的主机身份。",
            Self::TrustStorage => "可信设备存储不可用，主机将保持拒绝连接。",
            Self::RelayConfiguration => "已保存的中继服务器配置无效。",
            Self::Runtime => "无法启动加密的 Windows 主机。",
            Self::Clock => "Windows 系统时钟不可用，无法安全配对。",
            Self::Pairing => "DeskLink 无法创建受保护的一次性配对邀请。",
        };
        HostRuntimeSummary::unavailable(detail)
    }

    fn pairing_message(&self) -> &'static str {
        match self {
            Self::ConnectionStorage | Self::ConnectionProtection => {
                "开始配对前请保存有效的连接设置，已恢复普通主机模式。"
            }
            Self::Diagnostics => "受保护的诊断存储不可用，配对未启动，已恢复普通主机模式。",
            Self::Identity => "受保护的主机身份不可用，配对未启动，已恢复普通主机模式。",
            Self::TrustStorage => "可信设备存储不可用，配对将继续拒绝连接，已恢复普通主机模式。",
            Self::RelayConfiguration => {
                "已保存的中继服务器配置无效，配对未启动，已恢复普通主机模式。"
            }
            Self::Runtime | Self::Pairing | Self::Clock => {
                "DeskLink 无法创建安全配对会话，已恢复普通主机模式。"
            }
        }
    }
}

fn prepare_host(
    ui_observer: Arc<dyn HostLifecycleObserver>,
) -> Result<PreparedHost, HostPreparationFailure> {
    let diagnostics =
        DiagnosticLog::for_current_user().map_err(|_| HostPreparationFailure::Diagnostics)?;
    record_diagnostic(
        &diagnostics,
        &DiagnosticEvent::ApplicationStarted {
            pairing_mode: false,
        },
    );

    let connection_store = WindowsConnectionSettingsStore::for_current_user()
        .map_err(|_| HostPreparationFailure::ConnectionStorage)?;
    let Some(connection) = connection_store
        .load()
        .map_err(|_| HostPreparationFailure::ConnectionProtection)?
    else {
        return Ok(PreparedHost::Unconfigured { diagnostics });
    };
    let identity = WindowsIdentityStore::for_current_user()
        .map_err(|_| HostPreparationFailure::Identity)?
        .load_or_create(&mut OsRng)
        .map_err(|_| HostPreparationFailure::Identity)?;
    let trusted = WindowsTrustedControllerStore::for_current_user()
        .map_err(|_| HostPreparationFailure::TrustStorage)?;
    let authorizer = Arc::new(WindowsControllerAuthorizer::new(trusted));
    let lifecycle_diagnostics = diagnostics.clone();
    let observer: Arc<dyn HostLifecycleObserver> = Arc::new(move |event: HostLifecycleEvent| {
        record_diagnostic(
            &lifecycle_diagnostics,
            &DiagnosticEvent::Lifecycle(event.clone()),
        );
        ui_observer.publish(event);
    });
    let transport =
        crate::local_relay::client_config(connection.relay_address(), connection.server_name())
            .map_err(|_| HostPreparationFailure::RelayConfiguration)?;
    let supervisor = HostSupervisor::new(
        transport,
        connection.session_id(),
        *connection.authentication(),
        connection.stream_id(),
        identity,
        authorizer,
        None,
    )
    .map_err(|_| HostPreparationFailure::Runtime)?
    .with_observer(observer);
    Ok(PreparedHost::Ready {
        supervisor: Box::new(supervisor),
        diagnostics,
    })
}

fn prepare_pairing(
    ui_observer: Arc<dyn HostLifecycleObserver>,
) -> Result<PreparedPairing, HostPreparationFailure> {
    let diagnostics =
        DiagnosticLog::for_current_user().map_err(|_| HostPreparationFailure::Diagnostics)?;
    record_diagnostic(
        &diagnostics,
        &DiagnosticEvent::ApplicationStarted { pairing_mode: true },
    );

    let connection_store = WindowsConnectionSettingsStore::for_current_user()
        .map_err(|_| HostPreparationFailure::ConnectionStorage)?;
    let connection = connection_store
        .load()
        .map_err(|_| HostPreparationFailure::ConnectionProtection)?
        .ok_or(HostPreparationFailure::ConnectionProtection)?;
    let identity = WindowsIdentityStore::for_current_user()
        .map_err(|_| HostPreparationFailure::Identity)?
        .load_or_create(&mut OsRng)
        .map_err(|_| HostPreparationFailure::Identity)?;
    let trusted = WindowsTrustedControllerStore::for_current_user()
        .map_err(|_| HostPreparationFailure::TrustStorage)?;
    let now_unix_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HostPreparationFailure::Clock)?
        .as_secs();
    let invite = PairingInvite::for_connection(
        &identity,
        connection.session_id(),
        *connection.authentication(),
        now_unix_s,
        MAX_PAIRING_TTL_S,
    )
    .map_err(|_| HostPreparationFailure::Pairing)?;
    let encoded = invite
        .encode()
        .map_err(|_| HostPreparationFailure::Pairing)?;
    let device_id = crate::device_directory::public_device_id(identity.device_id);
    let access_code = PairingCode::generate(&mut OsRng);
    let directory_registration = RelayDirectoryRegistration::new(
        device_id,
        *access_code.as_bytes(),
        encoded.as_bytes().to_vec(),
        MAX_PAIRING_TTL_S
            .try_into()
            .map_err(|_| HostPreparationFailure::Pairing)?,
    )
    .map_err(|_| HostPreparationFailure::Pairing)?;
    let invitation = crate::hex(encoded.as_bytes());
    let session = PairingSessionSummary {
        device_id: crate::device_directory::format_device_id(device_id),
        temporary_password: access_code.to_string(),
        invitation: crate::local_relay::pairing_package(&connection, &invitation),
        expires_at_unix_s: invite.expires_at_unix_s(),
    };
    let session_id = invite.session_id();
    let authentication = *invite.relay_authentication();
    let expires_at_unix_s = invite.expires_at_unix_s();
    let authorizer = Arc::new(WindowsPairingAuthorizer::new(
        trusted,
        invite,
        Box::new(WindowsLocalApprovalDialog),
    ));
    let lifecycle_diagnostics = diagnostics.clone();
    let observer: Arc<dyn HostLifecycleObserver> = Arc::new(move |event: HostLifecycleEvent| {
        record_diagnostic(
            &lifecycle_diagnostics,
            &DiagnosticEvent::Lifecycle(event.clone()),
        );
        ui_observer.publish(event);
    });
    let transport =
        crate::local_relay::client_config(connection.relay_address(), connection.server_name())
            .map_err(|_| HostPreparationFailure::RelayConfiguration)?;
    let supervisor = HostSupervisor::new(
        transport,
        session_id,
        authentication,
        connection.stream_id(),
        identity,
        authorizer,
        Some(expires_at_unix_s),
    )
    .map_err(|_| HostPreparationFailure::Runtime)?
    .with_directory_registration(directory_registration)
    .with_observer(observer);
    Ok(PreparedPairing {
        supervisor: Box::new(supervisor),
        diagnostics,
        session,
    })
}

fn record_diagnostic(diagnostics: &DiagnosticLog, event: &DiagnosticEvent) {
    let _ = diagnostics.record(event);
}

pub fn tray_id() -> &'static str {
    TRAY_ID
}

#[cfg(test)]
mod tests {
    use super::HostRuntimeSummary;
    use apps_windows::runtime::HostLifecycleEvent;

    #[test]
    fn lifecycle_status_exposes_safe_connected_copy() {
        let status =
            HostRuntimeSummary::from_event(&HostLifecycleEvent::Connected { stream_id: 9 });
        assert_eq!(status.state, "connected");
        assert_eq!(status.title, "远程控制已连接");
        assert!(status.detail.contains("视频流 9"));
    }

    #[test]
    fn lifecycle_status_reports_an_idle_relay_connection_as_available() {
        let status =
            HostRuntimeSummary::from_event(&HostLifecycleEvent::Available { stream_id: 4 });
        assert_eq!(status.state, "available");
        assert_eq!(status.title, "此设备已在线");
        assert!(status.detail.contains("等待另一台电脑"));
    }

    #[test]
    fn lifecycle_status_sanitizes_internal_stop_reason() {
        let status = HostRuntimeSummary::from_event(&HostLifecycleEvent::Stopped {
            reason: "authorization backend failed: C:\\secret\\host.bin".to_owned(),
        });
        assert_eq!(status.state, "stopped");
        assert!(!status.detail.contains("secret"));
        assert!(!status.detail.contains("host.bin"));
    }
}
