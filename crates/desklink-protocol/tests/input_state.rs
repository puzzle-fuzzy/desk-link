use desklink_protocol::{ControlMessage, InputRestrictionReason, decode_control, encode_control};

#[test]
fn input_restriction_and_recovery_round_trip() {
    for reason in [
        Some(InputRestrictionReason::Blocked),
        Some(InputRestrictionReason::UnsupportedKey),
        Some(InputRestrictionReason::InvalidInput),
        None,
    ] {
        let message = ControlMessage::InputState { reason };
        let bytes = encode_control(&message).expect("encode input state");
        assert_eq!(
            bytes[0], 14,
            "append the variant; never shift existing control tags"
        );
        assert_eq!(decode_control(&bytes).expect("decode input state"), message);
    }
}

#[test]
fn input_state_rejects_unknown_reasons_and_truncated_payloads() {
    for bytes in [&[14][..], &[14, 1], &[14, 1, 3], &[14, 2]] {
        assert!(
            decode_control(bytes).is_err(),
            "invalid input state: {bytes:?}"
        );
    }
}

#[test]
fn existing_control_message_tags_remain_stable() {
    assert_eq!(
        encode_control(&ControlMessage::RequestKeyframe { stream_id: 1 }).unwrap(),
        [0, 1],
    );
    assert_eq!(
        encode_control(&ControlMessage::AudioState {
            available: true,
            enabled: false
        })
        .unwrap(),
        [7, 1, 0],
    );
    assert_eq!(
        encode_control(&ControlMessage::VideoPathCandidateAnswer {
            candidate_id: 1,
            accepted: false,
            candidate: None
        })
        .unwrap(),
        [13, 1, 0, 0],
    );
}
