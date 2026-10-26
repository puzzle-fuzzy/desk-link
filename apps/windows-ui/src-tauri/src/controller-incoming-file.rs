use std::{future::Future, path::PathBuf};

use apps_windows::transfer::IncomingFileWorker;
use desklink_protocol::{MAX_TRANSFER_CHUNK_BYTES, TransferId, TransferResult};
use tokio::sync::{mpsc, watch};

pub(super) const INCOMING_COMMAND_CAPACITY: usize = 2;
pub(super) const INCOMING_EVENT_CAPACITY: usize = 2;

enum DiskCommand {
    Write { offset: u64, bytes: Vec<u8> },
    Finish { hash: [u8; 32] },
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Stop {
    Active,
    Preserve,
    Cancel,
}

pub(super) enum IncomingFileEventKind {
    Opened {
        offset: u64,
        prefix_hash: Option<[u8; 32]>,
    },
    Written {
        transferred: u64,
    },
    Finished {
        path: PathBuf,
    },
    Failed(TransferResult),
}

pub(super) struct IncomingFileEvent {
    pub ticket: u64,
    pub kind: IncomingFileEventKind,
}

/// One async coordinator per active download, backed by the shared bounded
/// disk worker. The controller never awaits a disk command or its completion.
pub(super) struct IncomingFilePipeline {
    transfer_id: TransferId,
    commands: mpsc::Sender<DiskCommand>,
    stop: watch::Sender<Stop>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl IncomingFilePipeline {
    pub fn start(
        transfer_id: TransferId,
        ticket: u64,
        name: String,
        size: u64,
        events: mpsc::Sender<IncomingFileEvent>,
    ) -> Self {
        Self::spawn(
            transfer_id,
            ticket,
            IncomingFileWorker::create(transfer_id, name, size),
            events,
        )
    }

    fn spawn<F: ReceiveFile>(
        transfer_id: TransferId,
        ticket: u64,
        create: impl Future<Output = Result<F, TransferResult>> + Send + 'static,
        events: mpsc::Sender<IncomingFileEvent>,
    ) -> Self {
        let (commands, pending) = mpsc::channel(INCOMING_COMMAND_CAPACITY);
        let (stop, stopped) = watch::channel(Stop::Active);
        let task = tokio::spawn(run(ticket, create, pending, stopped, events));
        Self {
            transfer_id,
            commands,
            stop,
            task: Some(task),
        }
    }

    pub fn transfer_id(&self) -> TransferId {
        self.transfer_id
    }

    pub fn can_receive(&self) -> bool {
        !self.commands.is_closed() && self.commands.capacity() > 0
    }

    pub fn write_chunk(&self, offset: u64, bytes: Vec<u8>) -> Result<(), TransferResult> {
        if bytes.is_empty() || bytes.len() > MAX_TRANSFER_CHUNK_BYTES {
            return Err(TransferResult::InvalidData);
        }
        self.commands
            .try_send(DiskCommand::Write { offset, bytes })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TransferResult::Busy,
                mpsc::error::TrySendError::Closed(_) => TransferResult::IoFailed,
            })
    }

    pub fn finish(&self, hash: [u8; 32]) -> Result<(), TransferResult> {
        self.commands
            .try_send(DiskCommand::Finish { hash })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => TransferResult::Busy,
                mpsc::error::TrySendError::Closed(_) => TransferResult::IoFailed,
            })
    }

    pub fn cancel(mut self) {
        let _ = self.stop.send(Stop::Cancel);
        self.task.take(); // Detach: the coordinator performs cancel off the UI loop.
    }
}

impl Drop for IncomingFilePipeline {
    fn drop(&mut self) {
        if self.task.take().is_some() {
            // Do not abort a blocking create: once it returns the coordinator
            // drops its worker, preserving any already staged resume prefix.
            let _ = self.stop.send(Stop::Preserve);
        }
    }
}

trait ReceiveFile: Send + 'static {
    fn resume_offset(&self) -> u64;
    fn resume_prefix_hash(&self) -> Option<[u8; 32]>;
    fn write_chunk(
        &mut self,
        offset: u64,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<(), TransferResult>> + Send;
    fn finish(self, hash: [u8; 32])
    -> impl Future<Output = Result<PathBuf, TransferResult>> + Send;
    fn cancel(self);
}

