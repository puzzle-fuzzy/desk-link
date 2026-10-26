//! Narrow, bounded Apple ABI actions. The wire format remains the shared
//! encrypted protocol; only the callback presentation is UTF-8 JSON.
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use desklink_protocol::{
    ControlMessage, InputEvent, KeyCode, Modifiers, TransferMessage, TransferResult,
    VideoQualityPreference,
};
use serde_json::{Value, json};

use crate::worker::{CallbackTarget, CommandScope, SessionGate, SessionIo};
use crate::{ControllerError, ControllerEvent, DesklinkEventKind, DesklinkState, EventMeta};

pub(crate) const MAX_SESSION_JSON_BYTES: usize = 300 * 1024;
pub(crate) const MAX_TEXT_BYTES: usize = 4096;
const MAX_PENDING_CLIPBOARD: usize = 16;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(crate) enum SessionAction {
    // Constructed by tests and kept for stream-scoped input callers; the C
    // entry point currently routes through the captured-context send path.
    #[allow(dead_code)]
    Input(InputEvent),
    Text(String),
    RequestKeyframe,
    SelectDisplay(u32),
    SetVideoQuality(VideoQualityPreference),
    SetAudioEnabled(bool),
    File(crate::file_commands::FileAction),
    SendClipboard {
        request_id: u64,
        text: String,
    },
    RequestClipboard {
        request_id: u64,
    },
}

impl SessionAction {
    fn clipboard(&self) -> Option<(u64, &'static str)> {
        match self {
            Self::SendClipboard { request_id, .. } => Some((*request_id, "send")),
            Self::RequestClipboard { request_id } => Some((*request_id, "request")),
            _ => None,
        }
    }

    fn operation(&self) -> Option<&'static str> {
        match self {
            Self::SelectDisplay(_) => Some("select_display"),
            Self::SetVideoQuality(_) => Some("set_video_quality"),
            Self::Text(_) => Some("send_text"),
            Self::SetAudioEnabled(_) => Some("set_audio_enabled"),
            _ => None,
        }
    }
}

struct PendingClipboard {
    operation: &'static str,
    stream_id: u64,
    deadline: Instant,
}

pub(crate) struct SessionCommands {
    callback: CallbackTarget,
    gate: Arc<SessionGate>,
    pending: BTreeMap<u64, PendingClipboard>,
    last_request_id: u64,
    displays: Option<Value>,
    quality: Option<Value>,
    display_switch: Option<(u32, Instant)>,
    quality_change: Option<(VideoQualityPreference, Instant)>,
    audio: Option<Value>,
    audio_change: Option<(bool, Instant)>,
    audio_requested: bool,
    audio_error_reported: bool,
    audio_decoder: crate::audio_decode::AudioDecoder,
}

impl SessionCommands {
    pub fn new(callback: CallbackTarget, gate: Arc<SessionGate>) -> Self {
        Self {
            callback,
            gate,
            pending: BTreeMap::new(),
            last_request_id: 0,
            displays: None,
            quality: None,
            display_switch: None,
            quality_change: None,
            audio: None,
            audio_change: None,
            audio_requested: false,
            audio_error_reported: false,
            audio_decoder: crate::audio_decode::AudioDecoder::default(),
        }
    }

    fn emit(&self, kind: DesklinkEventKind, stream_id: u64, value: Value) {
        if let Ok(data) = serde_json::to_vec(&value)
            && data.len() <= MAX_SESSION_JSON_BYTES
        {
            self.callback.emit(
                kind,
                &data,
                EventMeta::for_stream(stream_id),
                DesklinkState::Connected,
            );
        }
    }

    fn clipboard_result(
        &self,
        request_id: u64,
        operation: &str,
        stream_id: u64,
        result: &str,
        text: Option<&str>,
    ) {
        let mut value = json!({"request_id": request_id, "operation": operation,
            "status": if result == "completed" { "completed" } else { "failed" }, "result": result});
        if let Some(text) = text {
            value["text"] = json!(text);
        }
        self.emit(DesklinkEventKind::Clipboard, stream_id, value);
    }

    fn operation_error(&self, operation: &str, stream_id: u64, message: &str) {
        self.emit(
            DesklinkEventKind::Session,
            stream_id,
            json!({"kind":"operation_error", "operation":operation, "message":message}),
        );
    }

    fn cancel_clipboard(&mut self) {
        for (request_id, pending) in std::mem::take(&mut self.pending) {
            self.clipboard_result(
                request_id,
                pending.operation,
                pending.stream_id,
                "stale_session",
                None,
            );
        }
    }

