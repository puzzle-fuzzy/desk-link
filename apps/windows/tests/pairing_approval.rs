use apps_windows::window::{
    ApprovalState, PairingApprovalError, PairingApprovalGate, controller_approval_prompt,
    controller_revocation_prompt,
};
use desklink_crypto::{
    DeviceIdentity, NoiseInitiator, NoiseResponder, PairingError, PairingInvite, PeerIdentity,
};

fn authenticated_controller(seed: u8) -> (DeviceIdentity, PeerIdentity) {
    let controller = DeviceIdentity::from_secret_key([seed; 16], &[seed.wrapping_add(1); 32]);
    let host =
        DeviceIdentity::from_secret_key([seed.wrapping_add(2); 16], &[seed.wrapping_add(3); 32]);
    let host_verify_key = host.verify_key();
    let (mut initiator, message_1) = NoiseInitiator::start(controller, host_verify_key).unwrap();
    let (mut responder, message_2) = NoiseResponder::accept_pairing(&message_1, host).unwrap();
    let message_3 = initiator.receive(&message_2).unwrap();
    responder.receive(&message_3).unwrap();
    let peer = responder.finish().unwrap().peer_identity();
    let controller = DeviceIdentity::from_secret_key([seed; 16], &[seed.wrapping_add(1); 32]);
    (controller, peer)
}

#[test]
fn approval_binds_and_consumes_one_time_invitation() {
    let host = DeviceIdentity::from_secret_key([1; 16], &[2; 32]);
    let (_, peer) = authenticated_controller(10);
    let mut invite = PairingInvite::new(&host, 1_000, 60).unwrap();
    let mut gate = PairingApprovalGate::new();

    let pending = gate.begin(&mut invite, peer, 1_001).unwrap();

    assert_eq!(pending.identity(), peer);
    assert_eq!(pending.session_id(), invite.session_id());
    assert_eq!(pending.expires_at_unix_s(), 1_060);
    assert_eq!(pending.verification_fingerprint().len(), 95);
    assert_eq!(gate.pending(), Some(pending));
    assert!(matches!(
        invite.consume(1_002),
        Err(PairingError::AlreadyConsumed)
    ));
    assert!(matches!(
        gate.begin(&mut invite, peer, 1_002),
        Err(PairingApprovalError::InvalidState)
    ));
}

#[test]
fn approval_produces_trust_only_for_the_identity_that_was_displayed() {
    let host = DeviceIdentity::from_secret_key([20; 16], &[21; 32]);
    let (controller, peer) = authenticated_controller(30);
    let (_, changed_peer) = authenticated_controller(40);
    let mut invite = PairingInvite::new(&host, 2_000, 60).unwrap();
    let mut gate = PairingApprovalGate::new();
    gate.begin(&mut invite, peer, 2_001).unwrap();

    assert!(matches!(
        gate.approve(changed_peer, 2_002),
        Err(PairingApprovalError::ControllerChanged)
    ));
    assert_eq!(gate.state(), ApprovalState::Waiting);
    let approved = gate.approve(peer, 2_002).unwrap();
    assert_eq!(approved.device_id(), controller.device_id);
    assert_eq!(approved.verify_key(), controller.verify_key());
    assert_eq!(approved.approved_at_unix_s(), 2_002);
    assert_eq!(gate.state(), ApprovalState::Accepted);
    assert!(matches!(
        gate.approve(peer, 2_003),
        Err(PairingApprovalError::InvalidState)
    ));
}

#[test]
fn expired_or_rejected_requests_never_produce_trust() {
    let host = DeviceIdentity::from_secret_key([50; 16], &[51; 32]);
    let (_, peer) = authenticated_controller(60);
    let mut expired_invite = PairingInvite::new(&host, 3_000, 10).unwrap();
    let mut expired_gate = PairingApprovalGate::new();
    expired_gate
        .begin(&mut expired_invite, peer, 3_001)
        .unwrap();
    assert!(matches!(
        expired_gate.approve(peer, 3_010),
        Err(PairingApprovalError::Expired)
    ));
    assert_eq!(expired_gate.state(), ApprovalState::Rejected);

    let mut rejected_invite = PairingInvite::new(&host, 4_000, 10).unwrap();
    let mut rejected_gate = PairingApprovalGate::new();
    rejected_gate
        .begin(&mut rejected_invite, peer, 4_001)
        .unwrap();
    rejected_gate.reject(peer).unwrap();
    assert_eq!(rejected_gate.state(), ApprovalState::Rejected);
    assert!(matches!(
        rejected_gate.approve(peer, 4_002),
        Err(PairingApprovalError::InvalidState)
    ));
}

#[test]
fn approval_prompt_contains_the_full_authenticated_identity_and_defaults_to_no_secret() {
    let host = DeviceIdentity::from_secret_key([70; 16], &[71; 32]);
    let (_, peer) = authenticated_controller(72);
    let mut invite = PairingInvite::new(&host, 5_000, 60).unwrap();
    let mut gate = PairingApprovalGate::new();
    let pending = gate.begin(&mut invite, peer, 5_001).unwrap();

    let prompt = controller_approval_prompt(pending);

    assert!(prompt.contains(&pending.verification_fingerprint()));
    assert!(prompt.contains("48:48:48:48:48:48:48:48:48:48:48:48:48:48:48:48"));
    assert!(prompt.contains("Ed25519 public-key fingerprint"));
    assert!(prompt.contains("screen viewing and input control"));
    assert!(!prompt.contains("relay"));
}

#[test]
fn revocation_prompt_identifies_the_exact_key_and_explains_repairing() {
    let controller = DeviceIdentity::from_secret_key([80; 16], &[81; 32]);
    let prompt = controller_revocation_prompt(controller.device_id, controller.verify_key());

    assert!(prompt.contains("50:50:50:50:50:50:50:50:50:50:50:50:50:50:50:50"));
    assert!(prompt.contains("Ed25519 public-key fingerprint"));
    assert!(prompt.contains("paired and approved again"));
}
