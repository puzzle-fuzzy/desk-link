use desklink_protocol::{
    FrameFlags, MAX_VIDEO_PACKET_BYTES, MAX_VIDEO_PACKET_PAYLOAD_BYTES, PROTOCOL_VERSION,
    VideoFrameHeader, VideoPacket, encode_video_packet,
};
use desklink_video::{
    AssembleResult, DropReason, EncodedFrame, FrameAssembler, LatestFrameQueue, packetize_frame,
};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

fn instant(milliseconds: u64) -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now) + Duration::from_millis(milliseconds)
}

fn packet(frame_id: u64, chunk_index: u16, chunk_count: u16) -> VideoPacket {
    let payload = vec![frame_id as u8];
    VideoPacket::new(
        VideoFrameHeader {
            protocol_version: PROTOCOL_VERSION,
            stream_id: 1,
            config_version: 1,
            frame_id,
            capture_timestamp_us: frame_id,
            width: 640,
            height: 480,
            flags: FrameFlags(0),
            chunk_index,
            chunk_count,
            payload_length: payload.len() as u32,
        },
        payload,
    )
    .unwrap()
}

fn frame(frame_id: u64) -> EncodedFrame {
    EncodedFrame {
        stream_id: 1,
        frame_id,
        config_version: 1,
        capture_timestamp_us: frame_id,
        width: 640,
        height: 480,
        flags: FrameFlags(0),
        data: vec![frame_id as u8],
    }
}

#[test]
fn packetizer_keeps_serialized_video_packets_inside_quic_datagram_budget() {
    let mut encoded = frame(42);
    encoded.stream_id = u64::MAX;
    encoded.frame_id = u64::MAX;
    encoded.config_version = u32::MAX;
    encoded.capture_timestamp_us = u64::MAX;
    encoded.width = 1920;
    encoded.height = 1080;
    encoded.flags = FrameFlags::KEYFRAME;
    encoded.data = vec![0x65; MAX_VIDEO_PACKET_PAYLOAD_BYTES * 2 + 17];

    let packets = packetize_frame(&encoded).expect("packetize frame");
    assert_eq!(packets.len(), 3);
    for (index, packet) in packets.iter().enumerate() {
        assert_eq!(usize::from(packet.header.chunk_index), index);
        assert_eq!(usize::from(packet.header.chunk_count), packets.len());
        assert!(encode_video_packet(packet).unwrap().len() <= MAX_VIDEO_PACKET_BYTES);
    }
    assert_eq!(
        packets
            .into_iter()
            .flat_map(|packet| packet.payload)
            .collect::<Vec<_>>(),
        encoded.data
    );
}

#[test]
fn packetizer_rejects_empty_or_unbounded_access_units() {
    let mut encoded = frame(42);
    encoded.data.clear();
    assert!(packetize_frame(&encoded).is_err());

    encoded.data = vec![
        0;
        MAX_VIDEO_PACKET_PAYLOAD_BYTES
            * (usize::from(desklink_protocol::MAX_VIDEO_CHUNKS) + 1)
    ];
    assert!(packetize_frame(&encoded).is_err());
}

#[test]
fn queue_evicts_oldest_when_full() {
    let mut queue = LatestFrameQueue::new(2);
    queue.push_latest(1);
    queue.push_latest(2);
    assert_eq!(queue.push_latest(3), Some(1));
    assert_eq!(queue.drain_newest_first(), vec![3, 2]);
}

#[test]
fn queue_takes_newest_without_retaining_stale_frames() {
    let mut queue = LatestFrameQueue::new(3);
    queue.push_latest(10);
    queue.push_latest(11);
    queue.push_latest(12);

    assert_eq!(queue.take_newest(), Some(12));
    assert!(queue.is_empty());
    assert_eq!(queue.take_newest(), None);
}

#[test]
fn queue_clear_discards_frames_without_changing_capacity() {
    let mut queue = LatestFrameQueue::new(2);
    queue.push_latest(20);
    queue.push_latest(21);
    queue.clear();

    assert!(queue.is_empty());
    queue.push_latest(22);
    assert_eq!(queue.pop_newest(), Some(22));
}

#[test]
fn incomplete_frame_expires_without_blocking_new_frame() {
    let mut assembler = FrameAssembler::new(3, Duration::from_millis(120));
    assert_eq!(
        assembler.push(instant(0), packet(10, 0, 2)),
        AssembleResult::Pending
    );
    assert_eq!(
        assembler.push(instant(121), packet(11, 0, 1)),
        AssembleResult::Complete(frame(11))
    );
    assert_eq!(assembler.take_dropped_chunks(), 1);
    assert_eq!(assembler.expire(instant(121)), 0);
}