    pub async fn execute(
        &mut self,
        io: &SessionIo,
        scope: CommandScope,
        action: SessionAction,
    ) -> Result<(), ControllerError> {
        if !self.gate.accepts(scope) || io.active_stream_id() != Some(scope.stream_id) {
            if let Some((request_id, operation)) = action.clipboard() {
                self.clipboard_result(
                    request_id,
                    operation,
                    scope.stream_id,
                    "stale_session",
                    None,
                );
            } else if let Some(operation) = action.operation() {
                self.operation_error(operation, scope.stream_id, "stale session action discarded");
            }
            return Ok(());
        }
        let clipboard = action.clipboard();
        let operation = action.operation();
        let audio_request = if let SessionAction::SetAudioEnabled(enabled) = &action {
            Some(*enabled)
        } else {
            None
        };
        if let Some((request_id, operation)) = clipboard {
            if request_id <= self.last_request_id || self.pending.len() >= MAX_PENDING_CLIPBOARD {
                self.clipboard_result(request_id, operation, scope.stream_id, "busy", None);
                return Ok(());
            }
            self.last_request_id = request_id;
            self.pending.insert(
                request_id,
                PendingClipboard {
                    operation,
                    stream_id: scope.stream_id,
                    deadline: Instant::now() + RESPONSE_TIMEOUT,
                },
            );
        }
        let result = tokio::time::timeout(SEND_TIMEOUT, async {
            match action {
                SessionAction::Input(event) => io.send_input(event).await,
                SessionAction::Text(text) => {
                    for scalar in text.chars() {
                        for pressed in [true, false] {
                            io.send_input(InputEvent::Key {
                                code: match scalar {
                                    '\r' | '\n' => KeyCode::Enter,
                                    '\t' => KeyCode::Tab,
                                    _ => KeyCode::Character(scalar),
                                },
                                pressed,
                                modifiers: Modifiers(0),
                            })
                            .await?;
                        }
                    }
                    Ok(())
                }
                SessionAction::RequestKeyframe => io.request_keyframe().await,
                SessionAction::SelectDisplay(display_id) => {
                    // Invalidate queued input even when the Windows host keeps
                    // the same stream id while reconfiguring its encoder.
                    self.gate.invalidate();
                    self.cancel_clipboard();
                    self.display_switch = Some((display_id, Instant::now() + RESPONSE_TIMEOUT));
                    io.select_display(display_id).await
                }
                SessionAction::SetVideoQuality(preference) => {
                    self.quality_change = Some((preference, Instant::now() + RESPONSE_TIMEOUT));
                    io.set_video_quality(preference).await
                }
                SessionAction::SetAudioEnabled(enabled) => {
                    self.audio_requested = enabled;
                    if !enabled {
                        self.audio_decoder.set_enabled(false);
                    }
                    self.audio_change = Some((enabled, Instant::now() + RESPONSE_TIMEOUT));
                    io.set_audio_enabled(enabled).await
                }
                SessionAction::File(_) => {
                    unreachable!("file actions run on their independent worker")
                }
                SessionAction::SendClipboard { request_id, text } => {
                    io.send_transfer(TransferMessage::ClipboardSet { request_id, text })
                        .await
                }
                SessionAction::RequestClipboard { request_id } => {
                    io.send_transfer(TransferMessage::ClipboardRequest { request_id })
                        .await
                }
            }
        })
        .await;
        match result {
            Ok(Ok(())) => {
                // Older Windows hosts only announce audio when its state
                // changes. An idempotent retry may therefore produce no new
                // acknowledgement. Reuse an authenticated matching state only
                // after the explicit command was successfully sent.
                if let Some(enabled) = audio_request
                    && let Some(state) = self.audio.as_ref()
                    && state["enabled"].as_bool() == Some(enabled)
                {
                    self.audio_change = None;
                    self.audio_decoder
                        .set_enabled(enabled && state["available"].as_bool() == Some(true));
                    self.emit(DesklinkEventKind::Session, scope.stream_id, state.clone());
                }
                Ok(())
            }
            result => {
                if let Some((request_id, operation)) = clipboard {
                    self.pending.remove(&request_id);
                    self.clipboard_result(
                        request_id,
                        operation,
                        scope.stream_id,
                        if result.is_err() {
                            "timeout"
                        } else {
                            "transport_error"
                        },
                        None,
                    );
                }
                if let Some(operation) = operation {
                    self.operation_error(
                        operation,
                        scope.stream_id,
                        "session command could not be sent",
                    );
                }
                // A cancelled reliable write may have partially sent a frame.
                // Reconnect rather than retrying non-idempotent text/clipboard.
                match result {
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(ControllerError::Transport(
                        desklink_transport::TransportError::Connection(
                            "session command send timed out".into(),
                        ),
                    )),
                    Ok(Ok(())) => unreachable!(),
                }
            }
        }
    }

