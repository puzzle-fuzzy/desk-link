mod codec;

pub use codec::{
    ProtocolError, decode_control, decode_cursor_update, decode_input, decode_noise_handshake,
    decode_session_input, decode_video_config, decode_video_header, decode_video_packet,
    encode_control, encode_cursor_update, encode_input, encode_noise_handshake,
    encode_video_config, encode_video_header, encode_video_packet,
};
use serde::{Deserialize, Serialize};
use std::ops::{BitOr, BitOrAssign};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_NOISE_HANDSHAKE_BYTES: usize = 4 * 1024;
pub const MAX_VIDEO_CONFIG_BYTES: usize = 16 * 1024;
pub const MAX_CURSOR_MESSAGE_BYTES: usize = 256;
/// Maximum serialized DeskLink video packet accepted by the QUIC datagram lane.
pub const MAX_VIDEO_PACKET_BYTES: usize = 1200;
/// Conservative H.264 chunk size that leaves room for the versioned packet header.
pub const MAX_VIDEO_PACKET_PAYLOAD_BYTES: usize = 1024;
pub const MAX_DATAGRAM_PAYLOAD_BYTES: u32 = MAX_VIDEO_PACKET_PAYLOAD_BYTES as u32;
pub const MAX_MVP_WIDTH: u16 = 1920;
pub const MAX_MVP_HEIGHT: u16 = 1080;
/// Maximum H.264 datagrams in one frame; bounds per-frame assembly memory while
/// allowing 4 MiB of encoded data at the 1024-byte MVP chunk size.
pub const MAX_VIDEO_CHUNKS: u16 = 4096;
pub const MAX_INPUT_AGE_US: u64 = 5_000_000;
pub const MAX_INPUT_FUTURE_SKEW_US: u64 = 1_000_000;
pub const MAX_POINTER_COORDINATE: i32 = 1_000_000;
pub const MAX_WHEEL_DELTA: i32 = 1_200;

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
pub enum NoiseHandshakeStep {
    InitiatorHello,
    ResponderHello,
    InitiatorFinish,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NoiseHandshake {
    pub protocol_version: u16,
    pub step: NoiseHandshakeStep,
    pub payload: Vec<u8>,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VideoConfig {
    pub protocol_version: u16,
    pub stream_id: u64,
    pub config_version: u32,
    pub codec: Codec,
    pub width: u16,
    pub height: u16,
    /// Annex B SPS/PPS bytes used to initialize the remote H.264 decoder.
    pub sequence_header: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CursorUpdate {
    pub protocol_version: u16,
    pub stream_id: u64,
    pub sequence: u64,
    pub timestamp_us: u64,
    pub x_millionths: i32,
    pub y_millionths: i32,
    pub visible: bool,
    pub shape_id: u64,
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
    AccessDenied {
        reason: AccessDenialReason,
    },
    DisplayList {
        displays: Vec<RemoteDisplay>,
        active_display_id: u32,
    },
    SelectDisplay {
        display_id: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteDisplay {
    pub id: u32,
    pub width: u16,
    pub height: u16,
    pub primary: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AccessDenialReason {
    ApprovalRejected,
    ApprovalExpired,
    ControllerNotTrusted,
    ControllerIdentityChanged,
    HostUnavailable,
    HostCaptureFailed,
    HostEncoderFailed,
    HostInputFailed,
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
        if self.width == 0
            || self.height == 0
            || self.width > MAX_MVP_WIDTH
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
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Function(u8),
    CapsLock,
}
impl KeyCode {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Character(character) => !character.is_control(),
            Self::Function(number) => (1..=12).contains(number),
            _ => true,
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
pub struct Modifiers(pub u8);
impl Modifiers {
    pub const SHIFT: Self = Self(1 << 0);
    pub const CONTROL: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);
    pub const META: Self = Self(1 << 3);
    pub const ALL_BITS: u8 = Self::SHIFT.0 | Self::CONTROL.0 | Self::ALT.0 | Self::META.0;

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn is_valid(self) -> bool {
        self.0 & !Self::ALL_BITS == 0
    }
}
impl BitOr for Modifiers {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}
impl BitOrAssign for Modifiers {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}
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
    MouseWheel {
        delta_x: i32,
        delta_y: i32,
    },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputEnvelope {
    pub sequence: u64,
    pub timestamp_us: u64,
    pub event: InputEvent,
}
