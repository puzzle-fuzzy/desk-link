use desklink_protocol::{
    FrameFlags, MAX_DATAGRAM_PAYLOAD_BYTES, MAX_VIDEO_CHUNKS, MAX_VIDEO_PACKET_BYTES,
    MAX_VIDEO_RELIABLE_BATCH_BYTES, PROTOCOL_VERSION, ProtocolError, VideoFrameHeader, VideoPacket,
    VideoReliableBatchBuilder, decode_video_reliable_batch, encode_video_packet,
    encode_video_reliable_batch,
};

fn packet(index: u16, count: u16, payload_size: usize) -> Vec<u8> {
    encode_video_packet(
        &VideoPacket::new(
            VideoFrameHeader {
                protocol_version: PROTOCOL_VERSION,
                stream_id: 1,
                config_version: 1,
                frame_id: 7,
                capture_timestamp_us: 42,
                width: 1920,
                height: 1080,
                flags: FrameFlags::KEYFRAME,
                chunk_index: index,
                chunk_count: count,
                payload_length: payload_size as u32,
            },
            vec![index as u8; payload_size],
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn builder_wire_matches_existing_encoder_and_validates_round_trip() {
    let packets = (0..3)
        .map(|index| packet(index, 3, 1024))
        .collect::<Vec<_>>();
    let mut builder = VideoReliableBatchBuilder::new();
    assert!(builder.is_empty());
    for (index, packet) in packets.iter().enumerate() {
        assert!(builder.try_push(packet).unwrap());
        assert_eq!(builder.packet_count() as usize, index + 1);
    }
    assert_eq!(
        builder.bytes(),
        encode_video_reliable_batch(&packets).unwrap()
    );
    let decoded = decode_video_reliable_batch(builder.bytes()).unwrap();
    assert_eq!(decoded.len(), 3);
    assert_eq!(decoded[2].header.chunk_index, 2);
    assert_eq!(decoded[2].payload, vec![2; 1024]);
}

#[test]
fn exact_wire_budget_and_rejected_push_leave_existing_batch_unchanged() {
    let mut builder = VideoReliableBatchBuilder::new();
    let start = builder.bytes().as_ptr();
    // Raw inner bytes exercise framing boundaries independently of postcard's
    // variable length header. Decode/round-trip validation is covered above.
    let full = vec![0; MAX_VIDEO_PACKET_BYTES];
    while MAX_VIDEO_RELIABLE_BATCH_BYTES - builder.bytes().len() > full.len() + 4 {
        assert!(builder.try_push(&full).unwrap());
        assert_eq!(builder.bytes().as_ptr(), start);
    }
    let last = vec![0; MAX_VIDEO_RELIABLE_BATCH_BYTES - builder.bytes().len() - 2];
    assert!(!last.is_empty() && last.len() <= MAX_VIDEO_PACKET_BYTES);
    assert!(builder.try_push(&last).unwrap());
    assert_eq!(builder.bytes().len(), MAX_VIDEO_RELIABLE_BATCH_BYTES);
    let before = builder.bytes().to_vec();
    assert!(!builder.try_push(&[1]).unwrap());
    assert_eq!(builder.bytes(), before);
    assert_eq!(builder.bytes().as_ptr(), start);
    builder.clear();
    assert!(builder.is_empty());
    assert_eq!(builder.packet_count(), 0);
    assert_eq!(builder.bytes(), &[0xd7, 0, 0]);
    assert!(builder.try_push(&[1]).unwrap());
    assert_eq!(builder.bytes().as_ptr(), start);
}

#[test]
fn packet_count_limit_and_single_oversize_are_bounded_without_mutation() {
    let mut builder = VideoReliableBatchBuilder::new();
    let start = builder.bytes().as_ptr();
    for _ in 0..MAX_VIDEO_CHUNKS {
        assert!(builder.try_push(&[1]).unwrap());
    }
    assert_eq!(builder.packet_count(), MAX_VIDEO_CHUNKS);
    assert!(!builder.try_push(&[1]).unwrap());
    let before = builder.bytes().to_vec();
    assert_eq!(builder.try_push(&[]), Err(ProtocolError::InvalidFrame));
    assert!(matches!(
        builder.try_push(&vec![0; MAX_VIDEO_PACKET_BYTES + 1]),
        Err(ProtocolError::MessageTooLarge {
            maximum: MAX_VIDEO_PACKET_BYTES,
            ..
        })
    ));
    assert!(
        matches!(builder.try_push(&vec![0; usize::from(u16::MAX) + 1]), Err(ProtocolError::MessageTooLarge { maximum, .. }) if maximum == usize::from(u16::MAX))
    );
    assert_eq!(builder.bytes(), before);
    assert_eq!(builder.bytes().as_ptr(), start);
    builder.clear();
    assert!(builder.try_push(&vec![0; MAX_VIDEO_PACKET_BYTES]).unwrap());
}

fn group_with_builder(packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut builder = VideoReliableBatchBuilder::new();
    let allocation = builder.bytes().as_ptr();
    let mut batches = Vec::new();
    for packet in packets {
        if !builder.try_push(&packet).unwrap() {
            assert!(!builder.is_empty());
            batches.push(builder.bytes().to_vec());
            builder.clear();
            assert!(builder.try_push(&packet).unwrap());
        }
        assert_eq!(builder.bytes().as_ptr(), allocation);
    }
    if !builder.is_empty() {
        batches.push(builder.bytes().to_vec());
    }
    batches
}

#[test]
fn maximal_legal_keyframe_crosses_batches_with_no_lost_or_duplicated_packets() {
    let packets = (0..MAX_VIDEO_CHUNKS)
        .map(|index| packet(index, MAX_VIDEO_CHUNKS, MAX_DATAGRAM_PAYLOAD_BYTES as usize))
        .collect::<Vec<_>>();
    let batches = group_with_builder(packets);
    assert!(batches.len() > 1);
    let decoded = batches
        .iter()
        .flat_map(|batch| {
            assert!(batch.len() <= MAX_VIDEO_RELIABLE_BATCH_BYTES);
            decode_video_reliable_batch(batch).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(decoded.len(), MAX_VIDEO_CHUNKS as usize);
    for (index, packet) in decoded.iter().enumerate() {
        assert_eq!(packet.header.chunk_index as usize, index);
        assert_eq!(
            packet.payload,
            vec![index as u8; MAX_DATAGRAM_PAYLOAD_BYTES as usize]
        );
    }
}

fn group_with_legacy_candidate_encoding(packets: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    let mut current = Vec::new();
    let mut batches = Vec::new();
    for packet in packets {
        if current.is_empty() {
            current.push(packet);
            continue;
        }
        let mut candidate = current.clone();
        candidate.push(packet.clone());
        match encode_video_reliable_batch(&candidate) {
            Ok(_) => current.push(packet),
            Err(ProtocolError::MessageTooLarge { .. }) => {
                batches.push(encode_video_reliable_batch(&current).unwrap());
                current.clear();
                current.push(packet);
            }
            Err(error) => panic!("unexpected legacy error: {error}"),
        }
    }
    if !current.is_empty() {
        batches.push(encode_video_reliable_batch(&current).unwrap());
    }
    batches
}

#[test]
#[ignore = "offline CPU microbenchmark; run in release with --ignored --nocapture"]
fn benchmark_maximal_keyframe_batch_construction() {
    use std::{hint::black_box, time::Instant};
    let packets = (0..MAX_VIDEO_CHUNKS)
        .map(|index| packet(index, MAX_VIDEO_CHUNKS, MAX_DATAGRAM_PAYLOAD_BYTES as usize))
        .collect::<Vec<_>>();
    let legacy_input = packets.clone();
    let started = Instant::now();
    let legacy = black_box(group_with_legacy_candidate_encoding(legacy_input));
    let legacy_us = started.elapsed().as_micros();
    let started = Instant::now();
    let batched = black_box(group_with_builder(packets));
    let builder_us = started.elapsed().as_micros();
    assert_eq!(legacy, batched);
    println!(
        "{{\"scope\":\"offline_cpu_batch_construction\",\"packets\":{},\"batches\":{},\"wire_bytes\":{},\"legacy_us\":{},\"builder_us\":{}}}",
        MAX_VIDEO_CHUNKS,
        batched.len(),
        batched.iter().map(Vec::len).sum::<usize>(),
        legacy_us,
        builder_us
    );
}
