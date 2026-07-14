use std::{fmt, ops::Deref};

use ed25519_dalek::{Signature, VerifyingKey};
use rand_core::{OsRng, RngCore};
use snow::{Builder, HandshakeState, TransportState, params::NoiseParams};
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

use crate::identity::DeviceIdentity;
use crate::resolver::DesklinkResolver;

const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
const AUTH_PAYLOAD_BYTES: usize = 32 + 64;
const SESSION_KEY_BYTES: usize = 32;
const INITIATOR_PAYLOAD_BYTES: usize = AUTH_PAYLOAD_BYTES + SESSION_KEY_BYTES;
const HANDSHAKE_OUTPUT_OVERHEAD: usize = 256;
const RESPONDER_SIGNATURE_DOMAIN: &[u8] = b"desklink-noise-xx-responder-v1";
const INITIATOR_SIGNATURE_DOMAIN: &[u8] = b"desklink-noise-xx-initiator-v1";

pub const MAX_ENCRYPTED_MESSAGE_BYTES: usize = 65_535;
pub const MAX_PLAINTEXT_BYTES: usize = MAX_ENCRYPTED_MESSAGE_BYTES - 16;
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
    fn new(bytes: [u8; SESSION_KEY_BYTES]) -> Self {
        Self(Zeroizing::new(bytes))
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

pub struct NoiseInitiator {
    state: Option<HandshakeState>,
    identity: Option<DeviceIdentity>,
    expected_peer: VerifyingKey,
    peer_verify_key: Option<VerifyingKey>,
    session_key: Option<SessionKey>,
}

impl NoiseInitiator {
    pub fn start(
        identity: DeviceIdentity,
        expected_peer: VerifyingKey,
    ) -> Result<(Self, Vec<u8>), CryptoError> {
        let state = build_handshake_state(true)?;
        let mut session_key = [0; SESSION_KEY_BYTES];
        OsRng
            .try_fill_bytes(&mut session_key)
            .map_err(|_| CryptoError::BackendFailure)?;
        let mut initiator = Self {
            state: Some(state),
            identity: Some(identity),
            expected_peer,
            peer_verify_key: None,
            session_key: Some(SessionKey::new(session_key)),
        };
        session_key.zeroize();
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
        let (peer_verify_key, signature) = decode_auth_payload(&payload)?;
        verify_peer(
            &peer_verify_key,
            &self.expected_peer,
            RESPONDER_SIGNATURE_DOMAIN,
            &transcript,
            &signature,
        )?;

        let identity = self.identity.take().ok_or(CryptoError::InvalidState)?;
        let signature = identity.sign(&signature_input(
            INITIATOR_SIGNATURE_DOMAIN,
            state.get_handshake_hash(),
        ));
        let mut initiator_payload = Zeroizing::new(Vec::with_capacity(INITIATOR_PAYLOAD_BYTES));
        initiator_payload.extend_from_slice(identity.verify_key().as_bytes());
        initiator_payload.extend_from_slice(&signature.to_bytes());
        initiator_payload.extend_from_slice(
            self.session_key
                .as_ref()
                .ok_or(CryptoError::InvalidState)?
                .as_bytes(),
        );
        let message_3 = write_handshake_message(state, &initiator_payload)?;
        self.peer_verify_key = Some(peer_verify_key);
        Ok(message_3)
    }

    pub fn finish(mut self) -> Result<TransportCipher, CryptoError> {
        let state = self.state.take().ok_or(CryptoError::InvalidState)?;
        finish_transport(state, self.peer_verify_key, self.session_key.take())
    }
}

pub struct NoiseResponder {
    state: Option<HandshakeState>,
    expected_peer: VerifyingKey,
    peer_verify_key: Option<VerifyingKey>,
    session_key: Option<SessionKey>,
}

impl NoiseResponder {
    pub fn accept(
        message_1: &[u8],
        identity: DeviceIdentity,
        expected_peer: VerifyingKey,
    ) -> Result<(Self, Vec<u8>), CryptoError> {
        ensure_bounded(message_1.len(), MAX_ENCRYPTED_MESSAGE_BYTES)?;
        let mut state = build_handshake_state(false)?;
        let payload = read_handshake_message(&mut state, message_1)?;
        if !payload.is_empty() {
            return Err(CryptoError::MalformedHandshake);
        }

        let signature = identity.sign(&signature_input(
            RESPONDER_SIGNATURE_DOMAIN,
            state.get_handshake_hash(),
        ));
        let mut responder_payload = Vec::with_capacity(AUTH_PAYLOAD_BYTES);
        responder_payload.extend_from_slice(identity.verify_key().as_bytes());
        responder_payload.extend_from_slice(&signature.to_bytes());
        let message_2 = write_handshake_message(&mut state, &responder_payload)?;
        responder_payload.zeroize();

        Ok((
            Self {
                state: Some(state),
                expected_peer,
                peer_verify_key: None,
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
        let payload = Zeroizing::new(read_handshake_message(state, message)?);
        if payload.len() != INITIATOR_PAYLOAD_BYTES {
            return Err(CryptoError::MalformedHandshake);
        }
        let (auth_payload, key_payload) = payload.split_at(AUTH_PAYLOAD_BYTES);
        let (peer_verify_key, signature) = decode_auth_payload(auth_payload)?;
        verify_peer(
            &peer_verify_key,
            &self.expected_peer,
            INITIATOR_SIGNATURE_DOMAIN,
            &transcript,
            &signature,
        )?;
        let mut session_key = [0; SESSION_KEY_BYTES];
        session_key.copy_from_slice(key_payload);
        self.session_key = Some(SessionKey::new(session_key));
        session_key.zeroize();
        self.peer_verify_key = Some(peer_verify_key);
        Ok(Vec::new())
    }

    pub fn finish(mut self) -> Result<TransportCipher, CryptoError> {
        let state = self.state.take().ok_or(CryptoError::InvalidState)?;
        finish_transport(state, self.peer_verify_key, self.session_key.take())
    }
}

pub struct TransportCipher {
    state: TransportState,
    peer_verify_key: VerifyingKey,
    session_key: SessionKey,
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
        self.peer_verify_key
    }
}

impl fmt::Debug for TransportCipher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportCipher")
            .field("state", &"[REDACTED]")
            .field("peer_verify_key", &self.peer_verify_key)
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
    let private_key = Zeroizing::new(keypair.private);
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
) -> Result<Vec<u8>, CryptoError> {
    ensure_bounded(message.len(), MAX_ENCRYPTED_MESSAGE_BYTES)?;
    let mut payload = vec![0; message.len()];
    let read = state
        .read_message(message, &mut payload)
        .map_err(map_handshake_error)?;
    payload.truncate(read);
    Ok(payload)
}

fn decode_auth_payload(payload: &[u8]) -> Result<(VerifyingKey, Signature), CryptoError> {
    if payload.len() != AUTH_PAYLOAD_BYTES {
        return Err(CryptoError::MalformedHandshake);
    }
    let verify_key_bytes: &[u8; 32] = payload[..32]
        .try_into()
        .map_err(|_| CryptoError::MalformedHandshake)?;
    let signature_bytes: &[u8; 64] = payload[32..]
        .try_into()
        .map_err(|_| CryptoError::MalformedHandshake)?;
    let verify_key =
        VerifyingKey::from_bytes(verify_key_bytes).map_err(|_| CryptoError::InvalidSignature)?;
    Ok((verify_key, Signature::from_bytes(signature_bytes)))
}

fn verify_peer(
    peer: &VerifyingKey,
    expected_peer: &VerifyingKey,
    domain: &[u8],
    transcript: &[u8],
    signature: &Signature,
) -> Result<(), CryptoError> {
    if expected_peer != peer
        || peer
            .verify_strict(&signature_input(domain, transcript), signature)
            .is_err()
    {
        return Err(CryptoError::InvalidSignature);
    }
    Ok(())
}

fn signature_input(domain: &[u8], transcript: &[u8]) -> Vec<u8> {
    let mut input = Vec::with_capacity(domain.len() + transcript.len());
    input.extend_from_slice(domain);
    input.extend_from_slice(transcript);
    input
}

fn finish_transport(
    state: HandshakeState,
    peer_verify_key: Option<VerifyingKey>,
    session_key: Option<SessionKey>,
) -> Result<TransportCipher, CryptoError> {
    if !state.is_handshake_finished() {
        return Err(CryptoError::InvalidState);
    }
    let peer_verify_key = peer_verify_key.ok_or(CryptoError::InvalidState)?;
    let session_key = session_key.ok_or(CryptoError::InvalidState)?;
    let state = state.into_transport_mode().map_err(map_handshake_error)?;
    Ok(TransportCipher {
        state,
        peer_verify_key,
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
