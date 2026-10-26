use desklink_protocol::{
    Codec, ControlMessage, DeviceCapabilities, DeviceRole, H264Profile,
    LAN_DATA_CHANNEL_AUDIO_DATAGRAM, LAN_DATA_CHANNEL_CONTROL, LAN_DATA_CHANNEL_CURSOR_DATAGRAM,
    LAN_DATA_CHANNEL_INPUT, LAN_DATA_CHANNEL_TRANSFER, LAN_DATA_CHANNEL_VIDEO_CONFIG,
    LAN_DATA_CHANNEL_VIDEO_DATAGRAM, LAN_DATA_CHANNEL_VIDEO_RELIABLE, LAN_DATA_ENVELOPE_VERSION,
    LAN_DATA_KNOWN_CHANNELS, LanDataCapabilities, LanDataSwitchPhase, MAX_CONTROL_MESSAGE_BYTES,
    Platform, ProtocolError, decode_control, encode_control,
};
use serde::Serialize;

fn lan(channels: u16, continuation: bool) -> LanDataCapabilities {
    LanDataCapabilities {
        envelope_version: LAN_DATA_ENVELOPE_VERSION,
        channels,
        allow_relay_loss_continuation: continuation,
    }
}

fn capabilities(lan_data: Option<LanDataCapabilities>) -> DeviceCapabilities {
    DeviceCapabilities {
        platform: Platform::Windows,
        role: DeviceRole::Controller,
        codecs: vec![Codec::H264],
        h264_profiles: vec![H264Profile::Main],
        width: 1920,
        height: 1080,
        lan_data,
    }
}

fn switch(
    candidate_id: u64,
    generation: u64,
    channels: u16,
    phase: LanDataSwitchPhase,
) -> ControlMessage {
    ControlMessage::LanDataSwitch {
        candidate_id,
        generation,
        channels,
        phase,
    }
}

#[test]
fn authenticated_capabilities_round_trip_explicit_absence_and_all_known_lanes() {
    for lane in [
        LAN_DATA_CHANNEL_CONTROL,
        LAN_DATA_CHANNEL_INPUT,
        LAN_DATA_CHANNEL_VIDEO_CONFIG,
        LAN_DATA_CHANNEL_VIDEO_DATAGRAM,
        LAN_DATA_CHANNEL_CURSOR_DATAGRAM,
        LAN_DATA_CHANNEL_TRANSFER,
        LAN_DATA_CHANNEL_AUDIO_DATAGRAM,
        LAN_DATA_CHANNEL_VIDEO_RELIABLE,
        LAN_DATA_KNOWN_CHANNELS,
    ] {
        let message = ControlMessage::Capabilities(capabilities(Some(lan(lane, true))));
        assert_eq!(
            decode_control(&encode_control(&message).unwrap()).unwrap(),
            message
        );
    }
    let message = ControlMessage::Capabilities(capabilities(None));
    assert_eq!(
        decode_control(&encode_control(&message).unwrap()).unwrap(),
        message
    );
}

#[test]
fn capability_codec_rejects_unknown_envelope_versions_empty_and_unknown_lane_masks() {
    let invalid = [
        LanDataCapabilities {
            envelope_version: 0,
            ..lan(1, false)
        },
        LanDataCapabilities {
            envelope_version: 2,
            ..lan(1, false)
        },
        LanDataCapabilities {
            envelope_version: u16::MAX,
            ..lan(1, false)
        },
        lan(0, false),
        lan(1 << 8, false),
        lan(LAN_DATA_KNOWN_CHANNELS | 1 << 15, true),
    ];
    for value in invalid {
        let message = ControlMessage::Capabilities(capabilities(Some(value)));
        assert_eq!(
            encode_control(&message),
            Err(ProtocolError::InvalidCapabilities)
        );
        assert_eq!(
            decode_control(&postcard::to_allocvec(&message).unwrap()),
            Err(ProtocolError::InvalidCapabilities)
        );
    }
}

