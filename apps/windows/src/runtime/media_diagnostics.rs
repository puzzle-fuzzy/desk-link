use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::JoinHandle,
};

use super::cursor_send::CursorDisableReason;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectListenerStopReason {
    ListenerClosed,
}

impl DirectListenerStopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ListenerClosed => "listener_closed",
        }
    }
}

/// Fixed fields only: no endpoint addresses, raw driver errors or input data.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostMediaDiagnostic {
    CursorSend {
        stream_id: u64,
        send_attempts: u64,
        send_errors: u64,
        disabled: bool,
        reason: CursorDisableReason,
    },
    DirectListenerStopped {
        stream_id: u64,
        retired_active: bool,
        reason: DirectListenerStopReason,
    },
}

pub type HostMediaDiagnosticsObserver = Arc<dyn Fn(HostMediaDiagnostic) + Send + Sync>;

struct Job {
    observer: Option<HostMediaDiagnosticsObserver>,
    event: HostMediaDiagnostic,
}
struct Dispatcher {
    sender: SyncSender<Job>,
    rejected: Arc<AtomicU64>,
}

impl Dispatcher {
    fn spawn(capacity: usize) -> std::io::Result<(Self, JoinHandle<()>)> {
        let (sender, received) = mpsc::sync_channel::<Job>(capacity);
        let rejected = Arc::new(AtomicU64::new(0));
        let worker = std::thread::Builder::new()
            .name("desklink-media-diagnostics".into())
            .spawn(move || {
                while let Ok(job) = received.recv() {
                    // The one process-wide worker isolates callback disk/HTTP work
                    // from media and input. A plugin panic cannot kill its receiver.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if let Some(observer) = job.observer {
                            observer(job.event);
                        } else if let Ok(event) = serde_json::to_string(&job.event) {
                            eprintln!("DeskLink media diagnostic {event}");
                        }
                    }));
                }
            })?;
        Ok((Self { sender, rejected }, worker))
    }

    fn emit(&self, observer: Option<HostMediaDiagnosticsObserver>, event: HostMediaDiagnostic) {
        if self.sender.try_send(Job { observer, event }).is_err() {
            let _ = self
                .rejected
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                    Some(value.saturating_add(1))
                });
        }
    }
}

static DISPATCHER: OnceLock<Option<Dispatcher>> = OnceLock::new();
fn dispatcher() -> Option<&'static Dispatcher> {
    // One bounded queue and one worker for the entire process, across peers.
    DISPATCHER
        .get_or_init(|| {
            Dispatcher::spawn(32)
                .ok()
                .map(|(dispatcher, _worker)| dispatcher)
        })
        .as_ref()
}

pub(super) fn initialize() {
    let _ = dispatcher();
}

pub(super) fn emit(observer: &Option<HostMediaDiagnosticsObserver>, event: HostMediaDiagnostic) {
    if let Some(Some(dispatcher)) = DISPATCHER.get() {
        dispatcher.emit(observer.clone(), event);
    }
}

/// Diagnostic backpressure is independent from datagram delivery/loss.
pub fn rejected_host_media_diagnostics() -> Option<u64> {
    DISPATCHER
        .get()
        .and_then(Option::as_ref)
        .map(|dispatcher| dispatcher.rejected.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::Mutex,
        time::{Duration, Instant},
    };
    fn event() -> HostMediaDiagnostic {
        HostMediaDiagnostic::CursorSend {
            stream_id: 7,
            send_attempts: 2,
            send_errors: 1,
            disabled: true,
            reason: CursorDisableReason::CapabilityOrSize,
        }
    }

    #[test]
    fn callback_delivery_serializes_only_fixed_send_operation_fields() {
        let (dispatcher, worker) = Dispatcher::spawn(2).unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let copy = received.clone();
        let observer: HostMediaDiagnosticsObserver =
            Arc::new(move |event| copy.lock().unwrap().push(event));
        dispatcher.emit(Some(observer), event());
        drop(dispatcher);
        worker.join().unwrap();
        assert_eq!(*received.lock().unwrap(), vec![event()]);
        let value = serde_json::to_value(event()).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 6);
        assert_eq!(value["kind"], "cursor_send");
        assert_eq!(value["send_errors"], 1);
        assert!(value.get("total_drops").is_none());
    }

    #[test]
    fn blocked_callback_keeps_media_emission_bounded_and_nonblocking() {
        let (dispatcher, worker) = Dispatcher::spawn(1).unwrap();
        let (entered, blocked) = mpsc::sync_channel(0);
        let (release, wait) = mpsc::sync_channel(0);
        let wait = Mutex::new(wait);
        let observer: HostMediaDiagnosticsObserver = Arc::new(move |_| {
            entered.send(()).unwrap();
            wait.lock().unwrap().recv().unwrap();
        });
        dispatcher.emit(Some(observer), event());
        blocked.recv_timeout(Duration::from_secs(1)).unwrap();
        dispatcher.emit(None, event());
        let started = Instant::now();
        for _ in 0..1000 {
            dispatcher.emit(None, event());
        }
        assert!(started.elapsed() < Duration::from_millis(100));
        assert_eq!(dispatcher.rejected.load(Ordering::Relaxed), 1000);
        release.send(()).unwrap();
        drop(dispatcher);
        worker.join().unwrap();
    }
}
