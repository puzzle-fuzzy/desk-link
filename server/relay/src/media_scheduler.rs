//! Per-connection media budgets. Enqueueing never waits for a slow peer.
//!
//! The relay sees opaque ciphertext, so overflow drops the arriving datagram,
//! rather than pretending to identify video frames or H.264 dependencies. A
//! frame-aware policy needs authenticated routing metadata from the endpoints.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use desklink_transport::ChannelKind;
use tokio::{sync::mpsc, time::Instant};

#[derive(Clone, Copy)]
pub(super) struct MediaBudget {
    pub channel: ChannelKind,
    pub capacity: usize,
    pub maximum_age: Duration,
}

// Including the one packet in flight, these reserve at most 361,491 bytes of
// payload per connection (299 * (MAX_DATAGRAM_BYTES + the 9-byte route header)).
pub(super) const MEDIA_BUDGETS: [MediaBudget; 3] = [
    MediaBudget {
        channel: ChannelKind::AudioDatagram,
        capacity: 32,
        maximum_age: Duration::from_millis(60),
    },
    MediaBudget {
        channel: ChannelKind::CursorDatagram,
        capacity: 8,
        maximum_age: Duration::from_millis(40),
    },
    MediaBudget {
        channel: ChannelKind::VideoDatagram,
        capacity: 256,
        maximum_age: Duration::from_millis(250),
    },
];

pub(super) struct QueuedMedia<T> {
    pub value: T,
    pub deadline: Instant,
    pub enqueued_at: Instant,
}

/// Aggregate counters only; no session IDs, payloads or authentication material.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RelayMediaLaneSnapshot {
    pub enqueued: u64,
    pub queue_full: u64,
    pub expired: u64,
    pub send_timeouts: u64,
    pub forwarded: u64,
    pub max_queue_age_micros: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RelayMediaSnapshot {
    pub audio: RelayMediaLaneSnapshot,
    pub cursor: RelayMediaLaneSnapshot,
    pub video: RelayMediaLaneSnapshot,
    pub active_workers: usize,
}

#[derive(Default)]
struct LaneCounters {
    enqueued: AtomicU64,
    queue_full: AtomicU64,
    expired: AtomicU64,
    send_timeouts: AtomicU64,
    forwarded: AtomicU64,
    max_queue_age_micros: AtomicU64,
}

#[derive(Default)]
pub(super) struct MediaStatistics {
    lanes: [LaneCounters; 3],
    active_workers: AtomicUsize,
}

impl MediaStatistics {
    pub fn snapshot(&self) -> RelayMediaSnapshot {
        let [audio, cursor, video] = self.lanes.each_ref().map(|lane| RelayMediaLaneSnapshot {
            enqueued: lane.enqueued.load(Ordering::Relaxed),
            queue_full: lane.queue_full.load(Ordering::Relaxed),
            expired: lane.expired.load(Ordering::Relaxed),
            send_timeouts: lane.send_timeouts.load(Ordering::Relaxed),
            forwarded: lane.forwarded.load(Ordering::Relaxed),
            max_queue_age_micros: lane.max_queue_age_micros.load(Ordering::Relaxed),
        });
        RelayMediaSnapshot {
            audio,
            cursor,
            video,
            active_workers: self.active_workers.load(Ordering::Relaxed),
        }
    }

