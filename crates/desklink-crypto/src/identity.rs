use std::fmt;

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::CryptoRngCore;

pub trait IdentityStore {
    type Error;

    /// Implementations must keep private key material in platform-protected local storage.
    fn load(&self) -> Result<Option<DeviceIdentity>, Self::Error>;
    /// Implementations must never log or place the identity in protocol messages.
    fn save(&self, identity: &DeviceIdentity) -> Result<(), Self::Error>;
}

pub struct DeviceIdentity {
    pub device_id: [u8; 16],
    signing_key: SigningKey,
}

impl DeviceIdentity {
    pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
        let mut device_id = [0; 16];
        rng.fill_bytes(&mut device_id);
        let signing_key = SigningKey::generate(rng);
        Self {
            device_id,
            signing_key,
        }
    }

    pub fn from_secret_key(device_id: [u8; 16], secret_key: &[u8; 32]) -> Self {
        Self {
            device_id,
            signing_key: SigningKey::from_bytes(secret_key),
        }
    }

    pub fn verify_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    pub fn sign(&self, payload: &[u8]) -> Signature {
        self.signing_key.sign(payload)
    }

    pub fn verify(&self, payload: &[u8], signature: &Signature) -> bool {
        self.verify_key().verify(payload, signature).is_ok()
    }

    pub fn with_secret_key_bytes<T>(&self, use_secret: impl FnOnce(&[u8; 32]) -> T) -> T {
        use_secret(self.signing_key.as_bytes())
    }
}

impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceIdentity")
            .field("device_id", &self.device_id)
            .field("signing_key", &"[REDACTED]")
            .finish()
    }
}
