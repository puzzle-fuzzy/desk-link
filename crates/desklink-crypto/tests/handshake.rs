use std::{cell::RefCell, convert::Infallible};

use desklink_crypto::{
    CryptoError, DeviceIdentity, EncryptedMessage, IdentityStore, MAX_ENCRYPTED_MESSAGE_BYTES,
    MAX_HANDSHAKE_PAYLOAD_BYTES, MAX_PAIRING_TTL_S, MAX_PLAINTEXT_BYTES, NoiseInitiator,
    NoiseResponder, PAIRING_INVITE_BYTES, PairingError, PairingInvite, PairingOffer, SecureLane,
    SecureRole, SessionId,
};
use rand_chacha::ChaCha20Rng;
use rand_core::{CryptoRng, RngCore, SeedableRng};

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
    let mut offer = PairingOffer::new_with_rng(
        SessionId::from_bytes([1; 16]),
        1_000,
        MAX_PAIRING_TTL_S,
        &mut rng,
    )
    .unwrap();
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
    let mut offer =
        PairingOffer::new_with_rng(SessionId::from_bytes([2; 16]), 100, 30, &mut rng).unwrap();
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
        PairingOffer::new_with_rng(SessionId::from_bytes([3; 16]), u64::MAX - 5, 10, &mut rng)
            .unwrap();

    assert_eq!(offer.expires_at_unix_s(), u64::MAX);
    assert!(
        offer
            .validate_code(&offer.code().to_string(), u64::MAX - 1)
            .is_ok()
    );
}

#[test]
fn pairing_ttl_must_be_between_one_and_ten_minutes() {
    let mut rng = ChaCha20Rng::from_seed([14; 32]);
    let session = SessionId::from_bytes([4; 16]);

    assert!(matches!(
        PairingOffer::new_with_rng(session, 1_000, 0, &mut rng),
        Err(PairingError::InvalidTtl)
    ));
    assert!(PairingOffer::new_with_rng(session, 1_000, MAX_PAIRING_TTL_S, &mut rng).is_ok());
    assert!(matches!(
        PairingOffer::new_with_rng(session, 1_000, MAX_PAIRING_TTL_S + 1, &mut rng),
        Err(PairingError::InvalidTtl)
    ));
}

#[test]
fn invalid_pairing_ttl_does_not_consume_randomness() {
    #[derive(Default)]
    struct CountingRng {
        bytes_requested: usize,
    }

    impl RngCore for CountingRng {
        fn next_u32(&mut self) -> u32 {
            self.bytes_requested += 4;
            0
        }

        fn next_u64(&mut self) -> u64 {
            self.bytes_requested += 8;
            0
        }

        fn fill_bytes(&mut self, destination: &mut [u8]) {
            self.bytes_requested += destination.len();
            destination.fill(0);
        }

        fn try_fill_bytes(&mut self, destination: &mut [u8]) -> Result<(), rand_core::Error> {
            self.fill_bytes(destination);
            Ok(())
        }
    }

    impl CryptoRng for CountingRng {}

    let mut rng = CountingRng::default();
    let session = SessionId::from_bytes([8; 16]);

    assert!(matches!(
        PairingOffer::new_with_rng(session, 1_000, 0, &mut rng),
        Err(PairingError::InvalidTtl)
    ));
    assert!(matches!(
        PairingOffer::new_with_rng(session, 1_000, MAX_PAIRING_TTL_S + 1, &mut rng),
        Err(PairingError::InvalidTtl)
    ));
    assert_eq!(rng.bytes_requested, 0);
}

#[test]
fn pairing_invite_round_trips_fixed_credentials_and_redacts_debug_output() {
    let host = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([15; 32]));
    let mut invite_rng = ChaCha20Rng::from_seed([16; 32]);
    let invite = PairingInvite::new_with_rng(&host, 1_000, 300, &mut invite_rng).unwrap();
    let encoded = invite.encode().unwrap();

    assert_eq!(encoded.as_bytes().len(), PAIRING_INVITE_BYTES);
    let decoded = PairingInvite::decode(encoded.as_bytes(), 1_001).unwrap();
    assert_eq!(decoded.session_id(), invite.session_id());
    assert_eq!(
        decoded.relay_authentication(),
        invite.relay_authentication()
    );
    assert_eq!(decoded.host_device_id(), host.device_id);
    assert_eq!(decoded.host_verify_key(), host.verify_key());
    assert_eq!(decoded.created_at_unix_s(), 1_000);
    assert_eq!(decoded.expires_at_unix_s(), 1_300);
    assert!(PairingInvite::decode(encoded.as_bytes(), 999).is_ok());

    let debug = format!("{invite:?} {encoded:?}");
    assert!(!debug.contains(&hex::encode(invite.relay_authentication())));
    assert!(debug.contains("[REDACTED]"));
}

