use std::{
    ffi::{CStr, c_char, c_void},
    net::{SocketAddr, ToSocketAddrs},
    ptr::{null, null_mut},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

use desklink_crypto::{
    DeviceIdentity, MAX_PAIRING_TTL_S, PAIRING_INVITE_BYTES, PairingInvite, PairingOffer, SessionId,
};
use desklink_protocol::{
    ControlMessage, InputEnvelope, InputEvent, KeyCode, MAX_CURSOR_MESSAGE_BYTES, MAX_MVP_HEIGHT,
    MAX_MVP_WIDTH, MAX_VIDEO_CHUNKS, MAX_VIDEO_CONFIG_BYTES, MAX_VIDEO_PACKET_PAYLOAD_BYTES,
    MAX_WHEEL_DELTA, Modifiers, MouseButton, encode_control, encode_input,
};
use desklink_session::{
    InputSequencer, PressedInputState, SessionEvent, SessionMachine, SessionState,
};
use rand_core::{OsRng, RngCore};
use zeroize::Zeroizing;

mod controller;
mod host;
mod host_worker;
mod worker;

pub use controller::{ControllerError, ControllerEvent, ControllerMetrics, ControllerRuntime};
pub use host::{
    HostCommand, HostError, HostEvent, HostIdentity, HostMetrics, HostRuntime, HostState,
};
use worker::{ControllerCommand, ControllerWorker, SecureConnectionConfigOwned};

pub const PACKAGE_NAME: &str = "desklink-ffi";
const PAIRING_CODE_BYTES: usize = 8;

#[repr(i32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesklinkResult {
    Ok = 0,
    InvalidArgument = 1,
    InvalidUtf8 = 2,
    InvalidState = 3,
    InternalError = 4,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesklinkState {
    Idle = 0,
    CreatingSession = 1,
    ConnectingRelay = 2,
    SecureHandshake = 3,
    WaitingForApproval = 4,
    NegotiatingCapabilities = 5,
    StartingVideo = 6,
    Connected = 7,
    Degraded = 8,
    RecoveringVideo = 9,
    Reconnecting = 10,
    Disconnecting = 11,
    Closed = 12,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesklinkEventKind {
    State = 1,
    Error = 2,
    Pairing = 3,
    Control = 4,
    Input = 5,
    VideoConfig = 6,
    H264AccessUnit = 7,
    Cursor = 8,
    Metrics = 9,
    ReleaseAll = 10,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesklinkHostEventKind {
    State = 1,
    Error = 2,
    ApprovalRequested = 3,
    Input = 4,
    KeyframeRequested = 5,
    ReleaseAll = 6,
    Metrics = 7,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesklinkHostState {
    Connecting = 1,
    WaitingForApproval = 2,
    NegotiatingCapabilities = 3,
    Connected = 4,
    Stopping = 5,
    Closed = 6,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesklinkInputKind {
    MouseMove = 1,
    MouseButton = 2,
    Key = 3,
    MouseWheel = 4,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DesklinkConfig {
    pub relay_url: *const c_char,
    pub log_level: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DesklinkHostConfig {
    pub relay_url: *const c_char,
    pub server_name: *const c_char,
    pub host_device_id: [u8; 16],
    pub host_secret_key: [u8; 32],
    pub log_level: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DesklinkSecureConnectionConfig {
    pub server_name: *const c_char,
    pub session_id: [u8; 16],
    pub relay_authentication: [u8; 32],
    pub controller_device_id: [u8; 16],
    pub controller_secret_key: [u8; 32],
    pub host_verify_key: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DesklinkPairingInviteConnectionConfig {
    pub server_name: *const c_char,
    pub invite: *const u8,
    pub invite_len: usize,
    pub controller_device_id: [u8; 16],
    pub controller_secret_key: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DesklinkPairingInfo {
    pub session_id: [u8; 16],
    pub code: [c_char; PAIRING_CODE_BYTES + 1],
    pub expires_at_unix_s: u64,
}

impl Default for DesklinkPairingInfo {
    fn default() -> Self {
        Self {
            session_id: [0; 16],
            code: [0; PAIRING_CODE_BYTES + 1],
            expires_at_unix_s: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DesklinkInput {
    pub kind: DesklinkInputKind,
    pub x: f32,
    pub y: f32,
    pub wheel_x: i32,
    pub wheel_y: i32,
    pub button: u32,
    pub key_code: u32,
    pub character: u32,
    pub pressed: u8,
    pub modifiers: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DesklinkEvent {
    pub kind: DesklinkEventKind,
    pub data: *const u8,
    pub data_len: usize,
    pub stream_id: u64,
    pub frame_id: u64,
    pub config_version: u32,
    pub width: u16,
    pub height: u16,
    pub state: DesklinkState,
}

pub type DesklinkEventCallback = extern "C" fn(*mut c_void, *const DesklinkEvent);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DesklinkHostInput {
    pub kind: DesklinkInputKind,
    pub x: f32,
    pub y: f32,
    pub wheel_x: i32,
    pub wheel_y: i32,
    pub button: u32,
    pub key_code: u32,
    pub character: u32,
    pub pressed: u8,
    pub modifiers: u8,
}

impl Default for DesklinkHostInput {
    fn default() -> Self {
        Self {
            kind: DesklinkInputKind::MouseMove,
            x: 0.0,
            y: 0.0,
            wheel_x: 0,
            wheel_y: 0,
            button: 0,
            key_code: 0,
            character: 0,
            pressed: 0,
            modifiers: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DesklinkHostMetrics {
    pub sent_video_configs: u64,
    pub sent_video_packets: u64,
    pub received_input_events: u64,
    pub keyframe_requests: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DesklinkHostEvent {
    pub kind: DesklinkHostEventKind,
    pub state: DesklinkHostState,
    pub data: *const u8,
    pub data_len: usize,
    pub controller_device_id: [u8; 16],
    pub controller_verify_key: [u8; 32],
    pub fingerprint: *const u8,
    pub fingerprint_len: usize,
    pub input: DesklinkHostInput,
    pub metrics: DesklinkHostMetrics,
}

pub type DesklinkHostEventCallback = extern "C" fn(*mut c_void, *const DesklinkHostEvent);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DesklinkSavedHostMaterial {
    pub session_id: [u8; 16],
    pub relay_authentication: [u8; 32],
    pub host_verify_key: [u8; 32],
    pub server_name: [c_char; 256],
}

pub struct DesklinkHandle {
    runtime: DesklinkRuntime,
}

pub struct DesklinkHostHandle {
    config: DesklinkHostConfigOwned,
    runtime: Mutex<Option<Arc<HostRuntime>>>,
    event_thread: Mutex<Option<JoinHandle<()>>>,
    callback: Arc<HostCallbackState>,
}

struct DesklinkHostConfigOwned {
    relay_url: String,
    server_name: String,
    host_device_id: [u8; 16],
    host_secret_key: Zeroizing<[u8; 32]>,
    _log_level: u32,
}

struct SavedHostMaterialOwned {
    session_id: [u8; 16],
    relay_authentication: Zeroizing<[u8; 32]>,
    host_verify_key: [u8; 32],
    server_name: String,
}

struct HostCallbackState {
    callback: Mutex<Option<(DesklinkHostEventCallback, usize)>>,
}

impl HostCallbackState {
    fn new(callback: Option<DesklinkHostEventCallback>, context: *mut c_void) -> Self {
        Self {
            callback: Mutex::new(callback.map(|callback| (callback, context as usize))),
        }
    }

    fn clear(&self) {
        if let Ok(mut callback) = self.callback.lock() {
            *callback = None;
        }
    }

    fn emit(&self, event: HostEvent, state: HostState) {
        let Ok(callback) = self.callback.lock().map(|callback| *callback) else {
            return;
        };
        let Some((callback, context)) = callback else {
            return;
        };

        let mut data: Option<Vec<u8>> = None;
        let mut fingerprint: Option<Vec<u8>> = None;
        let mut c_event = DesklinkHostEvent {
            kind: DesklinkHostEventKind::State,
            state: map_host_state(state),
            data: null(),
            data_len: 0,
            controller_device_id: [0; 16],
            controller_verify_key: [0; 32],
            fingerprint: null(),
            fingerprint_len: 0,
            input: DesklinkHostInput::default(),
            metrics: DesklinkHostMetrics::default(),
        };
        match event {
            HostEvent::State(host_state) => {
                c_event.kind = DesklinkHostEventKind::State;
                c_event.state = map_host_state(host_state);
            }
            HostEvent::Error(error) => {
                data = Some(error.to_string().into_bytes());
                c_event.kind = DesklinkHostEventKind::Error;
            }
            HostEvent::ApprovalRequested {
                device_id,
                verify_key,
                fingerprint: value,
            } => {
                fingerprint = Some(value.into_bytes());
                c_event.kind = DesklinkHostEventKind::ApprovalRequested;
                c_event.controller_device_id = device_id;
                c_event.controller_verify_key = verify_key;
            }
            HostEvent::Input(input) => {
                c_event.kind = DesklinkHostEventKind::Input;
                c_event.input = convert_host_input(input);
            }
            HostEvent::KeyframeRequested => {
                c_event.kind = DesklinkHostEventKind::KeyframeRequested;
            }
            HostEvent::ReleaseAll => {
                c_event.kind = DesklinkHostEventKind::ReleaseAll;
            }
            HostEvent::Metrics(metrics) => {
                c_event.kind = DesklinkHostEventKind::Metrics;
                c_event.metrics = DesklinkHostMetrics {
                    sent_video_configs: metrics.sent_video_configs,
                    sent_video_packets: metrics.sent_video_packets,
                    received_input_events: metrics.received_input_events,
                    keyframe_requests: metrics.keyframe_requests,
                };
            }
        }
        if let Some(data) = data.as_deref() {
            c_event.data = data.as_ptr();
            c_event.data_len = data.len();
        }
        if let Some(fingerprint) = fingerprint.as_deref() {
            c_event.fingerprint = fingerprint.as_ptr();
            c_event.fingerprint_len = fingerprint.len();
        }
        callback(context as *mut c_void, &c_event);
    }
}

#[derive(Clone, Copy)]
pub(crate) struct EventMeta {
    stream_id: u64,
    frame_id: u64,
    config_version: u32,
    width: u16,
    height: u16,
}

impl EventMeta {
    pub(crate) const fn for_stream(stream_id: u64) -> Self {
        Self {
            stream_id,
            frame_id: 0,
            config_version: 0,
            width: 0,
            height: 0,
        }
    }
}

struct DesklinkRuntime {
    relay_url: String,
    _log_level: u32,
    callback: Option<DesklinkEventCallback>,
    callback_context: *mut c_void,
    session: SessionMachine,
    pairing: Option<PairingOffer>,
    pressed: PressedInputState,
    input_sequence: InputSequencer,
    stream_id: u64,
    closed: bool,
    worker: Option<ControllerWorker>,
    saved_host_material: Option<SavedHostMaterialOwned>,
}

impl DesklinkRuntime {
    fn state(&self) -> DesklinkState {
        map_state(self.session.state())
    }

    fn emit(&self, kind: DesklinkEventKind, data: &[u8], meta: EventMeta) {
        let Some(callback) = self.callback else {
            return;
        };
        let event = DesklinkEvent {
            kind,
            data: if data.is_empty() {
                null()
            } else {
                data.as_ptr()
            },
            data_len: data.len(),
            stream_id: meta.stream_id,
            frame_id: meta.frame_id,
            config_version: meta.config_version,
            width: meta.width,
            height: meta.height,
            state: self.state(),
        };
        callback(self.callback_context, &event);
    }

    fn emit_state(&self) {
        self.emit(
            DesklinkEventKind::State,
            &[],
            EventMeta {
                stream_id: self.stream_id,
                frame_id: 0,
                config_version: 0,
                width: 0,
                height: 0,
            },
        );
    }

    fn emit_error(&self, message: &str) {
        self.emit(
            DesklinkEventKind::Error,
            message.as_bytes(),
            EventMeta {
                stream_id: self.stream_id,
                frame_id: 0,
                config_version: 0,
                width: 0,
                height: 0,
            },
        );
    }

    fn advance(&mut self, event: SessionEvent) -> Result<(), DesklinkResult> {
        let actions = self.session.apply(event).map_err(|error| {
            self.emit_error(&error.to_string());
            DesklinkResult::InvalidState
        })?;
        for action in actions {
            if let desklink_session::SessionAction::BeginStream { stream_id } = action {
                self.stream_id = stream_id;
            }
        }
        self.emit_state();
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), DesklinkResult> {
        if self.closed || self.session.state() == SessionState::Closed {
            Err(DesklinkResult::InvalidState)
        } else {
            Ok(())
        }
    }

    fn release_all(&mut self) {
        let events = self.pressed.release_all();
        for event in events {
            let _ = self.dispatch_input(event);
        }
        self.emit(
            DesklinkEventKind::ReleaseAll,
            &[],
            EventMeta {
                stream_id: self.stream_id,
                frame_id: 0,
                config_version: 0,
                width: 0,
                height: 0,
            },
        );
    }

    fn emit_input(&mut self, event: InputEvent) {
        let envelope = InputEnvelope {
            sequence: self.input_sequence.next_sequence(),
            timestamp_us: now_micros(),
            event,
        };
        if let Ok(bytes) = encode_input(&envelope) {
            self.emit(
                DesklinkEventKind::Input,
                &bytes,
                EventMeta {
                    stream_id: self.stream_id,
                    frame_id: 0,
                    config_version: 0,
                    width: 0,
                    height: 0,
                },
            );
        }
    }

    fn dispatch_input(&mut self, event: InputEvent) -> Result<(), DesklinkResult> {
        if let Some(worker) = &self.worker {
            worker
                .send(ControllerCommand::SendInput(event))
                .map_err(|_| DesklinkResult::InternalError)
        } else {
            self.emit_input(event);
            Ok(())
        }
    }

    fn stop_worker(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.shutdown();
        }
    }
}

/// Creates an opaque DeskLink runtime handle.
///
/// # Safety
/// `config` and `out_handle` must be valid writable pointers when non-null,
/// and the callback must remain callable for the lifetime of the handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_create(
    config: *const DesklinkConfig,
    callback: Option<DesklinkEventCallback>,
    callback_context: *mut c_void,
    out_handle: *mut *mut DesklinkHandle,
) -> DesklinkResult {
    if out_handle.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    unsafe { *out_handle = null_mut() };
    if config.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    let config = unsafe { &*config };
    if config.relay_url.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    let relay_url = match unsafe { CStr::from_ptr(config.relay_url) }.to_str() {
        Ok(url) if !url.is_empty() => url.to_owned(),
        Ok(_) => return DesklinkResult::InvalidArgument,
        Err(_) => return DesklinkResult::InvalidUtf8,
    };
    let runtime = DesklinkRuntime {
        relay_url,
        _log_level: config.log_level,
        callback,
        callback_context,
        session: SessionMachine::new(desklink_protocol::DeviceRole::Controller),
        pairing: None,
        pressed: PressedInputState::default(),
        input_sequence: InputSequencer::new(),
        stream_id: 0,
        closed: false,
        worker: None,
        saved_host_material: None,
    };
    unsafe { *out_handle = Box::into_raw(Box::new(DesklinkHandle { runtime })) };
    DesklinkResult::Ok
}

/// Derives the public Ed25519 verification key for a 32-byte secret key.
///
/// # Safety
/// `secret_key` must point to 32 readable bytes and `out_verify_key` must
/// point to 32 writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_identity_verify_key(
    secret_key: *const u8,
    out_verify_key: *mut u8,
) -> DesklinkResult {
    if secret_key.is_null() || out_verify_key.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    let secret: [u8; 32] = match unsafe { std::slice::from_raw_parts(secret_key, 32) }.try_into() {
        Ok(secret) => secret,
        Err(_) => return DesklinkResult::InvalidArgument,
    };
    let identity = DeviceIdentity::from_secret_key([0; 16], &secret);
    unsafe {
        std::ptr::copy_nonoverlapping(
            identity.verify_key().as_bytes().as_ptr(),
            out_verify_key,
            32,
        );
    }
    DesklinkResult::Ok
}

/// Creates a temporary pairing code and writes it to `out_pairing`.
///
/// # Safety
/// `handle` must be a live handle returned by `desklink_create`, and
/// `out_pairing` must point to writable storage for one `DesklinkPairingInfo`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_start_pairing(
    handle: *mut DesklinkHandle,
    out_pairing: *mut DesklinkPairingInfo,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if out_pairing.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    if let Err(error) = runtime.ensure_active() {
        return error;
    }
    let mut session_bytes = [0; 16];
    OsRng.fill_bytes(&mut session_bytes);
    let session_id = SessionId::from_bytes(session_bytes);
    let offer = match PairingOffer::new(session_id, now_unix_s(), MAX_PAIRING_TTL_S) {
        Ok(offer) => offer,
        Err(_) => return DesklinkResult::InternalError,
    };
    let code = offer.code().to_string();
    let mut info = DesklinkPairingInfo {
        session_id: session_bytes,
        ..DesklinkPairingInfo::default()
    };
    for (destination, source) in info.code.iter_mut().zip(code.bytes()) {
        *destination = source as c_char;
    }
    info.expires_at_unix_s = offer.expires_at_unix_s();
    unsafe { *out_pairing = info };
    runtime.pairing = Some(offer);
    runtime.emit(
        DesklinkEventKind::Pairing,
        &[],
        EventMeta {
            stream_id: runtime.stream_id,
            frame_id: 0,
            config_version: 0,
            width: 0,
            height: 0,
        },
    );
    DesklinkResult::Ok
}

/// Consumes a valid temporary pairing code.
///
/// # Safety
/// `handle` must be a live handle returned by `desklink_create`, and `code`
/// must be a valid NUL-terminated UTF-8 string when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_connect_with_code(
    handle: *mut DesklinkHandle,
    code: *const c_char,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if code.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    if let Err(error) = runtime.ensure_active() {
        return error;
    }
    let code = match unsafe { CStr::from_ptr(code) }.to_str() {
        Ok(code) => code,
        Err(_) => return DesklinkResult::InvalidUtf8,
    };
    let Some(offer) = runtime.pairing.as_mut() else {
        return DesklinkResult::InvalidState;
    };
    if offer.consume_code(code, now_unix_s()).is_err() {
        return DesklinkResult::InvalidArgument;
    }
    if runtime.advance(SessionEvent::RelayConnected).is_err()
        || runtime.advance(SessionEvent::HandshakeComplete).is_err()
        || runtime
            .advance(SessionEvent::CapabilitiesNegotiated)
            .is_err()
    {
        return DesklinkResult::InvalidState;
    }
    DesklinkResult::Ok
}

/// Starts the real QUIC/Noise controller runtime on a cancellable background thread.
///
/// # Safety
/// `handle` must be a live handle returned by `desklink_create`. `config`
/// must point to readable storage, and `server_name` must be a valid
/// NUL-terminated UTF-8 string for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_connect_secure(
    handle: *mut DesklinkHandle,
    config: *const DesklinkSecureConnectionConfig,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if config.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    if runtime.ensure_active().is_err() {
        return DesklinkResult::InvalidState;
    }
    if runtime
        .worker
        .as_ref()
        .is_some_and(|worker| !worker.is_finished())
    {
        return DesklinkResult::InvalidState;
    }
    runtime.stop_worker();
    let config = unsafe { &*config };
    if config.server_name.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    let server_name = match unsafe { CStr::from_ptr(config.server_name) }.to_str() {
        Ok(server_name) if !server_name.is_empty() => server_name.to_owned(),
        Ok(_) => return DesklinkResult::InvalidArgument,
        Err(_) => return DesklinkResult::InvalidUtf8,
    };
    if ed25519_dalek::VerifyingKey::from_bytes(&config.host_verify_key).is_err() {
        return DesklinkResult::InvalidArgument;
    }
    let saved = SavedHostMaterialOwned {
        session_id: config.session_id,
        relay_authentication: Zeroizing::new(config.relay_authentication),
        host_verify_key: config.host_verify_key,
        server_name: server_name.clone(),
    };
    let result = start_secure_worker(
        runtime,
        SecureConnectionConfigOwned {
            server_name,
            session_id: config.session_id,
            relay_authentication: config.relay_authentication,
            controller_device_id: config.controller_device_id,
            controller_secret_key: config.controller_secret_key,
            host_verify_key: config.host_verify_key,
            expires_at_unix_s: None,
        },
    );
    if result == DesklinkResult::Ok {
        runtime.saved_host_material = Some(saved);
    }
    result
}

/// Verifies a signed pairing invitation and starts the real controller runtime.
///
/// # Safety
/// `handle` must be a live handle returned by `desklink_create`. `config` must
/// point to readable storage, `server_name` must be a valid NUL-terminated
/// UTF-8 string, and `invite` must point to `invite_len` readable bytes for the
/// duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_connect_pairing_invite(
    handle: *mut DesklinkHandle,
    config: *const DesklinkPairingInviteConnectionConfig,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if config.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    if runtime.ensure_active().is_err() {
        return DesklinkResult::InvalidState;
    }
    if runtime
        .worker
        .as_ref()
        .is_some_and(|worker| !worker.is_finished())
    {
        return DesklinkResult::InvalidState;
    }
    runtime.stop_worker();
    let config = unsafe { &*config };
    if config.server_name.is_null()
        || config.invite.is_null()
        || config.invite_len != PAIRING_INVITE_BYTES
    {
        return DesklinkResult::InvalidArgument;
    }
    let server_name = match unsafe { CStr::from_ptr(config.server_name) }.to_str() {
        Ok(server_name) if !server_name.is_empty() => server_name.to_owned(),
        Ok(_) => return DesklinkResult::InvalidArgument,
        Err(_) => return DesklinkResult::InvalidUtf8,
    };
    let invite_bytes = unsafe { std::slice::from_raw_parts(config.invite, config.invite_len) };
    let invite = match PairingInvite::decode(invite_bytes, now_unix_s()) {
        Ok(invite) => invite,
        Err(_) => return DesklinkResult::InvalidArgument,
    };
    let session_id = *invite.session_id().as_bytes();
    let relay_authentication = *invite.relay_authentication();
    let host_verify_key = *invite.host_verify_key().as_bytes();
    let saved = SavedHostMaterialOwned {
        session_id,
        relay_authentication: Zeroizing::new(relay_authentication),
        host_verify_key,
        server_name: server_name.clone(),
    };
    let result = start_secure_worker(
        runtime,
        SecureConnectionConfigOwned {
            server_name,
            session_id,
            relay_authentication,
            controller_device_id: config.controller_device_id,
            controller_secret_key: config.controller_secret_key,
            host_verify_key,
            expires_at_unix_s: Some(invite.expires_at_unix_s()),
        },
    );
    if result == DesklinkResult::Ok {
        runtime.saved_host_material = Some(saved);
    }
    result
}

fn start_secure_worker(
    runtime: &mut DesklinkRuntime,
    config: SecureConnectionConfigOwned,
) -> DesklinkResult {
    let worker = match ControllerWorker::start(
        runtime.relay_url.clone(),
        config,
        runtime.callback,
        runtime.callback_context,
    ) {
        Ok(worker) => worker,
        Err(error) => {
            runtime.emit_error(&error.to_string());
            return DesklinkResult::InternalError;
        }
    };
    runtime.worker = Some(worker);
    DesklinkResult::Ok
}

/// Accepts a pending host approval.
///
/// # Safety
/// `handle` must be a live handle returned by `desklink_create`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_accept(handle: *mut DesklinkHandle) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if let Err(error) = runtime.ensure_active() {
        return error;
    }
    runtime
        .advance(SessionEvent::HostAccepted)
        .map_or_else(|error| error, |_| DesklinkResult::Ok)
}

/// Rejects the current session and releases all pressed input.
///
/// # Safety
/// `handle` must be a live handle returned by `desklink_create`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_reject(handle: *mut DesklinkHandle) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if runtime.closed {
        return DesklinkResult::InvalidState;
    }
    runtime.release_all();
    runtime.stop_worker();
    let _ = runtime.advance(SessionEvent::UserDisconnected);
    runtime.closed = true;
    DesklinkResult::Ok
}

/// Sends one normalized pointer, mouse-button, wheel, or keyboard input event.
///
/// # Safety
/// `handle` must be a live handle returned by `desklink_create`, and `input`
/// must point to readable `DesklinkInput` storage when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_send_input(
    handle: *mut DesklinkHandle,
    input: *const DesklinkInput,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if input.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    if let Err(error) = runtime.ensure_active() {
        return error;
    }
    let input = unsafe { &*input };
    let event = match convert_input(input) {
        Ok(event) => event,
        Err(error) => return error,
    };
    runtime.pressed.press(&event);
    if matches!(
        &event,
        InputEvent::MouseButton { pressed: false, .. } | InputEvent::Key { pressed: false, .. }
    ) {
        runtime.pressed.release(&event);
    }
    runtime
        .dispatch_input(event)
        .map_or_else(|error| error, |_| DesklinkResult::Ok)
}

/// Requests a keyframe for the active video stream.
///
/// # Safety
/// `handle` must be a live handle returned by `desklink_create`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_request_keyframe(handle: *mut DesklinkHandle) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if let Err(error) = runtime.ensure_active() {
        return error;
    }
    if let Some(worker) = &runtime.worker {
        return worker
            .send(ControllerCommand::RequestKeyframe)
            .map_or(DesklinkResult::InternalError, |_| DesklinkResult::Ok);
    }
    if runtime.stream_id == 0 {
        return DesklinkResult::InvalidState;
    }
    let message = ControlMessage::RequestKeyframe {
        stream_id: runtime.stream_id,
    };
    let Ok(bytes) = encode_control(&message) else {
        return DesklinkResult::InternalError;
    };
    runtime.emit(
        DesklinkEventKind::Control,
        &bytes,
        EventMeta {
            stream_id: runtime.stream_id,
            frame_id: 0,
            config_version: 0,
            width: 0,
            height: 0,
        },
    );
    DesklinkResult::Ok
}

/// Releases all currently pressed input without closing the session.
///
/// # Safety
/// `handle` must be a live handle returned by `desklink_create`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_release_all(handle: *mut DesklinkHandle) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if let Err(error) = runtime.ensure_active() {
        return error;
    }
    runtime.release_all();
    DesklinkResult::Ok
}

/// Destroys a handle and releases all pressed input.
///
/// # Safety
/// `handle` must be null or a live handle returned by `desklink_create`, and
/// must not be used again after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_destroy(handle: *mut DesklinkHandle) {
    if handle.is_null() {
        return;
    }
    let mut handle = unsafe { Box::from_raw(handle) };
    handle.runtime.release_all();
    handle.runtime.stop_worker();
}

/// Copies the validated controller connection material into caller-owned storage.
///
/// The returned buffer is intended for immediate Keychain staging by the macOS
/// controller. The Rust runtime never exposes the relay secret through a callback.
///
/// # Safety
/// `handle` must be null or a live controller handle, and `out_material` must
/// point to writable storage for one `DesklinkSavedHostMaterial`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_controller_copy_saved_host_material(
    handle: *mut DesklinkHandle,
    out_material: *mut DesklinkSavedHostMaterial,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if out_material.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    let Some(material) = runtime.saved_host_material.as_ref() else {
        return DesklinkResult::InvalidState;
    };
    let server_name = material.server_name.as_bytes();
    if server_name.len() >= 256 || server_name.contains(&0) {
        return DesklinkResult::InternalError;
    }
    let mut output = DesklinkSavedHostMaterial {
        session_id: material.session_id,
        relay_authentication: *material.relay_authentication,
        host_verify_key: material.host_verify_key,
        server_name: [0; 256],
    };
    for (destination, source) in output
        .server_name
        .iter_mut()
        .zip(server_name.iter().copied())
    {
        *destination = source as c_char;
    }
    unsafe { *out_material = output };
    DesklinkResult::Ok
}

