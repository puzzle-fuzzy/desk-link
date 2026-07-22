use std::collections::VecDeque;

pub struct LatestFrameQueue<T> {
    capacity: usize,
    items: VecDeque<T>,
}

impl<T> LatestFrameQueue<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "queue capacity must be non-zero");
        Self {
            capacity,
            items: VecDeque::with_capacity(capacity),
        }
    }

    pub fn push_latest(&mut self, item: T) -> Option<T> {
        self.items.push_back(item);
        if self.items.len() > self.capacity {
            self.items.pop_front()
        } else {
            None
        }
    }

    pub fn pop_newest(&mut self) -> Option<T> {
        self.items.pop_back()
    }

    /// Removes all queued frames and returns only the newest one.
    ///
    /// The capture pipeline deliberately drops stale frames to keep latency
    /// bounded.  Keeping that operation in-place avoids materialising a
    /// temporary `Vec` on every send-loop wakeup.
    pub fn take_newest(&mut self) -> Option<T> {
        let newest = self.items.pop_back();
        self.items.clear();
        newest
    }

    /// Drops every queued frame without allocating.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    pub fn drain_newest_first(&mut self) -> Vec<T> {
        self.items.drain(..).rev().collect()
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
