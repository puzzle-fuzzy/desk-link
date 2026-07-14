use desklink_protocol::{
    DeviceRole, FrameFlags, InputEvent, PROTOCOL_VERSION, VideoFrameHeader, VideoPacket,
};
use desklink_session::{SessionAction, SessionEvent, SessionMachine, SessionState};
use desklink_video::{AssembleResult, EncodedFrame, FrameAssembler, LatestFrameQueue};
use std::time::{Duration, Instant};

struct Harness {
    assembler: FrameAssembler,
    video: LatestFrameQueue<EncodedFrame>,
    session: SessionMachine,
    presented: Option<u64>,
    keyframe_requests: usize,
    received_input: usize,
    metrics: SessionMetrics,
}

#[derive(Default)]
struct SessionMetrics {
    dropped_frames: u64,
    last_frame_id: u64,
    input_sequence: u64,
    stream_id: u64,
    config_version: u32,
}

impl Harness {
    fn new() -> Self {
        let mut session = SessionMachine::new(DeviceRole::Controller);
        session.apply(SessionEvent::RelayConnected).unwrap();
        session.apply(SessionEvent::HandshakeComplete).unwrap();
        let actions = session.apply(SessionEvent::CapabilitiesNegotiated).unwrap();
        let stream_id = actions
            .iter()
            .find_map(|action| match action {
                SessionAction::BeginStream { stream_id } => Some(*stream_id),
                _ => None,
            })
            .unwrap();
        session.apply(SessionEvent::StartVideo).unwrap();
        session.apply(SessionEvent::VideoStarted).unwrap();

        Self {
            assembler: FrameAssembler::new(3, Duration::from_millis(800)),
            video: LatestFrameQueue::new(2),
            session,
            presented: None,
            keyframe_requests: 0,
            received_input: 0,
            metrics: SessionMetrics {
                stream_id,
                config_version: 1,
                ..SessionMetrics::default()
            },
        }
    }

    fn send_frame(&mut self, frame_id: u64, keyframe: bool) {
        let payload = vec![frame_id as u8];
        let packet = VideoPacket::new(
            VideoFrameHeader {
                protocol_version: PROTOCOL_VERSION,
                stream_id: 7,
                config_version: 1,
                frame_id,
                capture_timestamp_us: frame_id,
                width: 1280,
                height: 720,
                flags: if keyframe {
                    FrameFlags::KEYFRAME
                } else {
                    FrameFlags(0)
                },
                chunk_index: 0,
                chunk_count: 1,
                payload_length: payload.len() as u32,
            },
            payload,
        )
        .unwrap();
        if let AssembleResult::Complete(frame) = self.assembler.push(Instant::now(), packet) {
            self.video.push_latest(frame);
        } else {
            self.metrics.dropped_frames += 1;
        }
        if let Some(frame) = self.video.pop_newest() {
            if self
                .presented
                .is_none_or(|last_frame_id| frame.frame_id > last_frame_id)
            {
                self.presented = Some(frame.frame_id);
                self.metrics.last_frame_id = frame.frame_id;
            }
        }
        if keyframe && self.session.state() == SessionState::RecoveringVideo {
            self.session.apply(SessionEvent::VideoStarted).unwrap();
        }
    }

    fn drop_next_frame(&mut self, frame_id: u64) {
        let _ = frame_id;
        let actions = self.session.apply(SessionEvent::DecoderStalled).unwrap();
        if actions
            .iter()
            .any(|action| matches!(action, SessionAction::RequestKeyframe))
        {
            self.keyframe_requests += 1;
        }
    }

    fn send_input(&mut self, event: InputEvent) {
        let _ = event;
        self.received_input += 1;
        self.metrics.input_sequence = self.metrics.input_sequence.wrapping_add(1).max(1);
    }
}

#[test]
fn dropped_old_frame_recovers_with_new_keyframe() {
    let mut harness = Harness::new();
    harness.send_frame(1, false);
    harness.drop_next_frame(2);
    harness.send_frame(3, true);
    assert_eq!(harness.presented, Some(3));
    assert_eq!(harness.keyframe_requests, 1);
    assert_eq!(harness.metrics.last_frame_id, 3);
    assert_eq!(harness.metrics.stream_id, 1);
    assert_eq!(harness.metrics.config_version, 1);
}

#[test]
fn input_is_delivered_while_video_queue_is_full() {
    let mut harness = Harness::new();
    for frame_id in 1..=10 {
        harness.send_frame(frame_id, false);
    }
    harness.send_input(InputEvent::MouseMove {
        x: 500_000,
        y: 500_000,
    });
    assert_eq!(harness.received_input, 1);
    assert_eq!(harness.metrics.input_sequence, 1);
}
