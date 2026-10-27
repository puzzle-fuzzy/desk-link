use desklink_crypto::{
    DeviceIdentity, EncodedPairingInvite, MAX_PAIRING_TTL_S, PairingCode, PairingInvite, SessionId,
};
use desklink_transport::{RelayDirectoryRegistration, RelayJoin};
use rand_core::{OsRng, RngCore};
use zeroize::Zeroizing;

use crate::host::HostError;

/// Matches the deployed Windows directory's stable twelve-digit device number.
pub fn public_host_device_id(identity_device_id: [u8; 16]) -> u64 {
    let seed = u64::from_be_bytes(identity_device_id[..8].try_into().expect("fixed ID prefix"));
    100_000_000_000 + seed % 900_000_000_000
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostAccessMode {
    Temporary,
    Fixed,
}

/// This value intentionally does not implement Debug: its invitation grants access
/// to the authenticated Noise handshake, and must never enter diagnostics.
pub struct HostDirectoryCredentials {
    pub invitation: EncodedPairingInvite,
    pub expires_at_unix_s: u64,
}

pub(crate) struct HostDirectorySession {
    registration: RelayDirectoryRegistration,
    expires_at_unix_s: Option<u64>,
}

impl HostDirectorySession {
    pub(crate) fn prepare(
        identity: &DeviceIdentity,
        mode: HostAccessMode,
        password: &[u8],
        now_unix_s: u64,
    ) -> Result<(Self, PairingInvite, HostDirectoryCredentials), HostError> {
        // Any password bytes of sufficient length are accepted; the directory
        // code is derived so the controller typing the same password matches.
        let code = PairingCode::derive_from_password(password)
            .map_err(|_| HostError::InvalidAccessCode)?;
        let invite = match mode {
            HostAccessMode::Temporary => {
                PairingInvite::new(identity, now_unix_s, MAX_PAIRING_TTL_S)
                    .map_err(|_| HostError::InvalidAccessCode)?
            }
            HostAccessMode::Fixed => {
                let mut session_id = [0; 16];
                let mut authentication = Zeroizing::new([0; 32]);
                OsRng.fill_bytes(&mut session_id);
                OsRng.fill_bytes(&mut authentication[..]);
                PairingInvite::for_persistent_connection(
                    identity,
                    SessionId::from_bytes(session_id),
                    *authentication,
                )
            }
        };
        let invitation = invite.encode().map_err(|_| HostError::InvalidAccessCode)?;
        let expires = invite.expires_at_unix_s();
        let registration = RelayDirectoryRegistration::new(
            public_host_device_id(identity.device_id),
            *code.as_bytes(),
            invitation.as_bytes().to_vec(),
            if expires == 0 {
                0
            } else {
                MAX_PAIRING_TTL_S as u16
            },
        )
        .map_err(|_| HostError::InvalidAccessCode)?;
        Ok((
            Self {
                registration,
                expires_at_unix_s: (expires != 0).then_some(expires),
            },
            invite,
            HostDirectoryCredentials {
                invitation,
                expires_at_unix_s: expires,
            },
        ))
    }

    pub(crate) const fn expires_at_unix_s(&self) -> Option<u64> {
        self.expires_at_unix_s
    }

    pub(crate) fn decorate_join(
        &self,
        join: RelayJoin,
        now_unix_s: u64,
    ) -> Result<RelayJoin, HostError> {
        let ttl = match self.expires_at_unix_s {
            Some(expiry) => {
                let remaining = expiry.saturating_sub(now_unix_s);
                if remaining == 0 {
                    return Err(HostError::AccessExpired);
                }
                remaining.min(MAX_PAIRING_TTL_S) as u16
            }
            None => 0,
        };
        let registration = RelayDirectoryRegistration::new(
            self.registration.device_id(),
            *self.registration.access_code(),
            self.registration.invitation().to_vec(),
            ttl,
        )
        .map_err(|_| HostError::InvalidAccessCode)?;
        join.with_directory_registration(registration)
            .map_err(|_| HostError::InvalidAccessCode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_public_device_number_matches_windows_and_is_not_a_secret() {
        assert_eq!(public_host_device_id([7; 16]), 809_866_536_711);
        for first in [0, 1, 127, 255] {
            let number = public_host_device_id([first; 16]);
            assert!((100_000_000_000..1_000_000_000_000).contains(&number));
        }
    }

    #[test]
    fn temporary_registration_reconnect_never_extends_signed_invitation_lifetime() {
        let identity = DeviceIdentity::from_secret_key([7; 16], &[8; 32]);
        let (directory, invite, credentials) =
            HostDirectorySession::prepare(&identity, HostAccessMode::Temporary, b"ABCDEFGH", 1_000)
                .unwrap();
        assert_eq!(credentials.expires_at_unix_s, 1_600);
        let join = || {
            RelayJoin::host_with_participant(
                invite.session_id(),
                *invite.relay_authentication(),
                identity.device_id,
            )
        };
        assert_eq!(
            directory
                .decorate_join(join(), 1_599)
                .unwrap()
                .directory_registration()
                .unwrap()
                .ttl_s(),
            1
        );
        assert!(matches!(
            directory.decorate_join(join(), 1_600),
            Err(HostError::AccessExpired)
        ));
        assert!(format!("{:?}", directory.registration).contains("[REDACTED]"));
    }

    #[test]
    fn fixed_registration_lives_only_on_its_connection_and_new_start_rotates_invitation() {
        let identity = DeviceIdentity::from_secret_key([7; 16], &[8; 32]);
        let (directory, first, credentials) =
            HostDirectorySession::prepare(&identity, HostAccessMode::Fixed, b"ABCDEFGH", 1_000)
                .unwrap();
        assert_eq!(credentials.expires_at_unix_s, 0);
        assert_eq!(directory.registration.ttl_s(), 0);
        let (_, next, _) =
            HostDirectorySession::prepare(&identity, HostAccessMode::Fixed, b"ABCDEFGH", 1_001)
                .unwrap();
        assert_ne!(first.session_id(), next.session_id());
        assert_ne!(first.relay_authentication(), next.relay_authentication());
        // Any characters are accepted now; only insufficient length fails.
        assert!(
            HostDirectorySession::prepare(&identity, HostAccessMode::Fixed, b"p@55w0rd!", 1_000)
                .is_ok()
        );
        assert!(matches!(
            HostDirectorySession::prepare(&identity, HostAccessMode::Fixed, b"short", 1_000),
            Err(HostError::InvalidAccessCode)
        ));
    }
}