#[test]
fn older_frame_cannot_be_presented() {
    let mut assembler = FrameAssembler::new(3, Duration::from_millis(120));
    assert!(assembler.begin_stream(1));
    assert!(assembler.accept_for_present(frame(20)));
    assert!(!assembler.accept_for_present(frame(19)));
}

#[test]
fn duplicate_chunk_and_mismatched_metadata_are_dropped() {
    let mut assembler = FrameAssembler::new(3, Duration::from_millis(120));
    assert_eq!(
        assembler.push(instant(0), packet(30, 0, 2)),
        AssembleResult::Pending
    );
    assert_eq!(
        assembler.push(instant(1), packet(30, 0, 2)),
        AssembleResult::Dropped(DropReason::DuplicateChunk)
    );
    let mut mismatched = packet(30, 1, 2);
    mismatched.header.width = 800;
    assert_eq!(
        assembler.push(instant(2), mismatched),
        AssembleResult::Dropped(DropReason::MetadataMismatch)
    );
}

#[test]
fn assembler_evicts_oldest_incomplete_frame_at_capacity() {
    let mut assembler = FrameAssembler::new(2, Duration::from_millis(120));
    assert_eq!(
        assembler.push(instant(0), packet(1, 0, 2)),
        AssembleResult::Pending
    );
    assert_eq!(
        assembler.push(instant(1), packet(2, 0, 2)),
        AssembleResult::Pending
    );
    assert_eq!(
        assembler.push(instant(2), packet(3, 0, 1)),
        AssembleResult::Complete(frame(3))
    );
    assert_eq!(assembler.take_dropped_chunks(), 1);
    assert_eq!(
        assembler.push(instant(3), packet(1, 1, 2)),
        AssembleResult::Pending
    );
}

#[test]
fn presentation_order_includes_stream_id() {
    let mut assembler = FrameAssembler::new(1, Duration::from_millis(120));
    assert!(assembler.begin_stream(2));
    let mut next_stream = frame(1);
    next_stream.stream_id = 2;
    assert!(assembler.accept_for_present(next_stream));
    assert!(!assembler.accept_for_present(frame(99)));
}

#[test]
fn smaller_stream_rollover_clears_state_and_rejects_delayed_old_packets() {
    let mut assembler = FrameAssembler::new(2, Duration::from_millis(120));
    assert!(assembler.begin_stream(10));
    let mut old_packet = packet(1, 0, 2);
    old_packet.header.stream_id = 10;
    assert_eq!(
        assembler.push(instant(0), old_packet),
        AssembleResult::Pending
    );
    assert!(assembler.begin_stream(2));
    let mut new_packet = packet(7, 0, 1);
    new_packet.header.stream_id = 2;
    let mut expected = frame(7);
    expected.stream_id = 2;
    assert_eq!(
        assembler.push(instant(1), new_packet),
        AssembleResult::Complete(expected)
    );
    assert_eq!(
        assembler.push(instant(2), packet(1, 1, 2)),
        AssembleResult::Dropped(DropReason::Stale)
    );
}

#[test]
fn retired_stream_id_cannot_be_reactivated_after_rollover() {
    let mut assembler = FrameAssembler::new(2, Duration::from_millis(120));
    assert!(assembler.begin_stream(10));

    let mut old_packet = packet(1, 0, 2);
    old_packet.header.stream_id = 10;
    assert_eq!(
        assembler.push(instant(0), old_packet),
        AssembleResult::Pending
    );

    assert!(assembler.begin_stream(2));
    assert!(!assembler.begin_stream(10));

    let mut delayed_old_packet = packet(1, 1, 2);
    delayed_old_packet.header.stream_id = 10;
    assert_eq!(
        assembler.push(instant(1), delayed_old_packet),
        AssembleResult::Dropped(DropReason::Stale)
    );

    let mut current_packet = packet(7, 0, 1);
    current_packet.header.stream_id = 2;
    let mut expected = frame(7);
    expected.stream_id = 2;
    assert_eq!(
        assembler.push(instant(2), current_packet),
        AssembleResult::Complete(expected)
    );
}

#[test]
fn push_expires_overdue_partials_before_accepting_new_packet() {
    let mut assembler = FrameAssembler::new(2, Duration::from_millis(120));
    assert_eq!(
        assembler.push(instant(0), packet(10, 0, 2)),
        AssembleResult::Pending
    );
    assert_eq!(
        assembler.push(instant(121), packet(11, 0, 1)),
        AssembleResult::Complete(frame(11))
    );
    assert_eq!(
        assembler.push(instant(122), packet(10, 1, 2)),
        AssembleResult::Pending
    );
}
