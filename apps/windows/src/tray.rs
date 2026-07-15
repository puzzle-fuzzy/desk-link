use crate::runtime::HostLifecycleEvent;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostStatusViewModel {
    pub title: String,
    pub detail: String,
    pub tooltip: String,
}

impl HostStatusViewModel {
    pub fn starting() -> Self {
        Self {
            title: "正在启动 DeskLink".to_owned(),
            detail: "正在准备加密的 Windows 主机。".to_owned(),
            tooltip: "DeskLink：正在启动".to_owned(),
        }
    }

    pub fn apply(&mut self, event: &HostLifecycleEvent) {
        match event {
            HostLifecycleEvent::Connecting { attempt, stream_id } => {
                self.title = "正在连接中继服务器".to_owned();
                self.detail =
                    format!("第 {attempt} 次连接尝试。下一个视频流将使用 ID {stream_id}。");
                self.tooltip = "DeskLink：正在连接".to_owned();
            }
            HostLifecycleEvent::Connected { stream_id } => {
                self.title = "远程控制已连接".to_owned();
                self.detail = format!("已验证的控制端正通过加密视频流 {stream_id} 连接。");
                self.tooltip = "DeskLink：远程控制已连接".to_owned();
            }
            HostLifecycleEvent::Reconnecting {
                retry,
                maximum_retries,
                delay,
                reason,
            } => {
                self.title = "连接已中断".to_owned();
                self.detail = format!(
                    "第 {retry}/{maximum_retries} 次重试将在 {} 毫秒后开始。\r\n{reason}",
                    delay.as_millis(),
                    reason = user_facing_host_reason(reason)
                );
                self.tooltip = format!("DeskLink：正在重新连接（{retry}/{maximum_retries}）");
            }
            HostLifecycleEvent::Stopped { reason } => {
                self.title = "主机服务已停止".to_owned();
                self.detail = user_facing_host_reason(reason).to_owned();
                self.tooltip = "DeskLink：主机服务已停止".to_owned();
            }
        }
    }
}

