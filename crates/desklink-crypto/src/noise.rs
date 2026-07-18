use std::{fmt, ops::Deref};

use blake2::{Blake2s256, Digest};
use chacha20poly1305::{
    ChaCha20Poly1305,
    aead::{Aead, KeyInit, Payload},
};
use ed25519_dalek::{Signature, VerifyingKey};
use rand_core::{OsRng, RngCore};
use snow::{Builder, HandshakeState, TransportState, params::NoiseParams};
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::identity::DeviceIdentity;
use crate::resolver::DesklinkResolver;

const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const DEVICE_ID_BYTES: usize = 16;
const AUTH_PAYLOAD_BYTES: usize = DEVICE_ID_BYTES + 32 + 64;
const SESSION_KEY_BYTES: usize = 32;
const INITIATOR_PAYLOAD_BYTES: usize = AUTH_PAYLOAD_BYTES + SESSION_KEY_BYTES;
const HANDSHAKE_OUTPUT_OVERHEAD: usize = 256;
const RESPONDER_SIGNATURE_DOMAIN: &[u8] = b"desklink-noise-xx-responder-v1";
const INITIATOR_SIGNATURE_DOMAIN: &[u8] = b"desklink-noise-xx-initiator-v1";
const PACKET_KEY_DOMAIN: &[u8] = b"desklink-packet-key-v1";
const PACKET_AAD_DOMAIN: &[u8] = b"desklink-packet-v1";
const PACKET_SEQUENCE_BYTES: usize = 8;
const PACKET_TAG_BYTES: usize = 16;
const PACKET_OVERHEAD_BYTES: usize = PACKET_SEQUENCE_BYTES + PACKET_TAG_BYTES;
const REPLAY_WINDOW_BITS: u64 = 128;

pub const MAX_ENCRYPTED_MESSAGE_BYTES: usize = 65_535;
pub const MAX_PLAINTEXT_BYTES: usize = MAX_ENCRYPTED_MESSAGE_BYTES - 16;
pub const MAX_PACKET_PLAINTEXT_BYTES: usize = MAX_ENCRYPTED_MESSAGE_BYTES - PACKET_OVERHEAD_BYTES;
pub const MAX_HANDSHAKE_PAYLOAD_BYTES: usize =
    MAX_ENCRYPTED_MESSAGE_BYTES - HANDSHAKE_OUTPUT_OVERHEAD;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum CryptoError {
    #[error("crypto message is too large: {actual} bytes exceeds {maximum} bytes")]
    MessageTooLarge { actual: usize, maximum: usize },
    #[error("peer identity signature is invalid")]
    InvalidSignature,
    #[error("crypto operation is invalid in the current state")]
    InvalidState,
    #[error("Noise handshake message is malformed")]
    MalformedHandshake,
    #[error("encrypted message authentication failed")]
    AuthenticationFailed,
    #[error("encrypted packet sequence was already received or is outside the replay window")]
    ReplayRejected,
    #[error("cryptographic backend initialization failed")]
    BackendFailure,
}

#[derive(Eq, PartialEq)]
pub struct EncryptedMessage(Vec<u8>);

impl EncryptedMessage {
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl TryFrom<Vec<u8>> for EncryptedMessage {
    type Error = CryptoError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        ensure_bounded(bytes.len(), MAX_ENCRYPTED_MESSAGE_BYTES)?;
        Ok(Self(bytes))
    }
}

impl AsRef<[u8]> for EncryptedMessage {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Deref for EncryptedMessage {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Debug for EncryptedMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncryptedMessage")
            .field("len", &self.0.len())
            .finish()
    }
}

pub struct SessionKey(Zeroizing<[u8; SESSION_KEY_BYTES]>);

impl SessionKey {
    fn new(bytes: Zeroizing<[u8; SESSION_KEY_BYTES]>) -> Self {
        Self(bytes)
    }

    fn as_bytes(&self) -> &[u8; SESSION_KEY_BYTES] {
        &self.0
    }
}

impl PartialEq for SessionKey {
    fn eq(&self, other: &Self) -> bool {
        bool::from(self.as_bytes().ct_eq(other.as_bytes()))
    }
}

impl Eq for SessionKey {}