/// Creates an opaque host handle. Network activity starts only after a pairing
/// invite is created or a valid invite is supplied.
///
/// # Safety
/// `config` and `out_handle` must be valid pointers when non-null. C strings in
/// `config` must remain valid for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_host_create(
    config: *const DesklinkHostConfig,
    callback: Option<DesklinkHostEventCallback>,
    callback_context: *mut c_void,
    out_handle: *mut *mut DesklinkHostHandle,
) -> DesklinkResult {
    if out_handle.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    unsafe { *out_handle = null_mut() };
    if config.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    let config = unsafe { &*config };
    let relay_url = match c_string(config.relay_url, false) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let server_name = match c_string(config.server_name, false) {
        Ok(value) if value.len() < 256 => value,
        Ok(_) => return DesklinkResult::InvalidArgument,
        Err(error) => return error,
    };
    let handle = DesklinkHostHandle {
        config: DesklinkHostConfigOwned {
            relay_url,
            server_name,
            host_device_id: config.host_device_id,
            host_secret_key: Zeroizing::new(config.host_secret_key),
            _log_level: config.log_level,
        },
        runtime: Mutex::new(None),
        event_thread: Mutex::new(None),
        callback: Arc::new(HostCallbackState::new(callback, callback_context)),
    };
    unsafe { *out_handle = Box::into_raw(Box::new(handle)) };
    DesklinkResult::Ok
}