#[test]
fn pairing_invite_rejects_tampering_wrong_size_and_expiry() {
    let host = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([17; 32]));
    let invite =
        PairingInvite::new_with_rng(&host, 2_000, 60, &mut ChaCha20Rng::from_seed([18; 32]))
            .unwrap();
    let encoded = invite.encode().unwrap();
    let original = encoded.as_bytes();

    for index in [0, 4, 8, 24, 56, 72, 104, 112, PAIRING_INVITE_BYTES - 1] {
        let mut tampered = original.to_vec();
        tampered[index] ^= 1;
        assert!(matches!(
            PairingInvite::decode(&tampered, 2_001),
            Err(PairingError::InvalidInvite)
        ));
    }
    assert!(matches!(
        PairingInvite::decode(&original[..PAIRING_INVITE_BYTES - 1], 2_001),
        Err(PairingError::InvalidInvite)
    ));
    let mut extended = original.to_vec();
    extended.push(0);
    assert!(matches!(
        PairingInvite::decode(&extended, 2_001),
        Err(PairingError::InvalidInvite)
    ));
    assert!(matches!(
        PairingInvite::decode(original, 2_060),
        Err(PairingError::Expired)
    ));
}

#[test]
fn pairing_invite_consumption_is_one_time_and_has_independent_relay_entropy() {
    let host = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([23; 32]));
    let mut rng = ChaCha20Rng::from_seed([24; 32]);
    let offer =
        PairingOffer::new_with_rng(SessionId::from_bytes([1; 16]), 3_000, 60, &mut rng).unwrap();
    let mut invite = PairingInvite::new_with_rng(&host, 3_000, 60, &mut rng).unwrap();

    assert_eq!(offer.code().as_bytes().len(), 8);
    assert_eq!(invite.relay_authentication().len(), 32);
    assert_ne!(
        &invite.relay_authentication()[..8],
        offer.code().as_bytes(),
        "the display code must not be reused as relay authentication"
    );
    invite.consume(3_001).unwrap();
    assert!(matches!(
        invite.consume(3_001),
        Err(PairingError::AlreadyConsumed)
    ));
    assert!(matches!(
        invite.encode(),
        Err(PairingError::AlreadyConsumed)
    ));
}

#[test]
fn noise_initiator_and_responder_produce_same_session_key() {
    let initiator_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([19; 32]));
    let responder_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([20; 32]));
    let initiator_verify_key = initiator_identity.verify_key();
    let responder_verify_key = responder_identity.verify_key();
    let (mut initiator, message_1) =
        NoiseInitiator::start(initiator_identity, responder_verify_key).unwrap();
    let (mut responder, message_2) =
        NoiseResponder::accept(&message_1, responder_identity, initiator_verify_key).unwrap();
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
    let initiator_device_id = initiator_identity.device_id;
    let responder_device_id = responder_identity.device_id;
    let initiator_verify_key = initiator_identity.verify_key();
    let responder_verify_key = responder_identity.verify_key();

    let (mut initiator, message_1) =
        NoiseInitiator::start(initiator_identity, responder_verify_key).unwrap();
    let (mut responder, message_2) =
        NoiseResponder::accept(&message_1, responder_identity, initiator_verify_key).unwrap();
    let message_3 = initiator.receive(&message_2).unwrap();
    responder.receive(&message_3).unwrap();

    let initiator = initiator.finish().unwrap();
    let responder = responder.finish().unwrap();
    assert_eq!(initiator.peer_verify_key(), responder_verify_key);
    assert_eq!(responder.peer_verify_key(), initiator_verify_key);
    assert_eq!(initiator.peer_identity().device_id(), responder_device_id);
    assert_eq!(responder.peer_identity().device_id(), initiator_device_id);
    assert_eq!(initiator.session_key(), responder.session_key());
}

