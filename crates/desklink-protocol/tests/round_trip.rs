use desklink_protocol::{
    ControlMessage, FrameFlags, MAX_CONTROL_MESSAGE_BYTES, PROTOCOL_VERSION, VideoFrameHeader,
    decode_control, decode_video_header, encode_control, encode_video_header,
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
        Err(desklink_protocol::ProtocolError::MessageTooLarge { .. })
    ));
}
