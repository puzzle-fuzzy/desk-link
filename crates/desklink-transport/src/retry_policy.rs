use crate::{JoinRejectCode, TransportError};

/// The owner deciding whether to recover after a failed transport attempt.
/// Session lifetime, retry budgets and backoff remain with that owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportRetryContext {
    /// Rearm a host service after one controller attempt ends or is malformed.
    HostSupervisor,
    /// Reconnect one controller session without retrying invalid admission data.
    ControllerSession,
}

/// Classifies transport failures without inspecting error text or doing I/O.
/// A retryable result does not override cancellation, expiry or a retry budget.
pub fn transport_error_is_retryable(
    error: &TransportError,
    context: TransportRetryContext,
) -> bool {
    match error {
        TransportError::Connection(_)
        | TransportError::ConnectionLimit
        | TransportError::Stream(_)
        | TransportError::Datagram(_)
        | TransportError::Closed
        | TransportError::PeerDisconnected
        | TransportError::PeerReplaced
        | TransportError::JoinRejected(
            JoinRejectCode::Internal
            | JoinRejectCode::ConnectionLimit
            | JoinRejectCode::SessionLimit,
        ) => true,
        TransportError::Malformed
        | TransportError::JoinRejected(
            JoinRejectCode::SessionNotFound | JoinRejectCode::SessionOccupied,
        ) => context == TransportRetryContext::HostSupervisor,
        TransportError::MessageTooLarge { .. }
        | TransportError::NotJoined
        | TransportError::AlreadyJoined
        | TransportError::InvalidConfig(_)
        | TransportError::DirectoryNotFound
        | TransportError::DirectoryRateLimited
        | TransportError::DirectoryProtocolMismatch { .. }
        | TransportError::JoinRejected(
            JoinRejectCode::Malformed
            | JoinRejectCode::TooLarge
            | JoinRejectCode::AuthenticationMismatch
            | JoinRejectCode::RoleMismatch
            | JoinRejectCode::DirectoryConflict,
        ) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_transport_failures_keep_the_same_decision_in_both_contexts() {
        let cases = [
            (
                TransportError::Connection("synthetic connection loss".into()),
                true,
            ),
            (TransportError::ConnectionLimit, true),
            (TransportError::Stream("synthetic stream loss".into()), true),
            (
                TransportError::Datagram("synthetic datagram failure".into()),
                true,
            ),
            (TransportError::Closed, true),
            (TransportError::PeerDisconnected, true),
            (TransportError::PeerReplaced, true),
            (TransportError::JoinRejected(JoinRejectCode::Internal), true),
            (
                TransportError::JoinRejected(JoinRejectCode::ConnectionLimit),
                true,
            ),
            (
                TransportError::JoinRejected(JoinRejectCode::SessionLimit),
                true,
            ),
            (
                TransportError::MessageTooLarge {
                    actual: 2,
                    maximum: 1,
                },
                false,
            ),
            (TransportError::NotJoined, false),
            (TransportError::AlreadyJoined, false),
            (
                TransportError::InvalidConfig("synthetic invalid config".into()),
                false,
            ),
            (TransportError::DirectoryNotFound, false),
            (TransportError::DirectoryRateLimited, false),
            (
                TransportError::DirectoryProtocolMismatch {
                    controller: Some(12),
                    host: Some(13),
                },
                false,
            ),
            (
                TransportError::JoinRejected(JoinRejectCode::Malformed),
                false,
            ),
            (
                TransportError::JoinRejected(JoinRejectCode::TooLarge),
                false,
            ),
            (
                TransportError::JoinRejected(JoinRejectCode::AuthenticationMismatch),
                false,
            ),
            (
                TransportError::JoinRejected(JoinRejectCode::RoleMismatch),
                false,
            ),
            (
                TransportError::JoinRejected(JoinRejectCode::DirectoryConflict),
                false,
            ),
        ];
        for (error, expected) in cases {
            for context in [
                TransportRetryContext::HostSupervisor,
                TransportRetryContext::ControllerSession,
            ] {
                assert_eq!(
                    transport_error_is_retryable(&error, context),
                    expected,
                    "{context:?}: {error:?}"
                );
            }
        }
    }

    #[test]
    fn bad_or_missing_peer_attempts_rearm_the_host_without_retrying_the_controller() {
        for error in [
            TransportError::Malformed,
            TransportError::JoinRejected(JoinRejectCode::SessionNotFound),
            TransportError::JoinRejected(JoinRejectCode::SessionOccupied),
        ] {
            assert!(
                transport_error_is_retryable(&error, TransportRetryContext::HostSupervisor),
                "{error:?}"
            );
            assert!(
                !transport_error_is_retryable(&error, TransportRetryContext::ControllerSession),
                "{error:?}"
            );
        }
    }
}
