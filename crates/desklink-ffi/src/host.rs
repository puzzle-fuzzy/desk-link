use std::sync::Arc;

use desklink_crypto::{DeviceIdentity, SessionId};
use desklink_protocol::InputEvent;
use desklink_transport::QuicClient;
use thiserror::Error;
use tokio::sync::{Mutex, mpsc};

use crate::host_worker::{HostWorker, WorkerPhase};

pub const HOST_COMMAND_CAPACITY: usize = 1_024;
pub const HOST_EVENT_CAPACITY: usize = 1_024;

#[derive(Debug)]
pub struct HostIdentity {
    device_id: [u8; 16],
    identity: DeviceIdentity,
}

impl HostIdentity {
    pub fn from_secret_key(device_id: [u8; 16], secret_key: &[u8; 32]) -> Self {
        Self {
            device_id,
            identity: DeviceIdentity::from_secret_key(device_id, secret_key),
        }
    }

    pub const fn device_id(&self) -> [u8; 16] {
        self.device_id
    }

    pub fn verify_key(&self) -> ed25519_dalek::VerifyingKey {
        self.identity.verify_key()
    }

    pub(crate) fn into_device_identity(self) -> DeviceIdentity {
        self.identity
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostState {
    Connecting,
    WaitingForApproval,
    NegotiatingCapabilities,
    Connected,
    Stopping,
    Closed,
}

impl From<WorkerPhase> for HostState {
    fn from(phase: WorkerPhase) -> Self {
        match phase {
            WorkerPhase::Connecting => Self::Connecting,
            WorkerPhase::WaitingForApproval => Self::WaitingForApproval,
            WorkerPhase::NegotiatingCapabilities => Self::NegotiatingCapabilities,
            WorkerPhase::Connected => Self::Connected,
            WorkerPhase::Stopping => Self::Stopping,
            WorkerPhase::Closed => Self::Closed,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostCommand {
    Approve {
        controller_device_id: [u8; 16],
        controller_verify_key: [u8; 32],
    },
    Reject,
    SendVideoConfig {
        stream_id: u64,
        version: u32,
        width: u16,
        height: u16,
        bytes: Vec<u8>,
    },
    SendVideoAccessUnit {
        stream_id: u64,
        frame_id: u64,
        config_version: u32,
        bytes: Vec<u8>,
    },
    SendCursor {
        stream_id: u64,
        bytes: Vec<u8>,
    },
    RequestKeyframe,
    ReleaseAll,
    Stop,
}

impl HostCommand {
    pub(crate) const fn requires_connection(&self) -> bool {
        matches!(
            self,
            Self::SendVideoConfig { .. }
                | Self::SendVideoAccessUnit { .. }
                | Self::SendCursor { .. }
                | Self::RequestKeyframe
        )
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum HostError {
    #[error("host runtime is not in a state that accepts this command")]
    InvalidState,
    #[error("host command queue is full or closed")]
    CommandQueueFull,
    #[error("host worker stopped")]
    WorkerStopped,
    #[error("host transport failed: {0}")]
    Transport(String),
    #[error("host protocol failed: {0}")]
    Protocol(String),
    #[error("host cryptographic session failed: {0}")]
    Crypto(String),
    #[error("controller identity did not match the approved device")]
    ControllerIdentityMismatch,
    #[error("controller capabilities are invalid or incompatible")]
    InvalidControllerCapabilities,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HostMetrics {
    pub sent_video_configs: u64,
    pub sent_video_packets: u64,
    pub received_input_events: u64,
    pub keyframe_requests: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HostEvent {
    State(HostState),
    ApprovalRequested {
        device_id: [u8; 16],
        verify_key: [u8; 32],
        fingerprint: String,
    },
    Input(InputEvent),
    KeyframeRequested,
    ReleaseAll,
    Metrics(HostMetrics),
    Error(HostError),
}

pub struct HostRuntime {
    worker: HostWorker,
    events: Arc<Mutex<mpsc::Receiver<HostEvent>>>,
}

impl HostRuntime {
    pub fn start(
        client: QuicClient,
        identity: HostIdentity,
        session_id: SessionId,
        relay_authentication: [u8; 32],
    ) -> Result<Self, HostError> {
        let (events, receiver) = mpsc::channel(HOST_EVENT_CAPACITY);
        let worker = HostWorker::start(
            client,
            identity.into_device_identity(),
            session_id,
            relay_authentication,
            events,
        )?;
        Ok(Self {
            worker,
            events: Arc::new(Mutex::new(receiver)),
        })
    }

    pub fn state(&self) -> HostState {
        self.worker.state()
    }

    pub fn send(&self, command: HostCommand) -> Result<(), HostError> {
        self.worker.send(command)
    }

    pub async fn next_event(&self) -> Result<HostEvent, HostError> {
        self.events
            .lock()
            .await
            .recv()
            .await
            .ok_or(HostError::WorkerStopped)
    }

    pub fn destroy(mut self) {
        self.worker.shutdown();
    }
}
