use std::{fmt, net::SocketAddr, sync::Arc, time::Duration};

use desklink_crypto::SessionId;
use desklink_protocol::DeviceRole;
use thiserror::Error;

mod quic;

pub use quic::QuicClient;

pub const MAX_RELIABLE_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_DATAGRAM_BYTES: usize = 1200;
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(5);
pub const DEAD_TIMEOUT: Duration = Duration::from_secs(15);
/// Legacy join envelope size retained for compatibility with already released clients.
pub const JOIN_ENVELOPE_BYTES: usize = 4 + 1 + 1 + 16 + 32;
/// Version 2 adds a stable, non-secret participant identifier for safe reconnect takeover.
pub const JOIN_ENVELOPE_V2_BYTES: usize = JOIN_ENVELOPE_BYTES + 16;
pub const DIRECTORY_ACCESS_CODE_BYTES: usize = 8;
pub const MAX_DIRECTORY_INVITATION_BYTES: usize = 1_024;
pub const MAX_DIRECTORY_TTL_S: u16 = 600;
pub const DIRECTORY_LOOKUP_ENVELOPE_BYTES: usize = 4 + 1 + 8 + DIRECTORY_ACCESS_CODE_BYTES;
pub const MAX_JOIN_ENVELOPE_BYTES: usize = JOIN_ENVELOPE_V2_BYTES
    + 1
    + 8
    + DIRECTORY_ACCESS_CODE_BYTES
    + 2
    + 2
    + MAX_DIRECTORY_INVITATION_BYTES;
/// Application close code used when the relay cannot admit another QUIC connection.
pub const RELAY_CONNECTION_LIMIT_CLOSE_CODE: u32 = 0x444c_0001;

const JOIN_MAGIC: [u8; 4] = *b"DLJ1";
const JOIN_VERSION_V1: u8 = 1;
const JOIN_VERSION_V2: u8 = 2;
const JOIN_VERSION_V3: u8 = 3;
const DIRECTORY_LOOKUP_MAGIC: [u8; 4] = *b"DLL1";
const DIRECTORY_LOOKUP_VERSION: u8 = 1;

pub const DIRECTORY_LOOKUP_FOUND: u8 = 0;
pub const DIRECTORY_LOOKUP_NOT_FOUND: u8 = 1;
pub const DIRECTORY_LOOKUP_RATE_LIMITED: u8 = 2;
pub const DIRECTORY_LOOKUP_MALFORMED: u8 = 3;

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
    DirectoryConflict = 10,
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
            10 => Self::DirectoryConflict,
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
    participant_id: Option<[u8; 16]>,
    directory: Option<RelayDirectoryRegistration>,
}

#[derive(Clone, Eq, PartialEq)]
pub struct RelayDirectoryRegistration {
    device_id: u64,
    access_code: [u8; DIRECTORY_ACCESS_CODE_BYTES],
    invitation: Vec<u8>,
    ttl_s: u16,
}

impl RelayDirectoryRegistration {
    pub fn new(
        device_id: u64,
        access_code: [u8; DIRECTORY_ACCESS_CODE_BYTES],
        invitation: Vec<u8>,
        ttl_s: u16,
    ) -> Result<Self, TransportError> {
        if device_id == 0 {
            return Err(TransportError::InvalidConfig(
                "device directory ID must be nonzero".to_owned(),
            ));
        }
        if !valid_directory_access_code(&access_code) {
            return Err(TransportError::InvalidConfig(
                "device access code is invalid".to_owned(),
            ));
        }
        if invitation.is_empty() || invitation.len() > MAX_DIRECTORY_INVITATION_BYTES {
            return Err(TransportError::InvalidConfig(
                "device directory invitation has an invalid length".to_owned(),
            ));
        }
        // A zero TTL is a persistent entry whose lifetime is bound to the
        // publishing host's live relay connection.
        if ttl_s > MAX_DIRECTORY_TTL_S {
            return Err(TransportError::InvalidConfig(
                "device directory TTL is invalid".to_owned(),
            ));
        }
        Ok(Self {
            device_id,
            access_code,
            invitation,
            ttl_s,
        })
    }

