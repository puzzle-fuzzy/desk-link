//! Worker scheduling regressions over real local QUIC/Noise. No OS input is
//! injected. Test-only counters observe wire-send entry and reliable dequeue.
use super::*;
use crate::controller::{
    ControllerReceiveTestState, direct_silence_tests::connected_flow_control_fixture,
};
use desklink_crypto::SecureLane;
use desklink_protocol::{
    Codec, FrameFlags, VideoConfig, decode_session_input, encode_video_config, encode_video_packet,
};
use desklink_video::{EncodedFrame, packetize_frame};
use std::time::Instant;

#[derive(Debug)]
struct Observed {
    kind: DesklinkEventKind,
    epoch: u64,
    frame_id: u64,
}

extern "C" fn observe(context: *mut std::ffi::c_void, event: *const DesklinkEvent) {
    let sender = unsafe { &*(context as *const mpsc::Sender<Observed>) };
    let event = unsafe { &*event };
    let _ = sender.try_send(Observed {
        kind: event.kind,
        epoch: event.input_epoch,
        frame_id: event.frame_id,
    });
}

struct Driver {
    commands: mpsc::Sender<ControllerCommand>,
    cancel: watch::Sender<bool>,
    events: mpsc::Receiver<Observed>,
    releases: Arc<Mutex<Vec<CapturedInput>>>,
    release_pending: Arc<AtomicU8>,
    task: Option<tokio::task::JoinHandle<ConnectedOutcome>>,
}