impl fmt::Debug for SessionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionKey([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PeerIdentity {
    device_id: [u8; DEVICE_ID_BYTES],
    verify_key: VerifyingKey,
}

impl PeerIdentity {
    pub const fn device_id(&self) -> [u8; DEVICE_ID_BYTES] {
        self.device_id
    }

    pub fn verify_key(&self) -> VerifyingKey {
        self.verify_key
    }
}

pub struct NoiseInitiator {
    state: Option<HandshakeState>,
    identity: Option<DeviceIdentity>,
    expected_peer: VerifyingKey,
    peer_identity: Option<PeerIdentity>,
    session_key: Option<SessionKey>,
}

impl NoiseInitiator {
    pub fn start(
        identity: DeviceIdentity,
        expected_peer: VerifyingKey,
    ) -> Result<(Self, Vec<u8>), CryptoError> {
        let state = build_handshake_state(true)?;
        let mut session_key = Zeroizing::new([0; SESSION_KEY_BYTES]);
        OsRng
            .try_fill_bytes(&mut session_key[..])
            .map_err(|_| CryptoError::BackendFailure)?;
        let mut initiator = Self {
            state: Some(state),
            identity: Some(identity),
            expected_peer,
            peer_identity: None,
            session_key: Some(SessionKey::new(session_key)),
        };
        let message_1 = initiator.write_message(&[])?;
        Ok((initiator, message_1))
    }

    pub fn write_message(&mut self, payload: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let state = self.state.as_mut().ok_or(CryptoError::InvalidState)?;
        write_handshake_message(state, payload)
    }

    pub fn receive(&mut self, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        ensure_bounded(message.len(), MAX_ENCRYPTED_MESSAGE_BYTES)?;
        let state = self.state.as_mut().ok_or(CryptoError::InvalidState)?;
        if state.is_my_turn() || state.is_handshake_finished() {
            return Err(CryptoError::InvalidState);
        }

        let transcript = state.get_handshake_hash().to_vec();
        let payload = read_handshake_message(state, message)?;
        let (peer_identity, signature) = decode_auth_payload(&payload)?;
        verify_peer(
            &peer_identity,
            Some(&self.expected_peer),
            RESPONDER_SIGNATURE_DOMAIN,
            &transcript,
            &signature,
        )?;

        let identity = self.identity.take().ok_or(CryptoError::InvalidState)?;
        let auth_payload = encode_auth_payload(
            &identity,
            INITIATOR_SIGNATURE_DOMAIN,
            state.get_handshake_hash(),
        );
        let mut initiator_payload = Zeroizing::new(Vec::with_capacity(INITIATOR_PAYLOAD_BYTES));
        initiator_payload.extend_from_slice(&auth_payload);
        initiator_payload.extend_from_slice(
            self.session_key
                .as_ref()
                .ok_or(CryptoError::InvalidState)?
                .as_bytes(),
        );
        let message_3 = write_handshake_message(state, &initiator_payload)?;
        self.peer_identity = Some(peer_identity);
        Ok(message_3)
    }

    pub fn finish(mut self) -> Result<TransportCipher, CryptoError> {
        let state = self.state.take().ok_or(CryptoError::InvalidState)?;
        finish_transport(state, self.peer_identity, self.session_key.take())
    }
}

pub struct NoiseResponder {
    state: Option<HandshakeState>,
    expected_peer: Option<VerifyingKey>,
    peer_identity: Option<PeerIdentity>,
    session_key: Option<SessionKey>,
}

impl NoiseResponder {
    pub fn accept(
        message_1: &[u8],
        identity: DeviceIdentity,
        expected_peer: VerifyingKey,
    ) -> Result<(Self, Vec<u8>), CryptoError> {
        Self::accept_inner(message_1, identity, Some(expected_peer))
    }

    /// Accepts a self-authenticated controller whose key is not trusted yet.
    /// Callers must restrict this to a live, one-time pairing invitation and
    /// persist trust only after explicit local approval.
    pub fn accept_pairing(
        message_1: &[u8],
        identity: DeviceIdentity,
    ) -> Result<(Self, Vec<u8>), CryptoError> {
        Self::accept_inner(message_1, identity, None)
    }

    fn accept_inner(
        message_1: &[u8],
        identity: DeviceIdentity,
        expected_peer: Option<VerifyingKey>,
    ) -> Result<(Self, Vec<u8>), CryptoError> {
        ensure_bounded(message_1.len(), MAX_ENCRYPTED_MESSAGE_BYTES)?;
        let mut state = build_handshake_state(false)?;
        let payload = read_handshake_message(&mut state, message_1)?;
        if !payload.is_empty() {
            return Err(CryptoError::MalformedHandshake);
        }

        let responder_payload = encode_auth_payload(
            &identity,
            RESPONDER_SIGNATURE_DOMAIN,
            state.get_handshake_hash(),
        );
        let message_2 = write_handshake_message(&mut state, &responder_payload)?;

        Ok((
            Self {
                state: Some(state),
                expected_peer,
                peer_identity: None,
                session_key: None,
            },
            message_2,
        ))
    }

    pub fn write_message(&mut self, payload: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let state = self.state.as_mut().ok_or(CryptoError::InvalidState)?;
        write_handshake_message(state, payload)
    }

    pub fn receive(&mut self, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        ensure_bounded(message.len(), MAX_ENCRYPTED_MESSAGE_BYTES)?;
        let state = self.state.as_mut().ok_or(CryptoError::InvalidState)?;
        if state.is_my_turn() || state.is_handshake_finished() {
            return Err(CryptoError::InvalidState);
        }

        let transcript = state.get_handshake_hash().to_vec();
        let payload = read_handshake_message(state, message)?;
        if payload.len() != INITIATOR_PAYLOAD_BYTES {
            return Err(CryptoError::MalformedHandshake);
        }
        let (auth_payload, key_payload) = payload.split_at(AUTH_PAYLOAD_BYTES);
        let (peer_identity, signature) = decode_auth_payload(auth_payload)?;
        verify_peer(
            &peer_identity,
            self.expected_peer.as_ref(),
            INITIATOR_SIGNATURE_DOMAIN,
            &transcript,
            &signature,
        )?;
        let mut session_key = Zeroizing::new([0; SESSION_KEY_BYTES]);
        session_key.copy_from_slice(key_payload);
        self.session_key = Some(SessionKey::new(session_key));
        self.peer_identity = Some(peer_identity);
        Ok(Vec::new())
    }

    pub fn finish(mut self) -> Result<TransportCipher, CryptoError> {
        let state = self.state.take().ok_or(CryptoError::InvalidState)?;
        finish_transport(state, self.peer_identity, self.session_key.take())
    }
}

pub struct TransportCipher {
    state: TransportState,
    peer_identity: PeerIdentity,
    session_key: SessionKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecureRole {
    Initiator,
    Responder,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SecureLane {
    Control = 0,
    Input = 1,
    VideoConfig = 2,
    VideoDatagram = 3,
    CursorDatagram = 4,
    Transfer = 5,
    AudioDatagram = 6,
}

impl SecureLane {
    const COUNT: usize = 7;

    const ALL: [Self; Self::COUNT] = [
        Self::Control,
        Self::Input,
        Self::VideoConfig,
        Self::VideoDatagram,
        Self::CursorDatagram,
        Self::Transfer,
        Self::AudioDatagram,
    ];

    const fn index(self) -> usize {
        self as usize
    }
}

/// Independent packet protection for DeskLink's parallel reliable streams and
/// unreliable datagrams. Noise authenticates the peers and exchanges the
/// session secret; this layer derives a distinct key for every direction and
/// lane so packet loss or cross-stream reordering cannot desynchronize crypto.
pub struct SecureSession {
    outbound: [PacketCipher; SecureLane::COUNT],
    inbound: [PacketCipher; SecureLane::COUNT],
    peer_identity: PeerIdentity,
}

impl SecureSession {
    pub fn seal(&mut self, lane: SecureLane, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.outbound[lane.index()].seal(plaintext)
    }

    pub fn open(&mut self, lane: SecureLane, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.inbound[lane.index()].open(ciphertext)
    }

    pub fn peer_verify_key(&self) -> VerifyingKey {
        self.peer_identity.verify_key()
    }

    pub const fn peer_identity(&self) -> PeerIdentity {
        self.peer_identity
    }
}

impl fmt::Debug for SecureSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecureSession")
            .field("keys", &"[REDACTED]")
            .field("peer_identity", &self.peer_identity)
            .finish()
    }
}

struct PacketCipher {
    key: Zeroizing<[u8; SESSION_KEY_BYTES]>,
    direction: u8,
    lane: SecureLane,
    next_sequence: u64,
    highest_received: u64,
    replay_window: u128,
}

impl PacketCipher {
    fn new(session_key: &SessionKey, direction: u8, lane: SecureLane) -> Self {
        let mut hasher = Blake2s256::new();
        hasher.update(PACKET_KEY_DOMAIN);
        hasher.update(session_key.as_bytes());
        hasher.update([direction, lane as u8]);
        let derived = hasher.finalize();
        let mut key = Zeroizing::new([0; SESSION_KEY_BYTES]);
        key.copy_from_slice(&derived);
        Self {
            key,
            direction,
            lane,
            next_sequence: 1,
            highest_received: 0,
            replay_window: 0,
        }
    }

    fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        ensure_bounded(plaintext.len(), MAX_PACKET_PLAINTEXT_BYTES)?;
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(CryptoError::InvalidState)?;
        let sequence_bytes = sequence.to_be_bytes();
        let nonce = packet_nonce(sequence);
        let aad = packet_aad(self.direction, self.lane, sequence_bytes);
        let cipher = ChaCha20Poly1305::new_from_slice(self.key.as_ref())
            .map_err(|_| CryptoError::BackendFailure)?;
        let encrypted = cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::BackendFailure)?;
        let mut packet = Vec::with_capacity(PACKET_SEQUENCE_BYTES + encrypted.len());
        packet.extend_from_slice(&sequence_bytes);
        packet.extend_from_slice(&encrypted);
        Ok(packet)
    }

    fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        ensure_bounded(ciphertext.len(), MAX_ENCRYPTED_MESSAGE_BYTES)?;
        if ciphertext.len() < PACKET_OVERHEAD_BYTES {
            return Err(CryptoError::AuthenticationFailed);
        }
        let sequence_bytes: [u8; PACKET_SEQUENCE_BYTES] = ciphertext[..PACKET_SEQUENCE_BYTES]
            .try_into()
            .map_err(|_| CryptoError::AuthenticationFailed)?;
        let sequence = u64::from_be_bytes(sequence_bytes);
        self.ensure_not_replayed(sequence)?;
        let nonce = packet_nonce(sequence);
        let aad = packet_aad(self.direction, self.lane, sequence_bytes);
        let cipher = ChaCha20Poly1305::new_from_slice(self.key.as_ref())
            .map_err(|_| CryptoError::BackendFailure)?;
        let plaintext = cipher
            .decrypt(
                (&nonce).into(),
                Payload {
                    msg: &ciphertext[PACKET_SEQUENCE_BYTES..],
                    aad: &aad,
                },
            )
            .map_err(|_| CryptoError::AuthenticationFailed)?;
        self.mark_received(sequence);
        Ok(plaintext)
    }

    fn ensure_not_replayed(&self, sequence: u64) -> Result<(), CryptoError> {
        if sequence == 0 {
            return Err(CryptoError::ReplayRejected);
        }
        if sequence > self.highest_received {
            return Ok(());
        }
        let distance = self.highest_received - sequence;
        if distance >= REPLAY_WINDOW_BITS || self.replay_window & (1_u128 << distance) != 0 {
            return Err(CryptoError::ReplayRejected);
        }
        Ok(())
    }

    fn mark_received(&mut self, sequence: u64) {
        if sequence > self.highest_received {
            let distance = sequence - self.highest_received;
            self.replay_window = if distance >= REPLAY_WINDOW_BITS {
                1
            } else {
                (self.replay_window << distance) | 1
            };
            self.highest_received = sequence;
        } else {
            self.replay_window |= 1_u128 << (self.highest_received - sequence);
        }
    }
}

