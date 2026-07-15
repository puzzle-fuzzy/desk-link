use std::fmt;

use ed25519_dalek::{Signature, VerifyingKey};
use rand_core::{CryptoRngCore, OsRng};
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

const PAIRING_CODE_LENGTH: usize = 8;
const PAIRING_CODE_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
const PAIRING_INVITE_MAGIC: &[u8; 4] = b"DLPI";
const PAIRING_INVITE_VERSION: u8 = 1;
const PAIRING_INVITE_SIGNED_BYTES: usize = 117;
const PAIRING_INVITE_SIGNATURE_DOMAIN: &[u8] = b"desklink-pairing-invite-v1";
const RELAY_AUTHENTICATION_BYTES: usize = 32;
pub const MAX_PAIRING_TTL_S: u64 = 600;
pub const PAIRING_INVITE_BYTES: usize = PAIRING_INVITE_SIGNED_BYTES + 64;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionId([u8; 16]);

impl SessionId {
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct PairingCode {
    bytes: [u8; PAIRING_CODE_LENGTH],
}

impl PairingCode {
    fn generate(rng: &mut impl CryptoRngCore) -> Self {
        let mut random = Zeroizing::new([0; PAIRING_CODE_LENGTH]);
        rng.fill_bytes(&mut random[..]);
        let mut bytes = [0; PAIRING_CODE_LENGTH];
        for (output, random_byte) in bytes.iter_mut().zip(random.iter().copied()) {
            *output = PAIRING_CODE_ALPHABET[usize::from(random_byte & 31)];
        }
        Self { bytes }
    }

    pub fn as_bytes(&self) -> &[u8; PAIRING_CODE_LENGTH] {
        &self.bytes
    }
}

impl fmt::Display for PairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = std::str::from_utf8(&self.bytes).map_err(|_| fmt::Error)?;
        formatter.write_str(code)
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairingCode([REDACTED])")
    }
}

impl Drop for PairingCode {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum PairingError {
    #[error("pairing data has expired")]
    Expired,
    #[error("pairing offer has already been consumed")]
    AlreadyConsumed,
    #[error("pairing code is invalid")]
    InvalidCode,
    #[error("pairing TTL must be between 1 and 600 seconds")]
    InvalidTtl,
    #[error("pairing invite is malformed or failed authentication")]
    InvalidInvite,
}

pub struct EncodedPairingInvite {
    bytes: Zeroizing<[u8; PAIRING_INVITE_BYTES]>,
}

impl EncodedPairingInvite {
    pub fn as_bytes(&self) -> &[u8; PAIRING_INVITE_BYTES] {
        &self.bytes
    }
}

impl fmt::Debug for EncodedPairingInvite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EncodedPairingInvite([REDACTED])")
    }
}

pub struct PairingInvite {
    session_id: SessionId,
    relay_authentication: Zeroizing<[u8; RELAY_AUTHENTICATION_BYTES]>,
    host_device_id: [u8; 16],
    host_verify_key: VerifyingKey,
    created_at_unix_s: u64,
    expires_at_unix_s: u64,
    signature: Signature,
    consumed: bool,
}

impl PairingInvite {
    pub fn new(
        host: &crate::DeviceIdentity,
        now_unix_s: u64,
        ttl_s: u64,
    ) -> Result<Self, PairingError> {
        Self::new_with_rng(host, now_unix_s, ttl_s, &mut OsRng)
    }

