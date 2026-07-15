use std::fmt::Write;

use desklink_crypto::{PairingError, PairingInvite, PeerIdentity, SessionId};
use ed25519_dalek::VerifyingKey;
use thiserror::Error;

#[cfg(windows)]
use windows::{
    Win32::UI::WindowsAndMessaging::{
        IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_YESNO, MessageBoxW,
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
        "DeskLink received an authenticated remote-control request.\n\n\
         Device ID:\n{}\n\n\
         Ed25519 public-key fingerprint:\n{}\n\n\
         Session ID:\n{}\n\n\
         Request expires at Unix time {}.\n\n\
         Approve only if you recognize this controller. Approval grants screen viewing and input control.",
        grouped_hex(&pending.identity().device_id()),
        pending.verification_fingerprint(),
        grouped_hex(pending.session_id().as_bytes()),
        pending.expires_at_unix_s(),
    )
}

pub fn controller_revocation_prompt(device_id: [u8; 16], verify_key: VerifyingKey) -> String {
    format!(
        "Revoke this trusted DeskLink controller?\n\n\
         Device ID:\n{}\n\n\
         Ed25519 public-key fingerprint:\n{}\n\n\
         Revocation takes effect for the next connection. The controller must be paired and approved again to regain access.",
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
        let title = "DeskLink controller approval"
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND,
            ) == IDYES
        }
    }

    pub fn confirm_revocation(device_id: [u8; 16], verify_key: VerifyingKey) -> bool {
        let message = controller_revocation_prompt(device_id, verify_key)
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let title = "DeskLink trusted controller"
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        unsafe {
            MessageBoxW(
                None,
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND,
            ) == IDYES
        }
    }
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
    #[error("pairing invitation is not active: {0}")]
    Pairing(#[from] PairingError),
    #[error("an approval request is already active or has already been resolved")]
    InvalidState,
    #[error("there is no authenticated controller waiting for approval")]
    NoPendingController,
    #[error("the authenticated controller changed after the approval prompt was shown")]
    ControllerChanged,
    #[error("the approval request expired")]
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
