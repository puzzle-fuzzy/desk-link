const MAX_UNANSWERED_HANDSHAKES: u8 = 3;

/// Applies only after a peer has joined and an actual Noise handshake timed
/// out. Relay / offline transport failures do not consume this budget.
#[derive(Default)]
pub(super) struct HandshakeResponseBudget {
    unanswered: u8,
}

impl HandshakeResponseBudget {
    /// True permits another attempt; the third unanswered handshake is terminal.
    pub(super) fn record_no_response(&mut self) -> bool {
        self.unanswered = self.unanswered.saturating_add(1);
        self.unanswered < MAX_UNANSWERED_HANDSHAKES
    }

    /// Only a completed authenticated connection proves a fresh handshake budget.
    pub(super) fn authenticated(&mut self) {
        self.unanswered = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_unanswered_handshakes_stop_even_if_persistent_transport_would_retry() {
        let mut budget = HandshakeResponseBudget::default();
        assert!(budget.record_no_response());
        assert!(budget.record_no_response());
        assert!(!budget.record_no_response());
        for _ in 0..10 {
            assert!(!budget.record_no_response());
        }
    }

    #[test]
    fn authenticated_connection_alone_rearms_the_handshake_budget() {
        let mut budget = HandshakeResponseBudget::default();
        assert!(budget.record_no_response());
        assert!(budget.record_no_response());
        budget.authenticated();
        assert!(budget.record_no_response());
        assert!(budget.record_no_response());
        assert!(!budget.record_no_response());
    }
}
