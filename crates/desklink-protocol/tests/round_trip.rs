use desklink_protocol::{
    Codec, ControlMessage, CursorUpdate, DeviceCapabilities, DeviceRole, FrameFlags, InputEnvelope,
    InputEvent, MAX_CONTROL_MESSAGE_BYTES, MAX_CURSOR_MESSAGE_BYTES, MAX_DATAGRAM_PAYLOAD_BYTES,
    MAX_INPUT_AGE_US, MAX_INPUT_FUTURE_SKEW_US, MAX_NOISE_HANDSHAKE_BYTES, MAX_VIDEO_CHUNKS,
    MAX_VIDEO_CONFIG_BYTES, MAX_WHEEL_DELTA, Modifiers, NoiseHandshake, NoiseHandshakeStep,
    PROTOCOL_VERSION, Platform, ProtocolError, VideoConfig, VideoFrameHeader, VideoPacket,
    decode_control, decode_cursor_update, decode_input, decode_noise_handshake,
    decode_video_config, decode_video_header, decode_video_packet, encode_control,
    encode_cursor_update, encode_input, encode_noise_handshake, encode_video_config,
    encode_video_header, encode_video_packet,
};

#[test]
fn control_message_round_trips() {
    let message = ControlMessage::RequestKeyframe { stream_id: 7 };
    let encoded = encode_control(&message).expect("encode");
    assert_eq!(decode_control(&encoded).expect("decode"), message);
}

#[test]
fn noise_handshake_round_trips_and_rejects_invalid_envelopes() {
    let handshake = NoiseHandshake {
        protocol_version: PROTOCOL_VERSION,
        step: NoiseHandshakeStep::InitiatorHello,
        payload: vec![1, 2, 3],
    };
    let encoded = encode_noise_handshake(&handshake).expect("encode");
    assert_eq!(decode_noise_handshake(&encoded).expect("decode"), handshake);

    let mut invalid = handshake.clone();
    invalid.protocol_version += 1;
    assert!(matches!(
        encode_noise_handshake(&invalid),
        Err(ProtocolError::UnsupportedVersion(_))
    ));
    invalid.protocol_version = PROTOCOL_VERSION;
    invalid.payload.clear();
    assert_eq!(
        encode_noise_handshake(&invalid),
        Err(ProtocolError::Malformed)
    );
    invalid.payload = vec![0; MAX_NOISE_HANDSHAKE_BYTES + 1];
    assert_eq!(
        encode_noise_handshake(&invalid),
        Err(ProtocolError::Malformed)
    );
}

#[test]
fn video_config_round_trips_and_rejects_invalid_decoder_state() {
    let config = VideoConfig {
        protocol_version: PROTOCOL_VERSION,
        stream_id: 7,
        config_version: 3,
        codec: Codec::H264,
        width: 1920,
        height: 1080,
        sequence_header: vec![0, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 0, 1, 0x68, 4],
    };
    let encoded = encode_video_config(&config).expect("encode");
    assert_eq!(decode_video_config(&encoded).expect("decode"), config);

    let mut invalid = config.clone();
    invalid.sequence_header.clear();
    assert_eq!(
        encode_video_config(&invalid),
        Err(ProtocolError::InvalidVideoConfig)
    );
    invalid.sequence_header = vec![0; MAX_VIDEO_CONFIG_BYTES + 1];
    assert!(matches!(
        encode_video_config(&invalid),
        Err(ProtocolError::MessageTooLarge { .. })
    ));
}

