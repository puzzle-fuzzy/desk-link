use std::time::{Duration, Instant};

pub const KEYFRAME_RETRY_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VideoContinuityAction {
    Present,
    Drop,
    DropAndRequestKeyframe,
}

pub struct VideoContinuity {
    last_presented_frame_id: Option<u64>,
    awaiting_keyframe: bool,
    last_keyframe_request_at: Option<Instant>,
}

impl Default for VideoContinuity {
    fn default() -> Self {
        Self {
            last_presented_frame_id: None,
            awaiting_keyframe: true,
            last_keyframe_request_at: None,
        }
    }
}

impl VideoContinuity {
    pub fn reset_for_config(&mut self) {
        self.last_presented_frame_id = None;
        self.awaiting_keyframe = true;
        self.last_keyframe_request_at = None;
    }

    pub fn note_transport_loss(&mut self) {
        self.awaiting_keyframe = true;
    }

    pub fn note_keyframe_request(&mut self, now: Instant) {
        self.awaiting_keyframe = true;
        self.last_keyframe_request_at = Some(now);
    }

    pub fn observe_frame(
        &mut self,
        frame_id: u64,
        is_keyframe: bool,
        now: Instant,
    ) -> VideoContinuityAction {
        if is_keyframe {
            self.last_presented_frame_id = Some(frame_id);
            self.awaiting_keyframe = false;
            self.last_keyframe_request_at = None;
            return VideoContinuityAction::Present;
        }

        if self
            .last_presented_frame_id
            .is_some_and(|last| frame_id != last.wrapping_add(1))
        {
            self.awaiting_keyframe = true;
        }

        if self.awaiting_keyframe {
            let request_due = self.last_keyframe_request_at.is_none_or(|requested| {
                now.saturating_duration_since(requested) >= KEYFRAME_RETRY_INTERVAL
            });
            if request_due {
                self.last_keyframe_request_at = Some(now);
                VideoContinuityAction::DropAndRequestKeyframe
            } else {
                VideoContinuityAction::Drop
            }
        } else {
            self.last_presented_frame_id = Some(frame_id);
            VideoContinuityAction::Present
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{VideoContinuity, VideoContinuityAction};

    #[test]
    fn presents_a_keyframe_and_contiguous_delta_frames() {
        let now = Instant::now();
        let mut continuity = VideoContinuity::default();

        continuity.reset_for_config();
        assert_eq!(
            continuity.observe_frame(10, true, now),
            VideoContinuityAction::Present
        );
        assert_eq!(
            continuity.observe_frame(11, false, now),
            VideoContinuityAction::Present
        );
    }

    #[test]
    fn frame_gap_waits_for_a_keyframe_and_retries_once_per_second() {
        let now = Instant::now();
        let mut continuity = VideoContinuity::default();

        assert_eq!(
            continuity.observe_frame(10, true, now),
            VideoContinuityAction::Present
        );
        assert_eq!(
            continuity.observe_frame(12, false, now),
            VideoContinuityAction::DropAndRequestKeyframe
        );
        assert_eq!(
            continuity.observe_frame(13, false, now + Duration::from_millis(999)),
            VideoContinuityAction::Drop
        );
        assert_eq!(
            continuity.observe_frame(14, false, now + Duration::from_secs(1)),
            VideoContinuityAction::DropAndRequestKeyframe
        );
        assert_eq!(
            continuity.observe_frame(15, true, now + Duration::from_secs(1)),
            VideoContinuityAction::Present
        );
        assert_eq!(
            continuity.observe_frame(16, false, now + Duration::from_secs(1)),
            VideoContinuityAction::Present
        );
    }

    #[test]
    fn confirmed_transport_loss_breaks_an_otherwise_contiguous_delta_chain() {
        let now = Instant::now();
        let mut continuity = VideoContinuity::default();

        assert_eq!(
            continuity.observe_frame(20, true, now),
            VideoContinuityAction::Present
        );
        continuity.note_transport_loss();
        assert_eq!(
            continuity.observe_frame(21, false, now),
            VideoContinuityAction::DropAndRequestKeyframe
        );
    }

    #[test]
    fn configuration_reset_requires_a_new_keyframe() {
        let now = Instant::now();
        let mut continuity = VideoContinuity::default();

        assert_eq!(
            continuity.observe_frame(30, true, now),
            VideoContinuityAction::Present
        );
        continuity.reset_for_config();
        assert_eq!(
            continuity.observe_frame(31, false, now),
            VideoContinuityAction::DropAndRequestKeyframe
        );
        assert_eq!(
            continuity.observe_frame(32, true, now),
            VideoContinuityAction::Present
        );
    }

    #[test]
    fn contiguous_frame_ids_allow_u64_wraparound() {
        let now = Instant::now();
        let mut continuity = VideoContinuity::default();

        assert_eq!(
            continuity.observe_frame(u64::MAX, true, now),
            VideoContinuityAction::Present
        );
        assert_eq!(
            continuity.observe_frame(0, false, now),
            VideoContinuityAction::Present
        );
    }

    #[test]
    fn deferred_request_obeys_the_same_retry_cooldown() {
        let now = Instant::now();
        let mut continuity = VideoContinuity::default();

        continuity.reset_for_config();
        continuity.note_keyframe_request(now);
        assert_eq!(
            continuity.observe_frame(40, false, now + Duration::from_millis(999)),
            VideoContinuityAction::Drop
        );
        assert_eq!(
            continuity.observe_frame(41, false, now + Duration::from_secs(1)),
            VideoContinuityAction::DropAndRequestKeyframe
        );
    }
}