    pub const fn device_id(&self) -> u64 {
        self.device_id
    }

    pub const fn access_code(&self) -> &[u8; DIRECTORY_ACCESS_CODE_BYTES] {
        &self.access_code
    }

    pub fn invitation(&self) -> &[u8] {
        &self.invitation
    }

    pub const fn ttl_s(&self) -> u16 {
        self.ttl_s
    }
}

impl fmt::Debug for RelayDirectoryRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayDirectoryRegistration")
            .field("device_id", &self.device_id)
            .field("access_code", &"[REDACTED]")
            .field("invitation_bytes", &self.invitation.len())
            .field("ttl_s", &self.ttl_s)
            .finish()
    }
}

impl Drop for RelayDirectoryRegistration {
    fn drop(&mut self) {
        self.access_code.fill(0);
        self.invitation.fill(0);
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct RelayDirectoryLookup {
    device_id: u64,
    access_code: [u8; DIRECTORY_ACCESS_CODE_BYTES],
}

impl RelayDirectoryLookup {
    pub fn new(
        device_id: u64,
        access_code: [u8; DIRECTORY_ACCESS_CODE_BYTES],
    ) -> Result<Self, TransportError> {
        if device_id == 0 || !valid_directory_access_code(&access_code) {
            return Err(TransportError::InvalidConfig(
                "device ID or temporary password is invalid".to_owned(),
            ));
        }
        Ok(Self {
            device_id,
            access_code,
        })
    }

    pub const fn device_id(&self) -> u64 {
        self.device_id
    }

    pub const fn access_code(&self) -> &[u8; DIRECTORY_ACCESS_CODE_BYTES] {
        &self.access_code
    }

    pub fn encode(&self) -> [u8; DIRECTORY_LOOKUP_ENVELOPE_BYTES] {
        let mut bytes = [0; DIRECTORY_LOOKUP_ENVELOPE_BYTES];
        bytes[..4].copy_from_slice(&DIRECTORY_LOOKUP_MAGIC);
        bytes[4] = DIRECTORY_LOOKUP_VERSION;
        bytes[5..13].copy_from_slice(&self.device_id.to_be_bytes());
        bytes[13..].copy_from_slice(&self.access_code);
        bytes
    }
}

impl fmt::Debug for RelayDirectoryLookup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayDirectoryLookup")
            .field("device_id", &self.device_id)
            .field("access_code", &"[REDACTED]")
            .finish()
    }
}

impl Drop for RelayDirectoryLookup {
    fn drop(&mut self) {
        self.access_code.fill(0);
    }
}

impl RelayJoin {
    pub fn new(session_id: SessionId, role: DeviceRole, authentication: [u8; 32]) -> Self {
        Self {
            session_id,
            role,
            authentication,
            participant_id: None,
            directory: None,
        }
    }

    pub fn new_with_participant(
        session_id: SessionId,
        role: DeviceRole,
        authentication: [u8; 32],
        participant_id: [u8; 16],
    ) -> Self {
        Self {
            session_id,
            role,
            authentication,
            participant_id: Some(participant_id),
            directory: None,
        }
    }

    pub fn host(session_id: SessionId, authentication: [u8; 32]) -> Self {
        Self::new(session_id, DeviceRole::Host, authentication)
    }

    pub fn host_with_participant(
        session_id: SessionId,
        authentication: [u8; 32],
        participant_id: [u8; 16],
    ) -> Self {
        Self::new_with_participant(session_id, DeviceRole::Host, authentication, participant_id)
    }

    pub fn controller(session_id: SessionId, authentication: [u8; 32]) -> Self {
        Self::new(session_id, DeviceRole::Controller, authentication)
    }

