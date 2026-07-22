use desklink_protocol::{
    FrameFlags, MAX_VIDEO_CHUNKS, MAX_VIDEO_PACKET_PAYLOAD_BYTES, PROTOCOL_VERSION, ProtocolError,
    VideoFrameHeader, VideoPacket, encode_video_packet_parts,
};
use std::collections::{BTreeMap, BTreeSet};
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

pub fn packetize_frame(frame: &EncodedFrame) -> Result<Vec<VideoPacket>, ProtocolError> {
    let maximum = MAX_VIDEO_PACKET_PAYLOAD_BYTES * usize::from(MAX_VIDEO_CHUNKS);
    if frame.data.is_empty() {
        return Err(ProtocolError::InvalidFrame);
    }
    if frame.data.len() > maximum {
        return Err(ProtocolError::MessageTooLarge {
            actual: frame.data.len(),
            maximum,
        });
    }

    let chunk_count = frame.data.len().div_ceil(MAX_VIDEO_PACKET_PAYLOAD_BYTES) as u16;
    frame
        .data
        .chunks(MAX_VIDEO_PACKET_PAYLOAD_BYTES)
        .enumerate()
        .map(|(chunk_index, payload)| {
            VideoPacket::new(
                VideoFrameHeader {
                    protocol_version: PROTOCOL_VERSION,
                    stream_id: frame.stream_id,
                    config_version: frame.config_version,
                    frame_id: frame.frame_id,
                    capture_timestamp_us: frame.capture_timestamp_us,
                    width: frame.width,
                    height: frame.height,
                    flags: frame.flags,
                    chunk_index: chunk_index as u16,
                    chunk_count,
                    payload_length: payload.len() as u32,
                },
                payload.to_vec(),
            )
        })
        .collect()
}

