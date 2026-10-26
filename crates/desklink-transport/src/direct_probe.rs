use std::{net::SocketAddr, sync::Arc, time::Duration};

use bytes::Bytes;
use desklink_crypto::{SecureLane, SecureSession};
use desklink_protocol::{DirectLanCandidate, MAX_DIRECT_LAN_CANDIDATE_TTL_S};
use rand_core::{OsRng, RngCore};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use thiserror::Error;

use crate::{LanRelayCertificateVerifier, TransportError, discover_local_private_address};

const PROBE_MAGIC: [u8; 4] = *b"DLP1";
const PROBE_VERSION: u16 = 1;
const PROBE_REQUEST: u8 = 1;
const PROBE_RESPONSE: u8 = 2;
const PROBE_NONCE_BYTES: usize = 16;
const PROBE_WIRE_BYTES: usize = 4 + 2 + 1 + 8 + 8 + 16 + PROBE_NONCE_BYTES;
const MAX_PROBE_MESSAGE_BYTES: usize = 512;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const VIDEO_DATAGRAM_SEND_TIMEOUT: Duration = Duration::from_secs(3);
const PROBE_CLOCK_SKEW_S: u64 = 5;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum DirectLanProbeError {
    #[error("direct LAN candidate is invalid: {0}")]
    InvalidCandidate(String),
    #[error("direct LAN probe message is malformed")]
    Malformed,
    #[error("direct LAN probe session binding does not match")]
    SessionBindingMismatch,
    #[error("direct LAN probe candidate ID does not match")]
    CandidateMismatch,
    #[error("direct LAN probe timestamp is outside the accepted window")]
    TimestampOutsideWindow,
    #[error("direct LAN probe connection failed: {0}")]
    Connection(String),
    #[error("direct LAN probe stream failed: {0}")]
    Stream(String),
    #[error("direct LAN probe timed out")]
    Timeout,
    #[error("direct LAN probe encryption failed: {0}")]
    Crypto(String),
}