/// Starts a host session using a newly generated, signed invitation.
///
/// # Safety
/// `handle` must be a live host handle. The three output pointers must refer to
/// writable storage, and `invite_out` must have `invite_capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_host_start_pairing(
    handle: *mut DesklinkHostHandle,
    invite_out: *mut u8,
    invite_capacity: usize,
    invite_len_out: *mut usize,
    expires_at_unix_s_out: *mut u64,
) -> DesklinkResult {
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    if invite_out.is_null() || invite_len_out.is_null() || expires_at_unix_s_out.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    unsafe {
        *invite_len_out = 0;
        *expires_at_unix_s_out = 0;
    }
    if invite_capacity < PAIRING_INVITE_BYTES {
        return DesklinkResult::InvalidArgument;
    }
    if host_runtime(handle).is_some() {
        return DesklinkResult::InvalidState;
    }
    let identity = DeviceIdentity::from_secret_key(
        handle.config.host_device_id,
        &handle.config.host_secret_key,
    );
    let invite = match PairingInvite::new(&identity, now_unix_s(), MAX_PAIRING_TTL_S) {
        Ok(invite) => invite,
        Err(_) => return DesklinkResult::InternalError,
    };
    let expires_at = invite.expires_at_unix_s();
    let encoded = match invite.encode() {
        Ok(encoded) => encoded,
        Err(_) => return DesklinkResult::InternalError,
    };
    let result = start_host_runtime(
        handle,
        *invite.session_id().as_bytes(),
        *invite.relay_authentication(),
    );
    if result != DesklinkResult::Ok {
        return result;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            encoded.as_bytes().as_ptr(),
            invite_out,
            PAIRING_INVITE_BYTES,
        );
        *invite_len_out = PAIRING_INVITE_BYTES;
        *expires_at_unix_s_out = expires_at;
    }
    DesklinkResult::Ok
}

