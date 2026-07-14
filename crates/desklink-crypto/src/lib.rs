mod identity;
mod noise;
mod pairing;

pub use identity::{DeviceIdentity, IdentityStore};
pub use noise::{
    CryptoError, EncryptedMessage, MAX_ENCRYPTED_MESSAGE_BYTES, MAX_PLAINTEXT_BYTES,
    NoiseInitiator, NoiseResponder, SessionKey, TransportCipher,
};
pub use pairing::{PairingCode, PairingError, PairingOffer, SessionId};

pub const PACKAGE_NAME: &str = "desklink-crypto";
