use desklink_protocol::{FrameFlags, VideoPacket};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedFrame {
    pub stream_id: u64,
    pub frame_id: u64,
    pub config_version: u32,
    pub capture_timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    pub flags: FrameFlags,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropReason { Malformed, DuplicateChunk, MetadataMismatch, Stale, Evicted, Expired }

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssembleResult { Pending, Complete(EncodedFrame), Dropped(DropReason) }

struct PartialFrame {
    created: Instant,
    packet: VideoPacket,
    chunks: BTreeMap<u16, Vec<u8>>,
}

pub struct FrameAssembler {
    max_frames: usize,
    max_age: Duration,
    frames: BTreeMap<(u64, u64), PartialFrame>,
    last_presented: Option<(u64, u64)>,
}

impl FrameAssembler {
    pub fn new(max_frames: usize, max_age: Duration) -> Self {
        assert!(max_frames > 0, "assembler capacity must be non-zero");
        Self { max_frames, max_age, frames: BTreeMap::new(), last_presented: None }
    }

    pub fn push(&mut self, now: Instant, packet: VideoPacket) -> AssembleResult {
        let packet = match VideoPacket::new(packet.header.clone(), packet.payload.clone()) {
            Ok(packet) => packet,
            Err(_) => return AssembleResult::Dropped(DropReason::Malformed),
        };
        let key = (packet.header.stream_id, packet.header.frame_id);
        if self.last_presented.is_some_and(|last| key <= last) { return AssembleResult::Dropped(DropReason::Stale); }
        if let Some(partial) = self.frames.get_mut(&key) {
            let h = &packet.header;
            let p = &partial.packet.header;
            if (h.config_version, h.capture_timestamp_us, h.width, h.height, h.flags, h.chunk_count)
                != (p.config_version, p.capture_timestamp_us, p.width, p.height, p.flags, p.chunk_count) {
                return AssembleResult::Dropped(DropReason::MetadataMismatch);
            }
            if !partial.chunks.insert(h.chunk_index, packet.payload).is_none() {
                return AssembleResult::Dropped(DropReason::DuplicateChunk);
            }
            return Self::finish_if_ready(&mut self.frames, key);
        }
        if self.frames.len() >= self.max_frames {
            let oldest = self.frames.iter().min_by_key(|(_, frame)| frame.created).map(|(key, _)| *key).unwrap();
            self.frames.remove(&oldest);
        }
        let index = packet.header.chunk_index;
        let mut chunks = BTreeMap::new();
        chunks.insert(index, packet.payload.clone());
        self.frames.insert(key, PartialFrame { created: now, packet, chunks });
        Self::finish_if_ready(&mut self.frames, key)
    }

    fn finish_if_ready(frames: &mut BTreeMap<(u64, u64), PartialFrame>, key: (u64, u64)) -> AssembleResult {
        let ready = frames.get(&key).is_some_and(|frame| frame.chunks.len() == frame.packet.header.chunk_count as usize);
        if !ready { return AssembleResult::Pending; }
        let frame = frames.remove(&key).unwrap();
        AssembleResult::Complete(EncodedFrame {
            stream_id: key.0, frame_id: key.1, config_version: frame.packet.header.config_version,
            capture_timestamp_us: frame.packet.header.capture_timestamp_us, width: frame.packet.header.width,
            height: frame.packet.header.height, flags: frame.packet.header.flags,
            data: frame.chunks.into_values().flatten().collect(),
        })
    }

    pub fn expire(&mut self, now: Instant) -> usize {
        let before = self.frames.len();
        self.frames.retain(|_, frame| now.duration_since(frame.created) < self.max_age);
        before - self.frames.len()
    }

    pub fn accept_for_present(&mut self, frame: EncodedFrame) -> bool {
        let key = (frame.stream_id, frame.frame_id);
        if self.last_presented.is_some_and(|last| key <= last) { false } else { self.last_presented = Some(key); true }
    }
}
