use std::{fmt, net::SocketAddr, time::Duration};

use desklink_crypto::SessionId;
use desklink_protocol::DeviceRole;
use thiserror::Error;

mod quic;

pub use quic::QuicClient;

pub const MAX_RELIABLE_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_DATAGRAM_BYTES: usize = 1200;
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
pub const DEAD_TIMEOUT: Duration = Duration::from_secs(15);
pub const JOIN_ENVELOPE_BYTES: usize = 4 + 1 + 1 + 16 + 32;

const JOIN_MAGIC: [u8; 4] = *b"DLJ1";
const JOIN_VERSION: u8 = 1;

#[derive(Clone)]
pub struct QuicClientConfig {
    pub relay_addr: SocketAddr,
    pub server_name: String,
    pub client_config: quinn::ClientConfig,
    pub keep_alive: Duration,
    pub dead_timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum JoinRejectCode {
    Malformed = 1,
    TooLarge = 2,
    SessionNotFound = 3,
    SessionOccupied = 4,
    AuthenticationMismatch = 5,
    RoleMismatch = 6,
    Internal = 7,
    ConnectionLimit = 8,
    SessionLimit = 9,
}

impl JoinRejectCode {
    pub(crate) fn from_byte(value: u8) -> Self {
        match value {
            1 => Self::Malformed,
            2 => Self::TooLarge,
            3 => Self::SessionNotFound,
            4 => Self::SessionOccupied,
            5 => Self::AuthenticationMismatch,
            6 => Self::RoleMismatch,
            8 => Self::ConnectionLimit,
            9 => Self::SessionLimit,
            _ => Self::Internal,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ChannelKind {
    Control = 1,
    Input = 2,
    VideoConfig = 3,
    VideoDatagram = 4,
    CursorDatagram = 5,
}

impl ChannelKind {
    pub fn is_reliable(self) -> bool {
        matches!(self, Self::Control | Self::Input | Self::VideoConfig)
    }
}

impl TryFrom<u8> for ChannelKind {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Control),
            2 => Ok(Self::Input),
            3 => Ok(Self::VideoConfig),
            4 => Ok(Self::VideoDatagram),
            5 => Ok(Self::CursorDatagram),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RelayJoin {
    session_id: SessionId,
    role: DeviceRole,
    authentication: [u8; 32],
}

impl RelayJoin {
    pub fn new(session_id: SessionId, role: DeviceRole, authentication: [u8; 32]) -> Self {
        Self {
            session_id,
            role,
            authentication,
        }
    }

    pub fn host(session_id: SessionId, authentication: [u8; 32]) -> Self {
        Self::new(session_id, DeviceRole::Host, authentication)
    }

    pub fn controller(session_id: SessionId, authentication: [u8; 32]) -> Self {
        Self::new(session_id, DeviceRole::Controller, authentication)
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn role(&self) -> DeviceRole {
        self.role
    }

    pub fn authentication(&self) -> &[u8; 32] {
        &self.authentication
    }

    pub fn encode(&self) -> [u8; JOIN_ENVELOPE_BYTES] {
        let mut bytes = [0; JOIN_ENVELOPE_BYTES];
        bytes[..JOIN_MAGIC.len()].copy_from_slice(&JOIN_MAGIC);
        bytes[4] = JOIN_VERSION;
        bytes[5] = match self.role {
            DeviceRole::Host => 1,
            DeviceRole::Controller => 2,
        };
        bytes[6..22].copy_from_slice(self.session_id.as_bytes());
        bytes[22..].copy_from_slice(&self.authentication);
        bytes
    }
}

impl fmt::Debug for RelayJoin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayJoin")
            .field("session_id", &self.session_id)
            .field("role", &self.role)
            .field("authentication", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum JoinDecodeError {
    #[error("join envelope has an invalid length")]
    InvalidLength,
    #[error("join envelope has an invalid magic")]
    InvalidMagic,
    #[error("join envelope has an unsupported version")]
    UnsupportedVersion,
    #[error("join envelope has an invalid role")]
    InvalidRole,
}

pub fn decode_relay_join(bytes: &[u8]) -> Result<RelayJoin, JoinDecodeError> {
    if bytes.len() != JOIN_ENVELOPE_BYTES {
        return Err(JoinDecodeError::InvalidLength);
    }
    if bytes[..JOIN_MAGIC.len()] != JOIN_MAGIC {
        return Err(JoinDecodeError::InvalidMagic);
    }
    if bytes[4] != JOIN_VERSION {
        return Err(JoinDecodeError::UnsupportedVersion);
    }
    let role = match bytes[5] {
        1 => DeviceRole::Host,
        2 => DeviceRole::Controller,
        _ => return Err(JoinDecodeError::InvalidRole),
    };
    let mut session_bytes = [0; 16];
    session_bytes.copy_from_slice(&bytes[6..22]);
    let mut authentication = [0; 32];
    authentication.copy_from_slice(&bytes[22..]);
    Ok(RelayJoin::new(
        SessionId::from_bytes(session_bytes),
        role,
        authentication,
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportEvent {
    Control(Vec<u8>),
    Input(Vec<u8>),
    VideoConfig(Vec<u8>),
    VideoDatagram(Vec<u8>),
    CursorDatagram(Vec<u8>),
    Closed { reason: String },
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum TransportError {
    #[error("message too large: {actual} bytes (maximum {maximum})")]
    MessageTooLarge { actual: usize, maximum: usize },
    #[error("client must join before sending data")]
    NotJoined,
    #[error("client has already joined")]
    AlreadyJoined,
    #[error("join rejected: {0:?}")]
    JoinRejected(JoinRejectCode),
    #[error("malformed relay data")]
    Malformed,
    #[error("invalid transport configuration: {0}")]
    InvalidConfig(String),
    #[error("transport connection failed: {0}")]
    Connection(String),
    #[error("transport stream failed: {0}")]
    Stream(String),
    #[error("transport datagram failed: {0}")]
    Datagram(String),
    #[error("transport connection closed")]
    Closed,
}

impl QuicClientConfig {
    pub fn new(
        relay_addr: SocketAddr,
        server_name: impl Into<String>,
    ) -> Result<Self, TransportError> {
        let client_config = quinn::ClientConfig::try_with_platform_verifier()
            .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        Ok(Self::with_client_config(
            relay_addr,
            server_name,
            client_config,
        ))
    }

    pub fn with_client_config(
        relay_addr: SocketAddr,
        server_name: impl Into<String>,
        client_config: quinn::ClientConfig,
    ) -> Self {
        Self {
            relay_addr,
            server_name: server_name.into(),
            client_config,
            keep_alive: KEEPALIVE_INTERVAL,
            dead_timeout: DEAD_TIMEOUT,
        }
    }

    pub fn with_timeouts(mut self, keep_alive: Duration, dead_timeout: Duration) -> Self {
        self.keep_alive = keep_alive;
        self.dead_timeout = dead_timeout;
        self
    }

    pub fn try_with_timeouts(
        self,
        keep_alive: Duration,
        dead_timeout: Duration,
    ) -> Result<Self, TransportError> {
        let config = self.with_timeouts(keep_alive, dead_timeout);
        config.validate_timeouts()?;
        Ok(config)
    }

    pub(crate) fn validate_timeouts(&self) -> Result<(), TransportError> {
        if self.keep_alive.is_zero() || self.dead_timeout.is_zero() {
            return Err(TransportError::InvalidConfig(
                "keepalive and dead timeout must be nonzero".to_owned(),
            ));
        }
        if self.keep_alive >= self.dead_timeout {
            return Err(TransportError::InvalidConfig(
                "keepalive must be shorter than dead timeout".to_owned(),
            ));
        }
        Ok(())
    }
}
