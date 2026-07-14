use std::{cell::RefCell, convert::Infallible};

use desklink_crypto::{
    CryptoError, DeviceIdentity, EncryptedMessage, IdentityStore, MAX_ENCRYPTED_MESSAGE_BYTES,
    MAX_PLAINTEXT_BYTES, NoiseInitiator, NoiseResponder, PairingError, PairingOffer, SessionId,
};
use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

#[test]
fn identity_signature_verifies_only_for_original_payload() {
    let identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([7; 32]));
    let signature = identity.sign(b"desklink-handshake");

    assert!(identity.verify(b"desklink-handshake", &signature));
    assert!(!identity.verify(b"changed", &signature));
}

#[test]
fn identity_store_can_persist_and_restore_key_material() {
    struct MemoryStore(RefCell<Option<([u8; 16], [u8; 32])>>);

    impl IdentityStore for MemoryStore {
        type Error = Infallible;

        fn load(&self) -> Result<Option<DeviceIdentity>, Self::Error> {
            Ok(self
                .0
                .borrow()
                .as_ref()
                .map(|(device_id, secret)| DeviceIdentity::from_secret_key(*device_id, secret)))
        }

        fn save(&self, identity: &DeviceIdentity) -> Result<(), Self::Error> {
            identity.with_secret_key_bytes(|secret| {
                self.0.replace(Some((identity.device_id, *secret)));
            });
            Ok(())
        }
    }

    let store = MemoryStore(RefCell::new(None));
    let identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([9; 32]));
    let expected_device_id = identity.device_id;
    let expected_verify_key = identity.verify_key();
    store.save(&identity).unwrap();
    drop(identity);

    let restored = store.load().unwrap().unwrap();
    assert_eq!(restored.device_id, expected_device_id);
    assert_eq!(restored.verify_key(), expected_verify_key);
}

#[test]
fn pairing_code_expires_and_cannot_be_reused() {
    let mut rng = ChaCha20Rng::from_seed([11; 32]);
    let mut offer =
        PairingOffer::new_with_rng(SessionId::from_bytes([1; 16]), 1_000, 600, &mut rng);
    let code = offer.code().to_string();

    assert!(offer.validate_code(&code, 1_599).is_ok());
    assert!(matches!(
        offer.validate_code(&code, 1_600),
        Err(PairingError::Expired)
    ));
    offer.consume_code(&code, 1_599).unwrap();
    assert!(matches!(
        offer.consume(1_599),
        Err(PairingError::AlreadyConsumed)
    ));
    assert!(matches!(
        offer.validate_code(&code, 1_599),
        Err(PairingError::AlreadyConsumed)
    ));
}

#[test]
fn pairing_code_is_fixed_length_unambiguous_and_checked_before_consuming() {
    let mut rng = ChaCha20Rng::from_seed([12; 32]);
    let mut offer = PairingOffer::new_with_rng(SessionId::from_bytes([2; 16]), 100, 30, &mut rng);
    let code = offer.code().to_string();

    assert_eq!(code.len(), 8);
    assert!(
        code.bytes()
            .all(|byte| byte.is_ascii_uppercase() || (b'2'..=b'9').contains(&byte))
    );
    assert!(!code.bytes().any(|byte| b"01IO".contains(&byte)));
    assert_eq!(offer.session_id(), SessionId::from_bytes([2; 16]));
    assert!(matches!(
        offer.consume_code("AAAAAAAA", 101),
        Err(PairingError::InvalidCode)
    ));
    assert!(offer.consume_code(&code, 101).is_ok());
}

#[test]
fn pairing_expiry_saturates_instead_of_wrapping() {
    let mut rng = ChaCha20Rng::from_seed([13; 32]);
    let offer =
        PairingOffer::new_with_rng(SessionId::from_bytes([3; 16]), u64::MAX - 5, 10, &mut rng);

    assert_eq!(offer.expires_at_unix_s(), u64::MAX);
    assert!(
        offer
            .validate_code(&offer.code().to_string(), u64::MAX - 1)
            .is_ok()
    );
}

#[test]
fn noise_initiator_and_responder_produce_same_session_key() {
    let (mut initiator, message_1) = NoiseInitiator::start().unwrap();
    let (mut responder, message_2) = NoiseResponder::accept(&message_1).unwrap();
    let message_3 = initiator.receive(&message_2).unwrap();
    responder.receive(&message_3).unwrap();

    assert_eq!(
        initiator.finish().unwrap().session_key(),
        responder.finish().unwrap().session_key()
    );
}

