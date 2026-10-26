use std::sync::Arc;

#[derive(Debug)]
pub(super) enum InstallOutcome<T> {
    Installed {
        retired: Option<Arc<T>>,
    },
    Retained {
        rejected: Arc<T>,
    },
    /// The candidate aliases the active allocation. Closing a rejected clone
    /// here would close the active connection itself.
    AlreadyCurrent,
}

/// First healthy active path wins, including when inbound and outbound probes
/// complete in the opposite order. Return retired/rejected ownership so callers
/// close or drop connections after releasing their async slot mutex.
pub(super) fn install<T>(
    current: &mut Option<Arc<T>>,
    candidate: Arc<T>,
    is_closed: impl Fn(&T) -> bool,
) -> InstallOutcome<T> {
    if current
        .as_ref()
        .is_some_and(|active| Arc::ptr_eq(active, &candidate))
    {
        return InstallOutcome::AlreadyCurrent;
    }
    if is_closed(&candidate) || current.as_ref().is_some_and(|active| !is_closed(active)) {
        return InstallOutcome::Retained {
            rejected: candidate,
        };
    }
    InstallOutcome::Installed {
        retired: current.replace(candidate),
    }
}

/// An egress task may still hold a failed old connection after a new one has
/// been installed. Only allocation identity authorizes retirement; candidate
/// IDs can be reused by concurrent paths and are insufficient for this check.
pub(super) fn retire_if_current<T>(
    current: &mut Option<Arc<T>>,
    failed: &Arc<T>,
) -> Option<Arc<T>> {
    if current
        .as_ref()
        .is_some_and(|active| Arc::ptr_eq(active, failed))
    {
        current.take()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{InstallOutcome, install, retire_if_current};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct Path {
        candidate_id: u64,
        closed: AtomicBool,
        dropped: Arc<AtomicUsize>,
    }

    impl Path {
        fn new(candidate_id: u64, dropped: &Arc<AtomicUsize>) -> Arc<Self> {
            Arc::new(Self {
                candidate_id,
                closed: AtomicBool::new(false),
                dropped: dropped.clone(),
            })
        }

        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::Acquire)
        }

        fn close(&self) {
            self.closed.store(true, Ordering::Release);
        }
    }

    impl Drop for Path {
        fn drop(&mut self) {
            self.dropped.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[test]
    fn late_outbound_probe_cannot_evict_the_healthy_inbound_path() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let inbound = Path::new(7, &dropped);
        let outbound = Path::new(8, &dropped);
        let mut slot = None;
        assert!(matches!(
            install(&mut slot, inbound.clone(), Path::is_closed),
            InstallOutcome::Installed { retired: None }
        ));
        let outcome = install(&mut slot, outbound.clone(), Path::is_closed);
        assert!(Arc::ptr_eq(slot.as_ref().unwrap(), &inbound));
        match outcome {
            InstallOutcome::Retained { rejected } => {
                assert!(Arc::ptr_eq(&rejected, &outbound));
                rejected.close();
            }
            _ => panic!("a later probe replaced a healthy path"),
        }
        assert!(!inbound.is_closed());
        assert!(outbound.is_closed());
    }

    #[test]
    fn stale_egress_failure_cannot_clear_a_new_allocation_with_the_same_candidate_id() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let old = Path::new(7, &dropped);
        let new = Path::new(7, &dropped);
        assert_eq!(old.candidate_id, new.candidate_id);
        let mut slot = Some(old.clone());
        old.close();
        assert!(matches!(
            install(&mut slot, new.clone(), Path::is_closed),
            InstallOutcome::Installed { retired: Some(_) }
        ));
        assert!(retire_if_current(&mut slot, &old).is_none());
        assert!(Arc::ptr_eq(slot.as_ref().unwrap(), &new));
        let retired = retire_if_current(&mut slot, &new).unwrap();
        assert!(Arc::ptr_eq(&retired, &new));
        assert!(slot.is_none());
    }

    #[test]
    fn closed_path_is_returned_for_drop_after_the_slot_lock_is_released() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let old = Path::new(1, &dropped);
        old.close();
        let new = Path::new(2, &dropped);
        let mut slot = Some(old);
        let outcome = install(&mut slot, new.clone(), Path::is_closed);
        assert_eq!(dropped.load(Ordering::Acquire), 0);
        assert!(Arc::ptr_eq(slot.as_ref().unwrap(), &new));
        match outcome {
            InstallOutcome::Installed {
                retired: Some(retired),
            } => {
                assert!(retired.is_closed());
                drop(retired);
            }
            _ => panic!("closed path ownership was not returned"),
        }
        assert_eq!(dropped.load(Ordering::Acquire), 1);
    }

    #[test]
    fn installing_the_same_arc_never_returns_the_active_connection_for_close() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let active = Path::new(1, &dropped);
        let mut slot = Some(active.clone());
        assert!(matches!(
            install(&mut slot, active.clone(), Path::is_closed),
            InstallOutcome::AlreadyCurrent
        ));
        assert!(!active.is_closed());
        assert_eq!(Arc::strong_count(&active), 2);
        assert_eq!(dropped.load(Ordering::Acquire), 0);
    }

    #[test]
    fn rejected_probe_flood_retains_no_history_or_extra_slot_references() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let active = Path::new(1, &dropped);
        let mut slot = Some(active.clone());
        for candidate_id in 2..1002 {
            let outcome = install(
                &mut slot,
                Path::new(candidate_id, &dropped),
                Path::is_closed,
            );
            assert!(matches!(outcome, InstallOutcome::Retained { .. }));
            drop(outcome);
        }
        assert_eq!(dropped.load(Ordering::Acquire), 1000);
        assert_eq!(Arc::strong_count(&active), 2);
        assert!(Arc::ptr_eq(slot.as_ref().unwrap(), &active));
    }

    #[test]
    fn already_closed_candidate_does_not_occupy_an_empty_slot() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let candidate = Path::new(1, &dropped);
        candidate.close();
        let mut slot = None;
        assert!(matches!(
            install(&mut slot, candidate, Path::is_closed),
            InstallOutcome::Retained { .. }
        ));
        assert!(slot.is_none());
        assert_eq!(dropped.load(Ordering::Acquire), 1);
    }
}