fn packet_nonce(sequence: u64) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[..4].copy_from_slice(b"DLV1");
    nonce[4..].copy_from_slice(&sequence.to_be_bytes());
    nonce
}

fn packet_aad(direction: u8, lane: SecureLane, sequence: [u8; 8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(PACKET_AAD_DOMAIN.len() + 10);
    aad.extend_from_slice(PACKET_AAD_DOMAIN);
    aad.push(direction);
    aad.push(lane as u8);
    aad.extend_from_slice(&sequence);
    aad
}

impl TransportCipher {
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.encrypt_message(plaintext)
            .map(EncryptedMessage::into_bytes)
    }

    pub fn encrypt_message(&mut self, plaintext: &[u8]) -> Result<EncryptedMessage, CryptoError> {
        ensure_bounded(plaintext.len(), MAX_PLAINTEXT_BYTES)?;
        let mut output = vec![0; plaintext.len() + 16];
        let written = self
            .state
            .write_message(plaintext, &mut output)
            .map_err(map_transport_write_error)?;
        output.truncate(written);
        EncryptedMessage::try_from(output)
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        ensure_bounded(ciphertext.len(), MAX_ENCRYPTED_MESSAGE_BYTES)?;
        let mut output = vec![0; ciphertext.len()];
        let written = self
            .state
            .read_message(ciphertext, &mut output)
            .map_err(map_transport_read_error)?;
        output.truncate(written);
        Ok(output)
    }

    pub fn decrypt_message(
        &mut self,
        ciphertext: &EncryptedMessage,
    ) -> Result<Vec<u8>, CryptoError> {
        self.decrypt(ciphertext.as_ref())
    }

    /// Returns the zeroizing session secret exchanged inside authenticated
    /// Noise message 3. Snow's raw directional transport keys are never exposed.
    pub fn session_key(&self) -> &SessionKey {
        &self.session_key
    }

    pub fn peer_verify_key(&self) -> VerifyingKey {
        self.peer_identity.verify_key()
    }

    pub const fn peer_identity(&self) -> PeerIdentity {
        self.peer_identity
    }

    pub fn into_secure_session(self, role: SecureRole) -> SecureSession {
        let (outbound_direction, inbound_direction) = match role {
            SecureRole::Initiator => (0, 1),
            SecureRole::Responder => (1, 0),
        };
        let outbound = std::array::from_fn(|index| {
            PacketCipher::new(
                &self.session_key,
                outbound_direction,
                SecureLane::ALL[index],
            )
        });
        let inbound = std::array::from_fn(|index| {
            PacketCipher::new(&self.session_key, inbound_direction, SecureLane::ALL[index])
        });
        SecureSession {
            outbound,
            inbound,
            peer_identity: self.peer_identity,
        }
    }
}

