use crate::{
    ControlMessage, MAX_CONTROL_MESSAGE_BYTES, MAX_DATAGRAM_PAYLOAD_BYTES, PROTOCOL_VERSION,
    VideoFrameHeader,
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
}

pub fn encode_control(message: &ControlMessage) -> Result<Vec<u8>, ProtocolError> {
    let bytes = postcard::to_allocvec(message).map_err(|_| ProtocolError::Malformed)?;
    bounded(bytes, MAX_CONTROL_MESSAGE_BYTES)
}
pub fn decode_control(bytes: &[u8]) -> Result<ControlMessage, ProtocolError> {
    ensure(bytes, MAX_CONTROL_MESSAGE_BYTES)?;
    postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)
}
pub fn encode_video_header(header: &VideoFrameHeader) -> Result<Vec<u8>, ProtocolError> {
    validate(header)?;
    postcard::to_allocvec(header).map_err(|_| ProtocolError::Malformed)
}
pub fn decode_video_header(bytes: &[u8]) -> Result<VideoFrameHeader, ProtocolError> {
    ensure(bytes, 128)?;
    let header: VideoFrameHeader =
        postcard::from_bytes(bytes).map_err(|_| ProtocolError::Malformed)?;
    validate(&header)?;
    Ok(header)
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
fn validate(header: &VideoFrameHeader) -> Result<(), ProtocolError> {
    if header.protocol_version != PROTOCOL_VERSION
        || header.chunk_count == 0
        || header.chunk_index >= header.chunk_count
        || header.width > 3840
        || header.height > 2160
        || header.payload_length > MAX_DATAGRAM_PAYLOAD_BYTES
    {
        return Err(if header.protocol_version != PROTOCOL_VERSION {
            ProtocolError::UnsupportedVersion(header.protocol_version)
        } else {
            ProtocolError::InvalidFrame
        });
    }
    Ok(())
}
