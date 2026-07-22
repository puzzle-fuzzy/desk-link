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
}

impl DirectLanConnection {
    fn new(connection: quinn::Connection, candidate_id: u64) -> Self {
        Self {
            connection,
            candidate_id,
        }
    }

    pub const fn candidate_id(&self) -> u64 {
        self.candidate_id
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
        let server_config = quinn::ServerConfig::with_single_cert(
            vec![CertificateDer::from(certificate.cert.der().to_vec())],
            PrivateKeyDer::Pkcs8(certificate.key_pair.serialize_der().into()),
        )
        .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        let endpoint = quinn::Endpoint::server(server_config, bind_addr)
            .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        let tls = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(LanRelayCertificateVerifier::new()))
            .with_no_client_auth();
        let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
            .map_err(|error| TransportError::InvalidConfig(error.to_string()))?;
        Ok(Self {
            endpoint,
            client_config: quinn::ClientConfig::new(Arc::new(crypto)),
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
        candidate
            .validate(now_unix_s, expected_binding)
            .map_err(|error| DirectLanProbeError::InvalidCandidate(error.to_string()))?;
        let request =
            DirectLanProbe::request(candidate.candidate_id(), *expected_binding, now_unix_s);
        let ciphertext = secure
            .seal(SecureLane::VideoDatagram, &request.encode())
            .map_err(|error| DirectLanProbeError::Crypto(error.to_string()))?;
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
        let response = DirectLanProbe::decode(
            &secure
                .open(SecureLane::VideoDatagram, &response_ciphertext)
                .map_err(|error| DirectLanProbeError::Crypto(error.to_string()))?,
        )?;
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
            DirectLanConnection::new(connection, candidate.candidate_id()),
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
        let (mut send, mut receive) = tokio::time::timeout(PROBE_TIMEOUT, connection.accept_bi())
            .await
            .map_err(|_| DirectLanProbeError::Timeout)?
            .map_err(|error| DirectLanProbeError::Stream(error.to_string()))?;
        let request_ciphertext = read_probe(&mut receive).await?;
        let request = DirectLanProbe::decode(
            &secure
                .open(SecureLane::VideoDatagram, &request_ciphertext)
                .map_err(|error| DirectLanProbeError::Crypto(error.to_string()))?,
        )?;
        request.validate(
            ProbeKind::Request,
            expected_candidate_id,
            expected_binding,
            now_unix_s,
        )?;
        let response = DirectLanProbe::response(request, now_unix_s);
        let ciphertext = secure
            .seal(SecureLane::VideoDatagram, &response.encode())
            .map_err(|error| DirectLanProbeError::Crypto(error.to_string()))?;
        write_probe(&mut send, &ciphertext).await?;
        Ok((
            DirectLanConnection::new(connection, request.candidate_id),
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

        let initiator_identity =
            DeviceIdentity::generate(&mut rand_chacha::ChaCha20Rng::from_seed([1; 32]));
        let responder_identity =
            DeviceIdentity::generate(&mut rand_chacha::ChaCha20Rng::from_seed([2; 32]));
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