    pub fn record(&self, channel: ChannelKind, outcome: &str) {
        let lane = &self.lanes[lane_index(channel)];
        let counter = match outcome {
            "enqueued" => &lane.enqueued,
            "queue_full" => &lane.queue_full,
            "queue_expired" => &lane.expired,
            "peer_send_timeout" => &lane.send_timeouts,
            "forwarded" => &lane.forwarded,
            _ => return,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_queue_age(&self, channel: ChannelKind, age: Duration) {
        self.lanes[lane_index(channel)]
            .max_queue_age_micros
            .fetch_max(
                age.as_micros().min(u128::from(u64::MAX)) as u64,
                Ordering::Relaxed,
            );
    }

    pub fn worker(self: &Arc<Self>) -> MediaWorker {
        self.active_workers.fetch_add(1, Ordering::Relaxed);
        MediaWorker(self.clone())
    }
}

pub(super) struct MediaWorker(Arc<MediaStatistics>);

impl Drop for MediaWorker {
    fn drop(&mut self) {
        self.0.active_workers.fetch_sub(1, Ordering::Relaxed);
    }
}

fn lane_index(channel: ChannelKind) -> usize {
    MEDIA_BUDGETS
        .iter()
        .position(|b| b.channel == channel)
        .expect("media statistics only accept a validated media channel")
}

pub(super) struct MediaIngress<T> {
    senders: [mpsc::Sender<QueuedMedia<T>>; 3],
}

pub(super) fn media_queues<T>() -> (MediaIngress<T>, [mpsc::Receiver<QueuedMedia<T>>; 3]) {
    let pairs = MEDIA_BUDGETS.map(|budget| mpsc::channel(budget.capacity));
    let [(a_tx, a_rx), (c_tx, c_rx), (v_tx, v_rx)] = pairs;
    (
        MediaIngress {
            senders: [a_tx, c_tx, v_tx],
        },
        [a_rx, c_rx, v_rx],
    )
}

impl<T> MediaIngress<T> {
    pub fn enqueue(&self, channel: ChannelKind, value: T) -> Result<(), T> {
        let Some(index) = MEDIA_BUDGETS.iter().position(|b| b.channel == channel) else {
            return Err(value);
        };
        let enqueued_at = Instant::now();
        self.senders[index]
            .try_send(QueuedMedia {
                value,
                deadline: enqueued_at + MEDIA_BUDGETS[index].maximum_age,
                enqueued_at,
            })
            .map_err(|error| error.into_inner().value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saturated_video_does_not_borrow_audio_cursor_or_another_sessions_budget() {
        let (first, mut receivers) = media_queues();
        let (second, mut other_receivers) = media_queues();
        for packet in 0..256 {
            assert!(first.enqueue(ChannelKind::VideoDatagram, packet).is_ok());
        }
        assert_eq!(first.enqueue(ChannelKind::VideoDatagram, 256), Err(256));
        first.enqueue(ChannelKind::AudioDatagram, 17).unwrap();
        first.enqueue(ChannelKind::CursorDatagram, 18).unwrap();
        second.enqueue(ChannelKind::VideoDatagram, 19).unwrap();
        assert_eq!(receivers[0].recv().await.unwrap().value, 17);
        assert_eq!(receivers[1].recv().await.unwrap().value, 18);
        assert_eq!(other_receivers[2].recv().await.unwrap().value, 19);
        // Overflow preserves the already-queued burst, including its first packet.
        for packet in 0..256 {
            assert_eq!(receivers[2].recv().await.unwrap().value, packet);
        }
    }

    #[tokio::test]
    async fn queued_and_in_flight_media_share_one_absolute_age_budget() {
        let (ingress, mut receivers) = media_queues();
        let before = Instant::now();
        ingress.enqueue(ChannelKind::AudioDatagram, 1).unwrap();
        let packet = receivers[0].recv().await.unwrap();
        let after = Instant::now();
        assert!(packet.deadline >= before + Duration::from_millis(60));
        assert!(packet.deadline <= after + Duration::from_millis(60));
        assert!(
            tokio::time::timeout_at(packet.deadline, std::future::pending::<()>())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn closed_worker_rejects_without_waiting_or_allocating_another_task() {
        let (ingress, receivers) = media_queues();
        drop(receivers);
        assert_eq!(ingress.enqueue(ChannelKind::AudioDatagram, 1), Err(1));
        assert_eq!(ingress.enqueue(ChannelKind::Input, 2), Err(2));
    }
}