// Shared callers borrow the session only for authenticated packet transforms.
// The connection, stream and response waits never retain a crypto guard.
enum ProbeCrypto<'a> {
    Exclusive(&'a mut SecureSession),
    Shared(&'a tokio::sync::Mutex<SecureSession>),
}

impl ProbeCrypto<'_> {
    async fn seal(&mut self, bytes: &[u8]) -> Result<Vec<u8>, DirectLanProbeError> {
        match self {
            Self::Exclusive(secure) => secure.seal(SecureLane::DirectProbe, bytes),
            Self::Shared(secure) => secure.lock().await.seal(SecureLane::DirectProbe, bytes),
        }
        .map_err(|error| DirectLanProbeError::Crypto(error.to_string()))
    }

    async fn open(&mut self, bytes: &[u8]) -> Result<Vec<u8>, DirectLanProbeError> {
        match self {
            Self::Exclusive(secure) => secure.open(SecureLane::DirectProbe, bytes),
            Self::Shared(secure) => secure.lock().await.open(SecureLane::DirectProbe, bytes),
        }
        .map_err(|error| DirectLanProbeError::Crypto(error.to_string()))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProbeKind {
    Request,
    Response,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectLanProbe {
    kind: ProbeKind,
    candidate_id: u64,
    timestamp_unix_s: u64,
    session_binding: [u8; 16],
    nonce: [u8; PROBE_NONCE_BYTES],
}

impl DirectLanProbe {
    fn request(candidate_id: u64, session_binding: [u8; 16], timestamp_unix_s: u64) -> Self {
        let mut nonce = [0; PROBE_NONCE_BYTES];
        OsRng.fill_bytes(&mut nonce);
        Self {
            kind: ProbeKind::Request,
            candidate_id,
            timestamp_unix_s,
            session_binding,
            nonce,
        }
    }

    fn response(request: Self, timestamp_unix_s: u64) -> Self {
        Self {
            kind: ProbeKind::Response,
            timestamp_unix_s,
            ..request
        }
    }

    fn encode(self) -> [u8; PROBE_WIRE_BYTES] {
        let mut bytes = [0; PROBE_WIRE_BYTES];
        bytes[..4].copy_from_slice(&PROBE_MAGIC);
        bytes[4..6].copy_from_slice(&PROBE_VERSION.to_be_bytes());
        bytes[6] = match self.kind {
            ProbeKind::Request => PROBE_REQUEST,
            ProbeKind::Response => PROBE_RESPONSE,
        };
        bytes[7..15].copy_from_slice(&self.candidate_id.to_be_bytes());
        bytes[15..23].copy_from_slice(&self.timestamp_unix_s.to_be_bytes());
        bytes[23..39].copy_from_slice(&self.session_binding);
        bytes[39..].copy_from_slice(&self.nonce);
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, DirectLanProbeError> {
        if bytes.len() != PROBE_WIRE_BYTES
            || bytes[..4] != PROBE_MAGIC
            || u16::from_be_bytes(
                bytes[4..6]
                    .try_into()
                    .map_err(|_| DirectLanProbeError::Malformed)?,
            ) != PROBE_VERSION
        {
            return Err(DirectLanProbeError::Malformed);
        }
        let kind = match bytes[6] {
            PROBE_REQUEST => ProbeKind::Request,
            PROBE_RESPONSE => ProbeKind::Response,
            _ => return Err(DirectLanProbeError::Malformed),
        };
        let candidate_id = u64::from_be_bytes(
            bytes[7..15]
                .try_into()
                .map_err(|_| DirectLanProbeError::Malformed)?,
        );
        if candidate_id == 0 {
            return Err(DirectLanProbeError::Malformed);
        }
        let timestamp_unix_s = u64::from_be_bytes(
            bytes[15..23]
                .try_into()
                .map_err(|_| DirectLanProbeError::Malformed)?,
        );
        let session_binding = bytes[23..39]
            .try_into()
            .map_err(|_| DirectLanProbeError::Malformed)?;
        if session_binding == [0; 16] {
            return Err(DirectLanProbeError::Malformed);
        }
        let nonce = bytes[39..]
            .try_into()
            .map_err(|_| DirectLanProbeError::Malformed)?;
        if nonce == [0; PROBE_NONCE_BYTES] {
            return Err(DirectLanProbeError::Malformed);
        }
        Ok(Self {
            kind,
            candidate_id,
            timestamp_unix_s,
            session_binding,
            nonce,
        })
    }

    fn validate(
        self,
        expected_kind: ProbeKind,
        expected_candidate_id: u64,
        expected_binding: &[u8; 16],
        now_unix_s: u64,
    ) -> Result<(), DirectLanProbeError> {
        if self.kind != expected_kind {
            return Err(DirectLanProbeError::Malformed);
        }
        if self.candidate_id != expected_candidate_id {
            return Err(DirectLanProbeError::CandidateMismatch);
        }
        if &self.session_binding != expected_binding {
            return Err(DirectLanProbeError::SessionBindingMismatch);
        }
        let delta = self.timestamp_unix_s.abs_diff(now_unix_s);
        if delta > PROBE_CLOCK_SKEW_S {
            return Err(DirectLanProbeError::TimestampOutsideWindow);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectLanProbeResult {
    pub candidate_id: u64,
    pub rtt_ms: u32,
}

/// An authenticated direct-LAN QUIC connection that can be reused by the
/// video data plane after a probe succeeds. The connection remains separate
/// from the relay client so control and reliable transfer lanes are unchanged.
#[derive(Clone, Debug)]
pub struct DirectLanConnection {
    connection: quinn::Connection,
    candidate_id: u64,
    session_binding: [u8; 16],
}

impl DirectLanConnection {
    fn new(connection: quinn::Connection, candidate_id: u64, session_binding: [u8; 16]) -> Self {
        Self {
            connection,
            candidate_id,
            session_binding,
        }
    }

    pub const fn candidate_id(&self) -> u64 {
        self.candidate_id
    }

    /// Noise binding actually checked by the successful path probe. A data
    /// plane cannot attach a connection authenticated for a different session.
    pub const fn session_binding(&self) -> &[u8; 16] {
        &self.session_binding
    }

    pub(crate) fn authenticated_quic_connection(&self) -> quinn::Connection {
        self.connection.clone()
    }

    pub fn send_datagram(&self, bytes: Vec<u8>) -> Result<(), TransportError> {
        if bytes.len() > crate::MAX_DATAGRAM_BYTES {
            return Err(TransportError::MessageTooLarge {
                actual: bytes.len(),
                maximum: crate::MAX_DATAGRAM_BYTES,
            });
        }
        self.connection
            .send_datagram(Bytes::from(bytes))
            .map_err(|error| TransportError::Datagram(error.to_string()))
    }

    pub async fn send_datagram_wait(&self, bytes: Vec<u8>) -> Result<(), TransportError> {
        if bytes.len() > crate::MAX_DATAGRAM_BYTES {
            return Err(TransportError::MessageTooLarge {
                actual: bytes.len(),
                maximum: crate::MAX_DATAGRAM_BYTES,
            });
        }
        tokio::time::timeout(
            VIDEO_DATAGRAM_SEND_TIMEOUT,
            self.connection.send_datagram_wait(Bytes::from(bytes)),
        )
        .await
        .map_err(|_| TransportError::Datagram("video datagram send timed out".to_owned()))?
        .map_err(|error| TransportError::Datagram(error.to_string()))
    }

    pub async fn recv_datagram(&self) -> Result<Vec<u8>, TransportError> {
        self.connection
            .read_datagram()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| TransportError::Connection(error.to_string()))
    }

    pub fn close(&self, reason: &[u8]) {
        self.connection.close(quinn::VarInt::from_u32(0), reason);
    }

    pub fn is_closed(&self) -> bool {
        self.connection.close_reason().is_some()
    }

    pub async fn closed(&self) {
        self.connection.closed().await;
    }
}

pub struct DirectLanEndpoint {
    endpoint: quinn::Endpoint,
    client_config: quinn::ClientConfig,
}

/// Owns one authenticated direct-video candidate and its temporary QUIC
/// endpoint. Keeping these values together prevents a stale candidate from
/// outliving the port that backs it.
pub struct DirectLanSession {
    endpoint: Arc<DirectLanEndpoint>,
    candidate: DirectLanCandidate,
    session_binding: [u8; 16],
}

impl DirectLanSession {
    pub fn bind_for_client(
        bind_addr: SocketAddr,
        client: &crate::QuicClient,
        candidate_id: u64,
        session_binding: [u8; 16],
        now_unix_s: u64,
    ) -> Result<Self, TransportError> {
        Self::bind(
            bind_addr,
            client.remote_address(),
            candidate_id,
            session_binding,
            now_unix_s,
        )
    }

    pub fn bind(
        bind_addr: SocketAddr,
        route: SocketAddr,
        candidate_id: u64,
        session_binding: [u8; 16],
        now_unix_s: u64,
    ) -> Result<Self, TransportError> {
        let endpoint = Arc::new(DirectLanEndpoint::bind(bind_addr)?);
        let local_ip = discover_local_private_address(route).ok_or_else(|| {
            TransportError::InvalidConfig("no private local route is available".to_owned())
        })?;
        let candidate = DirectLanCandidate::new(
            candidate_id,
            SocketAddr::new(local_ip, endpoint.local_addr()?.port()),
            now_unix_s.saturating_add(u64::from(MAX_DIRECT_LAN_CANDIDATE_TTL_S)),
            session_binding,
            now_unix_s,
        )
        .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        Ok(Self {
            endpoint,
            candidate,
            session_binding,
        })
    }

    pub fn candidate(&self) -> &DirectLanCandidate {
        &self.candidate
    }

    pub fn endpoint(&self) -> Arc<DirectLanEndpoint> {
        self.endpoint.clone()
    }

    pub const fn session_binding(&self) -> &[u8; 16] {
        &self.session_binding
    }

    pub async fn probe(
        &self,
        secure: &mut SecureSession,
        now_unix_s: u64,
    ) -> Result<DirectLanProbeResult, DirectLanProbeError> {
        self.endpoint
            .probe(&self.candidate, &self.session_binding, secure, now_unix_s)
            .await
    }

    pub async fn connect(
        &self,
        secure: &mut SecureSession,
        now_unix_s: u64,
    ) -> Result<(DirectLanConnection, DirectLanProbeResult), DirectLanProbeError> {
        self.endpoint
            .connect(&self.candidate, &self.session_binding, secure, now_unix_s)
            .await
    }
}

impl DirectLanEndpoint {
    pub fn bind(bind_addr: SocketAddr) -> Result<Self, TransportError> {
        let certificate = rcgen::generate_simple_self_signed(vec!["desklink-lan".to_owned()])
            .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        let mut server_config = quinn::ServerConfig::with_single_cert(
            vec![CertificateDer::from(certificate.cert.der().to_vec())],
            PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into()),
        )
        .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        let mut transport = quinn::TransportConfig::default();
        transport
            .keep_alive_interval(Some(crate::KEEPALIVE_INTERVAL))
            .max_idle_timeout(Some(
                quinn::IdleTimeout::try_from(crate::DEAD_TIMEOUT)
                    .map_err(|error| TransportError::InvalidConfig(error.to_string()))?,
            ));
        let transport = Arc::new(transport);
        server_config.transport_config(transport.clone());
        let endpoint = quinn::Endpoint::server(server_config, bind_addr)
            .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        let tls = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(LanRelayCertificateVerifier::new()))
            .with_no_client_auth();
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        let mut client_config = quinn::ClientConfig::new(Arc::new(crypto));
        client_config.transport_config(transport);
        Ok(Self {
            endpoint,
            client_config,
        })
    }

    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.endpoint
            .local_addr()
            .map_err(|error| TransportError::InvalidConfig(error.to_string()))
    }

    pub async fn accept(&self) -> Option<Result<quinn::Connection, TransportError>> {
        let incoming = self.endpoint.accept().await?;
        let result = match tokio::time::timeout(PROBE_TIMEOUT, incoming).await {
            Ok(Ok(connection)) => Ok(connection),
            Ok(Err(error)) => Err(TransportError::Connection(error.to_string())),
            Err(_) => Err(TransportError::Connection(
                "direct LAN handshake timed out".to_owned(),
            )),
        };
        Some(result)
    }

    pub async fn probe(
        &self,
        candidate: &DirectLanCandidate,
        expected_binding: &[u8; 16],
        secure: &mut SecureSession,
        now_unix_s: u64,
    ) -> Result<DirectLanProbeResult, DirectLanProbeError> {
        let (connection, result) = self
            .connect(candidate, expected_binding, secure, now_unix_s)
            .await?;
        connection.close(b"desklink direct probe complete");
        Ok(result)
    }

    /// Connects, authenticates and probes a candidate while preserving the
    /// QUIC connection for subsequent video datagrams.
    pub async fn connect(
        &self,
        candidate: &DirectLanCandidate,
        expected_binding: &[u8; 16],
        secure: &mut SecureSession,
        now_unix_s: u64,
    ) -> Result<(DirectLanConnection, DirectLanProbeResult), DirectLanProbeError> {
        self.connect_with_crypto(
            candidate,
            expected_binding,
            ProbeCrypto::Exclusive(secure),
            now_unix_s,
        )
        .await
    }

    /// Probe without holding the shared media/control crypto lock over I/O.
    pub async fn connect_shared(
        &self,
        candidate: &DirectLanCandidate,
        expected_binding: &[u8; 16],
        secure: &tokio::sync::Mutex<SecureSession>,
        now_unix_s: u64,
    ) -> Result<(DirectLanConnection, DirectLanProbeResult), DirectLanProbeError> {
        self.connect_with_crypto(
            candidate,
            expected_binding,
            ProbeCrypto::Shared(secure),
            now_unix_s,
        )
        .await
    }

    async fn connect_with_crypto(
        &self,
        candidate: &DirectLanCandidate,
        expected_binding: &[u8; 16],
        mut secure: ProbeCrypto<'_>,
        now_unix_s: u64,
    ) -> Result<(DirectLanConnection, DirectLanProbeResult), DirectLanProbeError> {
        candidate
            .validate(now_unix_s, expected_binding)
            .map_err(|error| DirectLanProbeError::InvalidCandidate(error.to_string()))?;
        let request =
            DirectLanProbe::request(candidate.candidate_id(), *expected_binding, now_unix_s);
        let ciphertext = secure.seal(&request.encode()).await?;
        let started = std::time::Instant::now();
        let connection = tokio::time::timeout(
            PROBE_TIMEOUT,
            self.endpoint
                .connect_with(
                    self.client_config.clone(),
                    candidate.address(),
                    "desklink-lan",
                )
                .map_err(|error| DirectLanProbeError::Connection(error.to_string()))?,
        )
        .await
        .map_err(|_| DirectLanProbeError::Timeout)?
        .map_err(|error| DirectLanProbeError::Connection(error.to_string()))?;
        let (mut send, mut receive) = tokio::time::timeout(PROBE_TIMEOUT, connection.open_bi())
            .await
            .map_err(|_| DirectLanProbeError::Timeout)?
            .map_err(|error| DirectLanProbeError::Stream(error.to_string()))?;
        write_probe(&mut send, &ciphertext).await?;
        let response_ciphertext = read_probe(&mut receive).await?;
        let response = DirectLanProbe::decode(&secure.open(&response_ciphertext).await?)?;
        response.validate(
            ProbeKind::Response,
            candidate.candidate_id(),
            expected_binding,
            now_unix_s,
        )?;
        if response.nonce != request.nonce {
            return Err(DirectLanProbeError::Malformed);
        }
        Ok((
            DirectLanConnection::new(connection, candidate.candidate_id(), *expected_binding),
            DirectLanProbeResult {
                candidate_id: candidate.candidate_id(),
                rtt_ms: started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            },
        ))
    }

    pub async fn accept_probe(
        &self,
        connection: quinn::Connection,
        expected_candidate_id: u64,
        expected_binding: &[u8; 16],
        secure: &mut SecureSession,
        now_unix_s: u64,
    ) -> Result<DirectLanProbeResult, DirectLanProbeError> {
        let (connection, result) = self
            .accept_probe_connection(
                connection,
                expected_candidate_id,
                expected_binding,
                secure,
                now_unix_s,
            )
            .await?;
        // Keep the responder alive until the initiator has read the response.
        // This bounded wait prevents a fast drop from racing the final QUIC
        // stream flush on Windows.
        let _ = tokio::time::timeout(PROBE_TIMEOUT, connection.closed()).await;
        Ok(result)
    }

    /// Accepts and authenticates a probe while preserving the connection for
    /// the direct video datagram data plane.
    pub async fn accept_probe_connection(
        &self,
        connection: quinn::Connection,
        expected_candidate_id: u64,
        expected_binding: &[u8; 16],
        secure: &mut SecureSession,
        now_unix_s: u64,
    ) -> Result<(DirectLanConnection, DirectLanProbeResult), DirectLanProbeError> {
        self.accept_probe_with_crypto(
            connection,
            expected_candidate_id,
            expected_binding,
            ProbeCrypto::Exclusive(secure),
            now_unix_s,
        )
        .await
    }

    /// Accept a probe with short per-packet access to the shared session.
    pub async fn accept_probe_connection_shared(
        &self,
        connection: quinn::Connection,
        expected_candidate_id: u64,
        expected_binding: &[u8; 16],
        secure: &tokio::sync::Mutex<SecureSession>,
        now_unix_s: u64,
    ) -> Result<(DirectLanConnection, DirectLanProbeResult), DirectLanProbeError> {
        self.accept_probe_with_crypto(
            connection,
            expected_candidate_id,
            expected_binding,
            ProbeCrypto::Shared(secure),
            now_unix_s,
        )
        .await
    }

    async fn accept_probe_with_crypto(
        &self,
        connection: quinn::Connection,
        expected_candidate_id: u64,
        expected_binding: &[u8; 16],
        mut secure: ProbeCrypto<'_>,
        now_unix_s: u64,
    ) -> Result<(DirectLanConnection, DirectLanProbeResult), DirectLanProbeError> {
        let (mut send, mut receive) = tokio::time::timeout(PROBE_TIMEOUT, connection.accept_bi())
            .await
            .map_err(|_| DirectLanProbeError::Timeout)?
            .map_err(|error| DirectLanProbeError::Stream(error.to_string()))?;
        let request_ciphertext = read_probe(&mut receive).await?;
        let request = DirectLanProbe::decode(&secure.open(&request_ciphertext).await?)?;
        request.validate(
            ProbeKind::Request,
            expected_candidate_id,
            expected_binding,
            now_unix_s,
        )?;
        let response = DirectLanProbe::response(request, now_unix_s);
        let ciphertext = secure.seal(&response.encode()).await?;
        write_probe(&mut send, &ciphertext).await?;
        Ok((
            DirectLanConnection::new(connection, request.candidate_id, *expected_binding),
            DirectLanProbeResult {
                candidate_id: request.candidate_id,
                rtt_ms: 0,
            },
        ))
    }
}

impl Drop for DirectLanEndpoint {
    fn drop(&mut self) {
        self.endpoint
            .close(0u32.into(), b"desklink direct probe stopped");
    }
}

async fn write_probe(
    send: &mut quinn::SendStream,
    bytes: &[u8],
) -> Result<(), DirectLanProbeError> {
    if bytes.len() > MAX_PROBE_MESSAGE_BYTES {
        return Err(DirectLanProbeError::Malformed);
    }
    send.write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(|error| DirectLanProbeError::Stream(error.to_string()))?;
    send.write_all(bytes)
        .await
        .map_err(|error| DirectLanProbeError::Stream(error.to_string()))?;
    send.finish()
        .map_err(|error| DirectLanProbeError::Stream(error.to_string()))?;
    Ok(())
}

async fn read_probe(receive: &mut quinn::RecvStream) -> Result<Vec<u8>, DirectLanProbeError> {
    let mut length = [0; 4];
    tokio::time::timeout(PROBE_TIMEOUT, receive.read_exact(&mut length))
        .await
        .map_err(|_| DirectLanProbeError::Timeout)?
        .map_err(|error| DirectLanProbeError::Stream(error.to_string()))?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_PROBE_MESSAGE_BYTES {
        return Err(DirectLanProbeError::Malformed);
    }
    let mut bytes = vec![0; length];
    tokio::time::timeout(PROBE_TIMEOUT, receive.read_exact(&mut bytes))
        .await
        .map_err(|_| DirectLanProbeError::Timeout)?
        .map_err(|error| DirectLanProbeError::Stream(error.to_string()))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng_compat::ChaCha20Rng;
    use rand_core::SeedableRng;

    #[test]
    fn probe_round_trip_preserves_binding_and_nonce() {
        let request = DirectLanProbe::request(7, [4; 16], 100);
        let decoded = DirectLanProbe::decode(&request.encode()).unwrap();
        assert_eq!(decoded, request);
        let response = DirectLanProbe::response(request, 101);
        response
            .validate(ProbeKind::Response, 7, &[4; 16], 101)
            .unwrap();
        assert_eq!(response.nonce, request.nonce);
    }

    #[test]
    fn probe_rejects_wrong_binding_and_stale_clock() {
        let request = DirectLanProbe::request(7, [4; 16], 100);
        assert_eq!(
            request.validate(ProbeKind::Request, 7, &[5; 16], 100),
            Err(DirectLanProbeError::SessionBindingMismatch)
        );
        assert_eq!(
            request.validate(ProbeKind::Request, 7, &[4; 16], 106),
            Err(DirectLanProbeError::TimestampOutsideWindow)
        );
    }

    #[tokio::test]
    async fn loopback_endpoint_completes_probe_and_keeps_data_connection() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let responder = Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let initiator = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let binding = [9; 16];
        let candidate =
            DirectLanCandidate::new(7, responder.local_addr().unwrap(), 110, binding, 100).unwrap();
        let (mut initiator_secure, mut responder_secure) = connected_secure_sessions();
        let responder_for_task = responder.clone();
        let responder_task = tokio::spawn(async move {
            let connection = responder_for_task.accept().await.unwrap().unwrap();
            responder_for_task
                .accept_probe_connection(connection, 7, &binding, &mut responder_secure, 100)
                .await
        });
        let result = initiator
            .connect(&candidate, &binding, &mut initiator_secure, 100)
            .await;
        let responder_result = responder_task.await.unwrap();
        assert!(
            responder_result.is_ok(),
            "responder task failed: {responder_result:?}"
        );
        assert!(result.is_ok(), "initiator probe failed: {result:?}");
        let (initiator_connection, result) = result.unwrap();
        let (responder_connection, responder_probe) = responder_result.unwrap();
        assert_eq!(result.candidate_id, 7);
        assert!(result.rtt_ms < 3_000);
        assert_eq!(responder_probe.candidate_id, 7);
        assert_eq!(initiator_connection.candidate_id(), 7);
        assert_eq!(responder_connection.candidate_id(), 7);
        initiator_connection.send_datagram(vec![1, 2, 3]).unwrap();
        let received =
            tokio::time::timeout(Duration::from_secs(1), responder_connection.recv_datagram())
                .await
                .unwrap()
                .unwrap();
        assert_eq!(received, vec![1, 2, 3]);

        initiator_connection.close(b"loopback test complete");
        tokio::time::timeout(Duration::from_secs(1), responder_connection.closed())
            .await
            .expect("responder must observe an active direct-path close");
        assert!(initiator_connection.is_closed());
        assert!(responder_connection.is_closed());
    }

    #[tokio::test]
    async fn rejected_tls_handshake_is_one_accept_error_and_next_authenticated_probe_succeeds() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let responder = Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let initiator = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        // Unlike the production LAN verifier, this client trusts no certificate.
        // A real TLS alert must produce Some(Err), not terminate the endpoint.
        let rejected_tls = rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        let rejected_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(rejected_tls).unwrap(),
        ));
        let rejected = initiator
            .endpoint
            .connect_with(
                rejected_config,
                responder.local_addr().unwrap(),
                "desklink-lan",
            )
            .unwrap();
        let (accepted, rejected) = tokio::time::timeout(Duration::from_secs(6), async {
            tokio::join!(responder.accept(), rejected)
        })
        .await
        .expect("failed TLS handshake must finish within its accept budget");
        assert!(matches!(accepted, Some(Err(TransportError::Connection(_)))));
        assert!(rejected.is_err());

        // Use the same still-listening endpoint, with fresh authenticated Noise
        // sessions. Production callers already continue on the first Some(Err).
        let binding = [0x45; 16];
        let candidate =
            DirectLanCandidate::new(45, responder.local_addr().unwrap(), 110, binding, 100)
                .unwrap();
        let (mut initiator_secure, mut responder_secure) = connected_secure_sessions();
        let responder_task = tokio::spawn({
            let responder = responder.clone();
            async move {
                let connection = responder.accept().await.unwrap().unwrap();
                responder
                    .accept_probe_connection(connection, 45, &binding, &mut responder_secure, 100)
                    .await
            }
        });
        let (initiator_path, initiator_probe) = tokio::time::timeout(
            PROBE_TIMEOUT,
            initiator.connect(&candidate, &binding, &mut initiator_secure, 100),
        )
        .await
        .unwrap()
        .unwrap();
        let (responder_path, responder_probe) = responder_task.await.unwrap().unwrap();
        assert_eq!(initiator_probe.candidate_id, 45);
        assert_eq!(responder_probe.candidate_id, 45);
        initiator_path
            .send_datagram(b"after failed handshake".to_vec())
            .unwrap();
        let received = tokio::time::timeout(Duration::from_secs(1), responder_path.recv_datagram())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received, b"after failed handshake");
        initiator_path.close(b"handshake recovery test complete");
        responder_path.close(b"handshake recovery test complete");
    }

    #[tokio::test]
    async fn explicitly_closed_endpoint_returns_terminal_none_from_accept() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let endpoint = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        endpoint
            .endpoint
            .close(0_u32.into(), b"test endpoint shutdown");
        for _ in 0..2 {
            assert!(
                tokio::time::timeout(Duration::from_millis(100), endpoint.accept())
                    .await
                    .expect("local endpoint close must wake accept")
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn delayed_probe_survives_video_window_and_still_rejects_replays() {
        let (mut initiator, mut responder) = connected_secure_sessions();
        let outbound_probe = ProbeCrypto::Exclusive(&mut initiator)
            .seal(b"delayed request")
            .await
            .unwrap();
        let inbound_probe = ProbeCrypto::Exclusive(&mut responder)
            .seal(b"delayed response")
            .await
            .unwrap();
        for _ in 0..256 {
            let video = initiator
                .seal(SecureLane::VideoDatagram, b"host video")
                .unwrap();
            assert_eq!(
                responder.open(SecureLane::VideoDatagram, &video).unwrap(),
                b"host video"
            );
            let reverse = responder
                .seal(SecureLane::VideoDatagram, b"peer video")
                .unwrap();
            assert_eq!(
                initiator.open(SecureLane::VideoDatagram, &reverse).unwrap(),
                b"peer video"
            );
        }
        assert_eq!(
            ProbeCrypto::Exclusive(&mut responder)
                .open(&outbound_probe)
                .await
                .unwrap(),
            b"delayed request"
        );
        assert_eq!(
            ProbeCrypto::Exclusive(&mut initiator)
                .open(&inbound_probe)
                .await
                .unwrap(),
            b"delayed response"
        );
        assert!(
            ProbeCrypto::Exclusive(&mut responder)
                .open(&outbound_probe)
                .await
                .is_err()
        );
        assert!(
            ProbeCrypto::Exclusive(&mut initiator)
                .open(&inbound_probe)
                .await
                .is_err()
        );
        let probe = ProbeCrypto::Exclusive(&mut initiator)
            .seal(b"lane isolated")
            .await
            .unwrap();
        assert!(responder.open(SecureLane::VideoDatagram, &probe).is_err());
        assert_eq!(
            ProbeCrypto::Exclusive(&mut responder)
                .open(&probe)
                .await
                .unwrap(),
            b"lane isolated"
        );
    }

    #[tokio::test]
    async fn wrong_lane_rejection_before_video_advances_does_not_consume_either_packet() {
        use desklink_crypto::CryptoError;

        let (mut initiator, mut responder) = connected_secure_sessions();
        let request = DirectLanProbe::request(7, [9; 16], 100);
        let encrypted_probe = ProbeCrypto::Exclusive(&mut initiator)
            .seal(&request.encode())
            .await
            .unwrap();
        // Both lane replay windows are pristine. This must exercise actual
        // authentication failure, not an earlier stale-sequence rejection.
        assert_eq!(
            responder.open(SecureLane::VideoDatagram, &encrypted_probe),
            Err(CryptoError::AuthenticationFailed)
        );
        let opened_probe = ProbeCrypto::Exclusive(&mut responder)
            .open(&encrypted_probe)
            .await
            .unwrap();
        assert_eq!(DirectLanProbe::decode(&opened_probe).unwrap(), request);

        // The failed open must also leave the first genuine video sequence
        // available. Its counter starts at one independently of DirectProbe.
        let first_video = initiator
            .seal(SecureLane::VideoDatagram, b"first genuine video")
            .unwrap();
        assert_eq!(
            responder
                .open(SecureLane::VideoDatagram, &first_video)
                .unwrap(),
            b"first genuine video"
        );
        assert_eq!(
            responder.open(SecureLane::DirectProbe, &encrypted_probe),
            Err(CryptoError::ReplayRejected)
        );
    }

    #[tokio::test]
    async fn reflected_outbound_probe_is_rejected_without_consuming_a_genuine_reply() {
        use desklink_crypto::CryptoError;

        let (mut initiator, mut responder) = connected_secure_sessions();
        let request = DirectLanProbe::request(7, [9; 16], 100);
        let outbound_probe = ProbeCrypto::Exclusive(&mut initiator)
            .seal(&request.encode())
            .await
            .unwrap();
        // Same lane and sequence, opposite direction: reflection must fail
        // authentication while the initiator's receive window is still empty.
        assert_eq!(
            initiator.open(SecureLane::DirectProbe, &outbound_probe),
            Err(CryptoError::AuthenticationFailed)
        );
        let genuine_request = ProbeCrypto::Exclusive(&mut responder)
            .open(&outbound_probe)
            .await
            .unwrap();
        assert_eq!(DirectLanProbe::decode(&genuine_request).unwrap(), request);
        let response = DirectLanProbe::response(request, 100);
        let inbound_probe = ProbeCrypto::Exclusive(&mut responder)
            .seal(&response.encode())
            .await
            .unwrap();
        let genuine_reply = ProbeCrypto::Exclusive(&mut initiator)
            .open(&inbound_probe)
            .await
            .unwrap();
        assert_eq!(DirectLanProbe::decode(&genuine_reply).unwrap(), response);
        assert_eq!(
            initiator.open(SecureLane::DirectProbe, &inbound_probe),
            Err(CryptoError::ReplayRejected)
        );
        // Rejection and replay checks do not poison the next genuine packet
        // in either direction.
        let next_request = initiator
            .seal(SecureLane::DirectProbe, b"next request")
            .unwrap();
        let next_response = responder
            .seal(SecureLane::DirectProbe, b"next response")
            .unwrap();
        assert_eq!(
            responder
                .open(SecureLane::DirectProbe, &next_request)
                .unwrap(),
            b"next request"
        );
        assert_eq!(
            initiator
                .open(SecureLane::DirectProbe, &next_response)
                .unwrap(),
            b"next response"
        );
    }

    #[tokio::test]
    async fn shared_response_wait_survives_reverse_video_traffic_without_crypto_guards() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let responder = Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let initiator = Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let binding = [9; 16];
        let candidate =
            DirectLanCandidate::new(88, responder.local_addr().unwrap(), 110, binding, 100)
                .unwrap();
        let (initiator_secure, responder_secure) = connected_secure_sessions();
        let initiator_secure = Arc::new(tokio::sync::Mutex::new(initiator_secure));
        let responder_secure = Arc::new(tokio::sync::Mutex::new(responder_secure));
        let (response_ready, wait_ready) = tokio::sync::oneshot::channel();
        let (release_response, wait_release) = tokio::sync::oneshot::channel();
        let responder_task = tokio::spawn({
            let endpoint = responder.clone();
            let secure = responder_secure.clone();
            async move {
                let connection = endpoint.accept().await.unwrap().unwrap();
                let (mut send, mut receive) =
                    tokio::time::timeout(PROBE_TIMEOUT, connection.accept_bi())
                        .await
                        .unwrap()
                        .unwrap();
                let encrypted_request = read_probe(&mut receive).await.unwrap();
                let mut crypto = ProbeCrypto::Shared(&secure);
                let request =
                    DirectLanProbe::decode(&crypto.open(&encrypted_request).await.unwrap())
                        .unwrap();
                request
                    .validate(ProbeKind::Request, 88, &binding, 100)
                    .unwrap();
                let response = DirectLanProbe::response(request, 100);
                let encrypted_response = crypto.seal(&response.encode()).await.unwrap();
                response_ready.send(()).unwrap();
                // The response is sealed before media advances the reverse
                // video replay window, but is deliberately not written yet.
                wait_release.await.unwrap();
                write_probe(&mut send, &encrypted_response).await.unwrap();
                DirectLanConnection::new(connection, 88, binding)
            }
        });
        let initiator_task = tokio::spawn({
            let endpoint = initiator.clone();
            let secure = initiator_secure.clone();
            async move {
                endpoint
                    .connect_shared(&candidate, &binding, &secure, 100)
                    .await
            }
        });
        tokio::time::timeout(PROBE_TIMEOUT, wait_ready)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !initiator_task.is_finished(),
            "probe completed before response release"
        );
        let mut initiator_guard =
            tokio::time::timeout(Duration::from_millis(100), initiator_secure.lock())
                .await
                .expect("response read wait retained initiator crypto guard");
        let control = initiator_guard
            .seal(SecureLane::Control, b"control during response wait")
            .unwrap();
        drop(initiator_guard);
        let mut responder_guard =
            tokio::time::timeout(Duration::from_millis(100), responder_secure.lock())
                .await
                .expect("delayed response write retained responder crypto guard");
        assert_eq!(
            responder_guard.open(SecureLane::Control, &control).unwrap(),
            b"control during response wait"
        );
        drop(responder_guard);
        tokio::time::timeout(Duration::from_secs(1), async {
            for sequence in 0_u16..256 {
                let payload = sequence.to_be_bytes();
                let video = responder_secure
                    .lock()
                    .await
                    .seal(SecureLane::VideoDatagram, &payload)
                    .unwrap();
                assert_eq!(
                    initiator_secure
                        .lock()
                        .await
                        .open(SecureLane::VideoDatagram, &video)
                        .unwrap(),
                    payload
                );
            }
        })
        .await
        .expect("reverse video crypto stalled while the probe awaited its response");
        assert!(
            !initiator_task.is_finished(),
            "probe must still await the delayed response"
        );
        release_response.send(()).unwrap();
        let (initiator_path, result) = tokio::time::timeout(PROBE_TIMEOUT, initiator_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let responder_path = tokio::time::timeout(PROBE_TIMEOUT, responder_task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.candidate_id, 88);
        initiator_path.close(b"response wait test complete");
        responder_path.close(b"response wait test complete");
    }

    #[tokio::test]
    async fn shared_probe_network_wait_does_not_hold_media_crypto_lock() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let responder = Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let initiator = Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let binding = [9; 16];
        let candidate =
            DirectLanCandidate::new(77, responder.local_addr().unwrap(), 110, binding, 100)
                .unwrap();
        let (initiator_secure, responder_secure) = connected_secure_sessions();
        let initiator_secure = Arc::new(tokio::sync::Mutex::new(initiator_secure));
        let responder_secure = tokio::sync::Mutex::new(responder_secure);
        let initiator_for_task = initiator.clone();
        let secure_for_task = initiator_secure.clone();
        let task = tokio::spawn(async move {
            initiator_for_task
                .connect_shared(&candidate, &binding, &secure_for_task, 100)
                .await
        });
        let connection = responder.accept().await.unwrap().unwrap();
        // The peer has completed TLS but has deliberately not read/responded
        // to the encrypted probe. Normal reliable lanes must still encrypt.
        let mut secure = tokio::time::timeout(Duration::from_millis(100), initiator_secure.lock())
            .await
            .expect("network wait retained shared crypto lock");
        let packet = secure
            .seal(SecureLane::Control, b"input remains live")
            .unwrap();
        drop(secure);
        for _ in 0..256 {
            let video = initiator_secure
                .lock()
                .await
                .seal(SecureLane::VideoDatagram, b"moving desktop")
                .unwrap();
            responder_secure
                .lock()
                .await
                .open(SecureLane::VideoDatagram, &video)
                .unwrap();
        }

        assert_eq!(
            responder_secure
                .lock()
                .await
                .open(SecureLane::Control, &packet)
                .unwrap(),
            b"input remains live"
        );
        let (responder_path, _) = responder
            .accept_probe_connection_shared(connection, 77, &binding, &responder_secure, 100)
            .await
            .unwrap();
        let (initiator_path, _) = task.await.unwrap().unwrap();
        initiator_path.close(b"test complete");
        responder_path.close(b"test complete");
    }

    #[tokio::test]
    async fn shared_inbound_probe_wait_is_cancellable_without_crypto_guard() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let responder = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let initiator = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let connecting = initiator
            .endpoint
            .connect_with(
                initiator.client_config.clone(),
                responder.local_addr().unwrap(),
                "desklink-lan",
            )
            .unwrap();
        let (accepted, connected) = tokio::join!(responder.accept(), connecting);
        let connection = accepted.unwrap().unwrap();
        let peer = connected.unwrap();
        let (_, secure) = connected_secure_sessions();
        let secure = tokio::sync::Mutex::new(secure);
        // Peer sends no probe stream. Timeout cancels the inbound select future.
        let wait = responder.accept_probe_connection_shared(connection, 1, &[9; 16], &secure, 100);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), wait)
                .await
                .is_err()
        );
        assert!(secure.try_lock().is_ok());
        peer.close(0_u32.into(), b"cancelled");
    }

    #[tokio::test]
    async fn expired_candidate_is_rejected_before_network_probe() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let endpoint = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let binding = [5; 16];
        let candidate =
            DirectLanCandidate::new(11, endpoint.local_addr().unwrap(), 105, binding, 100).unwrap();
        let (mut initiator_secure, _) = connected_secure_sessions();

        let error = endpoint
            .connect(&candidate, &binding, &mut initiator_secure, 106)
            .await
            .expect_err("an expired candidate must never open a direct socket");
        assert!(matches!(
            error,
            DirectLanProbeError::InvalidCandidate(message)
                if message.contains("expiry is outside the allowed lifetime")
        ));
    }

    #[tokio::test]
    async fn probe_rejects_a_peer_with_a_different_session_binding() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let responder = Arc::new(DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap());
        let initiator = DirectLanEndpoint::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let binding = [7; 16];
        let wrong_binding = [8; 16];
        let candidate =
            DirectLanCandidate::new(13, responder.local_addr().unwrap(), 110, binding, 100)
                .unwrap();
        let (mut initiator_secure, mut responder_secure) = connected_secure_sessions();
        let responder_for_task = responder.clone();
        let responder_task = tokio::spawn(async move {
            let connection = responder_for_task.accept().await.unwrap().unwrap();
            responder_for_task
                .accept_probe_connection(connection, 13, &wrong_binding, &mut responder_secure, 100)
                .await
        });

        let initiator_result = initiator
            .connect(&candidate, &binding, &mut initiator_secure, 100)
            .await;
        let responder_result = responder_task.await.unwrap();
        assert!(
            initiator_result.is_err(),
            "binding mismatch must fail the probe"
        );
        assert!(matches!(
            responder_result,
            Err(DirectLanProbeError::SessionBindingMismatch)
        ));
    }

    #[tokio::test]
    async fn session_binds_candidate_to_endpoint_lifetime() {
        let session = DirectLanSession::bind(
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:9".parse().unwrap(),
            19,
            [6; 16],
            100,
        )
        .unwrap();
        assert_eq!(session.candidate().candidate_id(), 19);
        assert_eq!(
            session.candidate().address().port(),
            session.endpoint().local_addr().unwrap().port()
        );
        assert_eq!(session.session_binding(), &[6; 16]);
    }

    fn connected_secure_sessions() -> (SecureSession, SecureSession) {
        use desklink_crypto::{DeviceIdentity, NoiseInitiator, NoiseResponder, SecureRole};

        let initiator_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([1; 32]));
        let responder_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([2; 32]));
        let initiator_verify_key = initiator_identity.verify_key();
        let responder_verify_key = responder_identity.verify_key();
        let (mut initiator, message_1) =
            NoiseInitiator::start(initiator_identity, responder_verify_key).unwrap();
        let (mut responder, message_2) =
            NoiseResponder::accept(&message_1, responder_identity, initiator_verify_key).unwrap();
        let message_3 = initiator.receive(&message_2).unwrap();
        responder.receive(&message_3).unwrap();
        (
            initiator
                .finish()
                .unwrap()
                .into_secure_session(SecureRole::Initiator),
            responder
                .finish()
                .unwrap()
                .into_secure_session(SecureRole::Responder),
        )
    }
}