    pub fn observe(&mut self, event: &ControllerEvent, stream_id: u64) -> bool {
        let mut confirmed_display_switch = false;
        match event {
            ControllerEvent::VideoConfig(config) => {
                self.audio_decoder.set_stream(config.stream_id);
                let previous = self.gate.stream_id.load(Ordering::Acquire);
                if previous != 0 && previous != config.stream_id {
                    self.gate.invalidate();
                    self.cancel_clipboard();
                }
                if self.display_switch.is_none() {
                    self.gate
                        .stream_id
                        .store(config.stream_id, Ordering::Release);
                }
                self.gate
                    .input_epoch
                    .store(config.input_epoch, Ordering::Release);
            }
            ControllerEvent::Control(ControlMessage::DisplayList {
                displays,
                active_display_id,
            }) => {
                if let Some((requested, _)) = self.display_switch.take() {
                    confirmed_display_switch = requested == *active_display_id;
                    if requested != *active_display_id {
                        self.operation_error(
                            "select_display",
                            stream_id,
                            "host could not select the requested display",
                        );
                    }
                    self.gate.stream_id.store(stream_id, Ordering::Release);
                }
                let value = json!({"kind":"display_list", "selected_display_id":active_display_id,
                    "displays":displays.iter().map(|display| json!({"id":display.id,"width":display.width,"height":display.height,"is_primary":display.primary})).collect::<Vec<_>>()});
                self.displays = Some(value.clone());
                self.emit(DesklinkEventKind::Session, stream_id, value);
            }
            ControllerEvent::Control(ControlMessage::VideoQualityState { preference, preset }) => {
                if self
                    .quality_change
                    .is_some_and(|(pending, _)| pending == *preference)
                {
                    self.quality_change = None;
                }
                let value = json!({"kind":"video_quality_state", "quality_preference":preference,"applied_preset":preset});
                self.quality = Some(value.clone());
                self.emit(DesklinkEventKind::Session, stream_id, value);
            }
            ControllerEvent::Control(ControlMessage::AudioState { available, enabled }) => {
                if !*available
                    || self
                        .audio_change
                        .is_some_and(|(desired, _)| desired == *enabled)
                {
                    self.audio_change = None;
                }
                self.audio_decoder
                    .set_enabled(*available && *enabled && self.audio_requested);
                self.audio_error_reported = false;
                let value = json!({"kind":"audio_state","available":available,"enabled":enabled});
                self.audio = Some(value.clone());
                self.emit(DesklinkEventKind::Session, stream_id, value);
            }
            ControllerEvent::Audio(packet) => match self.audio_decoder.decode(packet) {
                Ok(Some(pcm)) => {
                    self.audio_error_reported = false;
                    self.callback.emit(
                        DesklinkEventKind::AudioPcm,
                        &pcm,
                        EventMeta {
                            stream_id: packet.stream_id,
                            frame_id: packet.sequence,
                            config_version: 0,
                            width: desklink_protocol::AUDIO_SAMPLE_RATE as u16,
                            height: 1,
                            input_epoch: 0,
                        },
                        DesklinkState::Connected,
                    );
                }
                Ok(None) => {}
                Err(message) if !self.audio_error_reported => {
                    self.audio_error_reported = true;
                    self.emit(
                        DesklinkEventKind::Session,
                        stream_id,
                        json!({"kind":"audio_error","message":message}),
                    );
                }
                Err(_) => {}
            },
            ControllerEvent::Transfer(TransferMessage::ClipboardData { request_id, text }) => {
                if let Some(pending) = self.pending.remove(request_id) {
                    if pending.operation == "request" {
                        self.clipboard_result(
                            *request_id,
                            pending.operation,
                            pending.stream_id,
                            "completed",
                            Some(text),
                        );
                    } else {
                        self.clipboard_result(
                            *request_id,
                            pending.operation,
                            pending.stream_id,
                            "invalid_data",
                            None,
                        );
                    }
                }
            }
            ControllerEvent::Transfer(TransferMessage::ClipboardResult { request_id, result }) => {
                if let Some(pending) = self.pending.remove(request_id) {
                    let result =
                        if pending.operation == "request" && *result == TransferResult::Completed {
                            "invalid_data"
                        } else {
                            transfer_result_name(*result)
                        };
                    self.clipboard_result(
                        *request_id,
                        pending.operation,
                        pending.stream_id,
                        result,
                        None,
                    );
                }
            }
            _ => {}
        }
        confirmed_display_switch
    }

    pub fn replay(&self, stream_id: u64) {
        // A new VideoConfig can overtake the control acknowledgement. Never
        // replay the old display as if it were the switch result.
        let displays = if self.display_switch.is_none() {
            &self.displays
        } else {
            &None
        };
        for state in [displays, &self.quality, &self.audio].into_iter().flatten() {
            self.emit(DesklinkEventKind::Session, stream_id, state.clone());
        }
    }