#[test]
fn authenticated_noise_handshake_binds_both_device_identities() {
    let initiator_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([21; 32]));
    let responder_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([22; 32]));
    let initiator_verify_key = initiator_identity.verify_key();
    let responder_verify_key = responder_identity.verify_key();

    let (mut initiator, message_1) =
        NoiseInitiator::start_authenticated(initiator_identity, Some(responder_verify_key))
            .unwrap();
    let (mut responder, message_2) = NoiseResponder::accept_authenticated(
        &message_1,
        responder_identity,
        Some(initiator_verify_key),
    )
    .unwrap();
    let message_3 = initiator.receive(&message_2).unwrap();
    responder.receive(&message_3).unwrap();

    let initiator = initiator.finish().unwrap();
    let responder = responder.finish().unwrap();
    assert_eq!(initiator.peer_verify_key(), responder_verify_key);
    assert_eq!(responder.peer_verify_key(), initiator_verify_key);
    assert_eq!(initiator.session_key(), responder.session_key());
}

#[test]
fn noise_transport_round_trips_in_both_directions() {
    let (mut initiator, mut responder) = connected_transport();

    let encrypted = initiator.encrypt(b"controller input").unwrap();
    assert_ne!(encrypted, b"controller input");
    assert_eq!(responder.decrypt(&encrypted).unwrap(), b"controller input");

    let encrypted = responder.encrypt(b"host frame metadata").unwrap();
    assert_eq!(
        initiator.decrypt(&encrypted).unwrap(),
        b"host frame metadata"
    );
}

#[test]
fn noise_transport_rejects_tampering_and_oversized_payloads() {
    let (mut initiator, mut responder) = connected_transport();
    let mut encrypted = initiator.encrypt(b"authenticated payload").unwrap();
    let last = encrypted.last_mut().unwrap();
    *last ^= 0x80;

    assert!(matches!(
        responder.decrypt(&encrypted),
        Err(CryptoError::AuthenticationFailed)
    ));
    assert!(matches!(
        initiator.encrypt(&vec![0; MAX_PLAINTEXT_BYTES + 1]),
        Err(CryptoError::MessageTooLarge { .. })
    ));
    assert!(matches!(
        responder.decrypt(&vec![0; MAX_ENCRYPTED_MESSAGE_BYTES + 1]),
        Err(CryptoError::MessageTooLarge { .. })
    ));
    assert!(EncryptedMessage::try_from(vec![0; MAX_ENCRYPTED_MESSAGE_BYTES + 1]).is_err());
}

#[test]
fn noise_handshake_returns_stable_errors_for_bad_state_messages_and_identity() {
    let (initiator, _) = NoiseInitiator::start().unwrap();
    assert!(matches!(initiator.finish(), Err(CryptoError::InvalidState)));
    assert!(matches!(
        NoiseResponder::accept(&[1, 2, 3]),
        Err(CryptoError::MalformedHandshake)
    ));
    assert!(matches!(
        NoiseResponder::accept(&vec![0; MAX_ENCRYPTED_MESSAGE_BYTES + 1]),
        Err(CryptoError::MessageTooLarge { .. })
    ));

    let initiator_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([31; 32]));
    let responder_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([32; 32]));
    let unexpected_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([33; 32]));
    let (mut initiator, message_1) = NoiseInitiator::start_authenticated(
        initiator_identity,
        Some(unexpected_identity.verify_key()),
    )
    .unwrap();
    let (_, message_2) =
        NoiseResponder::accept_authenticated(&message_1, responder_identity, None).unwrap();
    assert!(matches!(
        initiator.receive(&message_2),
        Err(CryptoError::InvalidSignature)
    ));
}

fn connected_transport() -> (
    desklink_crypto::TransportCipher,
    desklink_crypto::TransportCipher,
) {
    let (mut initiator, message_1) = NoiseInitiator::start().unwrap();
    let (mut responder, message_2) = NoiseResponder::accept(&message_1).unwrap();
    let message_3 = initiator.receive(&message_2).unwrap();
    responder.receive(&message_3).unwrap();
    (initiator.finish().unwrap(), responder.finish().unwrap())
}
