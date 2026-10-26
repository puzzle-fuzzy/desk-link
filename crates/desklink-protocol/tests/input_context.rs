use std::collections::{HashMap, VecDeque};

use desklink_protocol::{
    Codec, ControlMessage, InputContext, InputEnvelope, InputEvent, KeyCode, Modifiers,
    MouseButton, PROTOCOL_VERSION, ProtocolError, VideoConfig, decode_control, decode_input,
    decode_session_input, decode_video_config, encode_control, encode_input, encode_video_config,
};
use serde::Serialize;

fn tagged_raw_input(input: &InputEnvelope) -> Vec<u8> {
    let version = PROTOCOL_VERSION.to_le_bytes();
    postcard::to_extend(input, vec![0, version[0], version[1]]).unwrap()
}

fn envelope(context: InputContext, sequence: u64, event: InputEvent) -> InputEnvelope {
    InputEnvelope {
        sequence,
        timestamp_us: 1_000,
        stream_id: context.stream_id,
        input_epoch: context.input_epoch,
        event,
    }
}

fn video_config() -> VideoConfig {
    VideoConfig {
        protocol_version: PROTOCOL_VERSION,
        stream_id: 7,
        input_epoch: 15,
        config_version: 3,
        codec: Codec::H264,
        width: 1920,
        height: 1080,
        sequence_header: vec![0, 0, 0, 1, 0x67, 0x4d, 0, 0x1f],
    }
}

#[test]
fn input_context_is_a_copyable_serializable_nonzero_pair() {
    for context in [
        InputContext {
            stream_id: 1,
            input_epoch: 1,
        },
        InputContext {
            stream_id: u64::MAX,
            input_epoch: u64::MAX,
        },
    ] {
        let captured = context;
        assert_eq!(context, captured);
        assert!(context.is_valid());
        assert_eq!(context.validate(), Ok(()));
        let ledger = HashMap::from([(context, 1_u32)]);
        assert_eq!(ledger.get(&captured), Some(&1));
        let bytes = postcard::to_allocvec(&context).unwrap();
        assert_eq!(
            postcard::from_bytes::<InputContext>(&bytes).unwrap(),
            context
        );
    }
    for context in [
        InputContext {
            stream_id: 0,
            input_epoch: 0,
        },
        InputContext {
            stream_id: 0,
            input_epoch: 1,
        },
        InputContext {
            stream_id: 1,
            input_epoch: 0,
        },
    ] {
        assert!(!context.is_valid());
        assert_eq!(context.validate(), Err(ProtocolError::InvalidInput));
        let input = envelope(context, 1, InputEvent::MouseMove { x: 1, y: 2 });
        assert_eq!(encode_input(&input), Err(ProtocolError::InvalidInput));
        // Bypass encoder validation so decoder rejection is independently tested.
        let bytes = tagged_raw_input(&input);
        assert_eq!(
            decode_session_input(&bytes),
            Err(ProtocolError::InvalidInput)
        );
        assert_eq!(
            decode_input(&bytes, 1_000),
            Err(ProtocolError::InvalidInput)
        );
    }
}

#[test]
fn every_input_kind_round_trips_its_explicit_context() {
    let context = InputContext {
        stream_id: u64::MAX,
        input_epoch: u64::MAX - 1,
    };
    let events = [
        InputEvent::MouseMove { x: 1_000_000, y: 0 },
        InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        },
        InputEvent::Key {
            code: KeyCode::Character('中'),
            pressed: false,
            modifiers: Modifiers::SHIFT,
        },
        InputEvent::MouseWheel {
            delta_x: -120,
            delta_y: 240,
        },
    ];
    for (index, event) in events.into_iter().enumerate() {
        let input = envelope(context, index as u64 + 1, event);
        let bytes = encode_input(&input).unwrap();
        let version = PROTOCOL_VERSION.to_le_bytes();
        assert_eq!(&bytes[..3], &[0, version[0], version[1]]);
        let expected_order = (
            input.sequence,
            input.timestamp_us,
            input.stream_id,
            input.input_epoch,
            &input.event,
        );
        assert_eq!(&bytes[3..], postcard::to_allocvec(&expected_order).unwrap());
        let decoded = decode_session_input(&bytes).unwrap();
        assert_eq!(decoded, input);
        assert_eq!(decoded.context(), context);
        assert_eq!(decode_input(&bytes, 1_000).unwrap().context(), context);
    }
}

