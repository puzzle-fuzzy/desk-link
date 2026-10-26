use desklink_protocol::{
    AudioCodec, AudioPacket, Codec, CursorUpdate, FrameFlags, NoiseHandshake, NoiseHandshakeStep,
    PROTOCOL_VERSION, ProtocolError, VideoConfig, VideoFrameHeader, decode_audio_packet,
    decode_cursor_update, decode_noise_handshake, decode_video_config, decode_video_header,
};

#[test]
fn protocol_12_is_rejected_by_every_versioned_payload_boundary() {
    assert_eq!(PROTOCOL_VERSION, 13);
    let noise = NoiseHandshake {
        protocol_version: 12,
        step: NoiseHandshakeStep::ResponderHello,
        payload: vec![1],
    };
    assert_eq!(
        decode_noise_handshake(&postcard::to_allocvec(&noise).unwrap()),
        Err(ProtocolError::UnsupportedVersion(12))
    );
    let config = VideoConfig {
        protocol_version: 12,
        stream_id: 1,
        config_version: 1,
        codec: Codec::H264,
        width: 1920,
        height: 1080,
        sequence_header: vec![0, 0, 0, 1, 0x67],
    };
    assert_eq!(
        decode_video_config(&postcard::to_allocvec(&config).unwrap()),
        Err(ProtocolError::UnsupportedVersion(12))
    );
    let header = VideoFrameHeader {
        protocol_version: 12,
        stream_id: 1,
        config_version: 1,
        frame_id: 1,
        capture_timestamp_us: 1,
        width: 1920,
        height: 1080,
        flags: FrameFlags::KEYFRAME,
        chunk_index: 0,
        chunk_count: 1,
        payload_length: 1,
    };
    assert_eq!(
        decode_video_header(&postcard::to_allocvec(&header).unwrap()),
        Err(ProtocolError::UnsupportedVersion(12))
    );
    let cursor = CursorUpdate {
        protocol_version: 12,
        stream_id: 1,
        sequence: 1,
        timestamp_us: 1,
        x_millionths: 0,
        y_millionths: 0,
        visible: true,
        shape_id: 1,
    };
    assert_eq!(
        decode_cursor_update(&postcard::to_allocvec(&cursor).unwrap()),
        Err(ProtocolError::UnsupportedVersion(12))
    );
    let audio = AudioPacket {
        protocol_version: 12,
        stream_id: 1,
        sequence: 1,
        capture_timestamp_us: 1,
        codec: AudioCodec::PcmS16Le,
        sample_rate: 48_000,
        channels: 1,
        payload: vec![0, 0],
    };
    assert_eq!(
        decode_audio_packet(&postcard::to_allocvec(&audio).unwrap()),
        Err(ProtocolError::UnsupportedVersion(12))
    );
}
