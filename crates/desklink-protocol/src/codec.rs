use crate::{
    AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, AudioCodec, AudioPacket, Codec, ControlMessage,
    CursorUpdate, FrameFlags, InputEnvelope, InputEvent, MAX_AUDIO_PACKET_BYTES,
    MAX_AUDIO_PAYLOAD_BYTES, MAX_CONTROL_MESSAGE_BYTES, MAX_CURSOR_MESSAGE_BYTES,
    MAX_DATAGRAM_PAYLOAD_BYTES, MAX_INPUT_AGE_US, MAX_INPUT_FUTURE_SKEW_US, MAX_MVP_HEIGHT,
    MAX_MVP_WIDTH, MAX_NOISE_HANDSHAKE_BYTES, MAX_OPUS_AUDIO_PAYLOAD_BYTES, MAX_POINTER_COORDINATE,
    MAX_VIDEO_CHUNKS, MAX_VIDEO_CONFIG_BYTES, MAX_VIDEO_PACKET_BYTES, MAX_WHEEL_DELTA,
    NoiseHandshake, PROTOCOL_VERSION, TransferMessage, VideoConfig, VideoFrameHeader, VideoPacket,
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
    #[error("invalid audio packet")]
    InvalidAudio,
    #[error("input timestamp is outside the accepted window")]
    TimestampOutsideWindow,
    #[error("invalid input event")]
    InvalidInput,
    #[error("invalid remote display list")]
    InvalidDisplayList,
    #[error("invalid clipboard or file transfer message")]
    InvalidTransfer,
}

pub fn encode_control(message: &ControlMessage) -> Result<Vec<u8>, ProtocolError> {
    if let ControlMessage::Capabilities(capabilities) = message {
        capabilities.validate()?;
    }
    if let ControlMessage::DisplayList {
        displays,
        active_display_id,
    } = message
    {
        validate_display_list(displays, *active_display_id)?;
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
    if let ControlMessage::DisplayList {
        displays,
        active_display_id,
    } = &message
    {
        validate_display_list(displays, *active_display_id)?;
    }
    Ok(message)
}

pub fn encode_transfer(message: &TransferMessage) -> Result<Vec<u8>, ProtocolError> {
    validate_transfer(message)?;
    bounded(
        postcard::to_allocvec(message).map_err(|_| ProtocolError::Malformed)?,
        crate::MAX_TRANSFER_MESSAGE_BYTES,
    )
}

pub fn decode_transfer(bytes: &[u8]) -> Result<TransferMessage, ProtocolError> {
    ensure(bytes, crate::MAX_TRANSFER_MESSAGE_BYTES)?;
    let message = postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    validate_transfer(&message)?;
    Ok(message)
}

fn validate_transfer(message: &TransferMessage) -> Result<(), ProtocolError> {
    use TransferMessage::*;

    let valid_id = |id: &[u8; 16]| id.iter().any(|byte| *byte != 0);
    let valid = match message {
        ClipboardSet { request_id, text } | ClipboardData { request_id, text } => {
            *request_id != 0 && text.len() <= crate::MAX_CLIPBOARD_TEXT_BYTES
        }
        ClipboardRequest { request_id }
        | ClipboardResult { request_id, .. }
        | FileSelectionRequest { request_id }
        | FileSelectionCancel { request_id }
        | FileSelectionResult { request_id, .. } => *request_id != 0,
        FileOffer {
            transfer_id,
            name,
            size,
        } => {
            valid_id(transfer_id)
                && *size <= crate::MAX_TRANSFER_FILE_BYTES
                && crate::is_valid_transfer_file_name(name)
        }
        FileDecision { transfer_id, .. }
        | FileComplete { transfer_id, .. }
        | FileResult { transfer_id, .. }
        | Cancel { transfer_id } => valid_id(transfer_id),
        FileChunk {
            transfer_id,
            offset,
            bytes,
        } => {
            valid_id(transfer_id)
                && !bytes.is_empty()
                && bytes.len() <= crate::MAX_TRANSFER_CHUNK_BYTES
                && offset
                    .checked_add(bytes.len() as u64)
                    .is_some_and(|end| end <= crate::MAX_TRANSFER_FILE_BYTES)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(ProtocolError::InvalidTransfer)
    }
}

fn validate_display_list(
    displays: &[crate::RemoteDisplay],
    active_display_id: u32,
) -> Result<(), ProtocolError> {
    if displays.is_empty()
        || displays.len() > 16
        || !displays
            .iter()
            .any(|display| display.id == active_display_id)
        || displays
            .iter()
            .any(|display| display.width == 0 || display.height == 0)
    {
        return Err(ProtocolError::InvalidDisplayList);
    }
    for (index, display) in displays.iter().enumerate() {
        if displays[..index]
            .iter()
            .any(|previous| previous.id == display.id)
        {
            return Err(ProtocolError::InvalidDisplayList);
        }
    }
    Ok(())
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
pub fn encode_audio_packet(packet: &AudioPacket) -> Result<Vec<u8>, ProtocolError> {
    validate_audio_packet(packet)?;
    bounded(
        postcard::to_allocvec(packet).map_err(|_| ProtocolError::Malformed)?,
        MAX_AUDIO_PACKET_BYTES,
    )
}
pub fn decode_audio_packet(bytes: &[u8]) -> Result<AudioPacket, ProtocolError> {
    ensure(bytes, MAX_AUDIO_PACKET_BYTES)?;
    let packet = postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    validate_audio_packet(&packet)?;
    Ok(packet)
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
/// Decodes and structurally validates an input envelope without comparing its
/// wall-clock timestamp to the local machine. Authenticated sessions use this
/// entry point so they can establish a per-session clock offset before applying
/// freshness checks; two Windows computers are not guaranteed to have clocks
/// synchronized to within a second.
pub fn decode_session_input(bytes: &[u8]) -> Result<InputEnvelope, ProtocolError> {
    ensure(bytes, MAX_CONTROL_MESSAGE_BYTES)?;
    let input: InputEnvelope = postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    validate_input(&input)?;
    Ok(input)
}

pub fn decode_input(bytes: &[u8], now_us: u64) -> Result<InputEnvelope, ProtocolError> {
    let input = decode_session_input(bytes)?;
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
            InputEvent::Key {
                code, modifiers, ..
            } => {
                code.is_valid()
                    && modifiers.is_valid()
                    && code
                        .modifier_mask()
                        .is_none_or(|own_modifier| !modifiers.contains(own_modifier))
            }
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

fn validate_audio_packet(packet: &AudioPacket) -> Result<(), ProtocolError> {
    if packet.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(packet.protocol_version));
    }
    if packet.stream_id == 0
        || packet.sequence == 0
        || packet.capture_timestamp_us == 0
        || packet.sample_rate != AUDIO_SAMPLE_RATE
        || packet.channels != AUDIO_CHANNELS
    {
        return Err(ProtocolError::InvalidAudio);
    }
    let valid_payload = match packet.codec {
        AudioCodec::PcmS16Le => {
            !packet.payload.is_empty()
                && packet.payload.len() <= MAX_AUDIO_PAYLOAD_BYTES
                && packet.payload.len().is_multiple_of(2)
        }
        AudioCodec::Opus => {
            !packet.payload.is_empty() && packet.payload.len() <= MAX_OPUS_AUDIO_PAYLOAD_BYTES
        }
    };
    if !valid_payload {
        return Err(ProtocolError::InvalidAudio);
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
