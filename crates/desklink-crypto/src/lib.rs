mod identity;
mod noise;
mod pairing;
mod resolver;

pub use identity::{DeviceIdentity, IdentityStore};
pub use noise::{
    CryptoError, EncryptedMessage, MAX_ENCRYPTED_MESSAGE_BYTES, MAX_HANDSHAKE_PAYLOAD_BYTES,
    MAX_PACKET_PLAINTEXT_BYTES, MAX_PLAINTEXT_BYTES, NoiseInitiator, NoiseResponder, PeerIdentity,
    SecureLane, SecureRole, SecureSession, SessionKey, TransportCipher,
};
pub use pairing::{
    EncodedPairingInvite, MAX_PAIRING_TTL_S, PAIRING_INVITE_BYTES, PairingCode, PairingError,
    PairingInvite, PairingOffer, SessionId,
};

pub const PACKAGE_NAME: &str = "desklink-crypto";
