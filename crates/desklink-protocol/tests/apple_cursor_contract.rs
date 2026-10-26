use desklink_protocol::{
    CursorUpdate, PROTOCOL_VERSION, decode_cursor_update, encode_cursor_update,
};

const SWIFT_TESTS: &str = include_str!(
    "../../../apps/apple/Tests/DeskLinkAppleCoreTests/RemoteCursorPositionTests.swift"
);

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
fn apple_position_fixtures_match_the_production_postcard_codec() {
    let fixtures = [
        (
            "visibleFixture",
            CursorUpdate {
                protocol_version: PROTOCOL_VERSION,
                stream_id: 7,
                sequence: 11,
                timestamp_us: 42,
                x_millionths: 250_000,
                y_millionths: 750_000,
                visible: true,
                shape_id: 99,
            },
        ),
        (
            "hiddenFixture",
            CursorUpdate {
                protocol_version: PROTOCOL_VERSION,
                stream_id: 7,
                sequence: 12,
                timestamp_us: 43,
                x_millionths: 0,
                y_millionths: 1_000_000,
                visible: false,
                shape_id: u64::MAX,
            },
        ),
        (
            "cornerFixture",
            CursorUpdate {
                protocol_version: PROTOCOL_VERSION,
                stream_id: 8,
                sequence: 1,
                timestamp_us: 0,
                x_millionths: 1_000_000,
                y_millionths: 0,
                visible: true,
                shape_id: 0,
            },
        ),
    ];
    for (name, update) in fixtures {
        let fixture = swift_fixture(name);
        assert_eq!(encode_cursor_update(&update).unwrap(), fixture, "{name}");
        assert_eq!(decode_cursor_update(&fixture).unwrap(), update, "{name}");
    }
}
