use crate::{
    ControlMessage, FrameFlags, InputEnvelope, MAX_CONTROL_MESSAGE_BYTES,
    MAX_DATAGRAM_PAYLOAD_BYTES, MAX_INPUT_AGE_US, MAX_INPUT_FUTURE_SKEW_US, MAX_MVP_HEIGHT,
    MAX_MVP_WIDTH, MAX_VIDEO_CHUNKS, PROTOCOL_VERSION, VideoFrameHeader, VideoPacket,
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
    #[error("input timestamp is outside the accepted window")]
    TimestampOutsideWindow,
}

const MAX_VIDEO_PACKET_BYTES: usize = 128 + MAX_DATAGRAM_PAYLOAD_BYTES as usize;

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
    bounded(
        postcard::to_allocvec(input).map_err(|_| ProtocolError::Malformed)?,
        MAX_CONTROL_MESSAGE_BYTES,
    )
}
pub fn decode_input(bytes: &[u8], now_us: u64) -> Result<InputEnvelope, ProtocolError> {
    ensure(bytes, MAX_CONTROL_MESSAGE_BYTES)?;
    let input: InputEnvelope = postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    if input.timestamp_us < now_us.saturating_sub(MAX_INPUT_AGE_US)
        || input.timestamp_us > now_us.saturating_add(MAX_INPUT_FUTURE_SKEW_US)
    {
        return Err(ProtocolError::TimestampOutsideWindow);
    }
    Ok(input)
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
