//! Authenticated protocol-14 opt-in for one LAN/relay data envelope format.
//!
//! Capabilities are exchanged after Noise authentication and before production
//! business traffic. Both peers freeze the intersection for that session; a
//! switch changes the route only, never the envelope format. This module is a
//! source contract. An absent capability does not authorize LAN data traffic.

use serde::{Deserialize, Serialize};

pub const LAN_DATA_ENVELOPE_VERSION: u16 = 1;

/// Bit (ChannelKind numeric ID - 1), matching the existing eight business lanes.
/// These IDs do not change JOIN or the existing Noise SecureLane binding.
pub const LAN_DATA_CHANNEL_CONTROL: u16 = 1 << 0;
pub const LAN_DATA_CHANNEL_INPUT: u16 = 1 << 1;
pub const LAN_DATA_CHANNEL_VIDEO_CONFIG: u16 = 1 << 2;
pub const LAN_DATA_CHANNEL_VIDEO_DATAGRAM: u16 = 1 << 3;
pub const LAN_DATA_CHANNEL_CURSOR_DATAGRAM: u16 = 1 << 4;
pub const LAN_DATA_CHANNEL_TRANSFER: u16 = 1 << 5;
pub const LAN_DATA_CHANNEL_AUDIO_DATAGRAM: u16 = 1 << 6;
pub const LAN_DATA_CHANNEL_VIDEO_RELIABLE: u16 = 1 << 7;
pub const LAN_DATA_KNOWN_CHANNELS: u16 = (1 << 8) - 1;

pub const fn is_valid_lan_data_channels(channels: u16) -> bool {
    channels != 0 && channels & !LAN_DATA_KNOWN_CHANNELS == 0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LanDataCapabilities {
    pub envelope_version: u16,
    pub channels: u16,
    pub allow_relay_loss_continuation: bool,
}

impl LanDataCapabilities {
    pub const fn is_valid(self) -> bool {
        self.envelope_version == LAN_DATA_ENVELOPE_VERSION
            && is_valid_lan_data_channels(self.channels)
    }

    /// Call only with authenticated DeviceCapabilities. No overlap or an absent
    /// capability selects the existing mode; it cannot enable a subset later.
    pub fn intersect(self, other: Self) -> Option<Self> {
        if !self.is_valid() || !other.is_valid() {
            return None;
        }
        let channels = self.channels & other.channels;
        is_valid_lan_data_channels(channels).then_some(Self {
            envelope_version: LAN_DATA_ENVELOPE_VERSION,
            channels,
            allow_relay_loss_continuation: self.allow_relay_loss_continuation
                && other.allow_relay_loss_continuation,
        })
    }
}

/// Route coordination over the already authenticated control lane. Runtime
/// owners must enforce Offer -> Ready -> Commit for an exact candidate,
/// generation and channel mask, validate the candidate's Noise session binding,
/// reject stale/replayed generations and require a negotiated capability. Codec
/// validation deliberately cannot establish those stateful facts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum LanDataSwitchPhase {
    Offer,
    Ready,
    Commit,
}
