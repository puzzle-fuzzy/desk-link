use std::{collections::VecDeque, time::Duration};

use apps_windows::{
    capture::{CaptureError, CapturedFrame, DesktopCapturer},
    encoder::EncodedFrame,
    runtime::{
        CaptureOutcome, HostInboundPolicy, HostRuntimeError, HostVideoPipeline, PrepareVideo,
        host_error_is_retryable, next_frame_with_recovery, normalize_cursor,
    },
};
use desklink_protocol::{
    ControlMessage, FrameFlags, InputEnvelope, InputEvent, KeyCode, Modifiers, decode_video_config,
    decode_video_packet, encode_control, encode_input,
};
use desklink_session::DesktopRect;
use desklink_transport::{JoinRejectCode, TransportError};

fn encoded(frame_id: u64, config_version: u32, keyframe: bool) -> EncodedFrame {
    EncodedFrame {
        frame_id,
        config_version,
        keyframe,
        timestamp_us: frame_id * 1_000,
        access_unit: vec![frame_id as u8; 2_500],
        sequence_header: Some(vec![0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2]),
    }
}

#[test]
fn video_pipeline_sends_config_before_first_idr_and_only_once_per_version() {
    let mut pipeline = HostVideoPipeline::new(9);

    assert_eq!(
        pipeline.prepare(encoded(1, 1, false), 1280, 720).unwrap(),
        PrepareVideo::NeedKeyframe
    );
    let PrepareVideo::Ready(first) = pipeline.prepare(encoded(2, 1, true), 1280, 720).unwrap()
    else {
        panic!("expected first IDR to be ready");
    };
    let config = decode_video_config(first.video_config.as_deref().unwrap()).unwrap();
    assert_eq!(config.stream_id, 9);
    assert_eq!(config.config_version, 1);
    assert_eq!((config.width, config.height), (1280, 720));
    assert!(!first.datagrams.is_empty());
    let packet = decode_video_packet(&first.datagrams[0]).unwrap();
    assert_eq!(packet.header.stream_id, 9);
    assert_eq!(packet.header.frame_id, 2);
    assert_eq!(
        packet.header.flags,
        FrameFlags(FrameFlags::KEYFRAME.0 | FrameFlags::CONFIG.0)
    );

    let PrepareVideo::Ready(next) = pipeline.prepare(encoded(3, 1, false), 1280, 720).unwrap()
    else {
        panic!("expected delta frame");
    };
    assert!(next.video_config.is_none());
}

#[test]
fn inbound_policy_filters_wrong_stream_requests_and_replayed_input() {
    let mut policy = HostInboundPolicy::new(9);
    let wrong = encode_control(&ControlMessage::RequestKeyframe { stream_id: 8 }).unwrap();
    let right = encode_control(&ControlMessage::RequestKeyframe { stream_id: 9 }).unwrap();
    assert!(!policy.handle_control(&wrong).unwrap());
    assert!(policy.handle_control(&right).unwrap());

    let envelope = InputEnvelope {
        sequence: 7,
        timestamp_us: 1_000,
        event: InputEvent::Key {
            code: KeyCode::Enter,
            pressed: true,
            modifiers: Modifiers::CONTROL | Modifiers::SHIFT,
        },
    };
    let bytes = encode_input(&envelope).unwrap();
    assert_eq!(
        policy.decode_input(&bytes, 1_000).unwrap(),
        Some(envelope.event)
    );
    assert_eq!(policy.decode_input(&bytes, 1_000).unwrap(), None);
}

#[test]
fn access_lost_recreates_capture_before_the_next_attempt() {
    let mut capture = RecoveringCapture {
        recoveries: 0,
        errors: VecDeque::from([CaptureError::AccessLost]),
        recover_error: None,
    };

    assert!(matches!(
        next_frame_with_recovery(&mut capture, Duration::from_millis(10)).unwrap(),
        CaptureOutcome::Recovered
    ));
    assert_eq!(capture.recoveries, 1);
}