impl fmt::Debug for TransportCipher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportCipher")
            .field("state", &"[REDACTED]")
            .field("peer_identity", &self.peer_identity)
            .field("session_key", &"[REDACTED]")
            .finish()
    }
}

fn build_handshake_state(initiator: bool) -> Result<HandshakeState, CryptoError> {
    let params: NoiseParams = NOISE_PATTERN
        .parse()
        .map_err(|_| CryptoError::BackendFailure)?;
    let builder = Builder::with_resolver(params, Box::new(DesklinkResolver));
    let keypair = builder
        .generate_keypair()
        .map_err(|_| CryptoError::BackendFailure)?;
    let private_key = keypair.private;
    let builder = builder.local_private_key(private_key.as_slice());
    if initiator {
        builder
            .build_initiator()
            .map_err(|_| CryptoError::BackendFailure)
    } else {
        builder
            .build_responder()
            .map_err(|_| CryptoError::BackendFailure)
    }
}

fn write_handshake_message(
    state: &mut HandshakeState,
    payload: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    ensure_bounded(payload.len(), MAX_HANDSHAKE_PAYLOAD_BYTES)?;
    let output_len = payload.len() + HANDSHAKE_OUTPUT_OVERHEAD;
    let mut output = vec![0; output_len];
    let written = state
        .write_message(payload, &mut output)
        .map_err(map_handshake_error)?;
    output.truncate(written);
    Ok(output)
}

