use desklink_protocol::{DeviceRole, InputEvent, KeyCode, Modifiers, MouseButton};
use desklink_session::{
    DesktopRect, InputSequencer, NormalizedPoint, PressedInputState, ReconnectDecision,
    ReconnectPolicy, ReconnectPolicyError, ReconnectSchedule, SessionAction, SessionEvent,
    SessionMachine, SessionState, map_to_desktop, next_input_sequence,
};
use std::time::Duration;

#[test]
fn normalized_point_maps_to_desktop_origin_and_end() {
    assert_eq!(
        map_to_desktop(
            NormalizedPoint::new(0.0, 0.0),
            DesktopRect::new(-100, 20, 1920, 1080)
        ),
        (-100, 20)
    );
    assert_eq!(
        map_to_desktop(
            NormalizedPoint::new(1.0, 1.0),
            DesktopRect::new(-100, 20, 1920, 1080)
        ),
        (1819, 1099)
    );
}

#[test]
fn disconnect_emits_release_all() {
    let mut machine = SessionMachine::new(DeviceRole::Controller);
    machine.apply(SessionEvent::RelayConnected).unwrap();
    machine.apply(SessionEvent::HandshakeComplete).unwrap();
    let actions = machine
        .apply(SessionEvent::Disconnected { retryable: true })
        .unwrap();
    assert!(actions.contains(&SessionAction::ReleaseAll));
    assert_eq!(machine.state(), SessionState::Reconnecting);
}

#[test]
fn approval_is_required_before_video_start() {
    let mut machine = SessionMachine::new(DeviceRole::Host);
    machine.apply(SessionEvent::RelayConnected).unwrap();
    machine.apply(SessionEvent::HandshakeComplete).unwrap();
    assert_eq!(machine.state(), SessionState::WaitingForApproval);
    assert!(machine.apply(SessionEvent::StartVideo).is_err());
}

#[test]
fn coordinates_are_clamped() {
    let desktop = DesktopRect::new(10, 20, 100, 200);
    assert_eq!(
        map_to_desktop(NormalizedPoint::new(-1.0, 2.0), desktop),
        (10, 219)
    );
}

#[test]
fn sequences_reserve_zero_when_wrapping() {
    let mut sequence = u64::MAX;
    assert_eq!(next_input_sequence(&mut sequence), 1);
    let mut sequencer = InputSequencer::new();
    assert_eq!(sequencer.next_sequence(), 1);
}

#[test]
fn released_inputs_are_not_released_again() {
    let mut state = PressedInputState::default();
    state.press(&InputEvent::Key {
        code: KeyCode::Character('a'),
        pressed: true,
        modifiers: Modifiers(1),
    });
    state.press(&InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: true,
    });
    state.release(&InputEvent::Key {
        code: KeyCode::Character('a'),
        pressed: false,
        modifiers: Modifiers(1),
    });
    state.release(&InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: false,
    });
    assert!(state.release_all().is_empty());
}

#[test]
fn unicode_text_is_not_tracked_as_a_held_key() {
    let mut state = PressedInputState::default();
    state.press(&InputEvent::Key {
        code: KeyCode::Character('中'),
        pressed: true,
        modifiers: Modifiers(0),
    });

    assert!(state.release_events().is_empty());
}

#[test]
fn release_all_is_ordered_and_clears_multiple_inputs() {
    let mut state = PressedInputState::default();
    state.press(&InputEvent::Key {
        code: KeyCode::Enter,
        pressed: true,
        modifiers: Modifiers(1),
    });
    state.press(&InputEvent::Key {
        code: KeyCode::Escape,
        pressed: true,
        modifiers: Modifiers(2),
    });
    state.press(&InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: true,
    });
    state.press(&InputEvent::MouseButton {
        button: MouseButton::Right,
        pressed: true,
    });
    assert_eq!(
        state.release_all(),
        vec![
            InputEvent::Key {
                code: KeyCode::Escape,
                pressed: false,
                modifiers: Modifiers(2)
            },
            InputEvent::Key {
                code: KeyCode::Enter,
                pressed: false,
                modifiers: Modifiers(1)
            },
            InputEvent::MouseButton {
                button: MouseButton::Right,
                pressed: false
            },
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false
            },
        ]
    );
    assert!(state.release_all().is_empty());
}