fn user_facing_host_reason(reason: &str) -> &'static str {
    let reason = reason.to_ascii_lowercase();
    if reason.contains("configuration") && reason.contains("could not be loaded") {
        "无法打开已保存的连接设置。请打开托盘菜单，选择“连接设置”后重新填写。"
    } else if reason.contains("configuration") || reason.contains("not configured") {
        "DeskLink 尚未配置主机连接。请打开托盘菜单并选择“连接设置”。"
    } else if reason.contains("pairing") && reason.contains("expired") {
        "配对邀请已过期，请重新开始配对以创建新邀请。"
    } else if reason.contains("untrusted")
        || reason.contains("authentication")
        || reason.contains("cryptographic")
        || reason.contains("public key")
        || reason.contains("key changed")
    {
        "无法验证控制端身份，请检查设备后重新配对。"
    } else if reason.contains("rejected") {
        "此 Windows 设备未批准控制端请求。"
    } else if reason.contains("occupied") || reason.contains("already in use") {
        "此中继会话正在使用中，恢复可用后 DeskLink 将重试。"
    } else if reason.contains("capture") || reason.contains("desktop duplication") {
        "DeskLink 无法捕获 Windows 桌面，请检查当前显示器后重试。"
    } else if reason.contains("encoder") || reason.contains("media foundation") {
        "DeskLink 无法启动 Windows 视频编码器。"
    } else if reason.contains("input injection") || reason.contains("sendinput") {
        "DeskLink 无法启动远程输入控制。"
    } else if reason.contains("timeout") || reason.contains("timed out") {
        "安全中继连接超时，条件允许时 DeskLink 将重试。"
    } else if reason.contains("transport")
        || reason.contains("connection")
        || reason.contains("relay")
        || reason.contains("closed")
    {
        "安全中继连接已关闭，条件允许时 DeskLink 将重试。"
    } else {
        "DeskLink 因主机内部错误而停止，请查看本地诊断日志了解详情。"
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OperationFeedbackTone {
    Neutral,
    Success,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayMenuAction {
    Open,
    Configure,
    ManageTrust,
    Exit,
}

pub fn tray_menu_action(command_id: usize) -> Option<TrayMenuAction> {
    match command_id {
        4001 => Some(TrayMenuAction::Open),
        4002 => Some(TrayMenuAction::Configure),
        4003 => Some(TrayMenuAction::ManageTrust),
        4004 => Some(TrayMenuAction::Exit),
        _ => None,
    }
}

#[cfg(windows)]
mod windows_ui {
    use std::{
        ffi::c_void,
        mem::size_of,
        sync::mpsc::{self, Receiver, Sender, SyncSender},
        thread,
    };

    use thiserror::Error;
    use tokio::sync::watch;
    use windows::{
        Win32::{
            Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
            Graphics::Gdi::{
                CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, COLOR_WINDOW, CreateFontW, DEFAULT_CHARSET,
                DEFAULT_PITCH, DeleteObject, GetSysColorBrush, HDC, HFONT, HGDIOBJ,
                OUT_DEFAULT_PRECIS, SetBkMode, SetTextColor, TRANSPARENT, UpdateWindow,
            },
            System::LibraryLoader::GetModuleHandleW,
            UI::{
                HiDpi::{
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
                    SetProcessDpiAwarenessContext,
                },
                Input::KeyboardAndMouse::EnableWindow,
                Shell::{
                    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY,
                    NOTIFYICONDATAW, Shell_NotifyIconW,
                },
                WindowsAndMessaging::{
                    AppendMenuW, CW_USEDEFAULT, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
                    DestroyMenu, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect,
                    GetCursorPos, GetMessageW, GetWindowLongPtrW, HICON, HMENU, IDC_ARROW,
                    IDI_APPLICATION, LB_ADDSTRING, LB_ERR, LB_GETCURSEL, LB_RESETCONTENT,
                    LBN_SELCHANGE, LBS_NOTIFY, LoadCursorW, LoadIconW, MENU_ITEM_FLAGS,
                    MF_SEPARATOR, MF_STRING, MINMAXINFO, MSG, MoveWindow, PostMessageW,
                    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SW_HIDE, SW_RESTORE,
                    SW_SHOW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowTextW,
                    ShowWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TrackPopupMenu, TranslateMessage,
                    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_CREATE,
                    WM_CTLCOLORSTATIC, WM_DESTROY, WM_GETMINMAXINFO, WM_LBUTTONDBLCLK, WM_NCCREATE,
                    WM_NCDESTROY, WM_RBUTTONUP, WM_SETFONT, WM_SIZE, WNDCLASSW, WS_BORDER,
                    WS_CHILD, WS_CLIPCHILDREN, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
                    WS_VSCROLL,
                },
            },
        },
        core::{Error as WindowsError, PCWSTR, w},
    };

    use crate::{
        diagnostics::{DiagnosticEvent, DiagnosticLog, DiagnosticOperation},
        runtime::HostLifecycleEvent,
        tray::{HostStatusViewModel, OperationFeedbackTone, TrayMenuAction, tray_menu_action},
        trusted::{TrustedController, WindowsTrustedControllerStore},
        window::WindowsLocalApprovalDialog,
    };

    const WINDOW_CLASS: &str = "DeskLinkHostStatusWindow";
    const WINDOW_TITLE: &str = "DeskLink";
    const APP_ICON_RESOURCE_ID: usize = 101;
    const TRAY_ICON_ID: u32 = 1;
    const WM_TRAY_ICON: u32 = WM_APP + 1;
    const WM_TRAY_COMMAND: u32 = WM_APP + 2;

    const MENU_OPEN: usize = 4001;
    const MENU_CONFIGURE: usize = 4002;
    const MENU_MANAGE_TRUST: usize = 4003;
    const MENU_EXIT: usize = 4004;

    const CONTROL_STATUS_TITLE: usize = 5001;
    const CONTROL_STATUS_DETAIL: usize = 5002;
    const CONTROL_TRUST_HEADING: usize = 5003;
    const CONTROL_TRUST_LIST: usize = 5004;
    const CONTROL_TRUST_DETAIL: usize = 5005;
    const CONTROL_REFRESH: usize = 5006;
    const CONTROL_REVOKE: usize = 5007;
    const CONTROL_EXIT: usize = 5008;
    const CONTROL_OPERATION_FEEDBACK: usize = 5009;

    #[derive(Debug, Error)]
    pub enum WindowsTrayError {
        #[error("Windows 托盘操作失败：{0}")]
        Platform(#[from] WindowsError),
        #[error("Windows 托盘线程启动失败：{0}")]
        Thread(#[from] std::io::Error),
        #[error("Windows 托盘启动失败：{0}")]
        Startup(String),
        #[error("Windows 托盘命令通道已关闭")]
        Closed,
    }

    enum TrayCommand {
        Status(HostLifecycleEvent),
        Show,
        ManageTrust,
        Shutdown,
    }

    #[derive(Clone)]
    pub struct WindowsTrayHandle {
        sender: Sender<TrayCommand>,
        hwnd: isize,
    }

    impl WindowsTrayHandle {
        pub fn publish(&self, event: HostLifecycleEvent) {
            let _ = self.send(TrayCommand::Status(event));
        }

        pub fn show(&self) {
            let _ = self.send(TrayCommand::Show);
        }

        pub fn manage_trust(&self) {
            let _ = self.send(TrayCommand::ManageTrust);
        }

        fn shutdown(&self) {
            let _ = self.send(TrayCommand::Shutdown);
        }

        fn send(&self, command: TrayCommand) -> Result<(), WindowsTrayError> {
            self.sender
                .send(command)
                .map_err(|_| WindowsTrayError::Closed)?;
            let hwnd = HWND(self.hwnd as *mut c_void);
            unsafe { PostMessageW(Some(hwnd), WM_TRAY_COMMAND, WPARAM(0), LPARAM(0))? };
            Ok(())
        }
    }

    pub struct WindowsTrayApplication {
        handle: WindowsTrayHandle,
        exit_receiver: watch::Receiver<bool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl WindowsTrayApplication {
        pub fn start(store: WindowsTrustedControllerStore) -> Result<Self, WindowsTrayError> {
            Self::start_inner(store, None)
        }

        pub fn start_with_diagnostics(
            store: WindowsTrustedControllerStore,
            diagnostics: DiagnosticLog,
        ) -> Result<Self, WindowsTrayError> {
            Self::start_inner(store, Some(diagnostics))
        }

        fn start_inner(
            store: WindowsTrustedControllerStore,
            diagnostics: Option<DiagnosticLog>,
        ) -> Result<Self, WindowsTrayError> {
            let (sender, receiver) = mpsc::channel();
            let (exit_sender, exit_receiver) = watch::channel(false);
            let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
            let thread = thread::Builder::new()
                .name("desklink-tray".into())
                .spawn(move || {
                    let result =
                        run_message_loop(store, diagnostics, receiver, exit_sender, &ready_sender);
                    if let Err(error) = result {
                        let _ = ready_sender.send(Err(error.to_string()));
                    }
                })?;
            let hwnd = ready_receiver
                .recv()
                .map_err(|_| WindowsTrayError::Closed)?
                .map_err(WindowsTrayError::Startup)?;
            Ok(Self {
                handle: WindowsTrayHandle { sender, hwnd },
                exit_receiver,
                thread: Some(thread),
            })
        }

        pub fn handle(&self) -> WindowsTrayHandle {
            self.handle.clone()
        }

        pub fn exit_receiver(&self) -> watch::Receiver<bool> {
            self.exit_receiver.clone()
        }

        pub fn shutdown(mut self) {
            self.handle.shutdown();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    impl Drop for WindowsTrayApplication {
        fn drop(&mut self) {
            self.handle.shutdown();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    struct WindowState {
        commands: Receiver<TrayCommand>,
        exit_sender: watch::Sender<bool>,
        store: WindowsTrustedControllerStore,
        diagnostics: Option<DiagnosticLog>,
        status: HostStatusViewModel,
        records: Vec<TrustedController>,
        taskbar_created: u32,
        installer_shutdown: u32,
        tray_icon: HICON,
        tray_added: bool,
        status_title: HWND,
        status_detail: HWND,
        trust_heading: HWND,
        trust_list: HWND,
        trust_detail: HWND,
        operation_feedback: HWND,
        refresh_button: HWND,
        revoke_button: HWND,
        exit_button: HWND,
        title_font: HFONT,
        heading_font: HFONT,
        body_font: HFONT,
        operation_feedback_tone: OperationFeedbackTone,
    }

    impl WindowState {
        fn new(
            commands: Receiver<TrayCommand>,
            exit_sender: watch::Sender<bool>,
            store: WindowsTrustedControllerStore,
            diagnostics: Option<DiagnosticLog>,
            taskbar_created: u32,
            installer_shutdown: u32,
            tray_icon: HICON,
        ) -> Self {
            Self {
                commands,
                exit_sender,
                store,
                diagnostics,
                status: HostStatusViewModel::starting(),
                records: Vec::new(),
                taskbar_created,
                installer_shutdown,
                tray_icon,
                tray_added: false,
                status_title: HWND::default(),
                status_detail: HWND::default(),
                trust_heading: HWND::default(),
                trust_list: HWND::default(),
                trust_detail: HWND::default(),
                operation_feedback: HWND::default(),
                refresh_button: HWND::default(),
                revoke_button: HWND::default(),
                exit_button: HWND::default(),
                title_font: HFONT::default(),
                heading_font: HFONT::default(),
                body_font: HFONT::default(),
                operation_feedback_tone: OperationFeedbackTone::Neutral,
            }
        }

        unsafe fn initialize(
            &mut self,
            hwnd: HWND,
            instance: HINSTANCE,
        ) -> Result<(), WindowsError> {
            self.status_title = unsafe {
                create_control(hwnd, instance, w!("STATIC"), "", CONTROL_STATUS_TITLE, 0)?
            };
            self.status_detail = unsafe {
                create_control(hwnd, instance, w!("STATIC"), "", CONTROL_STATUS_DETAIL, 0)?
            };
            self.trust_heading = unsafe {
                create_control(
                    hwnd,
                    instance,
                    w!("STATIC"),
                    "可信控制端",
                    CONTROL_TRUST_HEADING,
                    0,
                )?
            };
            self.trust_list = unsafe {
                create_control(
                    hwnd,
                    instance,
                    w!("LISTBOX"),
                    "",
                    CONTROL_TRUST_LIST,
                    WS_TABSTOP.0 | WS_VSCROLL.0 | WS_BORDER.0 | LBS_NOTIFY as u32,
                )?
            };
            self.trust_detail = unsafe {
                create_control(hwnd, instance, w!("STATIC"), "", CONTROL_TRUST_DETAIL, 0)?
            };
            self.operation_feedback = unsafe {
                create_control(
                    hwnd,
                    instance,
                    w!("STATIC"),
                    "",
                    CONTROL_OPERATION_FEEDBACK,
                    0,
                )?
            };
            self.refresh_button = unsafe {
                create_control(
                    hwnd,
                    instance,
                    w!("BUTTON"),
                    "刷新列表",
                    CONTROL_REFRESH,
                    WS_TABSTOP.0,
                )?
            };
            self.revoke_button = unsafe {
                create_control(
                    hwnd,
                    instance,
                    w!("BUTTON"),
                    "撤销控制端",
                    CONTROL_REVOKE,
                    WS_TABSTOP.0,
                )?
            };
            self.exit_button = unsafe {
                create_control(
                    hwnd,
                    instance,
                    w!("BUTTON"),
                    "退出 DeskLink",
                    CONTROL_EXIT,
                    WS_TABSTOP.0,
                )?
            };
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
            self.title_font = create_ui_font(dpi, 22, 600)?;
            self.heading_font = create_ui_font(dpi, 15, 600)?;
            self.body_font = create_ui_font(dpi, 14, 400)?;
            set_control_font(self.status_title, self.title_font);
            set_control_font(self.trust_heading, self.heading_font);
            for control in [
                self.status_detail,
                self.trust_list,
                self.trust_detail,
                self.operation_feedback,
                self.refresh_button,
                self.revoke_button,
                self.exit_button,
            ] {
                unsafe {
                    SendMessageW(
                        control,
                        WM_SETFONT,
                        Some(WPARAM(self.body_font.0 as usize)),
                        Some(LPARAM(1)),
                    )
                };
            }
            self.apply_status(hwnd)?;
            let _ = unsafe { EnableWindow(self.revoke_button, false) };
            if let Err(error) = self.refresh_trusted_controllers() {
                let _ = set_text(self.trust_detail, "可信控制端详情暂时不可用。");
                self.report_operation_failure(
                    DiagnosticOperation::TrustedControllersRefresh,
                    &error,
                    "无法加载可信控制端，远程主机服务仍可使用。",
                );
            }
            unsafe { self.layout(hwnd)? };
            self.add_tray_icon(hwnd)?;
            Ok(())
        }

        fn apply_status(&mut self, hwnd: HWND) -> Result<(), WindowsError> {
            set_text(self.status_title, &self.status.title)?;
            set_text(self.status_detail, &self.status.detail)?;
            if self.tray_added {
                self.modify_tray_tooltip(hwnd)?;
            }
            Ok(())
        }

        fn set_operation_feedback(&mut self, message: &str, tone: OperationFeedbackTone) {
            self.operation_feedback_tone = tone;
            let _ = set_text(self.operation_feedback, message);
        }

        fn record_diagnostic(&self, event: DiagnosticEvent) {
            if let Some(diagnostics) = &self.diagnostics {
                let _ = diagnostics.record(&event);
            }
        }

        fn report_operation_failure(
            &mut self,
            operation: DiagnosticOperation,
            error: &WindowsError,
            message: &str,
        ) {
            self.record_diagnostic(DiagnosticEvent::OperationFailed {
                operation,
                reason: error.to_string(),
            });
            self.set_operation_feedback(message, OperationFeedbackTone::Error);
        }

        fn refresh_with_feedback(&mut self) {
            match self.refresh_trusted_controllers() {
                Ok(()) => {
                    self.record_diagnostic(DiagnosticEvent::OperationSucceeded(
                        DiagnosticOperation::TrustedControllersRefresh,
                    ));
                    self.set_operation_feedback(
                        "可信控制端列表已刷新。",
                        OperationFeedbackTone::Success,
                    );
                }
                Err(error) => self.report_operation_failure(
                    DiagnosticOperation::TrustedControllersRefresh,
                    &error,
                    "无法刷新可信控制端，远程主机服务仍可使用。",
                ),
            }
        }

        fn revoke_with_feedback(&mut self) {
            match self.revoke_selected_controller() {
                Ok(false) => {}
                Ok(true) => {
                    self.record_diagnostic(DiagnosticEvent::OperationSucceeded(
                        DiagnosticOperation::ControllerRevocation,
                    ));
                    match self.refresh_trusted_controllers() {
                        Ok(()) => self.set_operation_feedback(
                            "控制端权限已撤销，重新连接前必须再次配对。",
                            OperationFeedbackTone::Success,
                        ),
                        Err(error) => self.report_operation_failure(
                            DiagnosticOperation::TrustedControllersRefresh,
                            &error,
                            "控制端权限已撤销，但无法刷新列表，请重新打开此窗口。",
                        ),
                    }
                }
                Err(error) => self.report_operation_failure(
                    DiagnosticOperation::ControllerRevocation,
                    &error,
                    "无法撤销控制端，其信任状态未改变，请重试。",
                ),
            }
        }

        fn refresh_trusted_controllers(&mut self) -> Result<(), WindowsError> {
            self.records = self.store.list().map_err(|error| {
                WindowsError::new(
                    windows::core::HRESULT(0x8000_4005_u32 as i32),
                    error.to_string(),
                )
            })?;
            unsafe {
                SendMessageW(self.trust_list, LB_RESETCONTENT, None, None);
            }
            for record in &self.records {
                let label = format!(
                    "设备 {}  |  批准时间（Unix）{}",
                    grouped_hex(&record.device_id()),
                    record.approved_at_unix_s()
                );
                let label = wide(&label);
                unsafe {
                    SendMessageW(
                        self.trust_list,
                        LB_ADDSTRING,
                        None,
                        Some(LPARAM(label.as_ptr() as isize)),
                    );
                }
            }
            if self.records.is_empty() {
                set_text(
                    self.trust_detail,
                    "当前没有可信控制端。控制端通过身份验证并在本机获批后才会出现在这里。",
                )?;
            } else {
                set_text(
                    self.trust_detail,
                    "请选择一个控制端，在撤销前检查其完整公开身份。",
                )?;
            }
            let _ = unsafe { EnableWindow(self.revoke_button, false) };
            Ok(())
        }

        fn update_selected_controller(&self) -> Result<(), WindowsError> {
            let Some(record) = self.selected_controller() else {
                set_text(
                    self.trust_detail,
                    "请选择一个控制端，在撤销前检查其完整公开身份。",
                )?;
                let _ = unsafe { EnableWindow(self.revoke_button, false) };
                return Ok(());
            };
            let detail = format!(
                "设备 ID：\r\n{}\r\n\r\nEd25519 公钥指纹：\r\n{}",
                grouped_hex(&record.device_id()),
                grouped_hex(record.verify_key().as_bytes())
            );
            set_text(self.trust_detail, &detail)?;
            let _ = unsafe { EnableWindow(self.revoke_button, true) };
            Ok(())
        }

        fn selected_controller(&self) -> Option<TrustedController> {
            let selected = unsafe { SendMessageW(self.trust_list, LB_GETCURSEL, None, None) }.0;
            if selected == LB_ERR as isize {
                return None;
            }
            self.records.get(usize::try_from(selected).ok()?).copied()
        }

        fn revoke_selected_controller(&mut self) -> Result<bool, WindowsError> {
            let Some(record) = self.selected_controller() else {
                return Ok(false);
            };
            if !WindowsLocalApprovalDialog::confirm_revocation(
                record.device_id(),
                record.verify_key(),
            ) {
                return Ok(false);
            }
            self.store.revoke(record.fingerprint()).map_err(|error| {
                WindowsError::new(
                    windows::core::HRESULT(0x8000_4005_u32 as i32),
                    error.to_string(),
                )
            })
        }

        unsafe fn layout(&self, hwnd: HWND) -> Result<(), WindowsError> {
            let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
            let scale = |value: i32| value.saturating_mul(dpi as i32) / 96;
            let mut client = RECT::default();
            unsafe { GetClientRect(hwnd, &mut client)? };
            let width = client.right - client.left;
            let height = client.bottom - client.top;
            let padding = scale(28);
            let content_width = (width - padding * 2).max(scale(200));
            unsafe {
                MoveWindow(
                    self.status_title,
                    padding,
                    padding,
                    content_width,
                    scale(38),
                    true,
                )?;
                MoveWindow(
                    self.status_detail,
                    padding,
                    padding + scale(44),
                    content_width,
                    scale(58),
                    true,
                )?;
                MoveWindow(
                    self.trust_heading,
                    padding,
                    padding + scale(120),
                    content_width,
                    scale(28),
                    true,
                )?;
                MoveWindow(
                    self.trust_list,
                    padding,
                    padding + scale(154),
                    content_width,
                    (height - scale(390)).max(scale(100)),
                    true,
                )?;
                MoveWindow(
                    self.trust_detail,
                    padding,
                    height - scale(200),
                    content_width,
                    scale(84),
                    true,
                )?;
                MoveWindow(
                    self.operation_feedback,
                    padding,
                    height - scale(106),
                    content_width,
                    scale(42),
                    true,
                )?;
                MoveWindow(
                    self.refresh_button,
                    padding,
                    height - scale(56),
                    scale(116),
                    scale(36),
                    true,
                )?;
                MoveWindow(
                    self.revoke_button,
                    padding + scale(126),
                    height - scale(56),
                    scale(152),
                    scale(36),
                    true,
                )?;
                MoveWindow(
                    self.exit_button,
                    width - padding - scale(124),
                    height - scale(56),
                    scale(124),
                    scale(36),
                    true,
                )?;
            }
            Ok(())
        }

        fn add_tray_icon(&mut self, hwnd: HWND) -> Result<(), WindowsError> {
            let mut data = self.notification_data(hwnd);
            data.szTip = wide_array(&self.status.tooltip);
            if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
                return Err(WindowsError::from_win32());
            }
            self.tray_added = true;
            Ok(())
        }

        fn modify_tray_tooltip(&self, hwnd: HWND) -> Result<(), WindowsError> {
            let mut data = self.notification_data(hwnd);
            data.szTip = wide_array(&self.status.tooltip);
            if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
                return Err(WindowsError::from_win32());
            }
            Ok(())
        }

        fn delete_tray_icon(&mut self, hwnd: HWND) {
            if !self.tray_added {
                return;
            }
            let data = self.notification_data(hwnd);
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            }
            self.tray_added = false;
        }

        fn notification_data(&self, hwnd: HWND) -> NOTIFYICONDATAW {
            NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_ICON_ID,
                uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
                uCallbackMessage: WM_TRAY_ICON,
                hIcon: self.tray_icon,
                ..NOTIFYICONDATAW::default()
            }
        }

        fn drain_commands(&mut self, hwnd: HWND) -> Result<(), WindowsError> {
            while let Ok(command) = self.commands.try_recv() {
                match command {
                    TrayCommand::Status(event) => {
                        self.status.apply(&event);
                        self.apply_status(hwnd)?;
                    }
                    TrayCommand::Show => unsafe { show_window(hwnd) },
                    TrayCommand::ManageTrust => {
                        self.refresh_with_feedback();
                        unsafe { show_window(hwnd) };
                    }
                    TrayCommand::Shutdown => unsafe {
                        DestroyWindow(hwnd)?;
                        return Ok(());
                    },
                }
            }
            Ok(())
        }
    }

    impl Drop for WindowState {
        fn drop(&mut self) {
            for font in [self.title_font, self.heading_font, self.body_font] {
                if !font.0.is_null() {
                    unsafe {
                        let _ = DeleteObject(HGDIOBJ(font.0));
                    }
                }
            }
        }
    }

    fn run_message_loop(
        store: WindowsTrustedControllerStore,
        diagnostics: Option<DiagnosticLog>,
        commands: Receiver<TrayCommand>,
        exit_sender: watch::Sender<bool>,
        ready: &SyncSender<Result<isize, String>>,
    ) -> Result<(), WindowsTrayError> {
        unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        let module = unsafe { GetModuleHandleW(None)? };
        let instance = HINSTANCE(module.0);
        let class_name = wide(WINDOW_CLASS);
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW)? };
        let tray_icon = load_app_icon(instance)?;
        let class = WNDCLASSW {
            hCursor: cursor,
            hIcon: tray_icon,
            hInstance: instance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            lpfnWndProc: Some(window_proc),
            hbrBackground: unsafe { GetSysColorBrush(COLOR_WINDOW) },
            ..WNDCLASSW::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(WindowsError::from_win32().into());
        }
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        let installer_shutdown = unsafe { RegisterWindowMessageW(w!("DeskLinkInstallerShutdown")) };
        let state = Box::new(WindowState::new(
            commands,
            exit_sender,
            store,
            diagnostics,
            taskbar_created,
            installer_shutdown,
            tray_icon,
        ));
        let state = Box::into_raw(state);
        let title = wide(WINDOW_TITLE);
        let hwnd = match unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                760,
                620,
                None,
                None,
                Some(instance),
                Some(state.cast()),
            )
        } {
            Ok(hwnd) => hwnd,
            Err(error) => {
                unsafe { drop(Box::from_raw(state)) };
                return Err(error.into());
            }
        };
        let initialization = unsafe { (&mut *state).initialize(hwnd, instance) };
        if let Err(error) = initialization {
            unsafe { DestroyWindow(hwnd)? };
            return Err(error.into());
        }
        ready
            .send(Ok(hwnd.0 as isize))
            .map_err(|_| WindowsTrayError::Closed)?;
        let mut message = MSG::default();
        loop {
            let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
            if result.0 == -1 {
                return Err(WindowsError::from_win32().into());
            }
            if !result.as_bool() {
                break;
            }
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_NCCREATE {
            let create = lparam.0 as *const windows::Win32::UI::WindowsAndMessaging::CREATESTRUCTW;
            if !create.is_null() {
                let state = unsafe { (*create).lpCreateParams.cast::<WindowState>() };
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize) };
            }
        }
        let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState };
        if !state.is_null() {
            let state = unsafe { &mut *state };
            if message == state.taskbar_created {
                state.tray_added = false;
                let _ = state.add_tray_icon(hwnd);
                return LRESULT(0);
            }
            if message == state.installer_shutdown {
                let _ = state.exit_sender.send(true);
                let _ = unsafe { DestroyWindow(hwnd) };
                return LRESULT(0);
            }
            match message {
                WM_CREATE => return LRESULT(0),
                WM_TRAY_COMMAND => {
                    let _ = state.drain_commands(hwnd);
                    return LRESULT(0);
                }
                WM_TRAY_ICON => {
                    match lparam.0 as u32 {
                        WM_LBUTTONDBLCLK => unsafe { show_window(hwnd) },
                        WM_RBUTTONUP => unsafe { show_tray_menu(hwnd) },
                        _ => {}
                    }
                    return LRESULT(0);
                }
                WM_COMMAND => {
                    let identifier = wparam.0 & 0xffff;
                    let notification = ((wparam.0 >> 16) & 0xffff) as u32;
                    if let Some(action) = tray_menu_action(identifier) {
                        match action {
                            TrayMenuAction::Open => unsafe { show_window(hwnd) },
                            TrayMenuAction::Configure => match launch_connection_settings() {
                                Ok(()) => state.set_operation_feedback(
                                    "连接设置已在单独窗口中打开。",
                                    OperationFeedbackTone::Neutral,
                                ),
                                Err(_) => {
                                    state.set_operation_feedback(
                                        "无法打开连接设置，请从“开始”菜单重试。",
                                        OperationFeedbackTone::Error,
                                    );
                                    unsafe { show_window(hwnd) };
                                }
                            },
                            TrayMenuAction::ManageTrust => {
                                state.refresh_with_feedback();
                                unsafe { show_window(hwnd) };
                            }
                            TrayMenuAction::Exit => {
                                let _ = state.exit_sender.send(true);
                                let _ = unsafe { DestroyWindow(hwnd) };
                            }
                        }
                        return LRESULT(0);
                    }
                    match identifier {
                        CONTROL_TRUST_LIST if notification == LBN_SELCHANGE => {
                            let _ = state.update_selected_controller();
                        }
                        CONTROL_REFRESH => {
                            state.refresh_with_feedback();
                        }
                        CONTROL_REVOKE => {
                            state.revoke_with_feedback();
                        }
                        CONTROL_EXIT => {
                            let _ = state.exit_sender.send(true);
                            let _ = unsafe { DestroyWindow(hwnd) };
                        }
                        _ => {}
                    }
                    return LRESULT(0);
                }
                WM_CTLCOLORSTATIC => {
                    let device_context = HDC(wparam.0 as *mut c_void);
                    let control = HWND(lparam.0 as *mut c_void);
                    unsafe {
                        let _ = SetBkMode(device_context, TRANSPARENT);
                        let color = if control == state.operation_feedback {
                            match state.operation_feedback_tone {
                                OperationFeedbackTone::Neutral => COLORREF(0x0078_6052),
                                OperationFeedbackTone::Success => COLORREF(0x003C_7A28),
                                OperationFeedbackTone::Error => COLORREF(0x0017_29A4),
                            }
                        } else if control == state.status_detail || control == state.trust_detail {
                            COLORREF(0x0078_6052)
                        } else {
                            COLORREF(0x004A_3222)
                        };
                        let _ = SetTextColor(device_context, color);
                    }
                    return LRESULT(unsafe { GetSysColorBrush(COLOR_WINDOW) }.0 as isize);
                }
                WM_GETMINMAXINFO => {
                    let limits = lparam.0 as *mut MINMAXINFO;
                    if !limits.is_null() {
                        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as i32;
                        let scale = |value: i32| value.saturating_mul(dpi) / 96;
                        unsafe {
                            (*limits).ptMinTrackSize.x = scale(680);
                            (*limits).ptMinTrackSize.y = scale(600);
                        }
                    }
                    return LRESULT(0);
                }
                WM_SIZE => {
                    let _ = unsafe { state.layout(hwnd) };
                    return LRESULT(0);
                }
                WM_CLOSE => {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                    return LRESULT(0);
                }
                WM_DESTROY => {
                    state.delete_tray_icon(hwnd);
                    let _ = state.exit_sender.send(true);
                    unsafe { PostQuitMessage(0) };
                    return LRESULT(0);
                }
                WM_NCDESTROY => {
                    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
                    unsafe { drop(Box::from_raw(state)) };
                    return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
                }
                _ => {}
            }
        }
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }

    unsafe fn create_control(
        parent: HWND,
        instance: HINSTANCE,
        class_name: PCWSTR,
        text: &str,
        identifier: usize,
        additional_style: u32,
    ) -> Result<HWND, WindowsError> {
        let text = wide(text);
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                PCWSTR(text.as_ptr()),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | additional_style),
                0,
                0,
                0,
                0,
                Some(parent),
                Some(HMENU(identifier as *mut c_void)),
                Some(instance),
                None,
            )
        }
    }

    fn load_app_icon(instance: HINSTANCE) -> Result<HICON, WindowsError> {
        let resource = PCWSTR(APP_ICON_RESOURCE_ID as *const u16);
        unsafe { LoadIconW(Some(instance), resource) }
            .or_else(|_| unsafe { LoadIconW(None, IDI_APPLICATION) })
    }

    fn create_ui_font(dpi: u32, point_size: i32, weight: i32) -> Result<HFONT, WindowsError> {
        let pixel_height = -point_size.saturating_mul(dpi as i32) / 72;
        let face_name = wide("Segoe UI");
        let font = unsafe {
            CreateFontW(
                pixel_height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                DEFAULT_PITCH.0.into(),
                PCWSTR(face_name.as_ptr()),
            )
        };
        if font.0.is_null() {
            return Err(WindowsError::from_win32());
        }
        Ok(font)
    }

    fn set_control_font(control: HWND, font: HFONT) {
        unsafe {
            SendMessageW(
                control,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            )
        };
    }

    unsafe fn show_window(hwnd: HWND) {
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = UpdateWindow(hwnd);
        }
    }

    unsafe fn show_tray_menu(hwnd: HWND) {
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            return;
        };
        let result = unsafe {
            AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("打开 DeskLink"))
                .and_then(|_| AppendMenuW(menu, MF_STRING, MENU_CONFIGURE, w!("连接设置...")))
                .and_then(|_| AppendMenuW(menu, MF_STRING, MENU_MANAGE_TRUST, w!("管理可信控制端")))
                .and_then(|_| AppendMenuW(menu, MENU_ITEM_FLAGS(MF_SEPARATOR.0), 0, PCWSTR::null()))
                .and_then(|_| AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("退出 DeskLink")))
        };
        if result.is_ok() {
            let mut point = POINT::default();
            if unsafe { GetCursorPos(&mut point) }.is_ok() {
                unsafe {
                    let _ = SetForegroundWindow(hwnd);
                    let _ = TrackPopupMenu(
                        menu,
                        TPM_LEFTALIGN | TPM_BOTTOMALIGN,
                        point.x,
                        point.y,
                        None,
                        hwnd,
                        None,
                    );
                }
            }
        }
        let _ = unsafe { DestroyMenu(menu) };
    }

    fn launch_connection_settings() -> std::io::Result<()> {
        let executable = std::env::current_exe()?;
        let mut command = std::process::Command::new(executable);
        command.arg("--configure");
        for variable in [
            "DESKLINK_RELAY_ADDR",
            "DESKLINK_RELAY_SERVER_NAME",
            "DESKLINK_STREAM_ID",
            "DESKLINK_SESSION_ID",
            "DESKLINK_AUTH_KEY",
            "DESKLINK_PAIRING_MODE",
            "DESKLINK_PEER_VERIFY_KEY",
            "DESKLINK_APPROVE_SESSION",
        ] {
            command.env_remove(variable);
        }
        command.spawn()?;
        Ok(())
    }

    fn set_text(hwnd: HWND, value: &str) -> Result<(), WindowsError> {
        let value = wide(value);
        unsafe { SetWindowTextW(hwnd, PCWSTR(value.as_ptr())) }
    }

    fn grouped_hex(bytes: &[u8]) -> String {
        let mut output = String::with_capacity(bytes.len().saturating_mul(3).saturating_sub(1));
        for (index, byte) in bytes.iter().enumerate() {
            if index != 0 {
                output.push(':');
            }
            use std::fmt::Write as _;
            write!(&mut output, "{byte:02X}").expect("writing to String cannot fail");
        }
        output
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn wide_array<const N: usize>(value: &str) -> [u16; N] {
        let mut output = [0; N];
        for (destination, source) in output
            .iter_mut()
            .take(N.saturating_sub(1))
            .zip(value.encode_utf16())
        {
            *destination = source;
        }
        output
    }
}

