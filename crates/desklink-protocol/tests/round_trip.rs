use desklink_protocol::{
    Codec, ControlMessage, DeviceCapabilities, DeviceRole, FrameFlags, InputEnvelope, InputEvent,
    MAX_CONTROL_MESSAGE_BYTES, MAX_DATAGRAM_PAYLOAD_BYTES, PROTOCOL_VERSION, Platform,
    ProtocolError, VideoFrameHeader, VideoPacket, decode_control, decode_video_header,
    encode_control, encode_video_header, validate_input_timestamp,
};

#[test]
fn control_message_round_trips() {
    let message = ControlMessage::RequestKeyframe { stream_id: 7 };
    let encoded = encode_control(&message).expect("encode");
    assert_eq!(decode_control(&encoded).expect("decode"), message);
}

#[test]
fn frame_header_round_trips() {
    let header = VideoFrameHeader {
        protocol_version: PROTOCOL_VERSION,
        stream_id: 3,
        config_version: 2,
        frame_id: 41,
        capture_timestamp_us: 1234,
        width: 1920,
        height: 1080,
        flags: FrameFlags::KEYFRAME,
        chunk_index: 0,
        chunk_count: 2,
        payload_length: 900,
    };
    let encoded = encode_video_header(&header).expect("encode");
    assert_eq!(decode_video_header(&encoded).expect("decode"), header);
}

#[test]
fn oversized_control_payload_is_rejected() {
    let bytes = vec![0u8; MAX_CONTROL_MESSAGE_BYTES + 1];
    assert!(matches!(
        decode_control(&bytes),
        Err(ProtocolError::MessageTooLarge { .. })
    ));
}

fn header() -> VideoFrameHeader {
    VideoFrameHeader {
        protocol_version: PROTOCOL_VERSION,
        stream_id: 1,
        config_version: 1,
        frame_id: 1,
        capture_timestamp_us: 1,
        width: 1920,
        height: 1080,
        flags: FrameFlags::KEYFRAME,
        chunk_index: 0,
        chunk_count: 1,
        payload_length: 3,
    }
}

#[test]
fn malformed_header_version_is_rejected() {
    let mut value = header();
    value.protocol_version = 9;
    assert!(matches!(
        encode_video_header(&value),
        Err(ProtocolError::UnsupportedVersion(9))
    ));
}
#[test]
fn invalid_chunks_are_rejected() {
    let mut value = header();
    value.chunk_count = 0;
    assert!(matches!(
        encode_video_header(&value),
        Err(ProtocolError::InvalidFrame)
    ));
    value.chunk_count = 2;
    value.chunk_index = 2;
    assert!(matches!(
        encode_video_header(&value),
        Err(ProtocolError::InvalidFrame)
    ));
}
#[test]
fn oversized_dimensions_are_rejected() {
    let mut value = header();
    value.width = 1921;
    assert!(encode_video_header(&value).is_err());
}
#[test]
fn packet_payload_is_bounded_and_matches_header() {
    let mut value = header();
    value.payload_length = 4;
    assert!(matches!(
        VideoPacket::new(value.clone(), vec![1, 2, 3]),
        Err(ProtocolError::PayloadLengthMismatch { .. })
    ));
    value.payload_length = MAX_DATAGRAM_PAYLOAD_BYTES + 1;
    assert!(matches!(
        VideoPacket::new(value, vec![0; 1201]),
        Err(ProtocolError::MessageTooLarge { .. })
    ));
}
#[test]
fn unknown_frame_flags_are_rejected() {
    let mut value = header();
    value.flags = FrameFlags(0x8000);
    assert!(matches!(
        encode_video_header(&value),
        Err(ProtocolError::InvalidFlags)
    ));
}
#[test]
fn invalid_capabilities_are_rejected() {
    let value = DeviceCapabilities {
        platform: Platform::IOS,
        role: DeviceRole::Host,
        codecs: vec![Codec::H264],
        width: 1921,
        height: 1080,
    };
    assert!(matches!(
        value.validate(),
        Err(ProtocolError::InvalidCapabilities)
    ));
}
#[test]
fn input_timestamp_window_rejects_stale_and_future_values() {
    let envelope = InputEnvelope {
        sequence: 1,
        timestamp_us: 900,
        event: InputEvent::MouseMove { x: 1, y: 1 },
    };
    assert!(validate_input_timestamp(&envelope, 2_000, 500).is_err());
    let future = InputEnvelope {
        timestamp_us: 2_501,
        ..envelope
    };
    assert!(validate_input_timestamp(&future, 2_000, 500).is_err());
}