#[test]
fn cursor_update_round_trips_inside_datagram_budget() {
    let cursor = CursorUpdate {
        protocol_version: PROTOCOL_VERSION,
        stream_id: 7,
        sequence: 11,
        timestamp_us: 42,
        x_millionths: 250_000,
        y_millionths: 750_000,
        visible: true,
        shape_id: 99,
    };
    let encoded = encode_cursor_update(&cursor).expect("encode");
    assert!(encoded.len() <= MAX_CURSOR_MESSAGE_BYTES);
    assert_eq!(decode_cursor_update(&encoded).expect("decode"), cursor);

    let mut invalid = cursor;
    invalid.x_millionths = 1_000_001;
    assert_eq!(
        encode_cursor_update(&invalid),
        Err(ProtocolError::InvalidCursor)
    );
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
    value.chunk_count = MAX_VIDEO_CHUNKS + 1;
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
    let mut value = DeviceCapabilities {
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
    value.width = 1920;
    value.height = 0;
    assert!(matches!(
        value.validate(),
        Err(ProtocolError::InvalidCapabilities)
    ));
}
#[test]
fn input_timestamp_window_rejects_stale_and_future_values() {
    let envelope = InputEnvelope {
        sequence: 1,
        timestamp_us: 10,
        event: InputEvent::MouseMove { x: 1, y: 1 },
    };
    let stale_bytes = encode_input(&envelope).expect("encode");
    assert!(decode_input(&stale_bytes, 10 + MAX_INPUT_AGE_US + 1).is_err());
    let future = InputEnvelope {
        timestamp_us: 10 + MAX_INPUT_FUTURE_SKEW_US + 1,
        ..envelope
    };
    let future_bytes = encode_input(&future).expect("encode");
    assert!(decode_input(&future_bytes, 10).is_err());
}

#[test]
fn input_wire_decode_rejects_stale_and_future_values() {
    let stale = InputEnvelope {
        sequence: 1,
        timestamp_us: 10,
        event: InputEvent::MouseMove { x: 1, y: 1 },
    };
    let bytes = encode_input(&stale).expect("encode");
    assert!(matches!(
        decode_input(&bytes, 10 + MAX_INPUT_AGE_US + 1),
        Err(ProtocolError::TimestampOutsideWindow)
    ));
    let future = InputEnvelope {
        timestamp_us: 10 + MAX_INPUT_FUTURE_SKEW_US + 1,
        ..stale
    };
    let bytes = encode_input(&future).expect("encode");
    assert!(matches!(
        decode_input(&bytes, 10),
        Err(ProtocolError::TimestampOutsideWindow)
    ));
}

#[test]
fn input_round_trips_wheel_and_explicit_modifiers() {
    let cases = [
        InputEvent::MouseWheel {
            delta_x: -120,
            delta_y: 240,
        },
        InputEvent::Key {
            code: desklink_protocol::KeyCode::Character('c'),
            pressed: true,
            modifiers: Modifiers::CONTROL | Modifiers::SHIFT,
        },
    ];
    for (index, event) in cases.into_iter().enumerate() {
        let envelope = InputEnvelope {
            sequence: index as u64 + 1,
            timestamp_us: 1_000,
            event,
        };
        let bytes = encode_input(&envelope).expect("encode valid input");
        assert_eq!(decode_input(&bytes, 1_000).unwrap(), envelope);
    }
}

#[test]
fn input_rejects_out_of_bounds_pointer_wheel_and_modifier_values() {
    let invalid_events = [
        InputEvent::MouseMove { x: -1, y: 0 },
        InputEvent::MouseMove { x: 0, y: 1_000_001 },
        InputEvent::MouseWheel {
            delta_x: 0,
            delta_y: 0,
        },
        InputEvent::MouseWheel {
            delta_x: MAX_WHEEL_DELTA + 1,
            delta_y: 0,
        },
        InputEvent::Key {
            code: desklink_protocol::KeyCode::Enter,
            pressed: true,
            modifiers: Modifiers(0x80),
        },
    ];
    for (index, event) in invalid_events.into_iter().enumerate() {
        let envelope = InputEnvelope {
            sequence: index as u64 + 1,
            timestamp_us: 1_000,
            event,
        };
        assert!(matches!(
            encode_input(&envelope),
            Err(ProtocolError::InvalidInput)
        ));
        let bytes = postcard::to_allocvec(&envelope).unwrap();
        assert!(matches!(
            decode_input(&bytes, 1_000),
            Err(ProtocolError::InvalidInput)
        ));
    }
}

#[test]
fn control_channel_has_no_input_bypass() {
    let input = InputEnvelope {
        sequence: 1,
        timestamp_us: 10,
        event: InputEvent::MouseMove { x: 1, y: 1 },
    };
    let bytes = encode_input(&input).expect("encode input");
    assert!(matches!(
        decode_control(&bytes),
        Err(ProtocolError::Malformed)
    ));
    assert_eq!(
        decode_input(&bytes, 10).expect("separate input channel"),
        input
    );
}

#[test]
fn capabilities_require_nonempty_h264_list() {
    for codecs in [vec![], vec![Codec::H264]] {
        let mut value = DeviceCapabilities {
            platform: Platform::IOS,
            role: DeviceRole::Host,
            codecs,
            width: 1920,
            height: 1080,
        };
        if value.codecs.is_empty() {
            assert!(matches!(
                value.validate(),
                Err(ProtocolError::InvalidCapabilities)
            ));
        }
        value.codecs = vec![];
        assert!(matches!(
            value.validate(),
            Err(ProtocolError::InvalidCapabilities)
        ));
    }
}

#[test]
fn raw_video_packet_wire_round_trip_and_rejection() {
    let mut header = header();
    header.payload_length = 3;
    let packet = VideoPacket::new(header, vec![1, 2, 3]).expect("packet");
    let bytes = encode_video_packet(&packet).expect("encode");
    assert_eq!(decode_video_packet(&bytes).expect("decode"), packet);
    let mut raw = packet.clone();
    raw.header.payload_length = 2;
    let bytes = postcard::to_allocvec(&raw).expect("raw encode");
    assert!(matches!(
        decode_video_packet(&bytes),
        Err(ProtocolError::PayloadLengthMismatch { .. })
    ));
}