#[test]
fn queued_input_keeps_capture_context_after_stream_or_epoch_changes() {
    let mut current = InputContext {
        stream_id: 2,
        input_epoch: 15,
    };
    let captured = current;
    let mut queue = VecDeque::from([envelope(
        captured,
        1,
        InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        },
    )]);
    current.input_epoch += 1;
    let pending = queue.pop_front().unwrap();
    let encoded = encode_input(&pending).unwrap();
    let decoded = decode_session_input(&encoded).unwrap();
    assert_eq!(decoded.context(), captured);
    assert_ne!(decoded.context(), current);
    current.stream_id += 1;
    assert_eq!(decoded.context(), captured);
    assert_ne!(decoded.context(), current);
}

#[test]
fn video_config_transmits_epoch_after_stream_and_rejects_zero_on_both_paths() {
    let config = video_config();
    let expected_order = (
        config.protocol_version,
        config.stream_id,
        config.input_epoch,
        config.config_version,
        config.codec,
        config.width,
        config.height,
        &config.sequence_header,
    );
    let bytes = encode_video_config(&config).unwrap();
    assert_eq!(bytes, postcard::to_allocvec(&expected_order).unwrap());
    assert_eq!(decode_video_config(&bytes).unwrap(), config);
    for (stream_id, input_epoch) in [(0, 15), (7, 0), (0, 0)] {
        let invalid = VideoConfig {
            stream_id,
            input_epoch,
            ..config.clone()
        };
        assert_eq!(
            encode_video_config(&invalid),
            Err(ProtocolError::InvalidVideoConfig)
        );
        assert_eq!(
            decode_video_config(&postcard::to_allocvec(&invalid).unwrap()),
            Err(ProtocolError::InvalidVideoConfig)
        );
    }
}

#[test]
fn old_video_config_without_epoch_is_not_a_valid_new_configuration() {
    #[derive(Serialize)]
    struct LegacyVideoConfig<'a> {
        protocol_version: u16,
        stream_id: u64,
        config_version: u32,
        codec: Codec,
        width: u16,
        height: u16,
        sequence_header: &'a [u8],
    }
    let config = video_config();
    let legacy = LegacyVideoConfig {
        protocol_version: PROTOCOL_VERSION,
        stream_id: config.stream_id,
        config_version: config.config_version,
        codec: config.codec,
        width: config.width,
        height: config.height,
        sequence_header: &config.sequence_header,
    };
    assert!(decode_video_config(&postcard::to_allocvec(&legacy).unwrap()).is_err());
}

#[test]
fn input_with_missing_context_fields_is_rejected_without_filling_current_context() {
    #[derive(Serialize)]
    struct MissingContextInput {
        sequence: u64,
        timestamp_us: u64,
        event: InputEvent,
    }
    let legacy = MissingContextInput {
        sequence: 1,
        timestamp_us: 1_000,
        event: InputEvent::MouseMove { x: 1, y: 2 },
    };
    let bytes = postcard::to_allocvec(&legacy).unwrap();
    assert!(decode_session_input(&bytes).is_err());
    assert!(decode_input(&bytes, 1_000).is_err());
}

