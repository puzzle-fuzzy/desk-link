use desklink_protocol::{InputEvent, InputRestrictionReason};

const CONSECUTIVE_FAILURE_THRESHOLD: u8 = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(usize)]
enum InputClass {
    Pointer,
    Keyboard,
    Button,
    Wheel,
}

impl InputClass {
    fn for_event(event: &InputEvent) -> Option<Self> {
        match event {
            InputEvent::MouseMove { .. } => Some(Self::Pointer),
            InputEvent::Key { pressed: true, .. } => Some(Self::Keyboard),
            InputEvent::MouseButton { pressed: true, .. } => Some(Self::Button),
            InputEvent::MouseWheel { delta_x, delta_y } if *delta_x != 0 || *delta_y != 0 => {
                Some(Self::Wheel)
            }
            // Focus / display cleanup must never masquerade as proof of recovery.
            InputEvent::Key { pressed: false, .. }
            | InputEvent::MouseButton { pressed: false, .. }
            | InputEvent::MouseWheel { .. } => None,
        }
    }
}

/// Owns one authenticated session's feedback state. An unrelated cursor-position
/// fallback succeeding cannot clear a keyboard/permission failure.
#[derive(Default)]
pub(crate) struct InputRestrictionTracker {
    pending: [Option<(InputRestrictionReason, u8)>; 4],
    active: [Option<InputRestrictionReason>; 4],
    reported: Option<InputRestrictionReason>,
}

impl InputRestrictionTracker {
    /// Outer None: no transition. Some(None): real recovery. Some(Some): restriction.
    pub(crate) fn observe(
        &mut self,
        event: &InputEvent,
        failure: Option<InputRestrictionReason>,
    ) -> Option<Option<InputRestrictionReason>> {
        let class = InputClass::for_event(event)? as usize;
        match failure {
            Some(reason) => {
                let count = match self.pending[class] {
                    Some((pending_reason, count)) if pending_reason == reason => {
                        count.saturating_add(1)
                    }
                    _ => 1,
                };
                self.pending[class] = Some((reason, count));
                if count < CONSECUTIVE_FAILURE_THRESHOLD {
                    return None;
                }
                self.active[class] = Some(reason);
            }
            None => {
                self.pending[class] = None;
                self.active[class] = None;
            }
        }
        // Permission failures take precedence, but a second restricted input
        // class remains active after the first class recovers.
        let aggregate = [
            InputRestrictionReason::Blocked,
            InputRestrictionReason::UnsupportedKey,
            InputRestrictionReason::InvalidInput,
        ]
        .into_iter()
        .find(|reason| self.active.contains(&Some(*reason)));
        if aggregate == self.reported {
            return None;
        }
        self.reported = aggregate;
        Some(aggregate)
    }
}

