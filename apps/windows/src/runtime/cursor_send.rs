use desklink_transport::TransportError;

/// The transport groups unsupported, disabled and oversized QUIC datagrams
/// into one typed error. Do not infer a finer cause from raw error text.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorDisableReason {
    CapabilityOrSize,
}

impl CursorDisableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CapabilityOrSize => "capability_or_size",
        }
    }
}

/// Whitelisted local send-operation counters, never packet-delivery or
/// queue-eviction statistics. No error text, device ID or cursor content.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize)]
pub(super) struct CursorSendSnapshot {
    pub(super) send_attempts: u64,
    pub(super) send_errors: u64,
    pub(super) disabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CursorSendOutcome {
    /// Local acceptance can evict an older datagram; it does not prove delivery.
    LocallyAccepted { snapshot: CursorSendSnapshot },
    OptionalDisabled {
        reason: CursorDisableReason,
        snapshot: CursorSendSnapshot,
        should_report: bool,
    },
}

impl CursorSendOutcome {
    pub(super) fn snapshot(self) -> CursorSendSnapshot {
        match self {
            Self::LocallyAccepted { snapshot } | Self::OptionalDisabled { snapshot, .. } => {
                snapshot
            }
        }
    }

    pub(super) fn should_report(self) -> bool {
        match self {
            Self::LocallyAccepted { .. } => false,
            Self::OptionalDisabled { should_report, .. } => should_report,
        }
    }
}

#[derive(Default)]
pub(super) struct CursorSendState {
    snapshot: CursorSendSnapshot,
}

impl CursorSendState {
    #[cfg(test)]
    pub(super) fn snapshot(&self) -> CursorSendSnapshot {
        self.snapshot
    }

    pub(super) fn is_disabled(&self) -> bool {
        self.snapshot.disabled
    }

    fn disabled_outcome(&self, should_report: bool) -> CursorSendOutcome {
        CursorSendOutcome::OptionalDisabled {
            reason: CursorDisableReason::CapabilityOrSize,
            snapshot: self.snapshot,
            should_report,
        }
    }

    /// Disable this optional lane on its first capability/size error. The
    /// caller must park before sampling/sending again; a new peer gets new
    /// state. Late results never reactivate it or hide terminal errors.
    pub(super) fn record(
        &mut self,
        result: Result<(), TransportError>,
    ) -> Result<CursorSendOutcome, TransportError> {
        if self.snapshot.disabled {
            return match result {
                Ok(()) | Err(TransportError::Datagram(_)) => Ok(self.disabled_outcome(false)),
                Err(error) => Err(error),
            };
        }
        self.snapshot.send_attempts = self.snapshot.send_attempts.saturating_add(1);
        if result.is_err() {
            self.snapshot.send_errors = self.snapshot.send_errors.saturating_add(1);
        }
        match result {
            Ok(()) => Ok(CursorSendOutcome::LocallyAccepted {
                snapshot: self.snapshot,
            }),
            Err(TransportError::Datagram(_)) => {
                self.snapshot.disabled = true;
                Ok(self.disabled_outcome(true))
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn production_loop_disables_sampling_once_without_completing_the_peer_future() {
        use super::super::{
            HostMediaDiagnostic, HostMediaDiagnosticsObserver, media_diagnostics,
            send_cursor_samples,
        };
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
        use std::time::Duration;

        media_diagnostics::initialize();
        let (reported, mut received) = tokio::sync::mpsc::channel(64);
        let observer: HostMediaDiagnosticsObserver = Arc::new(move |event| {
            reported.try_send(event).unwrap();
        });
        let samples = Arc::new(AtomicU64::new(0));
        let count = samples.clone();
        let mut sending = Box::pin(send_cursor_samples(91, Some(observer), move |_| {
            count.fetch_add(1, Ordering::Relaxed);
            std::future::ready(Ok(Some(capability_or_size())))
        }));
        let event = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                event = received.recv() => event.unwrap(),
                result = &mut sending => panic!("optional cursor completed the peer future: {result:?}"),
            }
        }).await.unwrap();
        assert_eq!(
            event,
            HostMediaDiagnostic::CursorSend {
                stream_id: 91,
                send_attempts: 1,
                send_errors: 1,
                disabled: true,
                reason: CursorDisableReason::CapabilityOrSize,
            }
        );
        // Poll through several cursor intervals while the rest of a peer can run.
        assert!(
            tokio::time::timeout(Duration::from_millis(80), &mut sending)
                .await
                .is_err()
        );
        assert_eq!(samples.load(Ordering::Relaxed), 1);
        assert!(received.try_recv().is_err());
        drop(sending); // Peer cancellation still drops the parked optional future.
    }

    #[tokio::test]
    async fn production_loop_propagates_terminal_send_errors_without_optional_diagnosis() {
        use super::super::{HostRuntimeError, send_cursor_samples};
        let result = send_cursor_samples(91, None, |_| {
            std::future::ready(Ok(Some(Err(TransportError::PeerReplaced))))
        })
        .await;
        assert!(matches!(
            result,
            Err(HostRuntimeError::Transport(TransportError::PeerReplaced))
        ));
    }

    fn capability_or_size() -> Result<(), TransportError> {
        Err(TransportError::Datagram(
            "synthetic optional capability or size failure".into(),
        ))
    }

