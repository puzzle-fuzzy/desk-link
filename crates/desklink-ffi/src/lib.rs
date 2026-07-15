use std::{
    ffi::{CStr, c_char, c_void},
    ptr::{null, null_mut},
    time::{SystemTime, UNIX_EPOCH},
};

use desklink_crypto::{
    DeviceIdentity, MAX_PAIRING_TTL_S, PAIRING_INVITE_BYTES, PairingInvite, PairingOffer, SessionId,
};
use desklink_protocol::{
    ControlMessage, InputEnvelope, InputEvent, KeyCode, MAX_WHEEL_DELTA, Modifiers, MouseButton,
    encode_control, encode_input,
};
use desklink_session::{
    InputSequencer, PressedInputState, SessionEvent, SessionMachine, SessionState,
};
use rand_core::{OsRng, RngCore};

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

pub struct DesklinkHandle {
    runtime: DesklinkRuntime,
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
    start_secure_worker(
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
    )
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
    start_secure_worker(
        runtime,
        SecureConnectionConfigOwned {
            server_name,
            session_id: *invite.session_id().as_bytes(),
            relay_authentication: *invite.relay_authentication(),
            controller_device_id: config.controller_device_id,
            controller_secret_key: config.controller_secret_key,
            host_verify_key: *invite.host_verify_key().as_bytes(),
            expires_at_unix_s: Some(invite.expires_at_unix_s()),
        },
    )
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