    pub fn expire(&mut self, stream_id: u64) {
        let now = Instant::now();
        let expired = self
            .pending
            .iter()
            .filter_map(|(id, pending)| (pending.deadline <= now).then_some(*id))
            .collect::<Vec<_>>();
        for request_id in expired {
            if let Some(pending) = self.pending.remove(&request_id) {
                self.clipboard_result(
                    request_id,
                    pending.operation,
                    pending.stream_id,
                    "timeout",
                    None,
                );
            }
        }
        if self
            .display_switch
            .is_some_and(|(_, deadline)| deadline <= now)
        {
            self.display_switch = None;
            self.gate.stream_id.store(stream_id, Ordering::Release);
            self.operation_error(
                "select_display",
                stream_id,
                "host did not confirm display selection within 10 seconds",
            );
        }
        if self
            .quality_change
            .is_some_and(|(_, deadline)| deadline <= now)
        {
            self.quality_change = None;
            self.operation_error(
                "set_video_quality",
                stream_id,
                "host did not confirm video quality within 10 seconds",
            );
        }
        if self
            .audio_change
            .is_some_and(|(_, deadline)| deadline <= now)
        {
            self.audio_change = None;
            self.audio_requested = false;
            self.audio_decoder.set_enabled(false);
            self.operation_error(
                "set_audio_enabled",
                stream_id,
                "host did not confirm sound within 10 seconds",
            );
        }
    }
}

impl Drop for SessionCommands {
    fn drop(&mut self) {
        self.gate.invalidate();
        self.cancel_clipboard();
    }
}

