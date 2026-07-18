#[cfg(windows)]
mod windows {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    use apps_windows::{
        runtime::{ControllerAuthorization, ControllerAuthorizer},
        trusted::{
            LocalControllerApproval, TrustStatus, WindowsControllerAuthorizer,
            WindowsPairingAuthorizer, WindowsPersistentAccessAuthorizer,
            WindowsTrustedControllerError, WindowsTrustedControllerStore,
        },
        window::{PairingApprovalGate, PendingController},
    };
    use desklink_crypto::{
        DeviceIdentity, NoiseInitiator, NoiseResponder, PairingInvite, PeerIdentity, SessionId,
    };

    fn authenticated_peer(device_id: [u8; 16], secret: [u8; 32], seed: u8) -> PeerIdentity {
        let controller = DeviceIdentity::from_secret_key(device_id, &secret);
        let host = DeviceIdentity::from_secret_key([seed; 16], &[seed.wrapping_add(1); 32]);
        let host_verify_key = host.verify_key();
        let (mut initiator, message_1) =
            NoiseInitiator::start(controller, host_verify_key).unwrap();
        let (mut responder, message_2) = NoiseResponder::accept_pairing(&message_1, host).unwrap();
        let message_3 = initiator.receive(&message_2).unwrap();
        responder.receive(&message_3).unwrap();
        responder.finish().unwrap().peer_identity()
    }

    fn approve(peer: PeerIdentity, now_unix_s: u64) -> apps_windows::window::ApprovedController {
        let host = DeviceIdentity::from_secret_key([91; 16], &[92; 32]);
        let mut invite = PairingInvite::new(&host, now_unix_s, 60).unwrap();
        let mut gate = PairingApprovalGate::new();
        let displayed = gate.begin(&mut invite, peer, now_unix_s).unwrap();
        gate.approve(displayed.identity(), now_unix_s).unwrap()
    }

