use desklink_protocol::DeviceRole;
use desklink_session::{
    DesktopRect, NormalizedPoint, SessionAction, SessionEvent, SessionMachine, SessionState,
    map_to_desktop,
};

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
        (1820, 1100)
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