fn transfer_result_name(result: TransferResult) -> &'static str {
    match result {
        TransferResult::Completed => "completed",
        TransferResult::Rejected => "rejected",
        TransferResult::Cancelled => "cancelled",
        TransferResult::TooLarge => "too_large",
        TransferResult::InvalidData => "invalid_data",
        TransferResult::PermissionDenied => "permission_denied",
        TransferResult::IoFailed => "io_failed",
        TransferResult::InsufficientSpace => "insufficient_space",
        TransferResult::SourceChanged => "source_changed",
        TransferResult::Unsupported => "unsupported",
        TransferResult::Busy => "busy",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{ControllerRuntime, DesklinkEvent};
    use desklink_protocol::InputContext;
    use std::sync::Mutex;

    fn test_io(runtime: &ControllerRuntime) -> SessionIo {
        SessionIo::for_runtime(
            runtime,
            Some(InputContext {
                stream_id: 9,
                input_epoch: 1,
            }),
        )
    }

    type Events = Mutex<Vec<(DesklinkEventKind, u64, Value)>>;

    extern "C" fn record(context: *mut std::ffi::c_void, event: *const DesklinkEvent) {
        let events = unsafe { &*context.cast::<Events>() };
        let event = unsafe { &*event };
        let bytes = unsafe { std::slice::from_raw_parts(event.data, event.data_len) };
        let value = if event.kind == DesklinkEventKind::AudioPcm {
            json!({"bytes":bytes.len(),"sequence":event.frame_id,"sample_rate":event.width,"channels":event.height})
        } else {
            serde_json::from_slice(bytes).unwrap()
        };
        events
            .lock()
            .unwrap()
            .push((event.kind, event.stream_id, value));
    }

    fn state(events: &Events) -> SessionCommands {
        SessionCommands::new(
            CallbackTarget {
                callback: Some(record),
                context: (events as *const Events) as usize,
            },
            Arc::new(SessionGate::default()),
        )
    }

    #[test]
    fn clipboard_timeouts_are_correlated_and_unsolicited_or_late_text_is_ignored() {
        let events = Events::default();
        let mut state = state(&events);
        state.pending.insert(
            41,
            PendingClipboard {
                operation: "request",
                stream_id: 9,
                deadline: Instant::now(),
            },
        );
        state.expire(9);
        state.observe(
            &ControllerEvent::Transfer(TransferMessage::ClipboardData {
                request_id: 41,
                text: "expired text".into(),
            }),
            9,
        );
        state.observe(
            &ControllerEvent::Transfer(TransferMessage::ClipboardData {
                request_id: 999,
                text: "unsolicited".into(),
            }),
            9,
        );
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            (
                DesklinkEventKind::Clipboard,
                9,
                json!({"request_id":41,"status":"failed","operation":"request","result":"timeout"})
            )
        );
    }

    #[test]
    fn clipboard_success_direction_and_maximum_json_are_bounded() {
        let events = Events::default();
        let mut state = state(&events);
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        state.pending.insert(
            1,
            PendingClipboard {
                operation: "send",
                stream_id: 9,
                deadline,
            },
        );
        state.pending.insert(
            2,
            PendingClipboard {
                operation: "request",
                stream_id: 9,
                deadline,
            },
        );
        state.observe(
            &ControllerEvent::Transfer(TransferMessage::ClipboardResult {
                request_id: 1,
                result: TransferResult::Completed,
            }),
            9,
        );
        // Every byte needs six JSON escape bytes in this worst-case string.
        let text = "\u{1}".repeat(desklink_protocol::MAX_CLIPBOARD_TEXT_BYTES);
        state.observe(
            &ControllerEvent::Transfer(TransferMessage::ClipboardData {
                request_id: 2,
                text: text.clone(),
            }),
            9,
        );
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].2["status"], "completed");
        assert!(events[0].2.get("text").is_none());
        assert_eq!(events[1].2["text"], text);
        assert!(serde_json::to_vec(&events[1].2).unwrap().len() <= MAX_SESSION_JSON_BYTES);
    }

    #[test]
    fn display_reuse_invalidates_old_scope_and_only_host_ack_restores_input() {
        let events = Events::default();
        let mut state = state(&events);
        state.gate.stream_id.store(9, Ordering::Release);
        let old_scope = state.gate.scope(9).unwrap();
        state.gate.invalidate();
        state.display_switch = Some((2, Instant::now() + RESPONSE_TIMEOUT));
        state.pending.insert(
            1,
            PendingClipboard {
                operation: "request",
                stream_id: 9,
                deadline: Instant::now() + RESPONSE_TIMEOUT,
            },
        );
        state.cancel_clipboard();
        assert!(!state.gate.accepts(old_scope));
        assert!(state.gate.scope(9).is_none());
        state.observe(
            &ControllerEvent::Control(ControlMessage::DisplayList {
                displays: vec![desklink_protocol::RemoteDisplay {
                    id: 2,
                    width: 1920,
                    height: 1080,
                    primary: false,
                }],
                active_display_id: 2,
            }),
            9,
        );
        assert!(!state.gate.accepts(old_scope));
        assert!(state.gate.scope(9).is_some());
        assert!(state.pending.is_empty());
        let events = events.lock().unwrap();
        assert_eq!(events[0].2["result"], "stale_session");
        assert_eq!(events[1].2["selected_display_id"], 2);
    }

    #[test]
    fn host_quality_state_is_preserved_without_claiming_a_pending_request_succeeded() {
        let events = Events::default();
        let mut state = state(&events);
        state.quality_change = Some((
            VideoQualityPreference::Sharp,
            Instant::now() + RESPONSE_TIMEOUT,
        ));
        state.observe(
            &ControllerEvent::Control(ControlMessage::VideoQualityState {
                preference: VideoQualityPreference::Automatic,
                preset: desklink_protocol::VideoQualityPreset::Smooth,
            }),
            9,
        );
        assert!(state.quality_change.is_some());
        assert_eq!(
            events.lock().unwrap()[0].2,
            json!({"kind":"video_quality_state","quality_preference":"automatic","applied_preset":"smooth"})
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn encrypted_actions_round_trip_and_discard_stale_display_commands() {
        tokio::time::timeout(Duration::from_secs(5), encrypted_actions_probe())
            .await
            .expect("encrypted session actions must complete within five seconds");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn encrypted_audio_requires_host_ack_mutes_immediately_and_survives_invalid_datagrams() {
        use desklink_crypto::SecureLane;
        use desklink_protocol::{
            AUDIO_CHANNELS, AUDIO_SAMPLE_RATE, AudioCodec, AudioPacket, decode_control,
            encode_audio_packet, encode_control,
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            let (mut runtime, host, mut secure, relay_task) = encrypted_runtime().await;
            let events = Events::default();
            let mut state = state(&events);
            state.observe(&runtime.next_event().await.unwrap(), 9);
            let scope = state.gate.scope(9).unwrap();
            let packet = |sequence| AudioPacket {
                protocol_version: desklink_protocol::PROTOCOL_VERSION,
                stream_id: 9,
                sequence,
                capture_timestamp_us: sequence * 10_000,
                codec: AudioCodec::PcmS16Le,
                sample_rate: AUDIO_SAMPLE_RATE,
                channels: AUDIO_CHANNELS,
                payload: vec![1; 960],
            };
            state
                .execute(
                    &test_io(&runtime),
                    scope,
                    SessionAction::SetAudioEnabled(true),
                )
                .await
                .unwrap();
            let plaintext = secure
                .open(SecureLane::Control, &host.next_control().await.unwrap())
                .unwrap();
            assert_eq!(
                decode_control(&plaintext).unwrap(),
                ControlMessage::SetAudioEnabled { enabled: true }
            );
            let bytes = encode_audio_packet(&packet(1)).unwrap();
            host.send_audio_datagram(secure.seal(SecureLane::AudioDatagram, &bytes).unwrap())
                .await
                .unwrap();
            state.observe(&runtime.next_event().await.unwrap(), 9);
            assert!(
                events.lock().unwrap().is_empty(),
                "no PCM before acknowledgement"
            );
            let bytes = encode_control(&ControlMessage::AudioState {
                available: true,
                enabled: true,
            })
            .unwrap();
            host.send_control(secure.seal(SecureLane::Control, &bytes).unwrap())
                .await
                .unwrap();
            state.observe(&runtime.next_event().await.unwrap(), 9);
            // Invalid authenticated audio is ignored by the lossy lane rather
            // than causing a ControllerError and reconnecting the video.
            host.send_audio_datagram(b"invalid ciphertext".to_vec())
                .await
                .unwrap();
            host.send_audio_datagram(
                secure
                    .seal(SecureLane::AudioDatagram, b"invalid audio")
                    .unwrap(),
            )
            .await
            .unwrap();
            let bytes = encode_audio_packet(&packet(2)).unwrap();
            host.send_audio_datagram(secure.seal(SecureLane::AudioDatagram, &bytes).unwrap())
                .await
                .unwrap();
            state.observe(&runtime.next_event().await.unwrap(), 9);
            assert_eq!(
                events.lock().unwrap().last().unwrap().2,
                json!({"bytes":960,"sequence":2,"sample_rate":48000,"channels":1})
            );
            // The Windows host does not repeat AudioState for a no-op true
            // request. A local player retry can use the already authenticated
            // matching state instead of timing out waiting for another ack.
            state
                .execute(
                    &test_io(&runtime),
                    scope,
                    SessionAction::SetAudioEnabled(true),
                )
                .await
                .unwrap();
            let bytes = secure
                .open(SecureLane::Control, &host.next_control().await.unwrap())
                .unwrap();
            assert_eq!(
                decode_control(&bytes).unwrap(),
                ControlMessage::SetAudioEnabled { enabled: true }
            );
            assert!(state.audio_change.is_none());
            assert_eq!(
                events.lock().unwrap().last().unwrap().2,
                json!({"kind":"audio_state","available":true,"enabled":true})
            );
            state
                .execute(
                    &test_io(&runtime),
                    scope,
                    SessionAction::SetAudioEnabled(false),
                )
                .await
                .unwrap();
            let bytes = encode_audio_packet(&packet(3)).unwrap();
            host.send_audio_datagram(secure.seal(SecureLane::AudioDatagram, &bytes).unwrap())
                .await
                .unwrap();
            state.observe(&runtime.next_event().await.unwrap(), 9);
            assert_eq!(
                events
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|e| e.0 == DesklinkEventKind::AudioPcm)
                    .count(),
                1,
                "mute stops before the host acknowledgement arrives"
            );
            let plaintext = secure
                .open(SecureLane::Control, &host.next_control().await.unwrap())
                .unwrap();
            assert_eq!(
                decode_control(&plaintext).unwrap(),
                ControlMessage::SetAudioEnabled { enabled: false }
            );
            relay_task.abort();
        })
        .await
        .expect("audio probe must finish");
    }

    async fn encrypted_actions_probe() {
        use desklink_crypto::SecureLane;
        use desklink_protocol::{
            RemoteDisplay, VideoQualityPreset, decode_control, decode_input, decode_transfer,
            encode_control, encode_transfer,
        };
        let (mut runtime, host, mut secure, relay_task) = encrypted_runtime().await;
        let events = Events::default();
        let mut state = state(&events);
        let config = runtime.next_event().await.unwrap();
        assert!(matches!(config, ControllerEvent::VideoConfig(_)));
        state.observe(&config, 9);
        let scope = state.gate.scope(9).unwrap();

        state
            .execute(
                &test_io(&runtime),
                scope,
                SessionAction::Text("中文😀\n\t".to_owned()),
            )
            .await
            .unwrap();
        for code in [
            KeyCode::Character('中'),
            KeyCode::Character('文'),
            KeyCode::Character('😀'),
            KeyCode::Enter,
            KeyCode::Tab,
        ] {
            for pressed in [true, false] {
                let ciphertext = host.next_input().await.unwrap();
                let plaintext = secure.open(SecureLane::Input, &ciphertext).unwrap();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_micros() as u64;
                assert_eq!(
                    decode_input(&plaintext, now).unwrap().event,
                    InputEvent::Key {
                        code,
                        pressed,
                        modifiers: Modifiers(0)
                    }
                );
            }
        }
        state
            .execute(
                &test_io(&runtime),
                scope,
                SessionAction::SetVideoQuality(VideoQualityPreference::Sharp),
            )
            .await
            .unwrap();
        let plaintext = secure
            .open(SecureLane::Control, &host.next_control().await.unwrap())
            .unwrap();
        assert_eq!(
            decode_control(&plaintext).unwrap(),
            ControlMessage::SetVideoQuality {
                preference: VideoQualityPreference::Sharp
            }
        );
        let plaintext = encode_control(&ControlMessage::VideoQualityState {
            preference: VideoQualityPreference::Sharp,
            preset: VideoQualityPreset::Balanced,
        })
        .unwrap();
        host.send_control(secure.seal(SecureLane::Control, &plaintext).unwrap())
            .await
            .unwrap();
        state.observe(&runtime.next_event().await.unwrap(), 9);
        assert!(state.quality_change.is_none());
        assert_eq!(
            events.lock().unwrap().last().unwrap().2["applied_preset"],
            "balanced",
            "actual preset must come from the host, not the requested preference"
        );

        state
            .execute(
                &test_io(&runtime),
                scope,
                SessionAction::SendClipboard {
                    request_id: 41,
                    text: "双向😀\r\n".into(),
                },
            )
            .await
            .unwrap();
        let plaintext = secure
            .open(SecureLane::Transfer, &host.next_transfer().await.unwrap())
            .unwrap();
        assert_eq!(
            decode_transfer(&plaintext).unwrap(),
            TransferMessage::ClipboardSet {
                request_id: 41,
                text: "双向😀\r\n".into()
            }
        );
        let plaintext = encode_transfer(&TransferMessage::ClipboardResult {
            request_id: 41,
            result: TransferResult::Completed,
        })
        .unwrap();
        host.send_transfer(secure.seal(SecureLane::Transfer, &plaintext).unwrap())
            .await
            .unwrap();
        state.observe(&runtime.next_event().await.unwrap(), 9);
        state
            .execute(
                &test_io(&runtime),
                scope,
                SessionAction::RequestClipboard { request_id: 42 },
            )
            .await
            .unwrap();
        let plaintext = secure
            .open(SecureLane::Transfer, &host.next_transfer().await.unwrap())
            .unwrap();
        assert_eq!(
            decode_transfer(&plaintext).unwrap(),
            TransferMessage::ClipboardRequest { request_id: 42 }
        );
        let plaintext = encode_transfer(&TransferMessage::ClipboardData {
            request_id: 42,
            text: "来自Windows\n".into(),
        })
        .unwrap();
        host.send_transfer(secure.seal(SecureLane::Transfer, &plaintext).unwrap())
            .await
            .unwrap();
        state.observe(&runtime.next_event().await.unwrap(), 9);
        assert_eq!(
            events.lock().unwrap().last().unwrap().2["text"],
            "来自Windows\n"
        );
        // Reusing an id cannot let a delayed response overwrite a newer paste.
        state
            .execute(
                &test_io(&runtime),
                scope,
                SessionAction::RequestClipboard { request_id: 42 },
            )
            .await
            .unwrap();
        assert_eq!(events.lock().unwrap().last().unwrap().2["result"], "busy");
        assert!(
            tokio::time::timeout(Duration::from_millis(30), host.next_transfer())
                .await
                .is_err()
        );

        state
            .execute(&test_io(&runtime), scope, SessionAction::SelectDisplay(2))
            .await
            .unwrap();
        let plaintext = secure
            .open(SecureLane::Control, &host.next_control().await.unwrap())
            .unwrap();
        assert_eq!(
            decode_control(&plaintext).unwrap(),
            ControlMessage::SelectDisplay { display_id: 2 }
        );
        assert!(state.gate.scope(9).is_none());
        // Old commands are dropped before the encrypted input lane is touched.
        state
            .execute(
                &test_io(&runtime),
                scope,
                SessionAction::Input(InputEvent::MouseMove { x: 1, y: 2 }),
            )
            .await
            .unwrap();
        let plaintext = encode_control(&ControlMessage::DisplayList {
            displays: vec![RemoteDisplay {
                id: 2,
                width: 1920,
                height: 1080,
                primary: false,
            }],
            active_display_id: 2,
        })
        .unwrap();
        host.send_control(secure.seal(SecureLane::Control, &plaintext).unwrap())
            .await
            .unwrap();
        state.observe(&runtime.next_event().await.unwrap(), 9);
        assert!(
            state.gate.scope(9).is_some(),
            "a real ack can restore a reused stream id"
        );
        assert!(!state.gate.accepts(scope));
        state
            .execute(
                &test_io(&runtime),
                scope,
                SessionAction::Text("不得发往新屏幕".into()),
            )
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), host.next_input())
                .await
                .is_err()
        );
        let new_scope = state.gate.scope(9).unwrap();
        state
            .execute(
                &test_io(&runtime),
                new_scope,
                SessionAction::Input(InputEvent::MouseMove { x: 3, y: 4 }),
            )
            .await
            .unwrap();
        let plaintext = secure
            .open(SecureLane::Input, &host.next_input().await.unwrap())
            .unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        assert_eq!(
            decode_input(&plaintext, now).unwrap().event,
            InputEvent::MouseMove { x: 3, y: 4 }
        );
        relay_task.abort();
    }

    pub(crate) async fn encrypted_runtime() -> (
        ControllerRuntime,
        desklink_transport::QuicClient,
        desklink_crypto::SecureSession,
        tokio::task::JoinHandle<()>,
    ) {
        use desklink_crypto::{DeviceIdentity, NoiseResponder, SecureLane, SecureRole, SessionId};
        use desklink_protocol::{
            Codec, DeviceCapabilities, DeviceRole, H264Profile, NoiseHandshake, NoiseHandshakeStep,
            PROTOCOL_VERSION, Platform, VideoConfig, decode_noise_handshake, encode_control,
            encode_noise_handshake, encode_video_config,
        };
        use desklink_transport::{QuicClient, QuicClientConfig, RelayJoin};
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};
        let _ = rustls::crypto::ring::default_provider().install_default();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let certificate_der = certificate.cert.der().to_vec();
        let server_config = quinn::ServerConfig::with_single_cert(
            vec![CertificateDer::from(certificate_der.clone())],
            PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into()),
        )
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(certificate_der)).unwrap();
        let tls = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).unwrap(),
        ));
        let relay = desklink_relay::RelayServer::bind(
            "127.0.0.1:0".parse().unwrap(),
            server_config,
            desklink_relay::RelayConfig::default(),
        )
        .await
        .unwrap();
        let address = relay.local_addr().unwrap();
        let relay_task = tokio::spawn(async move {
            let _ = relay.run().await;
        });
        let config =
            || QuicClientConfig::with_client_config(address, "localhost", client_config.clone());
        let host = QuicClient::connect(config()).await.unwrap();
        let client = QuicClient::connect(config()).await.unwrap();
        let session_id = SessionId::from_bytes([201; 16]);
        host.join(RelayJoin::host_with_participant(
            session_id, [202; 32], [1; 16],
        ))
        .await
        .unwrap();
        client
            .join(RelayJoin::controller_with_participant(
                session_id, [202; 32], [2; 16],
            ))
            .await
            .unwrap();
        let host_identity = DeviceIdentity::from_secret_key([203; 16], &[204; 32]);
        let client_identity = DeviceIdentity::from_secret_key([205; 16], &[206; 32]);
        let host_key = host_identity.verify_key();
        let client_key = client_identity.verify_key();
        let host_task = tokio::spawn(async move {
            let first = decode_noise_handshake(&host.next_control().await.unwrap()).unwrap();
            let (mut responder, response) =
                NoiseResponder::accept(&first.payload, host_identity, client_key).unwrap();
            host.send_control(
                encode_noise_handshake(&NoiseHandshake {
                    protocol_version: PROTOCOL_VERSION,
                    step: NoiseHandshakeStep::ResponderHello,
                    payload: response,
                })
                .unwrap(),
            )
            .await
            .unwrap();
            let finish = decode_noise_handshake(&host.next_control().await.unwrap()).unwrap();
            responder.receive(&finish.payload).unwrap();
            let mut secure = responder
                .finish()
                .unwrap()
                .into_secure_session(SecureRole::Responder);
            for _ in 0..2 {
                secure
                    .open(SecureLane::Control, &host.next_control().await.unwrap())
                    .unwrap();
            }
            for message in [
                ControlMessage::Hello {
                    platform: Platform::Windows,
                    role: DeviceRole::Host,
                },
                ControlMessage::Capabilities(DeviceCapabilities {
                    platform: Platform::Windows,
                    role: DeviceRole::Host,
                    codecs: vec![Codec::H264],
                    h264_profiles: vec![H264Profile::Main],
                    width: 1280,
                    height: 720,
                    lan_data: None,
                }),
            ] {
                let plaintext = encode_control(&message).unwrap();
                host.send_control(secure.seal(SecureLane::Control, &plaintext).unwrap())
                    .await
                    .unwrap();
            }
            let config = encode_video_config(&VideoConfig {
                protocol_version: PROTOCOL_VERSION,
                stream_id: 9,
                input_epoch: 1,
                config_version: 3,
                codec: Codec::H264,
                width: 1280,
                height: 720,
                sequence_header: vec![
                    0, 0, 0, 1, 0x67, 0x64, 0, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80,
                ],
            })
            .unwrap();
            host.send_video_config(secure.seal(SecureLane::VideoConfig, &config).unwrap())
                .await
                .unwrap();
            (host, secure)
        });
        let runtime = ControllerRuntime::connect(client, client_identity, host_key)
            .await
            .unwrap();
        let (host, secure) = host_task.await.unwrap();
        (runtime, host, secure, relay_task)
    }
}
