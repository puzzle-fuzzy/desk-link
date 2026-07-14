use std::fmt;

use rand_core::{CryptoRngCore, OsRng};
use subtle::ConstantTimeEq;
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

const PAIRING_CODE_LENGTH: usize = 8;
const PAIRING_CODE_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
pub const MAX_PAIRING_TTL_S: u64 = 600;

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
    #[error("pairing code has expired")]
    Expired,
    #[error("pairing offer has already been consumed")]
    AlreadyConsumed,
    #[error("pairing code is invalid")]
    InvalidCode,
    #[error("pairing TTL must be between 1 and 600 seconds")]
    InvalidTtl,
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
        if !(1..=MAX_PAIRING_TTL_S).contains(&ttl_s) {
            return Err(PairingError::InvalidTtl);
        }
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