impl ReceiveFile for IncomingFileWorker {
    fn resume_offset(&self) -> u64 {
        self.resume_offset()
    }
    fn resume_prefix_hash(&self) -> Option<[u8; 32]> {
        self.resume_prefix_hash()
    }
    async fn write_chunk(&mut self, offset: u64, bytes: Vec<u8>) -> Result<(), TransferResult> {
        self.write_chunk(offset, bytes).await
    }
    async fn finish(self, hash: [u8; 32]) -> Result<PathBuf, TransferResult> {
        self.finish(hash).await
    }
    fn cancel(self) {
        self.cancel();
    }
}

async fn event(
    ticket: u64,
    kind: IncomingFileEventKind,
    events: &mpsc::Sender<IncomingFileEvent>,
    stop: &mut watch::Receiver<Stop>,
) -> bool {
    tokio::select! {
        biased;
        _ = stop.changed() => false,
        result = events.send(IncomingFileEvent { ticket, kind }) => result.is_ok(),
    }
}

fn stop_file<F: ReceiveFile>(file: F, stop: &watch::Receiver<Stop>) {
    if *stop.borrow() == Stop::Cancel {
        file.cancel();
    }
    // Drop preserves unfinished data for disconnect, without disk I/O here.
}

async fn run<F: ReceiveFile>(
    ticket: u64,
    create: impl Future<Output = Result<F, TransferResult>> + Send,
    mut commands: mpsc::Receiver<DiskCommand>,
    mut stop: watch::Receiver<Stop>,
    events: mpsc::Sender<IncomingFileEvent>,
) {
    tokio::pin!(create);
    let opened = tokio::select! {
        biased;
        _ = stop.changed() => {
            // The disk thread can have sent readiness before the async create
            // future has returned its handle. Keep that future alive here so
            // explicit cancellation cannot accidentally fall back to preserve.
            // This wait belongs only to the detached coordinator.
            if let Ok(file) = create.await { stop_file(file, &stop); }
            return;
        }
        result = &mut create => result,
    };
    let mut file = match opened {
        Ok(file) => file,
        Err(error) => {
            event(
                ticket,
                IncomingFileEventKind::Failed(error),
                &events,
                &mut stop,
            )
            .await;
            return;
        }
    };
    let opened = IncomingFileEventKind::Opened {
        offset: file.resume_offset(),
        prefix_hash: file.resume_prefix_hash(),
    };
    if !event(ticket, opened, &events, &mut stop).await {
        stop_file(file, &stop);
        return;
    }
    loop {
        let command = tokio::select! {
            biased;
            _ = stop.changed() => { stop_file(file, &stop); return; }
            command = commands.recv() => command,
        };
        match command {
            Some(DiskCommand::Write { offset, bytes }) => {
                let transferred = offset.saturating_add(bytes.len() as u64);
                let result = tokio::select! {
                    biased;
                    _ = stop.changed() => { stop_file(file, &stop); return; }
                    result = file.write_chunk(offset, bytes) => result,
                };
                if let Err(error) = result {
                    file.cancel();
                    event(
                        ticket,
                        IncomingFileEventKind::Failed(error),
                        &events,
                        &mut stop,
                    )
                    .await;
                    return;
                }
                if !event(
                    ticket,
                    IncomingFileEventKind::Written { transferred },
                    &events,
                    &mut stop,
                )
                .await
                {
                    stop_file(file, &stop);
                    return;
                }
            }
            Some(DiskCommand::Finish { hash }) => {
                let result = tokio::select! {
                    biased;
                    // Poll finish once before cancellation so its owned worker
                    // arms the finishing Drop guard even if stop was already
                    // ready when this select was built.
                    result = file.finish(hash) => result,
                    _ = stop.changed() => return,
                };
                let kind = match result {
                    Ok(path) => IncomingFileEventKind::Finished { path },
                    Err(error) => IncomingFileEventKind::Failed(error),
                };
                event(ticket, kind, &events, &mut stop).await;
                return;
            }
            None => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;

    #[derive(Default)]
    struct State {
        writes: Mutex<Vec<(u64, Vec<u8>)>>,
        cancelled: AtomicBool,
        preserved: AtomicBool,
        finished: AtomicBool,
    }

    struct ControlledFile {
        state: Arc<State>,
        write: Arc<tokio::sync::Semaphore>,
        finish: Arc<tokio::sync::Semaphore>,
        finishing: bool,
        fail_write: bool,
    }

    impl Drop for ControlledFile {
        fn drop(&mut self) {
            if self.finishing {
                self.state.cancelled.store(true, Ordering::Release);
            }
            if !self.state.cancelled.load(Ordering::Acquire)
                && !self.state.finished.load(Ordering::Acquire)
            {
                self.state.preserved.store(true, Ordering::Release);
            }
        }
    }

    impl ReceiveFile for ControlledFile {
        fn resume_offset(&self) -> u64 {
            0
        }
        fn resume_prefix_hash(&self) -> Option<[u8; 32]> {
            None
        }
        async fn write_chunk(&mut self, offset: u64, bytes: Vec<u8>) -> Result<(), TransferResult> {
            self.write.acquire().await.unwrap().forget();
            if self.fail_write {
                return Err(TransferResult::IoFailed);
            }
            self.state.writes.lock().unwrap().push((offset, bytes));
            Ok(())
        }
        async fn finish(mut self, _: [u8; 32]) -> Result<PathBuf, TransferResult> {
            self.finishing = true;
            self.finish.acquire().await.unwrap().forget();
            self.finishing = false;
            self.state.finished.store(true, Ordering::Release);
            Ok(PathBuf::from("synthetic-download.bin"))
        }
        fn cancel(self) {
            self.state.cancelled.store(true, Ordering::Release);
        }
    }

    struct Fixture {
        pipeline: IncomingFilePipeline,
        events: mpsc::Receiver<IncomingFileEvent>,
        state: Arc<State>,
        write: Arc<tokio::sync::Semaphore>,
        finish: Arc<tokio::sync::Semaphore>,
    }

    async fn fixture(fail_write: bool) -> Fixture {
        let state = Arc::new(State::default());
        let write = Arc::new(tokio::sync::Semaphore::new(0));
        let finish = Arc::new(tokio::sync::Semaphore::new(0));
        let file = ControlledFile {
            state: state.clone(),
            write: write.clone(),
            finish: finish.clone(),
            finishing: false,
            fail_write,
        };
        let (send, mut events) = mpsc::channel(INCOMING_EVENT_CAPACITY);
        let pipeline = IncomingFilePipeline::spawn([5; 16], 17, std::future::ready(Ok(file)), send);
        let opened = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(opened.ticket, 17);
        assert!(matches!(
            opened.kind,
            IncomingFileEventKind::Opened { offset: 0, .. }
        ));
        Fixture {
            pipeline,
            events,
            state,
            write,
            finish,
        }
    }

    async fn until_true(flag: &AtomicBool) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while !flag.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn slow_disk_keeps_media_and_cancel_live_with_a_fixed_byte_budget() {
        let mut fixture = fixture(false).await;
        fixture
            .pipeline
            .write_chunk(0, vec![1; MAX_TRANSFER_CHUNK_BYTES])
            .unwrap();
        tokio::task::yield_now().await; // The first chunk now waits on the disk gate.
        fixture
            .pipeline
            .write_chunk(
                MAX_TRANSFER_CHUNK_BYTES as u64,
                vec![2; MAX_TRANSFER_CHUNK_BYTES],
            )
            .unwrap();
        fixture
            .pipeline
            .write_chunk(
                2 * MAX_TRANSFER_CHUNK_BYTES as u64,
                vec![3; MAX_TRANSFER_CHUNK_BYTES],
            )
            .unwrap();
        assert!(!fixture.pipeline.can_receive());
        assert_eq!(
            fixture
                .pipeline
                .write_chunk(3 * MAX_TRANSFER_CHUNK_BYTES as u64, vec![4]),
            Err(TransferResult::Busy)
        );
        assert_eq!(INCOMING_COMMAND_CAPACITY * MAX_TRANSFER_CHUNK_BYTES, 98_304);
        let (send, mut media) = mpsc::channel(3);
        for lane in ["video", "audio", "control"] {
            send.try_send(lane).unwrap();
        }
        let mut received = Vec::new();
        while received.len() < 3 {
            tokio::select! {
                event = fixture.events.recv() => panic!("disk acknowledged before write gate opened: {}", event.is_some()),
                Some(lane) = media.recv() => received.push(lane),
                _ = tokio::time::sleep(Duration::from_millis(100)) => panic!("media was blocked by disk"),
            }
        }
        assert_eq!(received, ["video", "audio", "control"]);
        assert!(fixture.state.writes.lock().unwrap().is_empty());
        fixture.pipeline.cancel();
        until_true(&fixture.state.cancelled).await;
        assert!(fixture.events.try_recv().is_err());
        assert_eq!(
            fixture.write.available_permits(),
            0,
            "cancel did not wait for the artificial disk gate"
        );
    }

    #[tokio::test]
    async fn ordered_progress_and_completion_follow_actual_disk_acknowledgements() {
        let mut fixture = fixture(false).await;
        fixture.pipeline.write_chunk(0, vec![1, 2]).unwrap();
        fixture.pipeline.write_chunk(2, vec![3, 4]).unwrap();
        assert!(fixture.events.try_recv().is_err());
        fixture.write.add_permits(2);
        for expected in [2, 4] {
            let event = fixture.events.recv().await.unwrap();
            assert_eq!(event.ticket, 17);
            assert!(
                matches!(event.kind, IncomingFileEventKind::Written { transferred } if transferred == expected)
            );
        }
        fixture.pipeline.finish([9; 32]).unwrap();
        tokio::task::yield_now().await;
        assert!(
            fixture.events.try_recv().is_err(),
            "finish cannot publish before sync/rename acknowledgement"
        );
        assert!(!fixture.state.finished.load(Ordering::Acquire));
        fixture.finish.add_permits(1);
        let event = fixture.events.recv().await.unwrap();
        assert!(matches!(event.kind, IncomingFileEventKind::Finished { .. }));
        assert_eq!(
            *fixture.state.writes.lock().unwrap(),
            vec![(0, vec![1, 2]), (2, vec![3, 4])]
        );
    }

    #[tokio::test]
    async fn disconnect_preserves_but_explicit_cancel_and_io_failure_clean_up() {
        let preserved = fixture(false).await;
        preserved.pipeline.write_chunk(0, vec![1]).unwrap();
        tokio::task::yield_now().await;
        drop(preserved.pipeline);
        until_true(&preserved.state.preserved).await;
        assert!(!preserved.state.cancelled.load(Ordering::Acquire));

        let mut failed = fixture(true).await;
        failed.pipeline.write_chunk(0, vec![1]).unwrap();
        failed.write.add_permits(1);
        assert!(matches!(
            failed.events.recv().await.unwrap().kind,
            IncomingFileEventKind::Failed(TransferResult::IoFailed)
        ));
        until_true(&failed.state.cancelled).await;
        assert!(!failed.state.preserved.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn cancel_during_finish_cannot_publish_a_late_success() {
        let mut fixture = fixture(false).await;
        fixture.pipeline.finish([9; 32]).unwrap();
        tokio::task::yield_now().await;
        fixture.pipeline.cancel();
        until_true(&fixture.state.cancelled).await;
        fixture.finish.add_permits(1);
        assert!(!fixture.state.finished.load(Ordering::Acquire));
        assert!(fixture.events.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancel_immediately_after_finish_enqueue_still_cleans_up() {
        let mut fixture = fixture(false).await;
        fixture.pipeline.finish([9; 32]).unwrap();
        fixture.pipeline.cancel();
        until_true(&fixture.state.cancelled).await;
        assert!(!fixture.state.preserved.load(Ordering::Acquire));
        assert!(!fixture.state.finished.load(Ordering::Acquire));
        assert!(fixture.events.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_during_create_returns_immediately_and_cleans_up_after_readiness() {
        let state = Arc::new(State::default());
        let started = Arc::new(AtomicBool::new(false));
        let readiness = Arc::new(tokio::sync::Semaphore::new(0));
        let file = ControlledFile {
            state: state.clone(),
            write: Arc::new(tokio::sync::Semaphore::new(0)),
            finish: Arc::new(tokio::sync::Semaphore::new(0)),
            finishing: false,
            fail_write: false,
        };
        let create_started = started.clone();
        let ready = readiness.clone();
        let create = async move {
            create_started.store(true, Ordering::Release);
            ready.acquire().await.unwrap().forget();
            Ok(file)
        };
        let (events, mut receive) = mpsc::channel(INCOMING_EVENT_CAPACITY);
        let pipeline = IncomingFilePipeline::spawn([5; 16], 17, create, events);
        until_true(&started).await;
        pipeline.cancel();
        // Cancellation did not await the create/ready boundary on this thread.
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert!(!state.cancelled.load(Ordering::Acquire));
        assert!(!state.preserved.load(Ordering::Acquire));
        assert_eq!(readiness.available_permits(), 0);
        readiness.add_permits(1);
        until_true(&state.cancelled).await;
        assert!(!state.preserved.load(Ordering::Acquire));
        assert!(
            receive.try_recv().is_err(),
            "cancelled create must not announce readiness or progress"
        );
    }
}