fn read_handshake_message(
    state: &mut HandshakeState,
    message: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    ensure_bounded(message.len(), MAX_ENCRYPTED_MESSAGE_BYTES)?;
    let mut payload = Zeroizing::new(vec![0; message.len()]);
    let read = state
        .read_message(message, &mut payload)
        .map_err(map_handshake_error)?;
    payload.truncate(read);
    Ok(payload)
}

fn encode_auth_payload(
    identity: &DeviceIdentity,
    domain: &[u8],
    transcript: &[u8],
) -> Zeroizing<Vec<u8>> {
    let verify_key = identity.verify_key();
    let input = signature_input(domain, transcript, identity.device_id, &verify_key);
    let signature = identity.sign(&input);
    let mut payload = Zeroizing::new(Vec::with_capacity(AUTH_PAYLOAD_BYTES));
    payload.extend_from_slice(&identity.device_id);
    payload.extend_from_slice(verify_key.as_bytes());
    payload.extend_from_slice(&signature.to_bytes());
    payload
}

fn decode_auth_payload(payload: &[u8]) -> Result<(PeerIdentity, Signature), CryptoError> {
    if payload.len() != AUTH_PAYLOAD_BYTES {
        return Err(CryptoError::MalformedHandshake);
    }
    let device_id: [u8; DEVICE_ID_BYTES] = payload[..DEVICE_ID_BYTES]
        .try_into()
        .map_err(|_| CryptoError::MalformedHandshake)?;
    let verify_key_bytes: &[u8; 32] = payload[DEVICE_ID_BYTES..DEVICE_ID_BYTES + 32]
        .try_into()
        .map_err(|_| CryptoError::MalformedHandshake)?;
    let signature_bytes: &[u8; 64] = payload[DEVICE_ID_BYTES + 32..]
        .try_into()
        .map_err(|_| CryptoError::MalformedHandshake)?;
    let verify_key =
        VerifyingKey::from_bytes(verify_key_bytes).map_err(|_| CryptoError::InvalidSignature)?;
    Ok((
        PeerIdentity {
            device_id,
            verify_key,
        },
        Signature::from_bytes(signature_bytes),
    ))
}

