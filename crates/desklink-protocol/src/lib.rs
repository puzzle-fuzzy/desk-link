mod codec;

pub use codec::{
    ProtocolError, decode_audio_packet, decode_control, decode_cursor_update, decode_input,
    decode_noise_handshake, decode_session_input, decode_transfer, decode_video_config,
    decode_video_header, decode_video_packet, encode_audio_packet, encode_control,
    encode_cursor_update, encode_input, encode_noise_handshake, encode_transfer,
    encode_video_config, encode_video_header, encode_video_packet, encode_video_packet_parts,
};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, SocketAddr},
    ops::{BitOr, BitOrAssign},
};

/// Protocol 12 is the active development wire. The project has not shipped to
/// end users, so development binaries move together instead of carrying a
/// legacy protocol compatibility layer.
pub const PROTOCOL_VERSION: u16 = 12;
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 64 * 1024;
pub const MAX_NOISE_HANDSHAKE_BYTES: usize = 4 * 1024;
pub const MAX_VIDEO_CONFIG_BYTES: usize = 16 * 1024;
pub const MAX_CURSOR_MESSAGE_BYTES: usize = 256;
/// Maximum serialized audio packet before end-to-end packet protection.
pub const MAX_AUDIO_PACKET_BYTES: usize = 1_120;
pub const AUDIO_SAMPLE_RATE: u32 = 48_000;
pub const AUDIO_CHANNELS: u16 = 1;
pub const AUDIO_FRAME_SAMPLES: usize = 480;
/// Maximum 10 ms mono PCM frame size at 48 kHz.
pub const MAX_AUDIO_PAYLOAD_BYTES: usize = AUDIO_FRAME_SAMPLES * 2;
/// Conservative ceiling for one compressed 10 ms Opus frame. The production
/// encoder targets 64 kbit/s (about 80 bytes per frame), while this limit still
/// permits constrained-VBR bursts without approaching the datagram budget.
pub const MAX_OPUS_AUDIO_PAYLOAD_BYTES: usize = 512;
/// Maximum encoded message on the dedicated reliable clipboard/file lane.
pub const MAX_TRANSFER_MESSAGE_BYTES: usize = 64 * 1024;
/// Text clipboard limit. This leaves room for encryption and postcard framing.
pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 48 * 1024;
pub const MAX_TRANSFER_FILE_NAME_BYTES: usize = 255;
pub const MAX_TRANSFER_CHUNK_BYTES: usize = 48 * 1024;
pub const MAX_TRANSFER_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum serialized DeskLink video packet accepted by the QUIC datagram lane.
pub const MAX_VIDEO_PACKET_BYTES: usize = 1200;
/// Conservative H.264 chunk size that leaves room for the versioned packet header.
pub const MAX_VIDEO_PACKET_PAYLOAD_BYTES: usize = 1024;
pub const MAX_DATAGRAM_PAYLOAD_BYTES: u32 = MAX_VIDEO_PACKET_PAYLOAD_BYTES as u32;
pub const MAX_MVP_WIDTH: u16 = 2560;
pub const MAX_MVP_HEIGHT: u16 = 1440;
/// Upper bound used only by the offline 4K experiment budget calculator. The
/// live MVP validator intentionally remains at 2560×1440 until a reliable
/// direct/LAN transport and frame-recovery policy is available.
pub const MAX_EXPERIMENTAL_4K_WIDTH: u16 = 3840;
pub const MAX_EXPERIMENTAL_4K_HEIGHT: u16 = 2160;
/// Direct-LAN candidates are intentionally short-lived. This is a policy
/// bound for the future authenticated exchange, not permission to expose a
/// listening socket today.
pub const MAX_DIRECT_LAN_CANDIDATE_TTL_S: u16 = 10;
/// Maximum H.264 datagrams in one frame; bounds per-frame assembly memory while
/// allowing 4 MiB of encoded data at the 1024-byte MVP chunk size.
pub const MAX_VIDEO_CHUNKS: u16 = 4096;
pub const MAX_INPUT_AGE_US: u64 = 5_000_000;
pub const MAX_INPUT_FUTURE_SKEW_US: u64 = 1_000_000;
pub const MAX_POINTER_COORDINATE: i32 = 1_000_000;
pub const MAX_WHEEL_DELTA: i32 = 1_200;

/// Wire and capture cost estimate for a candidate desktop video mode. This is
/// deliberately a plain measurement type, not a negotiated protocol message.
/// It lets release checks compare 2560×1440 and 4K without enabling 4K in the
/// default relay path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VideoFrameBudget {
    pub width: u32,
    pub height: u32,
    pub bitrate_bps: u32,
    pub fps: u32,
    pub average_frame_bytes: u64,
    pub nv12_frame_bytes: u64,
    pub packet_count: u16,
    pub fits_current_datagram_budget: bool,
}

