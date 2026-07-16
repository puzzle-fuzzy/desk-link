use std::fmt::Write;

use desklink_crypto::{PairingError, PairingInvite, PeerIdentity, SessionId};
use ed25519_dalek::VerifyingKey;
use thiserror::Error;

#[cfg(windows)]
use windows::{
    Win32::UI::WindowsAndMessaging::{
        IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_YESNO, MESSAGEBOX_STYLE,
        MessageBoxW,
    },
    core::PCWSTR,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalState {
    Waiting,
    Accepted,
    Rejected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingController {
    session_id: SessionId,
    identity: PeerIdentity,
    expires_at_unix_s: u64,
}

impl PendingController {
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub const fn identity(&self) -> PeerIdentity {
        self.identity
    }

    pub const fn expires_at_unix_s(&self) -> u64 {
        self.expires_at_unix_s
    }

    /// Full, non-secret Ed25519 public-key fingerprint for local confirmation.
    pub fn verification_fingerprint(&self) -> String {
        let key = self.identity.verify_key().to_bytes();
        let mut fingerprint = String::with_capacity(key.len() * 3 - 1);
        for (index, byte) in key.iter().enumerate() {
            if index != 0 {
                fingerprint.push(':');
            }
            write!(&mut fingerprint, "{byte:02X}").expect("writing to String cannot fail");
        }
        fingerprint
    }
}

fn grouped_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 3 - 1);
    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 {
            output.push(':');
        }
        write!(&mut output, "{byte:02X}").expect("writing to String cannot fail");
    }
    output
}

pub fn controller_approval_prompt(pending: PendingController) -> String {
    format!(
        "DeskLink 收到一项已通过身份验证的远程控制请求。\n\n\
         设备 ID：\n{}\n\n\
         Ed25519 公钥指纹：\n{}\n\n\
         会话 ID：\n{}\n\n\
         请求过期时间（Unix）：{}。\n\n\
         仅当你确认这是自己的控制端时才批准。批准后，对方将获得屏幕查看和输入控制权限。",
        grouped_hex(&pending.identity().device_id()),
        pending.verification_fingerprint(),
        grouped_hex(pending.session_id().as_bytes()),
        pending.expires_at_unix_s(),
    )
}

pub fn controller_revocation_prompt(device_id: [u8; 16], verify_key: VerifyingKey) -> String {
    format!(
        "要撤销此可信 DeskLink 控制端吗？\n\n\
         设备 ID：\n{}\n\n\
         Ed25519 公钥指纹：\n{}\n\n\
         撤销将在下次连接时生效。此控制端必须重新配对并获得批准，才能再次访问。",
        grouped_hex(&device_id),
        grouped_hex(verify_key.as_bytes()),
    )
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsLocalApprovalDialog;

#[cfg(windows)]
impl WindowsLocalApprovalDialog {
    pub fn confirm(pending: PendingController) -> bool {
        let message = controller_approval_prompt(pending)
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let title = "DeskLink 控制端批准"
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                default_reject_warning_style(),
            ) == IDYES
        }
    }

    pub fn confirm_revocation(device_id: [u8; 16], verify_key: VerifyingKey) -> bool {
        let message = controller_revocation_prompt(device_id, verify_key)
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let title = "DeskLink 可信控制端"
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                default_reject_warning_style(),
            ) == IDYES
        }
    }
}

#[cfg(windows)]
fn default_reject_warning_style() -> MESSAGEBOX_STYLE {
    MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND
}

/// Proof that the exact authenticated controller shown locally was approved.
/// Its fields are private so persistence code cannot construct trust from an
/// arbitrary, unapproved public key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApprovedController {
    identity: PeerIdentity,
    approved_at_unix_s: u64,
}

impl ApprovedController {
    pub const fn device_id(&self) -> [u8; 16] {
        self.identity.device_id()
    }

    pub fn verify_key(&self) -> VerifyingKey {
        self.identity.verify_key()
    }