    #[test]
    fn first_datagram_error_disables_once_and_never_counts_a_retry() {
        let mut state = CursorSendState::default();
        let first = state.record(capability_or_size()).unwrap();
        assert!(matches!(
            first,
            CursorSendOutcome::OptionalDisabled {
                reason: CursorDisableReason::CapabilityOrSize,
                ..
            }
        ));
        assert!(first.should_report());
        assert!(state.is_disabled());
        assert_eq!(
            first.snapshot(),
            CursorSendSnapshot {
                send_attempts: 1,
                send_errors: 1,
                disabled: true
            }
        );
        for _ in 0..128 {
            let late = state.record(capability_or_size()).unwrap();
            assert!(matches!(late, CursorSendOutcome::OptionalDisabled { .. }));
            assert!(!late.should_report());
        }
        let late_success = state.record(Ok(())).unwrap();
        assert!(matches!(
            late_success,
            CursorSendOutcome::OptionalDisabled { .. }
        ));
        assert!(!late_success.should_report());
        assert_eq!(state.snapshot(), first.snapshot());
        assert!(state.is_disabled());
    }

    #[test]
    fn successful_send_is_only_local_acceptance_without_delivery_or_drop_counters() {
        let mut state = CursorSendState::default();
        for expected in 1..=3 {
            let outcome = state.record(Ok(())).unwrap();
            assert!(matches!(outcome, CursorSendOutcome::LocallyAccepted { .. }));
            assert_eq!(
                outcome.snapshot(),
                CursorSendSnapshot {
                    send_attempts: expected,
                    send_errors: 0,
                    disabled: false
                }
            );
            assert!(!outcome.should_report());
        }
        assert!(!state.is_disabled());
    }

    #[test]
    fn connection_identity_generation_and_preflight_errors_are_immediate() {
        for error in [
            TransportError::Closed,
            TransportError::PeerDisconnected,
            TransportError::PeerReplaced,
            TransportError::NotJoined,
            TransportError::AlreadyJoined,
            TransportError::Malformed,
            TransportError::ConnectionLimit,
            TransportError::Connection("synthetic connection failure".into()),
            TransportError::Stream("synthetic stream failure".into()),
            TransportError::InvalidConfig("synthetic invalid config".into()),
            TransportError::MessageTooLarge {
                actual: 2,
                maximum: 1,
            },
        ] {
            let mut state = CursorSendState::default();
            let expected = error.to_string();
            assert_eq!(state.record(Err(error)).unwrap_err().to_string(), expected);
            assert_eq!(
                state.snapshot(),
                CursorSendSnapshot {
                    send_attempts: 1,
                    send_errors: 1,
                    disabled: false
                }
            );
        }
    }

    #[test]
    fn disabled_optional_lane_never_masks_a_late_terminal_error() {
        let mut state = CursorSendState::default();
        state.record(capability_or_size()).unwrap();
        let snapshot = state.snapshot();
        assert!(matches!(
            state.record(Err(TransportError::PeerReplaced)),
            Err(TransportError::PeerReplaced)
        ));
        assert!(matches!(
            state.record(Err(TransportError::Closed)),
            Err(TransportError::Closed)
        ));
        assert_eq!(state.snapshot(), snapshot);
    }

    #[test]
    fn send_attempt_and_error_counters_saturate() {
        let mut state = CursorSendState {
            snapshot: CursorSendSnapshot {
                send_attempts: u64::MAX - 1,
                send_errors: u64::MAX - 1,
                disabled: false,
            },
        };
        state.record(Ok(())).unwrap();
        assert_eq!(state.snapshot().send_attempts, u64::MAX);
        state.record(capability_or_size()).unwrap();
        assert_eq!(
            state.snapshot(),
            CursorSendSnapshot {
                send_attempts: u64::MAX,
                send_errors: u64::MAX,
                disabled: true
            }
        );
        state.record(capability_or_size()).unwrap();
        assert_eq!(state.snapshot().send_errors, u64::MAX);
    }

    #[test]
    fn diagnostic_serialization_has_only_fixed_counters_and_disabled_boolean() {
        let mut state = CursorSendState::default();
        let outcome = state
            .record(Err(TransportError::Datagram(
                "SYNTHETIC-SECRET-CANARY-and-cursor-pixels".into(),
            )))
            .unwrap();
        assert_eq!(
            serde_json::to_value(outcome.snapshot()).unwrap(),
            serde_json::json!({ "send_attempts": 1, "send_errors": 1, "disabled": true })
        );
        assert!(!format!("{outcome:?}").contains("SYNTHETIC"));
        assert_eq!(
            CursorDisableReason::CapabilityOrSize.as_str(),
            "capability_or_size"
        );
    }

    #[test]
    fn raw_error_words_never_override_the_typed_transport_category() {
        let mut optional = CursorSendState::default();
        assert!(matches!(
            optional
                .record(Err(TransportError::Datagram("connection lost".into())))
                .unwrap(),
            CursorSendOutcome::OptionalDisabled { .. }
        ));
        let mut terminal = CursorSendState::default();
        assert!(matches!(
            terminal.record(Err(TransportError::Connection("datagram disabled".into()))),
            Err(TransportError::Connection(_))
        ));
        assert!(!terminal.is_disabled());
    }
}