#[test]
fn pairing_noise_accepts_an_unknown_self_authenticated_controller() {
    let controller = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([25; 32]));
    let host = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([26; 32]));
    let controller_device_id = controller.device_id;
    let controller_verify_key = controller.verify_key();
    let host_verify_key = host.verify_key();

    let (mut initiator, message_1) = NoiseInitiator::start(controller, host_verify_key).unwrap();
    let (mut responder, message_2) = NoiseResponder::accept_pairing(&message_1, host).unwrap();
    let message_3 = initiator.receive(&message_2).unwrap();
    responder.receive(&message_3).unwrap();

    let responder = responder.finish().unwrap();
    assert_eq!(responder.peer_verify_key(), controller_verify_key);
    assert_eq!(responder.peer_identity().device_id(), controller_device_id);
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
fn noise_transport_accepts_exact_maximum_plaintext_and_ciphertext() {
    let (mut initiator, mut responder) = connected_transport();
    let plaintext = vec![0x5a; MAX_PLAINTEXT_BYTES];

    let ciphertext = initiator.encrypt(&plaintext).unwrap();

    assert_eq!(ciphertext.len(), MAX_ENCRYPTED_MESSAGE_BYTES);
    assert_eq!(responder.decrypt(&ciphertext).unwrap(), plaintext);
    assert!(EncryptedMessage::try_from(vec![0; MAX_ENCRYPTED_MESSAGE_BYTES]).is_ok());
}

#[test]
fn secure_session_isolates_lanes_and_accepts_datagram_reordering() {
    let (initiator, responder) = connected_transport();
    let mut initiator = initiator.into_secure_session(SecureRole::Initiator);
    let mut responder = responder.into_secure_session(SecureRole::Responder);

    let first = initiator.seal(SecureLane::VideoDatagram, b"first").unwrap();
    let second = initiator
        .seal(SecureLane::VideoDatagram, b"second")
        .unwrap();
    let control = initiator.seal(SecureLane::Control, b"control").unwrap();

    assert_eq!(
        responder.open(SecureLane::VideoDatagram, &second).unwrap(),
        b"second"
    );
    assert_eq!(
        responder.open(SecureLane::VideoDatagram, &first).unwrap(),
        b"first"
    );
    assert_eq!(
        responder.open(SecureLane::Control, &control).unwrap(),
        b"control"
    );
}

#[test]
fn secure_session_rejects_replay_tampering_and_wrong_lane() {
    let (initiator, responder) = connected_transport();
    let mut initiator = initiator.into_secure_session(SecureRole::Initiator);
    let mut responder = responder.into_secure_session(SecureRole::Responder);
    let packet = initiator.seal(SecureLane::Input, b"input").unwrap();

    assert!(matches!(
        responder.open(SecureLane::Control, &packet),
        Err(CryptoError::AuthenticationFailed)
    ));
    assert_eq!(
        responder.open(SecureLane::Input, &packet).unwrap(),
        b"input"
    );
    assert!(matches!(
        responder.open(SecureLane::Input, &packet),
        Err(CryptoError::ReplayRejected)
    ));

    let mut tampered = initiator.seal(SecureLane::Input, b"next").unwrap();
    *tampered.last_mut().unwrap() ^= 1;
    assert!(matches!(
        responder.open(SecureLane::Input, &tampered),
        Err(CryptoError::AuthenticationFailed)
    ));
}

#[test]
fn handshake_receive_paths_reject_over_max_and_process_exact_max() {
    let (mut initiator, mut responder, _) = connected_handshake_states(51, 52);
    let oversized = vec![0; MAX_ENCRYPTED_MESSAGE_BYTES + 1];

    assert!(matches!(
        initiator.receive(&oversized),
        Err(CryptoError::MessageTooLarge {
            actual,
            maximum: MAX_ENCRYPTED_MESSAGE_BYTES
        }) if actual == MAX_ENCRYPTED_MESSAGE_BYTES + 1
    ));
    assert!(matches!(
        responder.receive(&oversized),
        Err(CryptoError::MessageTooLarge {
            actual,
            maximum: MAX_ENCRYPTED_MESSAGE_BYTES
        }) if actual == MAX_ENCRYPTED_MESSAGE_BYTES + 1
    ));

    let exact_maximum = vec![0; MAX_ENCRYPTED_MESSAGE_BYTES];
    assert_eq!(
        initiator.receive(&exact_maximum).unwrap_err(),
        CryptoError::MalformedHandshake
    );
    assert_eq!(
        responder.receive(&exact_maximum).unwrap_err(),
        CryptoError::AuthenticationFailed
    );
}

