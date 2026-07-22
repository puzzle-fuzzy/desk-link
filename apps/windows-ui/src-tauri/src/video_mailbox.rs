use std::{collections::VecDeque, sync::Mutex, time::Instant};

use desklink_video::{VideoContinuity, VideoContinuityAction};
use tokio::sync::Notify;

const VIDEO_MAILBOX_CAPACITY: usize = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VideoMailboxKey {
    pub(crate) stream_id: u64,
    pub(crate) config_version: u32,
}

impl VideoMailboxKey {
    pub(crate) const fn new(stream_id: u64, config_version: u32) -> Self {
        Self {
            stream_id,
            config_version,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VideoDeliveryFrame {
    pub(crate) key: VideoMailboxKey,
    pub(crate) frame_id: u64,
    pub(crate) keyframe: bool,
    pub(crate) payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VideoMailboxOffer {
    Queued,
    Dropped,
    RequestKeyframe,
    Ignored,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct VideoMailboxMetrics {
    pub(crate) delivered_frames: u64,
    pub(crate) overflow_drops: u64,
    pub(crate) keyframe_replacements: u64,
}

struct VideoMailboxState {
    key: Option<VideoMailboxKey>,
    frames: VecDeque<VideoDeliveryFrame>,
    closed: bool,
    continuity: VideoContinuity,
    metrics: VideoMailboxMetrics,
}

impl Default for VideoMailboxState {
    fn default() -> Self {
        Self {
            key: None,
            frames: VecDeque::with_capacity(VIDEO_MAILBOX_CAPACITY),
            closed: true,
            continuity: VideoContinuity::default(),
            metrics: VideoMailboxMetrics::default(),
        }
    }
}

#[derive(Default)]
pub(crate) struct ControllerVideoMailbox {
    state: Mutex<VideoMailboxState>,
    notify: Notify,
}

impl ControllerVideoMailbox {
    pub(crate) fn begin_config(&self, key: VideoMailboxKey) {
        let mut state = lock_unpoisoned(&self.state);
        if !state.closed && state.key == Some(key) {
            return;
        }
        state.key = Some(key);
        state.frames.clear();
        state.closed = false;
        state.continuity.reset_for_config();
        drop(state);
        self.notify.notify_waiters();
    }

    pub(crate) fn offer(&self, frame: VideoDeliveryFrame, now: Instant) -> VideoMailboxOffer {
        let mut state = lock_unpoisoned(&self.state);
        if state.closed || state.key != Some(frame.key) {
            return VideoMailboxOffer::Ignored;
        }

        match state
            .continuity
            .observe_frame(frame.frame_id, frame.keyframe, now)
        {
            VideoContinuityAction::Drop => VideoMailboxOffer::Dropped,
            VideoContinuityAction::DropAndRequestKeyframe => VideoMailboxOffer::RequestKeyframe,
            VideoContinuityAction::Present if state.frames.len() < VIDEO_MAILBOX_CAPACITY => {
                state.frames.push_back(frame);
                drop(state);
                self.notify.notify_one();
                VideoMailboxOffer::Queued
            }
            VideoContinuityAction::Present if frame.keyframe => {
                state.metrics.keyframe_replacements =
                    state.metrics.keyframe_replacements.saturating_add(1);
                state.frames.clear();
                state.frames.push_back(frame);
                drop(state);
                self.notify.notify_one();
                VideoMailboxOffer::Queued
            }
            VideoContinuityAction::Present => {
                state.metrics.overflow_drops = state.metrics.overflow_drops.saturating_add(1);
                state.continuity.note_transport_loss();
                state.continuity.note_keyframe_request(now);
                VideoMailboxOffer::RequestKeyframe
            }
        }
    }

    pub(crate) async fn next(&self, key: VideoMailboxKey) -> Result<VideoDeliveryFrame, String> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = lock_unpoisoned(&self.state);
                if state.closed {
                    return Err("远程视频流已结束。".to_owned());
                }
                if state.key != Some(key) {
                    return Err("远程视频流或画面配置已经切换。".to_owned());
                }
                if let Some(frame) = state.frames.pop_front() {
                    state.metrics.delivered_frames =
                        state.metrics.delivered_frames.saturating_add(1);
                    return Ok(frame);
                }
            }
            notified.await;
        }
    }

    pub(crate) fn close(&self) {
        let mut state = lock_unpoisoned(&self.state);
        state.key = None;
        state.frames.clear();
        state.closed = true;
        state.continuity.reset_for_config();
        drop(state);
        self.notify.notify_waiters();
    }

    pub(crate) fn metrics(&self) -> VideoMailboxMetrics {
        lock_unpoisoned(&self.state).metrics
    }

    pub(crate) fn reset_metrics(&self) {
        lock_unpoisoned(&self.state).metrics = VideoMailboxMetrics::default();
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use super::{ControllerVideoMailbox, VideoDeliveryFrame, VideoMailboxKey, VideoMailboxOffer};

    fn frame(key: VideoMailboxKey, frame_id: u64, keyframe: bool) -> VideoDeliveryFrame {
        VideoDeliveryFrame {
            key,
            frame_id,
            keyframe,
            payload: vec![frame_id as u8; 32],
        }
    }

    #[tokio::test]
    async fn delta_overflow_preserves_the_safe_slot_and_requests_a_keyframe() {
        let mailbox = ControllerVideoMailbox::default();
        let key = VideoMailboxKey::new(9, 3);
        let now = Instant::now();
        mailbox.begin_config(key);

        assert_eq!(
            mailbox.offer(frame(key, 10, true), now),
            VideoMailboxOffer::Queued
        );
        assert_eq!(
            mailbox.offer(frame(key, 11, false), now),
            VideoMailboxOffer::RequestKeyframe
        );
        assert_eq!(mailbox.next(key).await.unwrap().frame_id, 10);
        assert_eq!(
            mailbox.offer(frame(key, 12, false), now + Duration::from_millis(999)),
            VideoMailboxOffer::Dropped
        );
        assert_eq!(
            mailbox.offer(frame(key, 13, false), now + Duration::from_secs(1)),
            VideoMailboxOffer::RequestKeyframe
        );
        assert_eq!(
            mailbox.offer(frame(key, 14, true), now + Duration::from_secs(1)),
            VideoMailboxOffer::Queued
        );
        assert_eq!(mailbox.next(key).await.unwrap().frame_id, 14);
    }

    #[tokio::test]
    async fn a_new_keyframe_replaces_an_older_full_slot() {
        let mailbox = ControllerVideoMailbox::default();
        let key = VideoMailboxKey::new(9, 3);
        let now = Instant::now();
        mailbox.begin_config(key);

        assert_eq!(
            mailbox.offer(frame(key, 20, true), now),
            VideoMailboxOffer::Queued
        );
        assert_eq!(
            mailbox.offer(frame(key, 21, true), now),
            VideoMailboxOffer::Queued
        );
        assert_eq!(mailbox.next(key).await.unwrap().frame_id, 21);
    }

    #[tokio::test]
    async fn duplicate_configuration_does_not_discard_a_pending_frame() {
        let mailbox = ControllerVideoMailbox::default();
        let key = VideoMailboxKey::new(9, 3);
        mailbox.begin_config(key);
        assert_eq!(
            mailbox.offer(frame(key, 30, true), Instant::now()),
            VideoMailboxOffer::Queued
        );

        mailbox.begin_config(key);

        assert_eq!(mailbox.next(key).await.unwrap().frame_id, 30);
    }

    #[tokio::test]
    async fn configuration_switch_wakes_and_rejects_an_old_waiter() {
        let mailbox = Arc::new(ControllerVideoMailbox::default());
        let old_key = VideoMailboxKey::new(9, 3);
        mailbox.begin_config(old_key);
        let waiting_mailbox = mailbox.clone();
        let waiter = tokio::spawn(async move { waiting_mailbox.next(old_key).await });
        tokio::task::yield_now().await;

        mailbox.begin_config(VideoMailboxKey::new(9, 4));

        assert!(waiter.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn configuration_switch_wakes_every_stale_waiter() {
        let mailbox = Arc::new(ControllerVideoMailbox::default());
        let old_key = VideoMailboxKey::new(9, 3);
        mailbox.begin_config(old_key);
        let first_mailbox = mailbox.clone();
        let first = tokio::spawn(async move { first_mailbox.next(old_key).await });
        let second_mailbox = mailbox.clone();
        let second = tokio::spawn(async move { second_mailbox.next(old_key).await });
        tokio::task::yield_now().await;

        mailbox.begin_config(VideoMailboxKey::new(9, 4));

        let results = tokio::time::timeout(Duration::from_secs(1), async {
            (first.await.unwrap(), second.await.unwrap())
        })
        .await
        .expect("every stale waiter must wake after a configuration switch");
        assert!(results.0.is_err());
        assert!(results.1.is_err());
    }

    #[tokio::test]
    async fn close_wakes_a_waiter_without_leaking_a_frame() {
        let mailbox = Arc::new(ControllerVideoMailbox::default());
        let key = VideoMailboxKey::new(9, 3);
        mailbox.begin_config(key);
        let waiting_mailbox = mailbox.clone();
        let waiter = tokio::spawn(async move { waiting_mailbox.next(key).await });
        tokio::task::yield_now().await;

        mailbox.close();

        assert!(waiter.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn wrong_stream_or_configuration_cannot_offer_or_consume_frames() {
        let mailbox = ControllerVideoMailbox::default();
        let key = VideoMailboxKey::new(9, 3);
        let wrong = VideoMailboxKey::new(8, 3);
        mailbox.begin_config(key);

        assert_eq!(
            mailbox.offer(frame(wrong, 40, true), Instant::now()),
            VideoMailboxOffer::Ignored
        );
        assert!(mailbox.next(wrong).await.is_err());
    }

    #[tokio::test]
    async fn metrics_count_delivery_and_real_mailbox_pressure_only() {
        let mailbox = ControllerVideoMailbox::default();
        let key = VideoMailboxKey::new(9, 3);
        let wrong = VideoMailboxKey::new(8, 3);
        let now = Instant::now();
        mailbox.begin_config(key);

        assert_eq!(
            mailbox.offer(frame(key, 10, true), now),
            VideoMailboxOffer::Queued
        );
        assert_eq!(mailbox.next(key).await.unwrap().frame_id, 10);
        assert_eq!(
            mailbox.offer(frame(key, 11, false), now),
            VideoMailboxOffer::Queued
        );
        assert_eq!(
            mailbox.offer(frame(key, 12, false), now),
            VideoMailboxOffer::RequestKeyframe
        );
        assert_eq!(
            mailbox.offer(frame(key, 13, true), now),
            VideoMailboxOffer::Queued
        );
        assert_eq!(mailbox.next(key).await.unwrap().frame_id, 13);
        assert_eq!(
            mailbox.offer(frame(wrong, 14, true), now),
            VideoMailboxOffer::Ignored
        );

        let metrics = mailbox.metrics();
        assert_eq!(metrics.delivered_frames, 2);
        assert_eq!(metrics.overflow_drops, 1);
        assert_eq!(metrics.keyframe_replacements, 1);

        mailbox.close();
        assert_eq!(mailbox.metrics(), metrics);
        mailbox.reset_metrics();
        assert_eq!(mailbox.metrics(), Default::default());
    }
}