#[test]
fn lan_data_requires_both_opt_ins_intersects_lanes_and_ands_continuation() {
    let none = capabilities(None);
    let local = capabilities(Some(lan(
        LAN_DATA_CHANNEL_INPUT | LAN_DATA_CHANNEL_TRANSFER,
        true,
    )));
    assert_eq!(local.negotiated_lan_data(&none), None);
    assert_eq!(none.negotiated_lan_data(&local), None);
    let remote = capabilities(Some(lan(
        LAN_DATA_CHANNEL_TRANSFER | LAN_DATA_CHANNEL_AUDIO_DATAGRAM,
        false,
    )));
    assert_eq!(
        local.negotiated_lan_data(&remote),
        Some(lan(LAN_DATA_CHANNEL_TRANSFER, false))
    );
    let remote = capabilities(Some(lan(LAN_DATA_CHANNEL_TRANSFER, true)));
    assert_eq!(
        local.negotiated_lan_data(&remote),
        Some(lan(LAN_DATA_CHANNEL_TRANSFER, true))
    );
    let remote = capabilities(Some(lan(LAN_DATA_CHANNEL_AUDIO_DATAGRAM, true)));
    assert_eq!(local.negotiated_lan_data(&remote), None);
}

#[test]
fn switch_phase_round_trips_preserve_full_width_ids_generation_and_lane_masks() {
    for phase in [
        LanDataSwitchPhase::Offer,
        LanDataSwitchPhase::Ready,
        LanDataSwitchPhase::Commit,
    ] {
        let message = switch(u64::MAX, u64::MAX, LAN_DATA_KNOWN_CHANNELS, phase);
        assert_eq!(
            decode_control(&encode_control(&message).unwrap()).unwrap(),
            message
        );
    }
}

#[test]
fn switch_codec_rejects_zero_ids_empty_unknown_channels_and_invalid_phase() {
    for (candidate_id, generation, channels) in [
        (0, 1, 1),
        (1, 0, 1),
        (1, 1, 0),
        (1, 1, 1 << 8),
        (1, 1, u16::MAX),
    ] {
        let message = switch(
            candidate_id,
            generation,
            channels,
            LanDataSwitchPhase::Offer,
        );
        assert_eq!(
            encode_control(&message),
            Err(ProtocolError::InvalidLanDataMessage)
        );
        assert_eq!(
            decode_control(&postcard::to_allocvec(&message).unwrap()),
            Err(ProtocolError::InvalidLanDataMessage)
        );
    }
    let mut bytes = encode_control(&switch(1, 1, 1, LanDataSwitchPhase::Offer)).unwrap();
    *bytes.last_mut().unwrap() = 3;
    assert_eq!(decode_control(&bytes), Err(ProtocolError::Malformed));
}

#[test]
fn lan_control_keeps_existing_discriminants_and_exact_bounded_consumption() {
    assert_eq!(
        encode_control(&ControlMessage::InputState { reason: None }).unwrap(),
        [14, 0]
    );
    let message = switch(1, 2, LAN_DATA_KNOWN_CHANNELS, LanDataSwitchPhase::Offer);
    let bytes = encode_control(&message).unwrap();
    assert_eq!(bytes, [15, 1, 2, 255, 1, 0]);
    for end in 0..bytes.len() {
        assert!(
            decode_control(&bytes[..end]).is_err(),
            "truncation at {end}"
        );
    }
    let mut appended = bytes;
    appended.push(0);
    assert_eq!(decode_control(&appended), Err(ProtocolError::Malformed));
    assert!(matches!(
        decode_control(&vec![0; MAX_CONTROL_MESSAGE_BYTES + 1]),
        Err(ProtocolError::MessageTooLarge { .. })
    ));
}

#[test]
fn missing_wire_option_is_malformed_and_cannot_implicitly_enable_lan() {
    #[derive(Serialize)]
    struct EarlierCapabilities {
        platform: Platform,
        role: DeviceRole,
        codecs: Vec<Codec>,
        h264_profiles: Vec<H264Profile>,
        width: u16,
        height: u16,
    }
    let earlier = EarlierCapabilities {
        platform: Platform::Windows,
        role: DeviceRole::Controller,
        codecs: vec![Codec::H264],
        h264_profiles: vec![H264Profile::Main],
        width: 1920,
        height: 1080,
    };
    let mut bytes = vec![2]; // Existing Capabilities discriminant.
    bytes.extend(postcard::to_allocvec(&earlier).unwrap());
    assert_eq!(decode_control(&bytes), Err(ProtocolError::Malformed));
    assert_eq!(
        capabilities(None).negotiated_lan_data(&capabilities(Some(lan(1, true)))),
        None
    );
}
