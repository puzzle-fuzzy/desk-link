mod identity;
mod noise;
mod pairing;
mod resolver;

pub use identity::{DeviceIdentity, IdentityStore};
pub use noise::{
    CryptoError, EncryptedMessage, MAX_ENCRYPTED_MESSAGE_BYTES, MAX_HANDSHAKE_PAYLOAD_BYTES,
    MAX_PLAINTEXT_BYTES, NoiseInitiator, NoiseResponder, SessionKey, TransportCipher,
};
pub use pairing::{MAX_PAIRING_TTL_S, PairingCode, PairingError, PairingOffer, SessionId};

pub const PACKAGE_NAME: &str = "desklink-crypto";