impl VideoFrameBudget {
    pub fn estimate(width: u32, height: u32, bitrate_bps: u32, fps: u32) -> Option<Self> {
        if width == 0 || height == 0 || bitrate_bps == 0 || fps == 0 {
            return None;
        }
        let average_frame_bytes = u64::from(bitrate_bps).div_ceil(8).div_ceil(u64::from(fps));
        let nv12_frame_bytes = u64::from(width)
            .checked_mul(u64::from(height))?
            .checked_mul(3)?
            .div_ceil(2);
        let packet_count = average_frame_bytes.div_ceil(MAX_VIDEO_PACKET_PAYLOAD_BYTES as u64);
        Some(Self {
            width,
            height,
            bitrate_bps,
            fps,
            average_frame_bytes,
            nv12_frame_bytes,
            packet_count: u16::try_from(packet_count).unwrap_or(u16::MAX),
            fits_current_datagram_budget: packet_count <= u64::from(MAX_VIDEO_CHUNKS),
        })
    }
}

/// An authenticated LAN endpoint for the video-only data plane.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DirectLanCandidate {
    candidate_id: u64,
    address: SocketAddr,
    expires_at_unix_s: u64,
    session_binding: [u8; 16],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DirectLanCandidateError {
    #[error("direct candidate ID must be nonzero")]
    InvalidId,
    #[error("direct candidate address must be a private or loopback address")]
    InvalidAddress,
    #[error("direct candidate port must be nonzero")]
    InvalidPort,
    #[error("direct candidate session binding must be nonzero")]
    InvalidSessionBinding,
    #[error("direct candidate expiry is outside the allowed lifetime")]
    InvalidExpiry,
}

impl DirectLanCandidate {
    pub fn new(
        candidate_id: u64,
        address: SocketAddr,
        expires_at_unix_s: u64,
        session_binding: [u8; 16],
        now_unix_s: u64,
    ) -> Result<Self, DirectLanCandidateError> {
        let candidate = Self {
            candidate_id,
            address,
            expires_at_unix_s,
            session_binding,
        };
        candidate.validate(now_unix_s, &session_binding)?;
        Ok(candidate)
    }

    pub fn validate(
        &self,
        now_unix_s: u64,
        expected_session_binding: &[u8; 16],
    ) -> Result<(), DirectLanCandidateError> {
        if self.candidate_id == 0 {
            return Err(DirectLanCandidateError::InvalidId);
        }
        if self.address.port() == 0 {
            return Err(DirectLanCandidateError::InvalidPort);
        }
        if !is_private_or_loopback_address(self.address.ip()) {
            return Err(DirectLanCandidateError::InvalidAddress);
        }
        if self.session_binding == [0; 16] || &self.session_binding != expected_session_binding {
            return Err(DirectLanCandidateError::InvalidSessionBinding);
        }
        let Some(lifetime) = self.expires_at_unix_s.checked_sub(now_unix_s) else {
            return Err(DirectLanCandidateError::InvalidExpiry);
        };
        if lifetime == 0 || lifetime > u64::from(MAX_DIRECT_LAN_CANDIDATE_TTL_S) {
            return Err(DirectLanCandidateError::InvalidExpiry);
        }
        Ok(())
    }

    pub const fn candidate_id(&self) -> u64 {
        self.candidate_id
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    pub const fn expires_at_unix_s(&self) -> u64 {
        self.expires_at_unix_s
    }

    pub const fn session_binding(&self) -> &[u8; 16] {
        &self.session_binding
    }
}

pub(crate) fn is_private_or_loopback_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback() || address.is_private() || address.is_link_local()
        }
        IpAddr::V6(address) => {
            let segments = address.segments();
            address.is_loopback()
                || (segments[0] & 0xfe00) == 0xfc00
                || (segments[0] & 0xffc0) == 0xfe80
        }
    }
}

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

/// H.264 profiles supported by the remote decoder/encoder pair.
///
/// Main remains the interoperability baseline. High is an opt-in quality
/// upgrade and must only be selected after both peers advertise it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum H264Profile {
    Main,
    High,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AudioCodec {
    PcmS16Le,
    Opus,
}