    pub fn new_with_rng(
        host: &crate::DeviceIdentity,
        now_unix_s: u64,
        ttl_s: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<Self, PairingError> {
        validate_ttl(ttl_s)?;
        let mut session_id = [0; 16];
        rng.fill_bytes(&mut session_id);
        let mut relay_authentication = Zeroizing::new([0; RELAY_AUTHENTICATION_BYTES]);
        rng.fill_bytes(&mut relay_authentication[..]);
        let expires_at_unix_s = now_unix_s.saturating_add(ttl_s);
        let host_verify_key = host.verify_key();
        let unsigned = encode_unsigned_invite(
            SessionId::from_bytes(session_id),
            &relay_authentication,
            host.device_id,
            host_verify_key,
            now_unix_s,
            expires_at_unix_s,
        );
        let signature_input = invite_signature_input(&unsigned);
        Ok(Self {
            session_id: SessionId::from_bytes(session_id),
            relay_authentication,
            host_device_id: host.device_id,
            host_verify_key,
            created_at_unix_s: now_unix_s,
            expires_at_unix_s,
            signature: host.sign(&signature_input),
            consumed: false,
        })
    }

    pub fn decode(bytes: &[u8], now_unix_s: u64) -> Result<Self, PairingError> {
        if bytes.len() != PAIRING_INVITE_BYTES
            || &bytes[..4] != PAIRING_INVITE_MAGIC
            || bytes[4] != PAIRING_INVITE_VERSION
        {
            return Err(PairingError::InvalidInvite);
        }
        let session_id = SessionId::from_bytes(
            bytes[5..21]
                .try_into()
                .map_err(|_| PairingError::InvalidInvite)?,
        );
        let relay_authentication = Zeroizing::new(
            bytes[21..53]
                .try_into()
                .map_err(|_| PairingError::InvalidInvite)?,
        );
        let host_device_id = bytes[53..69]
            .try_into()
            .map_err(|_| PairingError::InvalidInvite)?;
        let host_verify_key_bytes = bytes[69..101]
            .try_into()
            .map_err(|_| PairingError::InvalidInvite)?;
        let host_verify_key = VerifyingKey::from_bytes(host_verify_key_bytes)
            .map_err(|_| PairingError::InvalidInvite)?;
        let created_at_unix_s = u64::from_be_bytes(
            bytes[101..109]
                .try_into()
                .map_err(|_| PairingError::InvalidInvite)?,
        );
        let expires_at_unix_s = u64::from_be_bytes(
            bytes[109..117]
                .try_into()
                .map_err(|_| PairingError::InvalidInvite)?,
        );
        let signature = Signature::from_bytes(
            bytes[PAIRING_INVITE_SIGNED_BYTES..]
                .try_into()
                .map_err(|_| PairingError::InvalidInvite)?,
        );
        let signature_input = invite_signature_input(&bytes[..PAIRING_INVITE_SIGNED_BYTES]);
        host_verify_key
            .verify_strict(&signature_input, &signature)
            .map_err(|_| PairingError::InvalidInvite)?;
        validate_window(created_at_unix_s, expires_at_unix_s, now_unix_s)?;
        Ok(Self {
            session_id,
            relay_authentication,
            host_device_id,
            host_verify_key,
            created_at_unix_s,
            expires_at_unix_s,
            signature,
            consumed: false,
        })
    }

    pub fn encode(&self) -> Result<EncodedPairingInvite, PairingError> {
        if self.consumed {
            return Err(PairingError::AlreadyConsumed);
        }
        let unsigned = encode_unsigned_invite(
            self.session_id,
            &self.relay_authentication,
            self.host_device_id,
            self.host_verify_key,
            self.created_at_unix_s,
            self.expires_at_unix_s,
        );
        let mut bytes = Zeroizing::new([0; PAIRING_INVITE_BYTES]);
        bytes[..PAIRING_INVITE_SIGNED_BYTES].copy_from_slice(&unsigned);
        bytes[PAIRING_INVITE_SIGNED_BYTES..].copy_from_slice(&self.signature.to_bytes());
        Ok(EncodedPairingInvite { bytes })
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn relay_authentication(&self) -> &[u8; RELAY_AUTHENTICATION_BYTES] {
        &self.relay_authentication
    }

    pub fn host_device_id(&self) -> [u8; 16] {
        self.host_device_id
    }

    pub fn host_verify_key(&self) -> VerifyingKey {
        self.host_verify_key
    }

    pub fn expires_at_unix_s(&self) -> u64 {
        self.expires_at_unix_s
    }

    pub fn created_at_unix_s(&self) -> u64 {
        self.created_at_unix_s
    }

    pub fn consume(&mut self, now_unix_s: u64) -> Result<(), PairingError> {
        self.ensure_active(now_unix_s)?;
        self.consumed = true;
        Ok(())
    }

    fn ensure_active(&self, now_unix_s: u64) -> Result<(), PairingError> {
        if self.consumed {
            return Err(PairingError::AlreadyConsumed);
        }
        validate_window(self.created_at_unix_s, self.expires_at_unix_s, now_unix_s)
    }
}

impl fmt::Debug for PairingInvite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingInvite")
            .field("session_id", &self.session_id)
            .field("relay_authentication", &"[REDACTED]")
            .field("host_device_id", &self.host_device_id)
            .field("host_verify_key", &self.host_verify_key)
            .field("created_at_unix_s", &self.created_at_unix_s)
            .field("expires_at_unix_s", &self.expires_at_unix_s)
            .field("signature", &"[REDACTED]")
            .field("consumed", &self.consumed)
            .finish()
    }
}

