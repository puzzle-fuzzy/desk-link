use crate::{
    Codec, ControlMessage, CursorUpdate, FrameFlags, InputEnvelope, InputEvent,
    MAX_CONTROL_MESSAGE_BYTES, MAX_CURSOR_MESSAGE_BYTES, MAX_DATAGRAM_PAYLOAD_BYTES,
    MAX_INPUT_AGE_US, MAX_INPUT_FUTURE_SKEW_US, MAX_MVP_HEIGHT, MAX_MVP_WIDTH,
    MAX_NOISE_HANDSHAKE_BYTES, MAX_POINTER_COORDINATE, MAX_VIDEO_CHUNKS, MAX_VIDEO_CONFIG_BYTES,
    MAX_VIDEO_PACKET_BYTES, MAX_WHEEL_DELTA, NoiseHandshake, PROTOCOL_VERSION, VideoConfig,
    VideoFrameHeader, VideoPacket,
};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("message too large: {actual} bytes (maximum {maximum})")]
    MessageTooLarge { actual: usize, maximum: usize },
    #[error("malformed protocol data")]
    Malformed,
    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u16),
    #[error("invalid frame header")]
    InvalidFrame,
    #[error("unknown frame flags")]
    InvalidFlags,
    #[error("video payload length mismatch: declared {declared}, actual {actual}")]
    PayloadLengthMismatch { declared: u32, actual: usize },
    #[error("invalid device capabilities")]
    InvalidCapabilities,
    #[error("invalid video configuration")]
    InvalidVideoConfig,
    #[error("invalid cursor update")]
    InvalidCursor,
    #[error("input timestamp is outside the accepted window")]
    TimestampOutsideWindow,
    #[error("invalid input event")]
    InvalidInput,
}

pub fn encode_control(message: &ControlMessage) -> Result<Vec<u8>, ProtocolError> {
    if let ControlMessage::Capabilities(capabilities) = message {
        capabilities.validate()?;
    }
    let bytes = postcard::to_allocvec(message).map_err(|_| ProtocolError::Malformed)?;
    bounded(bytes, MAX_CONTROL_MESSAGE_BYTES)
}
pub fn decode_control(bytes: &[u8]) -> Result<ControlMessage, ProtocolError> {
    ensure(bytes, MAX_CONTROL_MESSAGE_BYTES)?;
    let message = postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    if let ControlMessage::Capabilities(capabilities) = &message {
        capabilities.validate()?;
    }
    Ok(message)
}
pub fn encode_noise_handshake(handshake: &NoiseHandshake) -> Result<Vec<u8>, ProtocolError> {
    validate_noise_handshake(handshake)?;
    let bytes = postcard::to_allocvec(handshake).map_err(|_| ProtocolError::Malformed)?;
    bounded(bytes, MAX_CONTROL_MESSAGE_BYTES)
}
pub fn decode_noise_handshake(bytes: &[u8]) -> Result<NoiseHandshake, ProtocolError> {
    ensure(bytes, MAX_CONTROL_MESSAGE_BYTES)?;
    let handshake = postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    validate_noise_handshake(&handshake)?;
    Ok(handshake)
}
pub fn encode_video_config(config: &VideoConfig) -> Result<Vec<u8>, ProtocolError> {
    validate_video_config(config)?;
    let bytes = postcard::to_allocvec(config).map_err(|_| ProtocolError::Malformed)?;
    bounded(bytes, MAX_VIDEO_CONFIG_BYTES)
}
pub fn decode_video_config(bytes: &[u8]) -> Result<VideoConfig, ProtocolError> {
    ensure(bytes, MAX_VIDEO_CONFIG_BYTES)?;
    let config = postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    validate_video_config(&config)?;
    Ok(config)
}
pub fn encode_cursor_update(cursor: &CursorUpdate) -> Result<Vec<u8>, ProtocolError> {
    validate_cursor_update(cursor)?;
    let bytes = postcard::to_allocvec(cursor).map_err(|_| ProtocolError::Malformed)?;
    bounded(bytes, MAX_CURSOR_MESSAGE_BYTES)
}
pub fn decode_cursor_update(bytes: &[u8]) -> Result<CursorUpdate, ProtocolError> {
    ensure(bytes, MAX_CURSOR_MESSAGE_BYTES)?;
    let cursor = postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    validate_cursor_update(&cursor)?;
    Ok(cursor)
}
pub fn encode_video_header(header: &VideoFrameHeader) -> Result<Vec<u8>, ProtocolError> {
    validate_video_header(header)?;
    postcard::to_allocvec(header).map_err(|_| ProtocolError::Malformed)
}
pub fn decode_video_header(bytes: &[u8]) -> Result<VideoFrameHeader, ProtocolError> {
    ensure(bytes, 128)?;
    let header: VideoFrameHeader =
        postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    validate_video_header(&header)?;
    Ok(header)
}
pub fn encode_input(input: &InputEnvelope) -> Result<Vec<u8>, ProtocolError> {
    validate_input(input)?;
    bounded(
        postcard::to_allocvec(input).map_err(|_| ProtocolError::Malformed)?,
        MAX_CONTROL_MESSAGE_BYTES,
    )
}
pub fn decode_input(bytes: &[u8], now_us: u64) -> Result<InputEnvelope, ProtocolError> {
    ensure(bytes, MAX_CONTROL_MESSAGE_BYTES)?;
    let input: InputEnvelope = postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    validate_input(&input)?;
    if input.timestamp_us < now_us.saturating_sub(MAX_INPUT_AGE_US)
        || input.timestamp_us > now_us.saturating_add(MAX_INPUT_FUTURE_SKEW_US)
    {
        return Err(ProtocolError::TimestampOutsideWindow);
    }
    Ok(input)
}