/// Starts a host session from a signed invitation belonging to this host.
///
/// # Safety
/// `handle` must be a live host handle and `invite` must point to
/// `invite_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_host_start_from_invite(
    handle: *mut DesklinkHostHandle,
    invite: *const u8,
    invite_len: usize,
) -> DesklinkResult {
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    if invite.is_null() || invite_len != PAIRING_INVITE_BYTES {
        return DesklinkResult::InvalidArgument;
    }
    if host_runtime(handle).is_some() {
        return DesklinkResult::InvalidState;
    }
    let invite_bytes = unsafe { std::slice::from_raw_parts(invite, invite_len) };
    let invite = match PairingInvite::decode(invite_bytes, now_unix_s()) {
        Ok(invite) => invite,
        Err(_) => return DesklinkResult::InvalidArgument,
    };
    let identity = DeviceIdentity::from_secret_key(
        handle.config.host_device_id,
        &handle.config.host_secret_key,
    );
    if invite.host_device_id() != handle.config.host_device_id
        || invite.host_verify_key() != identity.verify_key()
    {
        return DesklinkResult::InvalidArgument;
    }
    start_host_runtime(
        handle,
        *invite.session_id().as_bytes(),
        *invite.relay_authentication(),
    )
}

#[unsafe(no_mangle)]
///
/// # Safety
/// `handle` must be a live host handle and both key pointers must point to the
/// stated fixed-size readable arrays.
pub unsafe extern "C" fn desklink_host_approve(
    handle: *mut DesklinkHostHandle,
    controller_device_id: *const u8,
    controller_verify_key: *const u8,
) -> DesklinkResult {
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    let (Some(device_id), Some(verify_key)) = (
        copy_fixed::<16>(controller_device_id),
        copy_fixed::<32>(controller_verify_key),
    ) else {
        return DesklinkResult::InvalidArgument;
    };
    send_host_command(
        handle,
        HostCommand::Approve {
            controller_device_id: device_id,
            controller_verify_key: verify_key,
        },
    )
}

