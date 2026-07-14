use desklink_protocol::{ControlMessage, DeviceRole};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
    Idle,
    CreatingSession,
    ConnectingRelay,
    SecureHandshake,
    WaitingForApproval,
    NegotiatingCapabilities,
    StartingVideo,
    Connected,
    Degraded,
    RecoveringVideo,
    Reconnecting,
    Disconnecting,
    Closed,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionEvent {
    RelayConnected,
    HandshakeComplete,
    HostAccepted,
    CapabilitiesNegotiated,
    StartVideo,
    VideoStarted,
    VideoProbeTimeout,
    DecoderStalled,
    Disconnected { retryable: bool },
    UserDisconnected,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionAction {
    SendControl(ControlMessage),
    StartVideo,
    BeginStream { stream_id: u64 },
    RebuildDecoder,
    RequestKeyframe,
    Reconnect,
    ReleaseAll,
    Close,
}
#[derive(Debug, Error, Eq, PartialEq)]
pub enum SessionError {
    #[error("invalid transition from {state:?} on {event:?}")]
    InvalidTransition {
        state: SessionState,
        event: SessionEvent,
    },
}

pub struct SessionMachine {
    role: DeviceRole,
    state: SessionState,
    next_stream_id: u64,
}
impl SessionMachine {
    pub fn new(role: DeviceRole) -> Self {
        Self {
            role,
            state: SessionState::Idle,
            next_stream_id: 0,
        }
    }
    pub fn state(&self) -> SessionState {
        self.state
    }
    fn next_stream_id(&mut self) -> u64 {
        self.next_stream_id = self.next_stream_id.wrapping_add(1);
        if self.next_stream_id == 0 {
            self.next_stream_id = 1;
        }
        self.next_stream_id
    }
    pub fn apply(&mut self, event: SessionEvent) -> Result<Vec<SessionAction>, SessionError> {
        use SessionAction::*;
        use SessionEvent::*;
        use SessionState::*;
        let old = self.state;
        let (state, actions) = match (old, event) {
            (Idle, RelayConnected) | (Reconnecting, RelayConnected) => (SecureHandshake, vec![]),
            (SecureHandshake, HandshakeComplete) if self.role == DeviceRole::Host => {
                (WaitingForApproval, vec![])
            }
            (SecureHandshake, HandshakeComplete) => (NegotiatingCapabilities, vec![]),
            (WaitingForApproval, HostAccepted) => (NegotiatingCapabilities, vec![]),
            (NegotiatingCapabilities, CapabilitiesNegotiated) => {
                let stream_id = self.next_stream_id();
                (
                    StartingVideo,
                    vec![SessionAction::BeginStream { stream_id }],
                )
            }
            (StartingVideo, SessionEvent::StartVideo) => {
                (StartingVideo, vec![SessionAction::StartVideo])
            }
            (StartingVideo, VideoStarted) => (Connected, vec![]),
            (Connected, VideoProbeTimeout) | (Connected, DecoderStalled) => {
                (RecoveringVideo, vec![RebuildDecoder, RequestKeyframe])
            }
            (RecoveringVideo, VideoStarted) => (Connected, vec![]),
            (_, Disconnected { retryable: true }) => (Reconnecting, vec![ReleaseAll, Reconnect]),
            (_, Disconnected { retryable: false }) | (_, UserDisconnected) => {
                (Closed, vec![ReleaseAll, Close])
            }
            (_, SessionEvent::StartVideo) => {
                return Err(SessionError::InvalidTransition { state: old, event });
            }
            _ => return Err(SessionError::InvalidTransition { state: old, event }),
        };
        self.state = state;
        Ok(actions)
    }
}