fn validate_input(input: &InputEnvelope) -> Result<(), ProtocolError> {
    let valid = input.sequence != 0
        && match &input.event {
            InputEvent::MouseMove { x, y } => {
                (0..=MAX_POINTER_COORDINATE).contains(x) && (0..=MAX_POINTER_COORDINATE).contains(y)
            }
            InputEvent::MouseButton { .. } => true,
            InputEvent::Key { modifiers, .. } => modifiers.is_valid(),
            InputEvent::MouseWheel { delta_x, delta_y } => {
                (*delta_x != 0 || *delta_y != 0)
                    && (-MAX_WHEEL_DELTA..=MAX_WHEEL_DELTA).contains(delta_x)
                    && (-MAX_WHEEL_DELTA..=MAX_WHEEL_DELTA).contains(delta_y)
            }
        };
    if valid {
        Ok(())
    } else {
        Err(ProtocolError::InvalidInput)
    }
}
pub fn encode_video_packet(packet: &VideoPacket) -> Result<Vec<u8>, ProtocolError> {
    let packet = VideoPacket::new(packet.header.clone(), packet.payload.clone())?;
    bounded(
        postcard::to_allocvec(&packet).map_err(|_| ProtocolError::Malformed)?,
        MAX_VIDEO_PACKET_BYTES,
    )
}
pub fn decode_video_packet(bytes: &[u8]) -> Result<VideoPacket, ProtocolError> {
    ensure(bytes, MAX_VIDEO_PACKET_BYTES)?;
    let packet: VideoPacket = postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    VideoPacket::new(packet.header, packet.payload)
}
fn bounded(bytes: Vec<u8>, maximum: usize) -> Result<Vec<u8>, ProtocolError> {
    if bytes.len() > maximum {
        Err(ProtocolError::MessageTooLarge {
            actual: bytes.len(),
            maximum,
        })
    } else {
        Ok(bytes)
    }
}
fn ensure(bytes: &[u8], maximum: usize) -> Result<(), ProtocolError> {
    if bytes.len() > maximum {
        Err(ProtocolError::MessageTooLarge {
            actual: bytes.len(),
            maximum,
        })
    } else {
        Ok(())
    }
}
pub(crate) fn validate_video_header(header: &VideoFrameHeader) -> Result<(), ProtocolError> {
    if header.protocol_version != PROTOCOL_VERSION
        || header.chunk_count == 0
        || header.chunk_count > MAX_VIDEO_CHUNKS
        || header.chunk_index >= header.chunk_count
        || header.width > MAX_MVP_WIDTH
        || header.height > MAX_MVP_HEIGHT
        || header.payload_length > MAX_DATAGRAM_PAYLOAD_BYTES
        || header.flags.0 & !FrameFlags::KNOWN_BITS != 0
    {
        return Err(if header.protocol_version != PROTOCOL_VERSION {
            ProtocolError::UnsupportedVersion(header.protocol_version)
        } else if header.flags.0 & !FrameFlags::KNOWN_BITS != 0 {
            ProtocolError::InvalidFlags
        } else {
            ProtocolError::InvalidFrame
        });
    }
    Ok(())
}

fn validate_video_config(config: &VideoConfig) -> Result<(), ProtocolError> {
    if config.sequence_header.len() > MAX_VIDEO_CONFIG_BYTES {
        return Err(ProtocolError::MessageTooLarge {
            actual: config.sequence_header.len(),
            maximum: MAX_VIDEO_CONFIG_BYTES,
        });
    }
    if config.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(config.protocol_version));
    }
    if config.stream_id == 0
        || config.config_version == 0
        || config.width == 0
        || config.height == 0
        || config.width > MAX_MVP_WIDTH
        || config.height > MAX_MVP_HEIGHT
        || config.sequence_header.is_empty()
        || !matches!(config.codec, Codec::H264)
    {
        return Err(ProtocolError::InvalidVideoConfig);
    }
    Ok(())
}

fn validate_cursor_update(cursor: &CursorUpdate) -> Result<(), ProtocolError> {
    if cursor.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(cursor.protocol_version));
    }
    if cursor.stream_id == 0
        || cursor.sequence == 0
        || !(0..=1_000_000).contains(&cursor.x_millionths)
        || !(0..=1_000_000).contains(&cursor.y_millionths)
    {
        return Err(ProtocolError::InvalidCursor);
    }
    Ok(())
}

fn validate_noise_handshake(handshake: &NoiseHandshake) -> Result<(), ProtocolError> {
    if handshake.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(
            handshake.protocol_version,
        ));
    }
    if handshake.payload.is_empty() || handshake.payload.len() > MAX_NOISE_HANDSHAKE_BYTES {
        return Err(ProtocolError::Malformed);
    }
    Ok(())
}