#[unsafe(no_mangle)]
///
/// # Safety
/// `handle` must be a live host handle.
pub unsafe extern "C" fn desklink_host_reject(handle: *mut DesklinkHostHandle) -> DesklinkResult {
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    send_host_command(handle, HostCommand::Reject)
}

#[unsafe(no_mangle)]
///
/// # Safety
/// `handle` must be live. When `bytes_len` is nonzero, `bytes` must point to
/// that many readable bytes.
pub unsafe extern "C" fn desklink_host_send_video_config(
    handle: *mut DesklinkHostHandle,
    stream_id: u64,
    config_version: u32,
    width: u16,
    height: u16,
    bytes: *const u8,
    bytes_len: usize,
) -> DesklinkResult {
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    if stream_id == 0
        || config_version == 0
        || width == 0
        || height == 0
        || width > MAX_MVP_WIDTH
        || height > MAX_MVP_HEIGHT
    {
        return DesklinkResult::InvalidArgument;
    }
    let Some(bytes) = copy_payload(bytes, bytes_len, MAX_VIDEO_CONFIG_BYTES) else {
        return DesklinkResult::InvalidArgument;
    };
    send_host_command(
        handle,
        HostCommand::SendVideoConfig {
            stream_id,
            version: config_version,
            width,
            height,
            bytes,
        },
    )
}