/// Encodes an access unit directly into wire datagrams without materialising
/// owned `VideoPacket` payloads for every chunk.
pub fn encode_video_frame(frame: &EncodedFrame) -> Result<Vec<Vec<u8>>, ProtocolError> {
    let maximum = MAX_VIDEO_PACKET_PAYLOAD_BYTES * usize::from(MAX_VIDEO_CHUNKS);
    if frame.data.is_empty() {
        return Err(ProtocolError::InvalidFrame);
    }
    if frame.data.len() > maximum {
        return Err(ProtocolError::MessageTooLarge {
            actual: frame.data.len(),
            maximum,
        });
    }

    let chunk_count = frame.data.len().div_ceil(MAX_VIDEO_PACKET_PAYLOAD_BYTES) as u16;
    let mut datagrams = Vec::with_capacity(usize::from(chunk_count));
    for (chunk_index, payload) in frame
        .data
        .chunks(MAX_VIDEO_PACKET_PAYLOAD_BYTES)
        .enumerate()
    {
        let header = VideoFrameHeader {
            protocol_version: PROTOCOL_VERSION,
            stream_id: frame.stream_id,
            config_version: frame.config_version,
            frame_id: frame.frame_id,
            capture_timestamp_us: frame.capture_timestamp_us,
            width: frame.width,
            height: frame.height,
            flags: frame.flags,
            chunk_index: chunk_index as u16,
            chunk_count,
            payload_length: payload.len() as u32,
        };
        datagrams.push(encode_video_packet_parts(header, payload)?);
    }
    Ok(datagrams)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropReason {
    Malformed,
    DuplicateChunk,
    MetadataMismatch,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssembleResult {
    Pending,
    Complete(EncodedFrame),
    Dropped(DropReason),
}

struct PartialFrame {
    created: Instant,
    packet: VideoPacket,
    chunks: BTreeMap<u16, Vec<u8>>,
}

pub struct FrameAssembler {
    max_frames: usize,
    max_age: Duration,
    frames: BTreeMap<(u64, u64), PartialFrame>,
    dropped_chunks: u64,
    last_presented: Option<(u64, u64)>,
    active_stream: Option<u64>,
    retired_streams: BTreeSet<u64>,
}

impl FrameAssembler {
    pub fn new(max_frames: usize, max_age: Duration) -> Self {
        assert!(max_frames > 0, "assembler capacity must be non-zero");
        Self {
            max_frames,
            max_age,
            frames: BTreeMap::new(),
            dropped_chunks: 0,
            last_presented: None,
            active_stream: None,
            retired_streams: BTreeSet::new(),
        }
    }

    pub fn push(&mut self, now: Instant, packet: VideoPacket) -> AssembleResult {
        self.expire(now);
        let packet = match VideoPacket::new(packet.header.clone(), packet.payload.clone()) {
            Ok(packet) => packet,
            Err(_) => return AssembleResult::Dropped(DropReason::Malformed),
        };
        if let Some(active) = self.active_stream {
            if packet.header.stream_id != active {
                return AssembleResult::Dropped(DropReason::Stale);
            }
        } else {
            self.active_stream = Some(packet.header.stream_id);
        }
        let key = (packet.header.stream_id, packet.header.frame_id);
        if self
            .last_presented
            .is_some_and(|(_, last_frame)| packet.header.frame_id <= last_frame)
        {
            return AssembleResult::Dropped(DropReason::Stale);
        }
        if let Some(partial) = self.frames.get_mut(&key) {
            let h = &packet.header;
            let p = &partial.packet.header;
            if (
                h.config_version,
                h.capture_timestamp_us,
                h.width,
                h.height,
                h.flags,
                h.chunk_count,
            ) != (
                p.config_version,
                p.capture_timestamp_us,
                p.width,
                p.height,
                p.flags,
                p.chunk_count,
            ) {
                return AssembleResult::Dropped(DropReason::MetadataMismatch);
            }
            if !partial
                .chunks
                .insert(h.chunk_index, packet.payload)
                .is_none()
            {
                return AssembleResult::Dropped(DropReason::DuplicateChunk);
            }
            return Self::finish_if_ready(&mut self.frames, key);
        }
        if self.frames.len() >= self.max_frames {
            let oldest = self
                .frames
                .iter()
                .min_by_key(|(_, frame)| frame.created)
                .map(|(key, _)| *key)
                .unwrap();
            if let Some(frame) = self.frames.remove(&oldest) {
                self.dropped_chunks = self
                    .dropped_chunks
                    .saturating_add(frame.missing_chunk_count());
            }
        }
        let index = packet.header.chunk_index;
        let mut chunks = BTreeMap::new();
        chunks.insert(index, packet.payload.clone());
        self.frames.insert(
            key,
            PartialFrame {
                created: now,
                packet,
                chunks,
            },
        );
        Self::finish_if_ready(&mut self.frames, key)
    }

    fn finish_if_ready(
        frames: &mut BTreeMap<(u64, u64), PartialFrame>,
        key: (u64, u64),
    ) -> AssembleResult {
        let ready = frames.get(&key).is_some_and(|frame| {
            debug_assert!(frame.packet.header.chunk_count <= MAX_VIDEO_CHUNKS);
            frame.chunks.len() == usize::from(frame.packet.header.chunk_count)
        });
        if !ready {
            return AssembleResult::Pending;
        }
        let frame = frames.remove(&key).unwrap();
        AssembleResult::Complete(EncodedFrame {
            stream_id: key.0,
            frame_id: key.1,
            config_version: frame.packet.header.config_version,
            capture_timestamp_us: frame.packet.header.capture_timestamp_us,
            width: frame.packet.header.width,
            height: frame.packet.header.height,
            flags: frame.packet.header.flags,
            data: frame.chunks.into_values().flatten().collect(),
        })
    }

    pub fn expire(&mut self, now: Instant) -> usize {
        let before = self.frames.len();
        let mut dropped_chunks = 0_u64;
        self.frames.retain(|_, frame| {
            let keep = now.duration_since(frame.created) < self.max_age;
            if !keep {
                dropped_chunks = dropped_chunks.saturating_add(frame.missing_chunk_count());
            }
            keep
        });
        self.dropped_chunks = self.dropped_chunks.saturating_add(dropped_chunks);
        before - self.frames.len()
    }

    pub fn take_dropped_chunks(&mut self) -> u64 {
        std::mem::take(&mut self.dropped_chunks)
    }

    pub fn accept_for_present(&mut self, frame: EncodedFrame) -> bool {
        if self.active_stream != Some(frame.stream_id) {
            return false;
        }
        if self
            .last_presented
            .is_some_and(|(_, last_frame)| frame.frame_id <= last_frame)
        {
            false
        } else {
            self.last_presented = Some((frame.stream_id, frame.frame_id));
            true
        }
    }

    pub fn begin_stream(&mut self, stream_id: u64) -> bool {
        if self.active_stream == Some(stream_id) || self.retired_streams.contains(&stream_id) {
            return false;
        }
        if let Some(active_stream) = self.active_stream {
            self.retired_streams.insert(active_stream);
        }
        self.active_stream = Some(stream_id);
        self.frames.clear();
        self.dropped_chunks = 0;
        self.last_presented = None;
        true
    }
}

impl PartialFrame {
    fn missing_chunk_count(&self) -> u64 {
        u64::from(self.packet.header.chunk_count)
            .saturating_sub(u64::try_from(self.chunks.len()).unwrap_or(u64::MAX))
    }
}