#[cfg(windows)]
pub use windows_ui::{WindowsTrayApplication, WindowsTrayError, WindowsTrayHandle};

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn status_model_distinguishes_active_recovery_and_stopped_states() {
        let mut model = HostStatusViewModel::starting();
        model.apply(&HostLifecycleEvent::Connected { stream_id: 7 });
        assert_eq!(model.title, "远程控制已连接");
        assert!(model.detail.contains("视频流 7"));

        model.apply(&HostLifecycleEvent::Reconnecting {
            retry: 2,
            maximum_retries: 6,
            delay: Duration::from_millis(500),
            reason: "relay unavailable".to_owned(),
        });
        assert_eq!(model.title, "连接已中断");
        assert!(model.detail.contains("第 2/6 次重试"));
        assert!(model.detail.contains("500 毫秒"));

        model.apply(&HostLifecycleEvent::Stopped {
            reason: "authentication rejected".to_owned(),
        });
        assert_eq!(model.title, "主机服务已停止");
        assert_eq!(model.tooltip, "DeskLink：主机服务已停止");
    }

    #[test]
    fn status_model_replaces_internal_errors_with_safe_recovery_copy() {
        let mut model = HostStatusViewModel::starting();
        model.apply(&HostLifecycleEvent::Reconnecting {
            retry: 1,
            maximum_retries: 6,
            delay: Duration::from_millis(250),
            reason: "transport error DESKLINK_AUTH_KEY=00112233445566778899aabbccddeeff".to_owned(),
        });
        assert!(model.detail.contains("安全中继连接"));
        assert!(!model.detail.contains("DESKLINK_AUTH_KEY"));
        assert!(!model.detail.contains("00112233"));

        model.apply(&HostLifecycleEvent::Stopped {
            reason: "capture error: AccessLost".to_owned(),
        });
        assert!(model.detail.contains("捕获 Windows 桌面"));
        assert!(!model.detail.contains("AccessLost"));
    }

    #[test]
    fn status_model_explains_unconfigured_installed_startup() {
        let mut model = HostStatusViewModel::starting();
        model.apply(&HostLifecycleEvent::Stopped {
            reason: "hosting configuration is missing".to_owned(),
        });
        assert!(model.detail.contains("尚未配置主机连接"));
        assert!(!model.detail.contains("internal host error"));
    }

    #[test]
    fn tray_menu_routes_only_explicit_commands() {
        assert_eq!(tray_menu_action(4001), Some(TrayMenuAction::Open));
        assert_eq!(tray_menu_action(4002), Some(TrayMenuAction::Configure));
        assert_eq!(tray_menu_action(4003), Some(TrayMenuAction::ManageTrust));
        assert_eq!(tray_menu_action(4004), Some(TrayMenuAction::Exit));
        assert_eq!(tray_menu_action(0), None);
        assert_eq!(tray_menu_action(5007), None);
    }
}