#[unsafe(no_mangle)]
///
/// # Safety
/// `handle` must be live. When `bytes_len` is nonzero, `bytes` must point to
/// that many readable bytes.
pub unsafe extern "C" fn desklink_host_send_video_access_unit(
    handle: *mut DesklinkHostHandle,
    stream_id: u64,
    frame_id: u64,
    config_version: u32,
    bytes: *const u8,
    bytes_len: usize,
) -> DesklinkResult {
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    if stream_id == 0 || frame_id == 0 || config_version == 0 {
        return DesklinkResult::InvalidArgument;
    }
    let maximum = MAX_VIDEO_PACKET_PAYLOAD_BYTES * usize::from(MAX_VIDEO_CHUNKS);
    let Some(bytes) = copy_payload(bytes, bytes_len, maximum) else {
        return DesklinkResult::InvalidArgument;
    };
    if bytes.is_empty() {
        return DesklinkResult::InvalidArgument;
    }
    send_host_command(
        handle,
        HostCommand::SendVideoAccessUnit {
            stream_id,
            frame_id,
            config_version,
            bytes,
        },
    )
}

#[unsafe(no_mangle)]
///
/// # Safety
/// `handle` must be live. When `bytes_len` is nonzero, `bytes` must point to
/// that many readable bytes.
pub unsafe extern "C" fn desklink_host_send_cursor(
    handle: *mut DesklinkHostHandle,
    stream_id: u64,
    bytes: *const u8,
    bytes_len: usize,
) -> DesklinkResult {
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    if stream_id == 0 {
        return DesklinkResult::InvalidArgument;
    }
    let Some(bytes) = copy_payload(bytes, bytes_len, MAX_CURSOR_MESSAGE_BYTES) else {
        return DesklinkResult::InvalidArgument;
    };
    if bytes.is_empty() {
        return DesklinkResult::InvalidArgument;
    }
    send_host_command(handle, HostCommand::SendCursor { stream_id, bytes })
}

#[unsafe(no_mangle)]
///
/// # Safety
/// `handle` must be a live host handle.
pub unsafe extern "C" fn desklink_host_request_keyframe(
    handle: *mut DesklinkHostHandle,
) -> DesklinkResult {
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    send_host_command(handle, HostCommand::RequestKeyframe)
}

#[unsafe(no_mangle)]
///
/// # Safety
/// `handle` must be a live host handle.
pub unsafe extern "C" fn desklink_host_release_all(
    handle: *mut DesklinkHostHandle,
) -> DesklinkResult {
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    if host_runtime(handle).is_none() {
        handle
            .callback
            .emit(HostEvent::ReleaseAll, HostState::Stopping);
        return DesklinkResult::Ok;
    }
    send_host_command(handle, HostCommand::ReleaseAll)
}

#[unsafe(no_mangle)]
///
/// # Safety
/// `handle` must be a live host handle.
pub unsafe extern "C" fn desklink_host_stop(handle: *mut DesklinkHostHandle) -> DesklinkResult {
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    send_host_command(handle, HostCommand::Stop)
}

/// Stops the host worker, waits for its callback dispatcher, clears callbacks,
/// and frees the opaque handle exactly once.
///
/// # Safety
/// `handle` must be null or a live host handle that is not being used by any
/// other thread, and it must not be used again after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_host_destroy(handle: *mut DesklinkHostHandle) {
    if handle.is_null() {
        return;
    }
    let handle = unsafe { Box::from_raw(handle) };
    if let Some(runtime) = host_runtime(&handle) {
        let _ = runtime.send(HostCommand::ReleaseAll);
        let _ = runtime.send(HostCommand::Stop);
    } else {
        handle
            .callback
            .emit(HostEvent::ReleaseAll, HostState::Stopping);
    }
    if let Ok(mut thread) = handle.event_thread.lock()
        && let Some(thread) = thread.take()
    {
        let _ = thread.join();
    }
    handle.callback.clear();
    if let Ok(mut runtime) = handle.runtime.lock()
        && let Some(runtime) = runtime.take()
        && let Ok(runtime) = Arc::try_unwrap(runtime)
    {
        runtime.destroy();
    }
}