/// A watch channel retains one latest transition while control I/O is busy. The
/// caller polls this as a separate session future, so dropping the session also
/// drops a pending send and never leaves a detached task or stale-session sender.
pub(crate) async fn forward_latest_input_state<Send, SendFuture, Error>(
    mut states: tokio::sync::watch::Receiver<Option<InputRestrictionReason>>,
    mut send: Send,
) -> Result<(), Error>
where
    Send: FnMut(Option<InputRestrictionReason>) -> SendFuture,
    SendFuture: std::future::Future<Output = Result<(), Error>>,
{
    while states.changed().await.is_ok() {
        let reason = *states.borrow_and_update();
        send(reason).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use desklink_protocol::{KeyCode, Modifiers, MouseButton};

    fn key(pressed: bool) -> InputEvent {
        InputEvent::Key {
            code: KeyCode::Character('a'),
            pressed,
            modifiers: Modifiers(0),
        }
    }

    #[test]
    fn transient_and_mixed_failures_do_not_claim_a_persistent_restriction() {
        let mut tracker = InputRestrictionTracker::default();
        let event = key(true);
        assert_eq!(
            tracker.observe(&event, Some(InputRestrictionReason::Blocked)),
            None
        );
        assert_eq!(
            tracker.observe(&event, Some(InputRestrictionReason::Blocked)),
            None
        );
        assert_eq!(tracker.observe(&event, None), None);
        assert_eq!(
            tracker.observe(&event, Some(InputRestrictionReason::Blocked)),
            None
        );
        assert_eq!(
            tracker.observe(&event, Some(InputRestrictionReason::InvalidInput)),
            None
        );
        assert_eq!(
            tracker.observe(&event, Some(InputRestrictionReason::UnsupportedKey)),
            None
        );
    }

    #[test]
    fn sustained_failures_report_once_and_real_same_class_success_recovers_once() {
        for reason in [
            InputRestrictionReason::Blocked,
            InputRestrictionReason::UnsupportedKey,
            InputRestrictionReason::InvalidInput,
        ] {
            let mut tracker = InputRestrictionTracker::default();
            assert_eq!(tracker.observe(&key(true), Some(reason)), None);
            assert_eq!(tracker.observe(&key(true), Some(reason)), None);
            assert_eq!(
                tracker.observe(&key(true), Some(reason)),
                Some(Some(reason))
            );
            for _ in 0..100 {
                assert_eq!(tracker.observe(&key(true), Some(reason)), None);
            }
            assert_eq!(tracker.observe(&key(true), None), Some(None));
            assert_eq!(tracker.observe(&key(true), None), None);
        }
    }

    #[test]
    fn focus_release_zero_wheel_and_unrelated_pointer_success_never_clear_keyboard_restriction() {
        let mut tracker = InputRestrictionTracker::default();
        for _ in 0..3 {
            tracker.observe(&key(true), Some(InputRestrictionReason::Blocked));
        }
        for event in [
            key(false),
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            },
            InputEvent::MouseMove { x: 1, y: 1 },
            InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 0,
            },
        ] {
            assert_eq!(tracker.observe(&event, None), None);
        }
        assert_eq!(tracker.observe(&key(true), None), Some(None));
    }

    #[test]
    fn release_only_errors_do_not_report_restriction_and_reason_changes_need_confirmation() {
        let mut tracker = InputRestrictionTracker::default();
        for _ in 0..10 {
            assert_eq!(
                tracker.observe(&key(false), Some(InputRestrictionReason::Blocked)),
                None
            );
        }
        for _ in 0..3 {
            tracker.observe(&key(true), Some(InputRestrictionReason::Blocked));
        }
        assert_eq!(
            tracker.observe(&key(true), Some(InputRestrictionReason::UnsupportedKey)),
            None
        );
        assert_eq!(
            tracker.observe(&key(true), Some(InputRestrictionReason::UnsupportedKey)),
            None
        );
        assert_eq!(
            tracker.observe(&key(true), Some(InputRestrictionReason::UnsupportedKey)),
            Some(Some(InputRestrictionReason::UnsupportedKey))
        );
    }

    #[test]
    fn successful_wheel_button_and_pointer_inputs_restore_their_own_restrictions() {
        for event in [
            InputEvent::MouseWheel {
                delta_x: 0,
                delta_y: 120,
            },
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            },
            InputEvent::MouseMove { x: 1, y: 1 },
        ] {
            let mut tracker = InputRestrictionTracker::default();
            for _ in 0..3 {
                tracker.observe(&event, Some(InputRestrictionReason::Blocked));
            }
            assert_eq!(tracker.observe(&event, None), Some(None));
        }
    }

    #[test]
    fn multiple_restricted_classes_require_each_class_to_recover() {
        let mut tracker = InputRestrictionTracker::default();
        let pointer = InputEvent::MouseMove { x: 1, y: 1 };
        for _ in 0..3 {
            tracker.observe(&key(true), Some(InputRestrictionReason::Blocked));
        }
        for _ in 0..3 {
            assert_eq!(
                tracker.observe(&pointer, Some(InputRestrictionReason::Blocked)),
                None
            );
        }
        assert_eq!(tracker.observe(&pointer, None), None);
        assert_eq!(tracker.observe(&key(true), None), Some(None));
    }

    #[tokio::test]
    async fn slow_feedback_send_coalesces_latest_state_and_cancels_with_the_session() {
        let (states, receiver) = tokio::sync::watch::channel(None);
        let (sent, mut observed) = tokio::sync::mpsc::channel(64);
        let gate = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut forward = Box::pin(forward_latest_input_state(receiver, {
            let gate = gate.clone();
            move |reason| {
                sent.try_send(reason).unwrap();
                let gate = gate.clone();
                async move {
                    gate.notified().await;
                    Ok::<(), ()>(())
                }
            }
        }));
        states.send_replace(Some(InputRestrictionReason::Blocked));
        tokio::select! {
            result = &mut forward => panic!("send should remain pending: {result:?}"),
            state = observed.recv() => assert_eq!(state, Some(Some(InputRestrictionReason::Blocked))),
        }
        for reason in [
            None,
            Some(InputRestrictionReason::InvalidInput),
            None,
            Some(InputRestrictionReason::UnsupportedKey),
        ] {
            states.send_replace(reason);
        }
        gate.notify_one();
        tokio::select! {
            result = &mut forward => panic!("next send should remain pending: {result:?}"),
            state = observed.recv() => assert_eq!(state, Some(Some(InputRestrictionReason::UnsupportedKey))),
        }
        assert_eq!(states.receiver_count(), 1);
        drop(forward);
        assert_eq!(states.receiver_count(), 0);
        assert!(observed.try_recv().is_err());
    }
}