fn encode_unsigned_invite(
    session_id: SessionId,
    relay_authentication: &[u8; RELAY_AUTHENTICATION_BYTES],
    host_device_id: [u8; 16],
    host_verify_key: VerifyingKey,
    created_at_unix_s: u64,
    expires_at_unix_s: u64,
) -> [u8; PAIRING_INVITE_SIGNED_BYTES] {
    let mut bytes = [0; PAIRING_INVITE_SIGNED_BYTES];
    bytes[..4].copy_from_slice(PAIRING_INVITE_MAGIC);
    bytes[4] = PAIRING_INVITE_VERSION;
    bytes[5..21].copy_from_slice(session_id.as_bytes());
    bytes[21..53].copy_from_slice(relay_authentication);
    bytes[53..69].copy_from_slice(&host_device_id);
    bytes[69..101].copy_from_slice(host_verify_key.as_bytes());
    bytes[101..109].copy_from_slice(&created_at_unix_s.to_be_bytes());
    bytes[109..117].copy_from_slice(&expires_at_unix_s.to_be_bytes());
    bytes
}

fn invite_signature_input(unsigned: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut input = Zeroizing::new(Vec::with_capacity(
        PAIRING_INVITE_SIGNATURE_DOMAIN.len() + unsigned.len(),
    ));
    input.extend_from_slice(PAIRING_INVITE_SIGNATURE_DOMAIN);
    input.extend_from_slice(unsigned);
    input
}

fn validate_ttl(ttl_s: u64) -> Result<(), PairingError> {
    if (1..=MAX_PAIRING_TTL_S).contains(&ttl_s) {
        Ok(())
    } else {
        Err(PairingError::InvalidTtl)
    }
}

fn validate_window(
    created_at_unix_s: u64,
    expires_at_unix_s: u64,
    now_unix_s: u64,
) -> Result<(), PairingError> {
    let ttl_s = expires_at_unix_s.saturating_sub(created_at_unix_s);
    if !(1..=MAX_PAIRING_TTL_S).contains(&ttl_s) {
        return Err(PairingError::InvalidInvite);
    }
    if now_unix_s >= expires_at_unix_s {
        return Err(PairingError::Expired);
    }
    Ok(())
}

pub struct PairingOffer {
    session_id: SessionId,
    code: PairingCode,
    expires_at_unix_s: u64,
    consumed: bool,
}

impl PairingOffer {
    pub fn new(session_id: SessionId, now_unix_s: u64, ttl_s: u64) -> Result<Self, PairingError> {
        Self::new_with_rng(session_id, now_unix_s, ttl_s, &mut OsRng)
    }

    pub fn new_with_rng(
        session_id: SessionId,
        now_unix_s: u64,
        ttl_s: u64,
        rng: &mut impl CryptoRngCore,
    ) -> Result<Self, PairingError> {
        validate_ttl(ttl_s)?;
        Ok(Self {
            session_id,
            code: PairingCode::generate(rng),
            expires_at_unix_s: now_unix_s.saturating_add(ttl_s),
            consumed: false,
        })
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn code(&self) -> PairingCode {
        self.code.clone()
    }

    pub fn expires_at_unix_s(&self) -> u64 {
        self.expires_at_unix_s
    }

    pub fn validate_code(&self, code: &str, now_unix_s: u64) -> Result<(), PairingError> {
        self.ensure_active(now_unix_s)?;
        if code.len() != PAIRING_CODE_LENGTH
            || !bool::from(self.code.as_bytes().ct_eq(code.as_bytes()))
        {
            return Err(PairingError::InvalidCode);
        }
        Ok(())
    }

    pub fn consume(&mut self, now_unix_s: u64) -> Result<(), PairingError> {
        self.ensure_active(now_unix_s)?;
        self.consumed = true;
        Ok(())
    }

    pub fn consume_code(&mut self, code: &str, now_unix_s: u64) -> Result<(), PairingError> {
        self.validate_code(code, now_unix_s)?;
        self.consumed = true;
        Ok(())
    }

    fn ensure_active(&self, now_unix_s: u64) -> Result<(), PairingError> {
        if self.consumed {
            return Err(PairingError::AlreadyConsumed);
        }
        if now_unix_s >= self.expires_at_unix_s {
            return Err(PairingError::Expired);
        }
        Ok(())
    }
}

impl fmt::Debug for PairingOffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingOffer")
            .field("session_id", &self.session_id)
            .field("code", &"[REDACTED]")
            .field("expires_at_unix_s", &self.expires_at_unix_s)
            .field("consumed", &self.consumed)
            .finish()
    }
}