    pub fn controller_with_participant(
        session_id: SessionId,
        authentication: [u8; 32],
        participant_id: [u8; 16],
    ) -> Self {
        Self::new_with_participant(
            session_id,
            DeviceRole::Controller,
            authentication,
            participant_id,
        )
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

    pub fn participant_id(&self) -> Option<&[u8; 16]> {
        self.participant_id.as_ref()
    }

    pub fn directory_registration(&self) -> Option<&RelayDirectoryRegistration> {
        self.directory.as_ref()
    }

    pub fn with_directory_registration(
        mut self,
        registration: RelayDirectoryRegistration,
    ) -> Result<Self, TransportError> {
        if self.role != DeviceRole::Host || self.participant_id.is_none() {
            return Err(TransportError::InvalidConfig(
                "only an identified host can publish a directory entry".to_owned(),
            ));
        }
        self.directory = Some(registration);
        Ok(self)
    }

    pub fn encode(&self) -> Vec<u8> {
        let length = if let Some(directory) = &self.directory {
            JOIN_ENVELOPE_V2_BYTES
                + 1
                + 8
                + DIRECTORY_ACCESS_CODE_BYTES
                + 2
                + 2
                + directory.invitation.len()
        } else if self.participant_id.is_some() {
            JOIN_ENVELOPE_V2_BYTES
        } else {
            JOIN_ENVELOPE_BYTES
        };
        let mut bytes = vec![0; length];
        bytes[..JOIN_MAGIC.len()].copy_from_slice(&JOIN_MAGIC);
        bytes[4] = if self.directory.is_some() {
            JOIN_VERSION_V3
        } else if self.participant_id.is_some() {
            JOIN_VERSION_V2
        } else {
            JOIN_VERSION_V1
        };
        bytes[5] = match self.role {
            DeviceRole::Host => 1,
            DeviceRole::Controller => 2,
        };
        bytes[6..22].copy_from_slice(self.session_id.as_bytes());
        bytes[22..54].copy_from_slice(&self.authentication);
        if let Some(participant_id) = self.participant_id {
            bytes[54..70].copy_from_slice(&participant_id);
        }
        if let Some(directory) = &self.directory {
            bytes[70] = 1;
            bytes[71..79].copy_from_slice(&directory.device_id.to_be_bytes());
            bytes[79..87].copy_from_slice(&directory.access_code);
            bytes[87..89].copy_from_slice(&directory.ttl_s.to_be_bytes());
            bytes[89..91].copy_from_slice(&(directory.invitation.len() as u16).to_be_bytes());
            bytes[91..].copy_from_slice(&directory.invitation);
        }
        bytes
    }
}

impl fmt::Debug for RelayJoin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelayJoin")
            .field("session_id", &self.session_id)
            .field("role", &self.role)
            .field("participant_id", &self.participant_id)
            .field("directory", &self.directory)
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

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DirectoryLookupDecodeError {
    #[error("device directory lookup envelope is malformed")]
    Malformed,
}

pub fn decode_directory_lookup(
    bytes: &[u8],
) -> Result<RelayDirectoryLookup, DirectoryLookupDecodeError> {
    if bytes.len() != DIRECTORY_LOOKUP_ENVELOPE_BYTES
        || bytes[..4] != DIRECTORY_LOOKUP_MAGIC
        || bytes[4] != DIRECTORY_LOOKUP_VERSION
    {
        return Err(DirectoryLookupDecodeError::Malformed);
    }
    let device_id = u64::from_be_bytes(
        bytes[5..13]
            .try_into()
            .map_err(|_| DirectoryLookupDecodeError::Malformed)?,
    );
    let access_code = bytes[13..]
        .try_into()
        .map_err(|_| DirectoryLookupDecodeError::Malformed)?;
    if device_id == 0 || !valid_directory_access_code(&access_code) {
        return Err(DirectoryLookupDecodeError::Malformed);
    }
    Ok(RelayDirectoryLookup {
        device_id,
        access_code,
    })
}

pub fn decode_relay_join(bytes: &[u8]) -> Result<RelayJoin, JoinDecodeError> {
    if bytes.len() < JOIN_ENVELOPE_BYTES || bytes.len() > MAX_JOIN_ENVELOPE_BYTES {
        return Err(JoinDecodeError::InvalidLength);
    }
    if bytes[..JOIN_MAGIC.len()] != JOIN_MAGIC {
        return Err(JoinDecodeError::InvalidMagic);
    }
    let version = bytes[4];
    if !matches!(
        (version, bytes.len()),
        (JOIN_VERSION_V1, JOIN_ENVELOPE_BYTES)
            | (JOIN_VERSION_V2, JOIN_ENVELOPE_V2_BYTES)
            | (
                JOIN_VERSION_V3,
                JOIN_ENVELOPE_V2_BYTES..=MAX_JOIN_ENVELOPE_BYTES
            )
    ) {
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
    authentication.copy_from_slice(&bytes[22..54]);
    let session_id = SessionId::from_bytes(session_bytes);
    if matches!(version, JOIN_VERSION_V2 | JOIN_VERSION_V3) {
        let mut participant_id = [0; 16];
        participant_id.copy_from_slice(&bytes[54..70]);
        let mut join =
            RelayJoin::new_with_participant(session_id, role, authentication, participant_id);
        if version == JOIN_VERSION_V3 {
            if role != DeviceRole::Host || bytes.len() < 91 || bytes[70] != 1 {
                return Err(JoinDecodeError::InvalidLength);
            }
            let device_id = u64::from_be_bytes(
                bytes[71..79]
                    .try_into()
                    .map_err(|_| JoinDecodeError::InvalidLength)?,
            );
            let access_code = bytes[79..87]
                .try_into()
                .map_err(|_| JoinDecodeError::InvalidLength)?;
            let ttl_s = u16::from_be_bytes(
                bytes[87..89]
                    .try_into()
                    .map_err(|_| JoinDecodeError::InvalidLength)?,
            );
            let invitation_length = u16::from_be_bytes(
                bytes[89..91]
                    .try_into()
                    .map_err(|_| JoinDecodeError::InvalidLength)?,
            ) as usize;
            if bytes.len() != 91 + invitation_length {
                return Err(JoinDecodeError::InvalidLength);
            }
            let registration = RelayDirectoryRegistration::new(
                device_id,
                access_code,
                bytes[91..].to_vec(),
                ttl_s,
            )
            .map_err(|_| JoinDecodeError::InvalidLength)?;
            join.directory = Some(registration);
        }
        Ok(join)
    } else {
        Ok(RelayJoin::new(session_id, role, authentication))
    }
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
    /// The relay rejected this connection because its hard admission cap is full.
    #[error("relay connection admission limit reached")]
    ConnectionLimit,
    #[error("transport stream failed: {0}")]
    Stream(String),
    #[error("transport datagram failed: {0}")]
    Datagram(String),
    #[error("transport connection closed")]
    Closed,
    #[error("device is offline or the temporary password is incorrect")]
    DirectoryNotFound,
    #[error("too many device lookup attempts; try again later")]
    DirectoryRateLimited,
}

fn valid_directory_access_code(code: &[u8; DIRECTORY_ACCESS_CODE_BYTES]) -> bool {
    code.iter()
        .all(|byte| b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ".contains(byte))
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

    /// Creates a QUIC client for DeskLink's explicitly selected local-LAN relay mode.
    ///
    /// The relay certificate is allowed to be self-signed because the remote host is still
    /// authenticated by the signed pairing invitation and the Noise handshake before video or
    /// input is enabled. Callers must restrict this mode to private or loopback relay addresses.
    pub fn new_lan(
        relay_addr: SocketAddr,
        server_name: impl Into<String>,
    ) -> Result<Self, TransportError> {
        let verifier = Arc::new(LanRelayCertificateVerifier::new());
        let tls = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        Ok(Self::with_client_config(
            relay_addr,
            server_name,
            quinn::ClientConfig::new(Arc::new(crypto)),
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

#[derive(Debug)]
struct LanRelayCertificateVerifier {
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl LanRelayCertificateVerifier {
    fn new() -> Self {
        Self {
            algorithms: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        }
    }
}

impl rustls::client::danger::ServerCertVerifier for LanRelayCertificateVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, certificate, signature, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, certificate, signature, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}