#[test]
fn reconnect_negotiation_gets_a_fresh_stream_id() {
    let mut machine = SessionMachine::new(DeviceRole::Controller);
    machine.apply(SessionEvent::RelayConnected).unwrap();
    machine.apply(SessionEvent::HandshakeComplete).unwrap();
    let first = machine.apply(SessionEvent::CapabilitiesNegotiated).unwrap();
    assert_eq!(first.len(), 1);
    assert!(
        !first
            .iter()
            .any(|action| matches!(action, SessionAction::StartVideo))
    );
    let first_id = first
        .iter()
        .find_map(|action| match action {
            SessionAction::BeginStream { stream_id } => Some(*stream_id),
            _ => None,
        })
        .unwrap();
    machine
        .apply(SessionEvent::Disconnected { retryable: true })
        .unwrap();
    machine.apply(SessionEvent::RelayConnected).unwrap();
    machine.apply(SessionEvent::HandshakeComplete).unwrap();
    let second = machine.apply(SessionEvent::CapabilitiesNegotiated).unwrap();
    assert_eq!(second.len(), 1);
    assert!(
        !second
            .iter()
            .any(|action| matches!(action, SessionAction::StartVideo))
    );
    let second_id = second
        .iter()
        .find_map(|action| match action {
            SessionAction::BeginStream { stream_id } => Some(*stream_id),
            _ => None,
        })
        .unwrap();
    assert_ne!(first_id, second_id);
}

#[test]
fn start_video_is_only_valid_after_capability_gate() {
    let mut machine = SessionMachine::new(DeviceRole::Controller);
    assert!(machine.apply(SessionEvent::StartVideo).is_err());
    machine.apply(SessionEvent::RelayConnected).unwrap();
    machine.apply(SessionEvent::HandshakeComplete).unwrap();
    machine.apply(SessionEvent::CapabilitiesNegotiated).unwrap();
    assert_eq!(machine.state(), SessionState::StartingVideo);
    assert_eq!(
        machine.apply(SessionEvent::StartVideo).unwrap(),
        vec![SessionAction::StartVideo]
    );
    assert_eq!(machine.apply(SessionEvent::StartVideo).unwrap(), Vec::new());
}

#[test]
fn close_releases_before_close() {
    let mut machine = SessionMachine::new(DeviceRole::Controller);
    let actions = machine.apply(SessionEvent::UserDisconnected).unwrap();
    assert_eq!(
        actions,
        vec![SessionAction::ReleaseAll, SessionAction::Close]
    );
}

#[test]
fn reconnect_schedule_uses_bounded_exponential_delays_and_exhausts() {
    let policy =
        ReconnectPolicy::new(Duration::from_millis(250), Duration::from_secs(1), 5).unwrap();
    let mut schedule = ReconnectSchedule::new(policy, None);

    let delays = (1..=5)
        .map(|retry| match schedule.next(1_000) {
            ReconnectDecision::RetryAfter {
                retry: actual,
                delay,
            } => {
                assert_eq!(actual, retry);
                delay
            }
            decision => panic!("unexpected decision: {decision:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        delays,
        vec![
            Duration::from_millis(250),
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(1),
            Duration::from_secs(1),
        ]
    );
    assert_eq!(schedule.next(1_000), ReconnectDecision::Exhausted);
}

#[test]
fn reconnect_schedule_honors_expiry_and_resets_after_stable_connection() {
    let policy = ReconnectPolicy::new(Duration::from_secs(4), Duration::from_secs(8), 3).unwrap();
    let mut schedule = ReconnectSchedule::new(policy, Some(1_005));

    assert_eq!(
        schedule.next(1_003),
        ReconnectDecision::RetryAfter {
            retry: 1,
            delay: Duration::from_secs(2)
        }
    );
    assert_eq!(schedule.next(1_005), ReconnectDecision::SessionExpired);
    schedule.reset();
    assert_eq!(schedule.retries_used(), 0);
    assert_eq!(
        schedule.next(1_004),
        ReconnectDecision::RetryAfter {
            retry: 1,
            delay: Duration::from_secs(1)
        }
    );
}

#[test]
fn reconnect_policy_rejects_unsafe_configuration() {
    assert_eq!(
        ReconnectPolicy::new(Duration::ZERO, Duration::from_secs(1), 1),
        Err(ReconnectPolicyError::ZeroBaseDelay)
    );
    assert_eq!(
        ReconnectPolicy::new(Duration::from_secs(2), Duration::from_secs(1), 1),
        Err(ReconnectPolicyError::MaxBelowBase)
    );
    assert_eq!(
        ReconnectPolicy::new(Duration::from_secs(1), Duration::from_secs(1), 0),
        Err(ReconnectPolicyError::ZeroRetries)
    );
}