/// A self-describing 10 ms system-audio packet carried on an independent,
/// lossy encrypted datagram lane.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AudioPacket {
    pub protocol_version: u16,
    pub stream_id: u64,
    pub sequence: u64,
    pub capture_timestamp_us: u64,
    pub codec: AudioCodec,
    pub sample_rate: u32,
    pub channels: u16,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VideoQualityPreset {
    Smooth,
    Balanced,
    Sharp,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VideoQualityPreference {
    Automatic,
    Smooth,
    Balanced,
    Sharp,
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
    SetAudioEnabled {
        enabled: bool,
    },
    AudioState {
        available: bool,
        enabled: bool,
    },
    SetVideoQuality {
        preference: VideoQualityPreference,
    },
    SetVideoProfile {
        profile: H264Profile,
    },
    VideoQualityState {
        preference: VideoQualityPreference,
        preset: VideoQualityPreset,
    },
    VideoNetworkFeedback {
        received_packets: u32,
        dropped_packets: u32,
        decode_queue_peak: u16,
        freshness_recoveries: u16,
    },
    VideoPathCandidateOffer {
        candidate: DirectLanCandidate,
    },
    VideoPathCandidateAnswer {
        candidate_id: u64,
        accepted: bool,
        candidate: Option<DirectLanCandidate>,
    },
}

pub type TransferId = [u8; 16];

/// Identifies a partially received file that the controller can explicitly
/// request again after reconnecting. The peer must still ask its local user to
/// choose the file and only reuses the transfer id when the metadata matches.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileResumeHint {
    pub transfer_id: TransferId,
    pub name: String,
    pub size: u64,
}

/// Explicit clipboard and file operations carried on their own reliable lane.
/// File contents never share the latency-sensitive input or video streams.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TransferMessage {
    ClipboardSet {
        request_id: u64,
        text: String,
    },
    ClipboardRequest {
        request_id: u64,
    },
    ClipboardData {
        request_id: u64,
        text: String,
    },
    ClipboardResult {
        request_id: u64,
        result: TransferResult,
    },
    /// Ask the peer to let its local user choose one file to send back.
    FileSelectionRequest {
        request_id: u64,
        resume: Option<FileResumeHint>,
    },
    /// Cancel a pending peer-side file selection request.
    FileSelectionCancel {
        request_id: u64,
    },
    /// Close a peer-side file selection request without a file offer.
    FileSelectionResult {
        request_id: u64,
        result: TransferResult,
    },
    FileOffer {
        transfer_id: TransferId,
        /// Present only when this offer answers a `FileSelectionRequest`.
        /// Direct controller-to-host uploads use `None`.
        request_id: Option<u64>,
        name: String,
        size: u64,
    },
    FileDecision {
        transfer_id: TransferId,
        /// `Completed` means the receiver is ready for chunks. Other values
        /// explain why the offer could not be accepted.
        result: TransferResult,
        /// Number of verified sequential bytes already staged by the receiver.
        /// Senders hash the complete source but only transmit bytes at and
        /// after this offset.
        resume_offset: u64,
        /// BLAKE2s digest of the staged prefix. It is present exactly when
        /// `resume_offset` is non-zero and prevents resuming from a different
        /// same-sized source file.
        resume_prefix_hash: Option<[u8; 32]>,
    },
    FileChunk {
        transfer_id: TransferId,
        offset: u64,
        bytes: Vec<u8>,
    },
    FileComplete {
        transfer_id: TransferId,
        content_hash: [u8; 32],
    },
    FileResult {
        transfer_id: TransferId,
        result: TransferResult,
    },
    Cancel {
        transfer_id: TransferId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TransferResult {
    Completed,
    Rejected,
    Cancelled,
    TooLarge,
    InvalidData,
    PermissionDenied,
    IoFailed,
    InsufficientSpace,
    SourceChanged,
    Unsupported,
    Busy,
}

pub fn is_valid_transfer_file_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_TRANSFER_FILE_NAME_BYTES
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*'
                )
        })
    {
        return false;
    }

    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(['.', ' ']);
    !matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
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
    pub h264_profiles: Vec<H264Profile>,
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
            || self.h264_profiles.is_empty()
            || !self.h264_profiles.contains(&H264Profile::Main)
        {
            return Err(codec::ProtocolError::InvalidCapabilities);
        }
        Ok(())
    }

    pub fn supports_h264_profile(&self, profile: H264Profile) -> bool {
        self.codecs.contains(&Codec::H264) && self.h264_profiles.contains(&profile)
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
    Control,
    Alt,
    Shift,
    Meta,
}
impl KeyCode {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Character(character) => !character.is_control(),
            Self::Function(number) => (1..=12).contains(number),
            _ => true,
        }
    }

    pub const fn modifier_mask(self) -> Option<Modifiers> {
        match self {
            Self::Control => Some(Modifiers::CONTROL),
            Self::Alt => Some(Modifiers::ALT),
            Self::Shift => Some(Modifiers::SHIFT),
            Self::Meta => Some(Modifiers::META),
            _ => None,
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