#[test]
fn legacy_function_key_cannot_alias_a_new_mouse_button_context() {
    // Under an untagged field-only extension these old bytes become a valid
    // stream=2, epoch=15, MouseButton(Right, false) envelope. The discriminator
    // must reject them before the new struct can reinterpret the old fields.
    let legacy_function_key = [1, 1, 2, 15, 1, 1, 0];
    let aliased: InputEnvelope = postcard::from_bytes(&legacy_function_key).unwrap();
    assert_eq!(
        aliased.context(),
        InputContext {
            stream_id: 2,
            input_epoch: 15
        }
    );
    assert_eq!(
        aliased.event,
        InputEvent::MouseButton {
            button: MouseButton::Right,
            pressed: false
        }
    );
    assert_eq!(
        decode_session_input(&legacy_function_key),
        Err(ProtocolError::Malformed)
    );
    assert_eq!(
        decode_input(&legacy_function_key, 1),
        Err(ProtocolError::Malformed)
    );
}

#[test]
fn input_wire_requires_a_complete_discriminator_and_matching_version() {
    let input = envelope(
        InputContext {
            stream_id: 1,
            input_epoch: 1,
        },
        1,
        InputEvent::MouseMove { x: 1, y: 2 },
    );
    let bytes = encode_input(&input).unwrap();
    for truncated in [&bytes[..0], &bytes[..1], &bytes[..2], &bytes[..3]] {
        assert_eq!(
            decode_session_input(truncated),
            Err(ProtocolError::Malformed)
        );
    }
    let mut wrong_discriminator = bytes.clone();
    wrong_discriminator[0] = 1;
    assert_eq!(
        decode_session_input(&wrong_discriminator),
        Err(ProtocolError::Malformed)
    );
    for version in [0_u16, PROTOCOL_VERSION - 1, PROTOCOL_VERSION + 1] {
        let mut wrong_version = bytes.clone();
        wrong_version[1..3].copy_from_slice(&version.to_le_bytes());
        assert_eq!(
            decode_session_input(&wrong_version),
            Err(ProtocolError::UnsupportedVersion(version))
        );
    }
    let zero_sequence = InputEnvelope {
        sequence: 0,
        ..input
    };
    assert_eq!(
        encode_input(&zero_sequence),
        Err(ProtocolError::InvalidInput)
    );
    assert_eq!(
        decode_session_input(&tagged_raw_input(&zero_sequence)),
        Err(ProtocolError::InvalidInput)
    );
    assert_eq!(
        decode_input(&tagged_raw_input(&zero_sequence), 1_000),
        Err(ProtocolError::InvalidInput)
    );
}

#[test]
fn complete_input_video_config_and_control_values_reject_any_trailing_bytes() {
    let input = envelope(
        InputContext {
            stream_id: 1,
            input_epoch: 1,
        },
        1,
        InputEvent::MouseMove { x: 1, y: 2 },
    );
    let input_bytes = encode_input(&input).unwrap();
    let config_bytes = encode_video_config(&video_config()).unwrap();
    let control_bytes = encode_control(&ControlMessage::RequestKeyframe { stream_id: 1 }).unwrap();
    let version = PROTOCOL_VERSION.to_le_bytes();
    let input_tag = [0, version[0], version[1]];
    for suffix in [&[0_u8][..], input_tag.as_slice(), input_bytes.as_slice()] {
        let mut trailing_input = input_bytes.clone();
        trailing_input.extend_from_slice(suffix);
        assert_eq!(
            decode_session_input(&trailing_input),
            Err(ProtocolError::Malformed)
        );
        assert_eq!(
            decode_input(&trailing_input, 1_000),
            Err(ProtocolError::Malformed)
        );
        let mut trailing_config = config_bytes.clone();
        trailing_config.extend_from_slice(suffix);
        assert_eq!(
            decode_video_config(&trailing_config),
            Err(ProtocolError::Malformed)
        );
        let mut trailing_control = control_bytes.clone();
        trailing_control.extend_from_slice(suffix);
        assert_eq!(
            decode_control(&trailing_control),
            Err(ProtocolError::Malformed)
        );
    }
    // A tagged input is not a RequestKeyframe plus ignored leftover bytes.
    assert_eq!(decode_control(&input_bytes), Err(ProtocolError::Malformed));
}