    pub const fn approved_at_unix_s(&self) -> u64 {
        self.approved_at_unix_s
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PairingApprovalError {
    #[error("配对邀请当前未生效：{0}")]
    Pairing(#[from] PairingError),
    #[error("已有批准请求正在处理或该请求已处理")]
    InvalidState,
    #[error("没有等待批准且已通过身份验证的控制端")]
    NoPendingController,
    #[error("显示批准提示后，已验证的控制端发生了变化")]
    ControllerChanged,
    #[error("批准请求已过期")]
    Expired,
}

/// Binds a one-time invitation to the controller authenticated by Noise, then
/// produces a persistable trust candidate only after an identity-matched local
/// approval. Beginning approval consumes the invitation whether the user later
/// accepts or rejects it.
pub struct PairingApprovalGate {
    state: ApprovalState,
    pending: Option<PendingController>,
}

impl PairingApprovalGate {
    pub const fn new() -> Self {
        Self {
            state: ApprovalState::Waiting,
            pending: None,
        }
    }

    pub fn begin(
        &mut self,
        invite: &mut PairingInvite,
        identity: PeerIdentity,
        now_unix_s: u64,
    ) -> Result<PendingController, PairingApprovalError> {
        if self.state != ApprovalState::Waiting || self.pending.is_some() {
            return Err(PairingApprovalError::InvalidState);
        }
        invite.consume(now_unix_s)?;
        let pending = PendingController {
            session_id: invite.session_id(),
            identity,
            expires_at_unix_s: invite.expires_at_unix_s(),
        };
        self.pending = Some(pending);
        Ok(pending)
    }

    pub fn approve(
        &mut self,
        displayed_identity: PeerIdentity,
        now_unix_s: u64,
    ) -> Result<ApprovedController, PairingApprovalError> {
        if self.state != ApprovalState::Waiting {
            return Err(PairingApprovalError::InvalidState);
        }
        let pending = self
            .pending
            .as_ref()
            .ok_or(PairingApprovalError::NoPendingController)?;
        if now_unix_s >= pending.expires_at_unix_s {
            self.pending = None;
            self.state = ApprovalState::Rejected;
            return Err(PairingApprovalError::Expired);
        }
        if pending.identity != displayed_identity {
            return Err(PairingApprovalError::ControllerChanged);
        }
        let pending = self
            .pending
            .take()
            .ok_or(PairingApprovalError::NoPendingController)?;
        self.state = ApprovalState::Accepted;
        Ok(ApprovedController {
            identity: pending.identity,
            approved_at_unix_s: now_unix_s,
        })
    }

    pub fn reject(&mut self, displayed_identity: PeerIdentity) -> Result<(), PairingApprovalError> {
        if self.state != ApprovalState::Waiting {
            return Err(PairingApprovalError::InvalidState);
        }
        let pending = self
            .pending
            .as_ref()
            .ok_or(PairingApprovalError::NoPendingController)?;
        if pending.identity != displayed_identity {
            return Err(PairingApprovalError::ControllerChanged);
        }
        self.pending = None;
        self.state = ApprovalState::Rejected;
        Ok(())
    }

    pub const fn pending(&self) -> Option<PendingController> {
        self.pending
    }

    pub const fn state(&self) -> ApprovalState {
        self.state
    }
}

impl Default for PairingApprovalGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Serializes approval requests for fixed-password access. Unlike a one-time
/// invitation, this gate returns to its empty state after every decision.
pub struct PersistentApprovalGate {
    session_id: SessionId,
    pending: Option<PendingController>,
}

impl PersistentApprovalGate {
    pub const fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            pending: None,
        }
    }

    pub fn begin(
        &mut self,
        identity: PeerIdentity,
        now_unix_s: u64,
        ttl_s: u64,
    ) -> Result<PendingController, PairingApprovalError> {
        if self.pending.is_some() || ttl_s == 0 {
            return Err(PairingApprovalError::InvalidState);
        }
        let pending = PendingController {
            session_id: self.session_id,
            identity,
            expires_at_unix_s: now_unix_s.saturating_add(ttl_s),
        };
        self.pending = Some(pending);
        Ok(pending)
    }

    pub fn approve(
        &mut self,
        displayed_identity: PeerIdentity,
        now_unix_s: u64,
    ) -> Result<ApprovedController, PairingApprovalError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or(PairingApprovalError::NoPendingController)?;
        if now_unix_s >= pending.expires_at_unix_s {
            self.pending = None;
            return Err(PairingApprovalError::Expired);
        }
        if pending.identity != displayed_identity {
            return Err(PairingApprovalError::ControllerChanged);
        }
        let pending = self
            .pending
            .take()
            .ok_or(PairingApprovalError::NoPendingController)?;
        Ok(ApprovedController {
            identity: pending.identity,
            approved_at_unix_s: now_unix_s,
        })
    }

    pub fn reject(&mut self, displayed_identity: PeerIdentity) -> Result<(), PairingApprovalError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or(PairingApprovalError::NoPendingController)?;
        if pending.identity != displayed_identity {
            return Err(PairingApprovalError::ControllerChanged);
        }
        self.pending = None;
        Ok(())
    }
}

pub struct HostApprovalWindow {
    state: ApprovalState,
}

impl HostApprovalWindow {
    pub fn new() -> Self {
        Self {
            state: ApprovalState::Waiting,
        }
    }

    pub fn accept(&mut self) {
        self.state = ApprovalState::Accepted;
    }

    pub fn reject(&mut self) {
        self.state = ApprovalState::Rejected;
    }

    pub fn state(&self) -> ApprovalState {
        self.state
    }
}

impl Default for HostApprovalWindow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn native_confirmation_uses_second_no_button_as_default() {
        let style = super::default_reject_warning_style();
        assert_eq!(style.0 & 0x0000_0300, 0x0000_0100);
        assert_ne!(style.0 & 0x0000_0004, 0);
    }
}