#[test]
fn public_handshake_writes_enforce_payload_boundary_before_state() {
    let (mut initiator, mut responder, _) = connected_handshake_states(53, 54);
    let oversized = vec![0; MAX_HANDSHAKE_PAYLOAD_BYTES + 1];

    assert!(matches!(
        initiator.write_message(&oversized),
        Err(CryptoError::MessageTooLarge {
            actual,
            maximum: MAX_HANDSHAKE_PAYLOAD_BYTES
        }) if actual == MAX_HANDSHAKE_PAYLOAD_BYTES + 1
    ));
    assert!(matches!(
        responder.write_message(&oversized),
        Err(CryptoError::MessageTooLarge {
            actual,
            maximum: MAX_HANDSHAKE_PAYLOAD_BYTES
        }) if actual == MAX_HANDSHAKE_PAYLOAD_BYTES + 1
    ));

    let exact_maximum = vec![0; MAX_HANDSHAKE_PAYLOAD_BYTES];
    assert!(matches!(
        initiator.write_message(&exact_maximum),
        Err(CryptoError::InvalidState)
    ));
    assert!(matches!(
        responder.write_message(&exact_maximum),
        Err(CryptoError::InvalidState)
    ));
}

#[test]
fn noise_handshake_returns_stable_errors_for_bad_state_messages_and_identity() {
    let initiator_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([27; 32]));
    let responder_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([28; 32]));
    let initiator_verify_key = initiator_identity.verify_key();
    let responder_verify_key = responder_identity.verify_key();
    let (initiator, _) = NoiseInitiator::start(initiator_identity, responder_verify_key).unwrap();
    assert!(matches!(initiator.finish(), Err(CryptoError::InvalidState)));
    assert!(matches!(
        NoiseResponder::accept(&[1, 2, 3], responder_identity, initiator_verify_key),
        Err(CryptoError::MalformedHandshake)
    ));
    let initiator_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([29; 32]));
    let responder_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([30; 32]));
    assert!(matches!(
        NoiseResponder::accept(
            &vec![0; MAX_ENCRYPTED_MESSAGE_BYTES + 1],
            responder_identity,
            initiator_identity.verify_key()
        ),
        Err(CryptoError::MessageTooLarge { .. })
    ));

    let initiator_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([31; 32]));
    let responder_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([32; 32]));
    let unexpected_identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([33; 32]));
    let initiator_verify_key = initiator_identity.verify_key();
    let (mut initiator, message_1) =
        NoiseInitiator::start(initiator_identity, unexpected_identity.verify_key()).unwrap();
    let (_, message_2) =
        NoiseResponder::accept(&message_1, responder_identity, initiator_verify_key).unwrap();
    assert!(matches!(
        initiator.receive(&message_2),
        Err(CryptoError::InvalidSignature)
    ));
}

fn connected_transport() -> (
    desklink_crypto::TransportCipher,
    desklink_crypto::TransportCipher,
) {
    let (mut initiator, mut responder, message_2) = connected_handshake_states(41, 42);
    let message_3 = initiator.receive(&message_2).unwrap();
    responder.receive(&message_3).unwrap();
    (initiator.finish().unwrap(), responder.finish().unwrap())
}

fn connected_handshake_states(
    initiator_seed: u8,
    responder_seed: u8,
) -> (NoiseInitiator, NoiseResponder, Vec<u8>) {
    let initiator_identity =
        DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([initiator_seed; 32]));
    let responder_identity =
        DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([responder_seed; 32]));
    let initiator_verify_key = initiator_identity.verify_key();
    let responder_verify_key = responder_identity.verify_key();
    let (initiator, message_1) =
        NoiseInitiator::start(initiator_identity, responder_verify_key).unwrap();
    let (responder, message_2) =
        NoiseResponder::accept(&message_1, responder_identity, initiator_verify_key).unwrap();
    (initiator, responder, message_2)
}