impl Drop for Driver {
    fn drop(&mut self) {
        let _ = self.cancel.send(true);
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

impl Driver {
    fn start(runtime: ControllerRuntime) -> Self {
        let (commands, mut receiver) = mpsc::channel(COMMAND_CAPACITY + RELEASE_RESERVE);
        let (cancel, mut cancellation) = watch::channel(false);
        let (observed, events) = mpsc::channel(64);
        let callback_owner = Arc::new(observed);
        let pointer = Arc::as_ptr(&callback_owner) as usize;
        let releases = Arc::new(Mutex::new(Vec::new()));
        let worker_releases = releases.clone();
        let release_pending = Arc::new(AtomicU8::new(0));
        let worker_pending = release_pending.clone();
        let task = tokio::spawn(async move {
            // Keep the callback address alive even when a failed test aborts
            // this task before Drop has been polled by the Tokio executor.
            let _callback_owner = callback_owner;
            run_connected(
                runtime,
                &mut receiver,
                &mut None,
                &mut cancellation,
                CallbackTarget {
                    callback: Some(observe),
                    context: pointer,
                },
                &worker_releases,
                &worker_pending,
            )
            .await
        });
        Self {
            commands,
            cancel,
            events,
            releases,
            release_pending,
            task: Some(task),
        }
    }

    async fn event(&mut self, kind: DesklinkEventKind, epoch: u64) -> Observed {
        tokio::time::timeout(Duration::from_millis(500), async {
            loop {
                let event = self.events.recv().await.expect("worker callback ended");
                if event.kind == kind && (epoch == 0 || event.epoch == epoch) {
                    return event;
                }
            }
        })
        .await
        .expect("worker stopped receiving while a reliable send was pending")
    }

    async fn stop(mut self) -> ConnectedOutcome {
        self.cancel.send(true).unwrap();
        let mut task = self.task.take().unwrap();
        match tokio::time::timeout(Duration::from_millis(500), &mut task).await {
            Ok(outcome) => {
                let outcome = outcome.unwrap();
                if let ConnectedOutcome::Failed {
                    failure,
                    stream_id,
                    stable,
                } = &outcome
                {
                    eprintln!(
                        "worker test failed before shutdown: stream={stream_id} stable={stable} reason={}",
                        failure.message
                    );
                }
                outcome
            }
            Err(_) => {
                task.abort();
                panic!("shutdown waited for a queued/reliably blocked send")
            }
        }
    }
}

fn context(epoch: u64) -> InputContext {
    InputContext {
        stream_id: 1,
        input_epoch: epoch,
    }
}

fn input(epoch: u64) -> CapturedInput {
    CapturedInput {
        context: context(epoch),
        event: InputEvent::MouseWheel {
            delta_x: 1,
            delta_y: 1,
        },
    }
}

async fn config(
    host: &QuicClient,
    secure: &tokio::sync::Mutex<desklink_crypto::SecureSession>,
    epoch: u64,
    version: u32,
) {
    let plaintext = encode_video_config(&VideoConfig {
        protocol_version: desklink_protocol::PROTOCOL_VERSION,
        stream_id: 1,
        input_epoch: epoch,
        config_version: version,
        codec: Codec::H264,
        width: 16,
        height: 16,
        sequence_header: vec![0, 0, 1, 0x67, 0x4d, 0, 0x1f, 0, 0, 1, 0x68, 1],
    })
    .unwrap();
    let encrypted = secure
        .lock()
        .await
        .seal(SecureLane::VideoConfig, &plaintext)
        .unwrap();
    host.send_video_config(encrypted).await.unwrap();
}

async fn frame(
    host: &QuicClient,
    secure: &tokio::sync::Mutex<desklink_crypto::SecureSession>,
    version: u32,
) {
    for packet in packetize_frame(&EncodedFrame {
        stream_id: 1,
        frame_id: 1,
        config_version: version,
        capture_timestamp_us: 1,
        width: 16,
        height: 16,
        flags: FrameFlags::KEYFRAME,
        data: vec![0x65; 64],
    })
    .unwrap()
    {
        let plaintext = encode_video_packet(&packet).unwrap();
        let encrypted = secure
            .lock()
            .await
            .seal(SecureLane::VideoDatagram, &plaintext)
            .unwrap();
        host.send_video_datagram(encrypted).await.unwrap();
    }
}

async fn blocked_input(state: &ControllerReceiveTestState) -> u64 {
    tokio::time::timeout(Duration::from_millis(800), async {
        loop {
            let before = state.writes_started.load(Ordering::SeqCst);
            if before > 0
                && state.input_order.try_lock().is_err()
                && state.secure.try_lock().is_ok()
            {
                tokio::time::sleep(Duration::from_millis(40)).await;
                if before == state.writes_started.load(Ordering::SeqCst)
                    && state.input_order.try_lock().is_err()
                    && state.secure.try_lock().is_ok()
                {
                    return before;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        eprintln!(
            "flow-control observation: writes_started={} input_order_locked={} crypto_available={}",
            state.writes_started.load(Ordering::SeqCst),
            state.input_order.try_lock().is_err(),
            state.secure.try_lock().is_ok()
        );
        panic!("fixture did not create actual reliable Input flow-control backpressure: {error}")
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn blocked_input_keeps_receiving_and_only_unstarted_old_context_is_filtered() {
    let fixture = connected_flow_control_fixture().await;
    let state = fixture.runtime.receive_test_state();
    let host = fixture.host.clone();
    let secure = fixture.host_secure.clone();
    let mut driver = Driver::start(fixture.runtime);
    config(&host, &secure, 11, 1).await;
    driver.event(DesklinkEventKind::VideoConfig, 11).await;
    for _ in 0..COMMAND_CAPACITY {
        driver
            .commands
            .send(ControllerCommand::SendInput(input(11)))
            .await
            .unwrap();
    }
    let started = blocked_input(&state).await;
    config(&host, &secure, 22, 2).await;
    driver.event(DesklinkEventKind::VideoConfig, 22).await;
    frame(&host, &secure, 2).await;
    assert_eq!(
        driver
            .event(DesklinkEventKind::H264AccessUnit, 0)
            .await
            .frame_id,
        1
    );
    assert_eq!(
        state.writes_started.load(Ordering::SeqCst),
        started,
        "the old frame must remain in flight during reconfiguration"
    );
    driver
        .commands
        .send(ControllerCommand::SendInput(input(22)))
        .await
        .unwrap();
    let drain = tokio::spawn(async move {
        let mut count = 0_u64;
        loop {
            let encrypted = host.next_input().await.unwrap();
            let plaintext = secure
                .lock()
                .await
                .open(SecureLane::Input, &encrypted)
                .unwrap();
            let event = decode_session_input(&plaintext).unwrap();
            count += 1;
            assert_eq!(event.sequence, count, "input sequence/wire order changed");
            if event.input_epoch == 22 {
                return count;
            }
            assert_eq!(
                event.input_epoch, 11,
                "an in-flight old context was relabelled"
            );
        }
    });
    let received = tokio::time::timeout(Duration::from_millis(800), drain)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        received,
        started + 1,
        "old unstarted inputs must not be replayed after the new epoch"
    );
    assert!(matches!(
        driver.stop().await,
        ConnectedOutcome::Shutdown { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_release_batch_keeps_media_and_shutdown_does_not_wait_for_backpressure() {
    let fixture = connected_flow_control_fixture().await;
    let state = fixture.runtime.receive_test_state();
    let mut driver = Driver::start(fixture.runtime);
    config(&fixture.host, &fixture.host_secure, 11, 1).await;
    driver.event(DesklinkEventKind::VideoConfig, 11).await;
    let mut release = input(11);
    release.event = InputEvent::Key {
        code: desklink_protocol::KeyCode::Character('a'),
        pressed: false,
        modifiers: desklink_protocol::Modifiers::default(),
    };
    enqueue_release_all(
        &driver.commands,
        &driver.releases,
        &driver.release_pending,
        vec![release; MAX_RELEASE_EVENTS],
    )
    .unwrap();
    blocked_input(&state).await;
    config(&fixture.host, &fixture.host_secure, 22, 2).await;
    driver.event(DesklinkEventKind::VideoConfig, 22).await;
    frame(&fixture.host, &fixture.host_secure, 2).await;
    driver.event(DesklinkEventKind::H264AccessUnit, 0).await;
    let started = Instant::now();
    assert!(matches!(
        driver.stop().await,
        ConnectedOutcome::Shutdown { .. }
    ));
    assert!(started.elapsed() < Duration::from_millis(500));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dequeued_reliable_event_survives_commands_while_waiting_for_crypto() {
    let fixture = connected_flow_control_fixture().await;
    let state = fixture.runtime.receive_test_state();
    let mut driver = Driver::start(fixture.runtime);
    config(&fixture.host, &fixture.host_secure, 11, 1).await;
    driver.event(DesklinkEventKind::VideoConfig, 11).await;
    let baseline = state.dequeued.load(Ordering::SeqCst);
    let crypto_guard = state.secure.clone().lock_owned().await;
    let message = encode_control(&ControlMessage::SetAudioEnabled { enabled: true }).unwrap();
    let encrypted = fixture
        .host_secure
        .lock()
        .await
        .seal(SecureLane::Control, &message)
        .unwrap();
    fixture.host.send_control(encrypted).await.unwrap();
    tokio::time::timeout(Duration::from_millis(300), async {
        while state.dequeued.load(Ordering::SeqCst) == baseline {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reliable message never reached the decrypt wait");
    driver
        .commands
        .send(ControllerCommand::RequestKeyframe)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_millis(300), async {
        while driver.commands.capacity() != COMMAND_CAPACITY + RELEASE_RESERVE {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(crypto_guard);
    driver.event(DesklinkEventKind::Control, 0).await;
    assert!(matches!(
        driver.stop().await,
        ConnectedOutcome::Shutdown { .. }
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn standalone_control_backpressure_has_one_second_budget_and_closes_session() {
    let fixture = connected_flow_control_fixture().await;
    let sender = fixture.runtime.control_sender();
    let client = fixture.runtime.receive_test_state().client;
    for sent in 0..COMMAND_CAPACITY {
        let started = Instant::now();
        let result = sender
            .send(ControlMessage::SetAudioEnabled {
                enabled: sent % 2 == 0,
            })
            .await;
        if let Err(error) = result {
            assert!(
                matches!(&error, ControllerError::Transport(TransportError::Connection(message))
                if message.contains("control send deadline exceeded")),
                "unexpected control failure: {error}"
            );
            assert!(
                sent >= 128,
                "the real non-reading peer's bounded receive queue was not exercised"
            );
            assert!(
                started.elapsed()
                    <= crate::controller::CONTROLLER_SEND_TIMEOUT + Duration::from_millis(250)
            );
            tokio::time::timeout(Duration::from_millis(250), client.next_closed_reason())
                .await
                .expect("control timeout returned before closing its session");
            eprintln!(
                "control flow-control timeout: completed_messages={sent} last_send_ms={}",
                started.elapsed().as_millis()
            );
            return;
        }
    }
    panic!("test fixture absorbed every control message instead of producing real backpressure");
}

#[tokio::test]
async fn only_an_unpolled_old_input_may_be_discarded() {
    let mut send = Some(OwnedConnectedSend {
        context: Some(context(11)),
        started: false,
        future: Box::pin(std::future::pending()),
    });
    discard_unstarted_input(&mut send, context(22));
    assert!(send.is_none());
    send = Some(OwnedConnectedSend {
        context: Some(context(11)),
        started: false,
        future: Box::pin(std::future::pending()),
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(1), Pin::new(send.as_mut().unwrap()))
            .await
            .is_err()
    );
    assert!(send.as_ref().unwrap().started);
    discard_unstarted_input(&mut send, context(22));
    assert!(
        send.is_some(),
        "an entered send may not be cancelled by a normal epoch change"
    );
}

#[test]
fn release_aggregation_is_bounded_and_preserves_original_contexts() {
    let (commands, _receiver) = mpsc::channel(1);
    let events = Mutex::new(Vec::new());
    let pending = AtomicU8::new(0);
    enqueue_release_all(
        &commands,
        &events,
        &pending,
        vec![input(11); MAX_RELEASE_EVENTS],
    )
    .unwrap();
    assert!(enqueue_release_all(&commands, &events, &pending, vec![input(22)]).is_err());
    let batch = take_release_events(&events);
    assert_eq!(batch.len(), MAX_RELEASE_EVENTS);
    assert!(batch.iter().all(|input| input.context == context(11)));
}