    fn temporary_store(name: &str) -> (std::path::PathBuf, WindowsTrustedControllerStore) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "desklink-trusted-{name}-{}-{unique}",
            std::process::id()
        ));
        let store = WindowsTrustedControllerStore::new(directory.join("controllers.bin"));
        (directory, store)
    }

    #[test]
    fn dpapi_store_persists_approved_identity_and_revokes_atomically() {
        let (directory, store) = temporary_store("round-trip");
        let peer = authenticated_peer([1; 16], [2; 32], 3);
        let approved = approve(peer, 1_000);

        let trusted = store.trust(approved).unwrap();
        let protected = std::fs::read(store.path()).unwrap();
        assert!(!protected.windows(16).any(|bytes| bytes == [1; 16]));
        assert!(
            !protected
                .windows(32)
                .any(|bytes| bytes == peer.verify_key().as_bytes())
        );
        assert!(!store.path().with_extension("tmp").exists());
        assert_eq!(store.status(peer).unwrap(), TrustStatus::Trusted(trusted));
        assert_eq!(store.list().unwrap(), vec![trusted]);

        assert!(store.revoke(trusted.fingerprint()).unwrap());
        assert_eq!(store.status(peer).unwrap(), TrustStatus::Unknown);
        assert!(!store.revoke(trusted.fingerprint()).unwrap());
        assert!(!store.path().with_extension("tmp").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn device_id_key_replacement_requires_fresh_approval() {
        let (directory, store) = temporary_store("replacement");
        let original = authenticated_peer([10; 16], [11; 32], 12);
        let replacement = authenticated_peer([10; 16], [13; 32], 14);
        let original_record = store.trust(approve(original, 2_000)).unwrap();

        assert_eq!(
            store.status(replacement).unwrap(),
            TrustStatus::KeyChanged {
                trusted: original_record
            }
        );
        assert!(matches!(
            store.trust(approve(replacement, 2_001)),
            Err(WindowsTrustedControllerError::IdentityConflict)
        ));

        let replacement_record = store
            .replace_after_approval(approve(replacement, 2_002))
            .unwrap();
        assert_eq!(
            store.status(replacement).unwrap(),
            TrustStatus::Trusted(replacement_record)
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn corrupted_protected_store_fails_closed() {
        let (directory, store) = temporary_store("corrupt");
        let peer = authenticated_peer([20; 16], [21; 32], 22);
        store.trust(approve(peer, 3_000)).unwrap();
        std::fs::write(store.path(), b"not a DPAPI payload").unwrap();

        assert!(matches!(
            store.status(peer),
            Err(WindowsTrustedControllerError::CorruptProtectedData)
        ));
        assert!(matches!(
            store.list(),
            Err(WindowsTrustedControllerError::CorruptProtectedData)
        ));
        std::fs::remove_dir_all(directory).unwrap();
    }

    struct RecordingApproval {
        accept: bool,
        calls: AtomicUsize,
        identity_changes: AtomicUsize,
    }

    struct SharedApproval(Arc<RecordingApproval>);

    impl LocalControllerApproval for SharedApproval {
        fn approve(&self, pending: PendingController) -> bool {
            self.0.calls.fetch_add(1, Ordering::Relaxed);
            if pending.identity_changed() {
                self.0.identity_changes.fetch_add(1, Ordering::Relaxed);
            }
            self.0.accept
        }
    }

    #[test]
    fn live_pairing_authorizer_promotes_only_local_approval_then_reuses_trust() {
        let (directory, store) = temporary_store("pairing-authorizer");
        let peer = authenticated_peer([30; 16], [31; 32], 32);
        let host = DeviceIdentity::from_secret_key([33; 16], &[34; 32]);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let invite = PairingInvite::new(&host, now, 60).unwrap();
        let approval = Arc::new(RecordingApproval {
            accept: true,
            calls: AtomicUsize::new(0),
            identity_changes: AtomicUsize::new(0),
        });
        let authorizer = WindowsPairingAuthorizer::new(
            store.clone(),
            invite,
            Box::new(SharedApproval(approval.clone())),
        );

        assert_eq!(
            authorizer.authorize(peer).unwrap(),
            ControllerAuthorization::Authorized
        );
        assert_eq!(approval.calls.load(Ordering::Relaxed), 1);
        assert!(matches!(
            store.status(peer).unwrap(),
            TrustStatus::Trusted(_)
        ));
        assert_eq!(
            authorizer.authorize(peer).unwrap(),
            ControllerAuthorization::Authorized
        );
        assert_eq!(approval.calls.load(Ordering::Relaxed), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejection_and_development_fallback_never_persist_silent_trust() {
        let (directory, store) = temporary_store("authorization-fallback");
        let peer = authenticated_peer([40; 16], [41; 32], 42);
        let other = authenticated_peer([43; 16], [44; 32], 45);
        let host = DeviceIdentity::from_secret_key([46; 16], &[47; 32]);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let rejected = WindowsPairingAuthorizer::new(
            store.clone(),
            PairingInvite::new(&host, now, 60).unwrap(),
            Box::new(SharedApproval(Arc::new(RecordingApproval {
                accept: false,
                calls: AtomicUsize::new(0),
                identity_changes: AtomicUsize::new(0),
            }))),
        );
        assert_eq!(
            rejected.authorize(peer).unwrap(),
            ControllerAuthorization::Rejected
        );
        assert_eq!(store.status(peer).unwrap(), TrustStatus::Unknown);

        let fallback = WindowsControllerAuthorizer::with_development_fallback(
            store.clone(),
            peer.verify_key(),
        );
        assert_eq!(
            fallback.authorize(peer).unwrap(),
            ControllerAuthorization::Authorized
        );
        assert_eq!(
            fallback.authorize(other).unwrap(),
            ControllerAuthorization::Unknown
        );
        assert!(store.list().unwrap().is_empty());
        if directory.exists() {
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn fixed_password_access_prompts_before_replacing_a_changed_controller_key() {
        let (directory, store) = temporary_store("persistent-key-change");
        let original = authenticated_peer([50; 16], [51; 32], 52);
        let replacement = authenticated_peer([50; 16], [53; 32], 54);
        store.trust(approve(original, 4_000)).unwrap();
        let approval = Arc::new(RecordingApproval {
            accept: true,
            calls: AtomicUsize::new(0),
            identity_changes: AtomicUsize::new(0),
        });
        let authorizer = WindowsPersistentAccessAuthorizer::new(
            store.clone(),
            SessionId::from_bytes([55; 16]),
            Box::new(SharedApproval(approval.clone())),
        );

        assert_eq!(
            authorizer.authorize(replacement).unwrap(),
            ControllerAuthorization::Authorized
        );
        assert_eq!(approval.calls.load(Ordering::Relaxed), 1);
        assert_eq!(approval.identity_changes.load(Ordering::Relaxed), 1);
        assert!(matches!(
            store.status(replacement).unwrap(),
            TrustStatus::Trusted(_)
        ));
        assert_eq!(store.list().unwrap().len(), 1);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