fn c_string(pointer: *const c_char, allow_empty: bool) -> Result<String, DesklinkResult> {
    if pointer.is_null() {
        return Err(DesklinkResult::InvalidArgument);
    }
    match unsafe { CStr::from_ptr(pointer) }.to_str() {
        Ok(value) if allow_empty || !value.is_empty() => Ok(value.to_owned()),
        Ok(_) => Err(DesklinkResult::InvalidArgument),
        Err(_) => Err(DesklinkResult::InvalidUtf8),
    }
}

fn copy_fixed<const N: usize>(pointer: *const u8) -> Option<[u8; N]> {
    if pointer.is_null() {
        return None;
    }
    unsafe { std::slice::from_raw_parts(pointer, N) }
        .try_into()
        .ok()
}

fn copy_payload(pointer: *const u8, length: usize, maximum: usize) -> Option<Vec<u8>> {
    if length > maximum || (length != 0 && pointer.is_null()) {
        return None;
    }
    if length == 0 {
        return Some(Vec::new());
    }
    Some(unsafe { std::slice::from_raw_parts(pointer, length) }.to_vec())
}

fn host_runtime(handle: &DesklinkHostHandle) -> Option<Arc<HostRuntime>> {
    handle.runtime.lock().ok()?.as_ref().cloned()
}

fn host_handle_ref<'a>(handle: *mut DesklinkHostHandle) -> Option<&'a DesklinkHostHandle> {
    unsafe { handle.as_ref() }
}

fn start_host_runtime(
    handle: &DesklinkHostHandle,
    session_id: [u8; 16],
    relay_authentication: [u8; 32],
) -> DesklinkResult {
    if host_runtime(handle).is_some() {
        return DesklinkResult::InvalidState;
    }
    let relay_addr = match resolve_relay(&handle.config.relay_url) {
        Ok(address) => address,
        Err(_) => return DesklinkResult::InvalidArgument,
    };
    let transport_config = match desklink_transport::QuicClientConfig::new(
        relay_addr,
        handle.config.server_name.clone(),
    ) {
        Ok(config) => config,
        Err(_) => return DesklinkResult::InternalError,
    };
    let client = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(desklink_transport::QuicClient::connect(transport_config)),
        Err(_) => return DesklinkResult::InternalError,
    };
    let client = match client {
        Ok(client) => client,
        Err(_) => return DesklinkResult::InternalError,
    };
    let identity =
        HostIdentity::from_secret_key(handle.config.host_device_id, &handle.config.host_secret_key);
    let runtime = match HostRuntime::start(
        client,
        identity,
        SessionId::from_bytes(session_id),
        relay_authentication,
    ) {
        Ok(runtime) => Arc::new(runtime),
        Err(_) => return DesklinkResult::InternalError,
    };
    let callback = handle.callback.clone();
    let event_runtime = runtime.clone();
    let event_thread = match thread::Builder::new()
        .name("desklink-host-callback".into())
        .spawn(move || dispatch_host_events(event_runtime, callback))
    {
        Ok(thread) => thread,
        Err(_) => return DesklinkResult::InternalError,
    };
    let Ok(mut runtime_slot) = handle.runtime.lock() else {
        let _ = event_thread.join();
        return DesklinkResult::InternalError;
    };
    *runtime_slot = Some(runtime);
    drop(runtime_slot);
    let Ok(mut thread_slot) = handle.event_thread.lock() else {
        return DesklinkResult::InternalError;
    };
    *thread_slot = Some(event_thread);
    DesklinkResult::Ok
}

