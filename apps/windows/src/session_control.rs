use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use tokio::sync::{oneshot, watch};

/// A local Host-only command handle. Commands are bound to the active stream,
/// so a late click or queued command cannot terminate a replacement session.
#[derive(Clone)]
pub struct HostSessionControl {
    active_stream: Arc<AtomicU64>,
    requests: watch::Sender<Option<Arc<EndControlRequest>>>,
    request_guard: Arc<Mutex<()>>,
}

impl Default for HostSessionControl {
    fn default() -> Self {
        Self {
            active_stream: Arc::new(AtomicU64::new(0)),
            requests: watch::channel(None).0,
            request_guard: Arc::new(Mutex::new(())),
        }
    }
}

pub(crate) struct EndControlRequest {
    stream_id: u64,
    completed: Mutex<Option<oneshot::Sender<bool>>>,
}

impl EndControlRequest {
    pub(crate) fn complete(&self, notified: bool) {
        if let Some(sender) = self
            .completed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = sender.send(notified);
        }
    }
}

pub(crate) struct ActiveHostSession {
    control: HostSessionControl,
    stream_id: u64,
}

impl Drop for ActiveHostSession {
    fn drop(&mut self) {
        let _ = self.control.active_stream.compare_exchange(
            self.stream_id,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

impl HostSessionControl {
    pub fn request_end(&self) -> Result<oneshot::Receiver<bool>, &'static str> {
        let _guard = self.request_guard.lock().unwrap_or_else(|e| e.into_inner());
        let stream_id = self.active_stream.load(Ordering::Acquire);
        if stream_id == 0 {
            return Err("当前没有正在控制本机的连接。");
        }
        if let Some(request) = self.requests.borrow().as_ref()
            && request.stream_id == stream_id
            && request
                .completed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
        {
            return Err("正在结束当前控制，请稍候。");
        }
        let (sender, receiver) = oneshot::channel();
        self.requests.send_replace(Some(Arc::new(EndControlRequest {
            stream_id,
            completed: Mutex::new(Some(sender)),
        })));
        Ok(receiver)
    }

    pub(crate) fn activate(&self, stream_id: u64) -> ActiveHostSession {
        let _guard = self.request_guard.lock().unwrap_or_else(|e| e.into_inner());
        self.requests.send_replace(None);
        self.active_stream.store(stream_id, Ordering::Release);
        ActiveHostSession {
            control: self.clone(),
            stream_id,
        }
    }

    pub(crate) async fn next_end(&self, stream_id: u64) -> Arc<EndControlRequest> {
        let mut requests = self.requests.subscribe();
        loop {
            let request = requests.borrow_and_update().clone();
            if let Some(request) = request
                && request.stream_id == stream_id
            {
                return request;
            }
            // The sender is retained by self. This only waits for a local
            // command; dropping the runtime cancels the wait immediately.
            if requests.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn ending_is_stream_bound_single_flight_and_acknowledged() {
        let control = HostSessionControl::default();
        assert!(control.request_end().is_err());
        let active = control.activate(10);
        let completion = control.request_end().unwrap();
        assert!(control.request_end().is_err());
        let request = control.next_end(10).await;
        drop(active);
        assert!(control.request_end().is_err());
        request.complete(true);
        assert!(completion.await.unwrap());
    }

    #[tokio::test]
    async fn old_end_request_never_ends_the_replacement_session() {
        let control = HostSessionControl::default();
        let first = control.activate(20);
        let old_completion = control.request_end().unwrap();
        drop(first);
        let _replacement = control.activate(21);
        assert!(old_completion.await.is_err());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), control.next_end(21))
                .await
                .is_err()
        );
        let completion = control.request_end().unwrap();
        control.next_end(21).await.complete(true);
        assert!(completion.await.unwrap());
    }
}