#[test]
fn timeout_access_loss_and_recovery_sequences_remain_distinct() {
    let mut capture = RecoveringCapture {
        recoveries: 0,
        errors: VecDeque::from([
            CaptureError::Timeout,
            CaptureError::AccessLost,
            CaptureError::Timeout,
            CaptureError::AccessLost,
        ]),
        recover_error: None,
    };

    assert!(matches!(
        next_frame_with_recovery(&mut capture, Duration::from_millis(10)).unwrap(),
        CaptureOutcome::Idle
    ));
    assert_eq!(capture.recoveries, 0);
    assert!(matches!(
        next_frame_with_recovery(&mut capture, Duration::from_millis(10)).unwrap(),
        CaptureOutcome::Recovered
    ));
    assert_eq!(capture.recoveries, 1);
    assert!(matches!(
        next_frame_with_recovery(&mut capture, Duration::from_millis(10)).unwrap(),
        CaptureOutcome::Idle
    ));
    assert!(matches!(
        next_frame_with_recovery(&mut capture, Duration::from_millis(10)).unwrap(),
        CaptureOutcome::Recovered
    ));
    assert_eq!(capture.recoveries, 2);
}

#[test]
fn capture_recovery_failure_is_not_misreported_as_a_successful_rebuild() {
    let mut capture = RecoveringCapture {
        recoveries: 0,
        errors: VecDeque::from([CaptureError::AccessLost]),
        recover_error: Some(CaptureError::NoDisplay),
    };

    assert_eq!(
        next_frame_with_recovery(&mut capture, Duration::from_millis(10)).unwrap_err(),
        CaptureError::NoDisplay
    );
    assert_eq!(capture.recoveries, 1);
}

#[test]
fn cursor_coordinates_are_normalized_and_clamped() {
    let rect = DesktopRect::new(-1920, 0, 1920, 1080);
    assert_eq!(normalize_cursor(rect, -1920, 0), (0, 0));
    assert_eq!(normalize_cursor(rect, -960, 540), (500_000, 500_000));
    assert_eq!(normalize_cursor(rect, 100, 2000), (1_000_000, 1_000_000));
}

#[test]
fn host_rearms_after_network_or_peer_session_failures() {
    assert!(host_error_is_retryable(&HostRuntimeError::TransportClosed(
        "relay restarted".to_owned()
    )));
    assert!(host_error_is_retryable(&HostRuntimeError::HandshakeTimeout));
    assert!(host_error_is_retryable(
        &HostRuntimeError::UntrustedController
    ));
    assert!(host_error_is_retryable(
        &HostRuntimeError::ControllerKeyChanged
    ));
    assert!(host_error_is_retryable(&HostRuntimeError::Transport(
        TransportError::JoinRejected(JoinRejectCode::SessionOccupied)
    )));
    assert!(host_error_is_retryable(
        &HostRuntimeError::UnexpectedHandshakeStep
    ));
    assert!(host_error_is_retryable(
        &HostRuntimeError::InvalidControllerCapabilities
    ));
    assert!(host_error_is_retryable(&HostRuntimeError::Transport(
        TransportError::Malformed
    )));
    assert!(!host_error_is_retryable(&HostRuntimeError::Transport(
        TransportError::JoinRejected(JoinRejectCode::AuthenticationMismatch)
    )));
    assert!(!host_error_is_retryable(&HostRuntimeError::PairingRejected));
    assert!(!host_error_is_retryable(&HostRuntimeError::Capture(
        CaptureError::NoDisplay
    )));
}

struct RecoveringCapture {
    recoveries: usize,
    errors: VecDeque<CaptureError>,
    recover_error: Option<CaptureError>,
}

impl DesktopCapturer for RecoveringCapture {
    fn next_frame(&mut self, _timeout: Duration) -> Result<CapturedFrame, CaptureError> {
        Err(self.errors.pop_front().unwrap_or(CaptureError::Timeout))
    }

    fn dimensions(&self) -> (u32, u32) {
        (1920, 1080)
    }

    fn desktop_rect(&self) -> DesktopRect {
        DesktopRect::new(0, 0, 1920, 1080)
    }

    fn recover(&mut self) -> Result<(), CaptureError> {
        self.recoveries += 1;
        self.recover_error.take().map_or(Ok(()), Err)
    }
}