fn verify_peer(
    peer: &PeerIdentity,
    expected_peer: Option<&VerifyingKey>,
    domain: &[u8],
    transcript: &[u8],
    signature: &Signature,
) -> Result<(), CryptoError> {
    if expected_peer.is_some_and(|expected| expected != &peer.verify_key)
        || peer
            .verify_key
            .verify_strict(
                &signature_input(domain, transcript, peer.device_id, &peer.verify_key),
                signature,
            )
            .is_err()
    {
        return Err(CryptoError::InvalidSignature);
    }
    Ok(())
}

fn signature_input(
    domain: &[u8],
    transcript: &[u8],
    device_id: [u8; DEVICE_ID_BYTES],
    verify_key: &VerifyingKey,
) -> Zeroizing<Vec<u8>> {
    let mut input = Zeroizing::new(Vec::with_capacity(
        domain.len() + transcript.len() + DEVICE_ID_BYTES + 32,
    ));
    input.extend_from_slice(domain);
    input.extend_from_slice(transcript);
    input.extend_from_slice(&device_id);
    input.extend_from_slice(verify_key.as_bytes());
    input
}

fn finish_transport(
    state: HandshakeState,
    peer_identity: Option<PeerIdentity>,
    session_key: Option<SessionKey>,
) -> Result<TransportCipher, CryptoError> {
    if !state.is_handshake_finished() {
        return Err(CryptoError::InvalidState);
    }
    let peer_identity = peer_identity.ok_or(CryptoError::InvalidState)?;
    let session_key = session_key.ok_or(CryptoError::InvalidState)?;
    let state = state.into_transport_mode().map_err(map_handshake_error)?;
    Ok(TransportCipher {
        state,
        peer_identity,
        session_key,
    })
}

fn ensure_bounded(actual: usize, maximum: usize) -> Result<(), CryptoError> {
    if actual > maximum {
        return Err(CryptoError::MessageTooLarge { actual, maximum });
    }
    Ok(())
}

fn map_handshake_error(error: snow::Error) -> CryptoError {
    match error {
        snow::Error::Decrypt => CryptoError::AuthenticationFailed,
        snow::Error::State(_) => CryptoError::InvalidState,
        snow::Error::Input | snow::Error::Dh => CryptoError::MalformedHandshake,
        _ => CryptoError::BackendFailure,
    }
}

fn map_transport_write_error(error: snow::Error) -> CryptoError {
    match error {
        snow::Error::State(_) => CryptoError::InvalidState,
        snow::Error::Input => CryptoError::MessageTooLarge {
            actual: MAX_PLAINTEXT_BYTES + 1,
            maximum: MAX_PLAINTEXT_BYTES,
        },
        _ => CryptoError::BackendFailure,
    }
}

fn map_transport_read_error(error: snow::Error) -> CryptoError {
    match error {
        snow::Error::Decrypt | snow::Error::Input => CryptoError::AuthenticationFailed,
        snow::Error::State(_) => CryptoError::InvalidState,
        _ => CryptoError::BackendFailure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_signature_binds_device_id_to_verify_key_and_transcript() {
        let identity = DeviceIdentity::from_secret_key([7; DEVICE_ID_BYTES], &[9; 32]);
        let transcript = b"noise-transcript";
        let payload = encode_auth_payload(&identity, INITIATOR_SIGNATURE_DOMAIN, transcript);
        let (mut peer, signature) = decode_auth_payload(&payload).unwrap();

        assert!(
            verify_peer(
                &peer,
                None,
                INITIATOR_SIGNATURE_DOMAIN,
                transcript,
                &signature
            )
            .is_ok()
        );
        peer.device_id[0] ^= 1;
        assert_eq!(
            verify_peer(
                &peer,
                None,
                INITIATOR_SIGNATURE_DOMAIN,
                transcript,
                &signature
            ),
            Err(CryptoError::InvalidSignature)
        );
    }
}
