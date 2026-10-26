use desklink_protocol::{
    ControlMessage, InputRestrictionReason, RemoteDisplay, VideoQualityPreference,
    VideoQualityPreset, decode_control, encode_control,
};

const SWIFT_TESTS: &str =
    include_str!("../../../apps/apple/Tests/DeskLinkAppleCoreTests/SessionControlStateTests.swift");

fn swift_fixture(name: &str) -> Vec<u8> {
    let declaration = format!("let {name}: [UInt8] = [");
    SWIFT_TESTS
        .split_once(declaration.as_str())
        .expect("Swift fixture declaration")
        .1
        .split_once(']')
        .expect("Swift fixture end")
        .0
        .split(',')
        .map(|byte| byte.trim().parse().expect("fixture byte"))
        .collect()
}

#[test]
fn apple_session_state_fixtures_match_the_real_control_codec() {
    let fixtures = [
        (
            "displaysFixture",
            ControlMessage::DisplayList {
                displays: vec![
                    RemoteDisplay {
                        id: 7,
                        width: 1920,
                        height: 1080,
                        primary: true,
                    },
                    RemoteDisplay {
                        id: 300,
                        width: 2560,
                        height: 1440,
                        primary: false,
                    },
                ],
                active_display_id: 300,
            },
        ),
        (
            "automaticQualityFixture",
            ControlMessage::VideoQualityState {
                preference: VideoQualityPreference::Automatic,
                preset: VideoQualityPreset::Balanced,
            },
        ),
        (
            "sharpQualityFixture",
            ControlMessage::VideoQualityState {
                preference: VideoQualityPreference::Sharp,
                preset: VideoQualityPreset::Sharp,
            },
        ),
        (
            "audioFixture",
            ControlMessage::AudioState {
                available: true,
                enabled: false,
            },
        ),
        (
            "inputBlockedFixture",
            ControlMessage::InputState {
                reason: Some(InputRestrictionReason::Blocked),
            },
        ),
        (
            "inputRecoveredFixture",
            ControlMessage::InputState { reason: None },
        ),
    ];
    for (name, message) in fixtures {
        let bytes = swift_fixture(name);
        assert_eq!(encode_control(&message).unwrap(), bytes, "{name}");
        assert_eq!(decode_control(&bytes).unwrap(), message, "{name}");
    }
}
