mod codec;

pub use codec::{
    ProtocolError, decode_control, decode_input, decode_video_header, decode_video_packet,
    encode_control, encode_input, encode_video_header, encode_video_packet,
};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_DATAGRAM_PAYLOAD_BYTES: u32 = 1200;
pub const MAX_MVP_WIDTH: u16 = 1920;
pub const MAX_MVP_HEIGHT: u16 = 1080;
/// Maximum H.264 datagrams in one frame; bounds per-frame assembly memory while
/// allowing roughly 5 MiB of encoded data at the 1200-byte MVP payload size.
pub const MAX_VIDEO_CHUNKS: u16 = 4096;
pub const MAX_INPUT_AGE_US: u64 = 5_000_000;
pub const MAX_INPUT_FUTURE_SKEW_US: u64 = 1_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Platform {
    Windows,
    MacOS,
    IOS,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DeviceRole {
    Controller,
    Host,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Codec {
    H264,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameFlags(pub u16);
impl FrameFlags {
    pub const KEYFRAME: Self = Self(1 << 0);
    pub const CONFIG: Self = Self(1 << 1);
    pub const VIDEO_ALIVE: Self = Self(1 << 2);
    pub const KNOWN_BITS: u16 = Self::KEYFRAME.0 | Self::CONFIG.0 | Self::VIDEO_ALIVE.0;
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VideoFrameHeader {
    pub protocol_version: u16,
    pub stream_id: u64,
    pub config_version: u32,
    pub frame_id: u64,
    pub capture_timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    pub flags: FrameFlags,
    pub chunk_index: u16,
    pub chunk_count: u16,
    pub payload_length: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VideoPacket {
    pub header: VideoFrameHeader,
    pub payload: Vec<u8>,
}
impl VideoPacket {
    pub fn new(header: VideoFrameHeader, payload: Vec<u8>) -> Result<Self, codec::ProtocolError> {
        if payload.len() > MAX_DATAGRAM_PAYLOAD_BYTES as usize {
            return Err(codec::ProtocolError::MessageTooLarge {
                actual: payload.len(),
                maximum: MAX_DATAGRAM_PAYLOAD_BYTES as usize,
            });
        }
        codec::validate_video_header(&header)?;
        if header.payload_length as usize != payload.len() {
            return Err(codec::ProtocolError::PayloadLengthMismatch {
                declared: header.payload_length,
                actual: payload.len(),
            });
        }
        Ok(Self { header, payload })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ControlMessage {
    RequestKeyframe {
        stream_id: u64,
    },
    Hello {
        platform: Platform,
        role: DeviceRole,
    },
    Capabilities(DeviceCapabilities),
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    pub platform: Platform,
    pub role: DeviceRole,
    pub codecs: Vec<Codec>,
    pub width: u16,
    pub height: u16,
}
impl DeviceCapabilities {
    pub fn validate(&self) -> Result<(), codec::ProtocolError> {
        if self.width > MAX_MVP_WIDTH
            || self.height > MAX_MVP_HEIGHT
            || self.codecs.is_empty()
            || !self.codecs.iter().all(|codec| matches!(codec, Codec::H264))
        {
            return Err(codec::ProtocolError::InvalidCapabilities);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ErrorCode {
    InvalidMessage,
    UnsupportedVersion,
    Unauthorized,
    UnsupportedCodec,
    Internal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum KeyCode {
    Character(char),
    Enter,
    Escape,
    Backspace,
    Tab,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
pub struct Modifiers(pub u8);
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    MouseMove {
        x: i32,
        y: i32,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },
    Key {
        code: KeyCode,
        pressed: bool,
        modifiers: Modifiers,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputEnvelope {
    pub sequence: u64,
    pub timestamp_us: u64,
    pub event: InputEvent,
}