fn dispatch_host_events(runtime: Arc<HostRuntime>, callback: Arc<HostCallbackState>) {
    let Ok(async_runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    loop {
        let event = async_runtime.block_on(runtime.next_event());
        match event {
            Ok(HostEvent::State(state)) => {
                callback.emit(HostEvent::State(state), state);
                if state == HostState::Closed {
                    break;
                }
            }
            Ok(event @ HostEvent::Error(_)) => callback.emit(event, runtime.state()),
            Ok(event @ HostEvent::ApprovalRequested { .. }) => {
                callback.emit(event, HostState::WaitingForApproval)
            }
            Ok(event @ HostEvent::Input(_)) => callback.emit(event, HostState::Connected),
            Ok(event @ HostEvent::KeyframeRequested) => callback.emit(event, HostState::Connected),
            Ok(event @ HostEvent::ReleaseAll) => callback.emit(event, runtime.state()),
            Ok(event @ HostEvent::Metrics(_)) => callback.emit(event, runtime.state()),
            Err(_) => break,
        }
    }
}

fn send_host_command(handle: &DesklinkHostHandle, command: HostCommand) -> DesklinkResult {
    let Some(runtime) = host_runtime(handle) else {
        return DesklinkResult::InvalidState;
    };
    runtime
        .send(command)
        .map_or_else(map_host_error, |_| DesklinkResult::Ok)
}

fn map_host_error(error: HostError) -> DesklinkResult {
    match error {
        HostError::InvalidState
        | HostError::ControllerIdentityMismatch
        | HostError::InvalidControllerCapabilities => DesklinkResult::InvalidState,
        HostError::CommandQueueFull
        | HostError::WorkerStopped
        | HostError::Transport(_)
        | HostError::Protocol(_)
        | HostError::Crypto(_) => DesklinkResult::InternalError,
    }
}

fn resolve_relay(relay_url: &str) -> Result<SocketAddr, String> {
    let authority = relay_url
        .strip_prefix("quic://")
        .ok_or_else(|| "relay URL must start with quic://".to_owned())?;
    authority
        .to_socket_addrs()
        .map_err(|error| format!("relay address resolution failed: {error}"))?
        .next()
        .ok_or_else(|| "relay address did not resolve".to_owned())
}

fn map_host_state(state: HostState) -> DesklinkHostState {
    match state {
        HostState::Connecting => DesklinkHostState::Connecting,
        HostState::WaitingForApproval => DesklinkHostState::WaitingForApproval,
        HostState::NegotiatingCapabilities => DesklinkHostState::NegotiatingCapabilities,
        HostState::Connected => DesklinkHostState::Connected,
        HostState::Stopping => DesklinkHostState::Stopping,
        HostState::Closed => DesklinkHostState::Closed,
    }
}

fn convert_host_input(input: InputEvent) -> DesklinkHostInput {
    match input {
        InputEvent::MouseMove { x, y } => DesklinkHostInput {
            kind: DesklinkInputKind::MouseMove,
            x: x as f32 / 1_000_000.0,
            y: y as f32 / 1_000_000.0,
            ..DesklinkHostInput::default()
        },
        InputEvent::MouseButton { button, pressed } => DesklinkHostInput {
            kind: DesklinkInputKind::MouseButton,
            button: match button {
                MouseButton::Left => 1,
                MouseButton::Right => 2,
                MouseButton::Middle => 3,
            },
            pressed: u8::from(pressed),
            ..DesklinkHostInput::default()
        },
        InputEvent::Key {
            code,
            pressed,
            modifiers,
        } => {
            let (key_code, character) = match code {
                KeyCode::Character(character) => (0, character as u32),
                KeyCode::Enter => (1, 0),
                KeyCode::Escape => (2, 0),
                KeyCode::Backspace => (3, 0),
                KeyCode::Tab => (4, 0),
                KeyCode::ArrowUp => (5, 0),
                KeyCode::ArrowDown => (6, 0),
                KeyCode::ArrowLeft => (7, 0),
                KeyCode::ArrowRight => (8, 0),
            };
            DesklinkHostInput {
                kind: DesklinkInputKind::Key,
                key_code,
                character,
                pressed: u8::from(pressed),
                modifiers: modifiers.0,
                ..DesklinkHostInput::default()
            }
        }
        InputEvent::MouseWheel { delta_x, delta_y } => DesklinkHostInput {
            kind: DesklinkInputKind::MouseWheel,
            wheel_x: delta_x,
            wheel_y: delta_y,
            ..DesklinkHostInput::default()
        },
    }
}

unsafe fn runtime_mut<'a>(handle: *mut DesklinkHandle) -> Option<&'a mut DesklinkRuntime> {
    unsafe { handle.as_mut().map(|handle| &mut handle.runtime) }
}

fn convert_input(input: &DesklinkInput) -> Result<InputEvent, DesklinkResult> {
    match input.kind {
        DesklinkInputKind::MouseMove => {
            if !input.x.is_finite() || !input.y.is_finite() {
                return Err(DesklinkResult::InvalidArgument);
            }
            Ok(InputEvent::MouseMove {
                x: (input.x.clamp(0.0, 1.0) * 1_000_000.0).round() as i32,
                y: (input.y.clamp(0.0, 1.0) * 1_000_000.0).round() as i32,
            })
        }
        DesklinkInputKind::MouseButton => {
            let button = match input.button {
                1 => MouseButton::Left,
                2 => MouseButton::Right,
                3 => MouseButton::Middle,
                _ => return Err(DesklinkResult::InvalidArgument),
            };
            Ok(InputEvent::MouseButton {
                button,
                pressed: input.pressed != 0,
            })
        }
        DesklinkInputKind::Key => {
            let modifiers = Modifiers(input.modifiers);
            if !modifiers.is_valid() {
                return Err(DesklinkResult::InvalidArgument);
            }
            Ok(InputEvent::Key {
                code: convert_key(input.key_code, input.character)?,
                pressed: input.pressed != 0,
                modifiers,
            })
        }
        DesklinkInputKind::MouseWheel => {
            if (input.wheel_x == 0 && input.wheel_y == 0)
                || !(-MAX_WHEEL_DELTA..=MAX_WHEEL_DELTA).contains(&input.wheel_x)
                || !(-MAX_WHEEL_DELTA..=MAX_WHEEL_DELTA).contains(&input.wheel_y)
            {
                return Err(DesklinkResult::InvalidArgument);
            }
            Ok(InputEvent::MouseWheel {
                delta_x: input.wheel_x,
                delta_y: input.wheel_y,
            })
        }
    }
}

fn convert_key(key_code: u32, character: u32) -> Result<KeyCode, DesklinkResult> {
    match key_code {
        0 => char::from_u32(character)
            .map(KeyCode::Character)
            .ok_or(DesklinkResult::InvalidArgument),
        1 => Ok(KeyCode::Enter),
        2 => Ok(KeyCode::Escape),
        3 => Ok(KeyCode::Backspace),
        4 => Ok(KeyCode::Tab),
        5 => Ok(KeyCode::ArrowUp),
        6 => Ok(KeyCode::ArrowDown),
        7 => Ok(KeyCode::ArrowLeft),
        8 => Ok(KeyCode::ArrowRight),
        _ => Err(DesklinkResult::InvalidArgument),
    }
}

fn map_state(state: SessionState) -> DesklinkState {
    match state {
        SessionState::Idle => DesklinkState::Idle,
        SessionState::CreatingSession => DesklinkState::CreatingSession,
        SessionState::ConnectingRelay => DesklinkState::ConnectingRelay,
        SessionState::SecureHandshake => DesklinkState::SecureHandshake,
        SessionState::WaitingForApproval => DesklinkState::WaitingForApproval,
        SessionState::NegotiatingCapabilities => DesklinkState::NegotiatingCapabilities,
        SessionState::StartingVideo => DesklinkState::StartingVideo,
        SessionState::Connected => DesklinkState::Connected,
        SessionState::Degraded => DesklinkState::Degraded,
        SessionState::RecoveringVideo => DesklinkState::RecoveringVideo,
        SessionState::Reconnecting => DesklinkState::Reconnecting,
        SessionState::Disconnecting => DesklinkState::Disconnecting,
        SessionState::Closed => DesklinkState::Closed,
    }
}

fn now_unix_s() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration
                .as_secs()
                .saturating_mul(1_000_000)
                .saturating_add(u64::from(duration.subsec_micros()))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(kind: DesklinkInputKind) -> DesklinkInput {
        DesklinkInput {
            kind,
            x: 0.0,
            y: 0.0,
            wheel_x: 0,
            wheel_y: 0,
            button: 0,
            key_code: 0,
            character: 0,
            pressed: 0,
            modifiers: 0,
        }
    }

    #[test]
    fn ffi_input_conversion_validates_wheel_and_modifier_bounds() {
        let mut wheel = input(DesklinkInputKind::MouseWheel);
        wheel.wheel_x = -120;
        wheel.wheel_y = 240;
        assert_eq!(
            convert_input(&wheel),
            Ok(InputEvent::MouseWheel {
                delta_x: -120,
                delta_y: 240
            })
        );
        wheel.wheel_y = MAX_WHEEL_DELTA + 1;
        assert_eq!(convert_input(&wheel), Err(DesklinkResult::InvalidArgument));

        let mut key = input(DesklinkInputKind::Key);
        key.key_code = 1;
        key.modifiers = 0x80;
        assert_eq!(convert_input(&key), Err(DesklinkResult::InvalidArgument));
    }
}
