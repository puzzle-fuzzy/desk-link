use std::{
    collections::HashMap,
    ffi::{CStr, c_char, c_void},
    net::{SocketAddr, ToSocketAddrs},
    ptr::{null, null_mut},
    sync::{Arc, Mutex},
    thread::{self, JoinHandle, ThreadId},
    time::{SystemTime, UNIX_EPOCH},
};

use desklink_crypto::{
    DeviceIdentity, MAX_PAIRING_TTL_S, PAIRING_INVITE_BYTES, PairingInvite, PairingOffer, SessionId,
};
use desklink_protocol::{
    ControlMessage, InputContext, InputEnvelope, InputEvent, KeyCode, MAX_CURSOR_MESSAGE_BYTES,
    MAX_MVP_HEIGHT, MAX_MVP_WIDTH, MAX_VIDEO_CHUNKS, MAX_VIDEO_CONFIG_BYTES,
    MAX_VIDEO_PACKET_PAYLOAD_BYTES, MAX_WHEEL_DELTA, Modifiers, MouseButton, Platform,
    TransferMessage, TransferResult, encode_control, encode_input, encode_transfer,
};
use desklink_session::{
    InputSequencer, PressedInputState, SessionEvent, SessionMachine, SessionState,
};
use rand_core::{OsRng, RngCore};
use zeroize::Zeroizing;

mod audio_decode;
mod controller;
mod file_commands;
mod file_io;
mod host;
mod host_directory;
mod host_worker;
mod session_commands;
mod worker;

pub use controller::{
    ControllerError, ControllerEvent, ControllerInputSender, ControllerMetrics, ControllerRuntime,
    ControllerTransferSender,
};
pub use host::{
    HostCommand, HostError, HostEvent, HostIdentity, HostMetrics, HostRuntime, HostState,
};
pub use host_directory::{HostAccessMode, HostDirectoryCredentials, public_host_device_id};
use session_commands::{MAX_TEXT_BYTES, SessionAction};
use worker::{
    CapturedInput, ControllerCommand, ControllerWorker, DirectoryConnectionConfigOwned,
    SecureConnectionConfigOwned,
};

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
    StaleGeneration = 5,
}

/// Apple controller platforms exposed through the stable C ABI.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesklinkPlatform {
    MacOS = 1,
    IOS = 2,
}

impl TryFrom<u32> for DesklinkPlatform {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::MacOS),
            2 => Ok(Self::IOS),
            _ => Err(()),
        }
    }
}

impl From<DesklinkPlatform> for Platform {
    fn from(platform: DesklinkPlatform) -> Self {
        match platform {
            DesklinkPlatform::MacOS => Self::MacOS,
            DesklinkPlatform::IOS => Self::IOS,
        }
    }
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
    Transfer = 11,
    Audio = 12,
    Capabilities = 13,
    Session = 14,
    Clipboard = 15,
    AudioPcm = 16,
    File = 17,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesklinkControllerCommand {
    SelectDisplay = 1,
    SetAudioEnabled = 2,
    SetVideoQuality = 3,
    SetVideoProfile = 4,
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
    Transfer = 8,
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
    pub expires_at_unix_s: u64,
    /// 0 unknown (requires re-pair), 1 temporary, 2 persistent.
    pub authorization_type: u32,
    pub directory_device_id: u64,
    pub directory_access_code: [u8; 8],
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
pub struct DesklinkDirectoryConnectionConfig {
    pub server_name: *const c_char,
    pub device_id: u64,
    /// Access password bytes (any characters, 8..=64 UTF-8 bytes). The
    /// directory code is derived internally; callers never pass final codes.
    pub access_code: *const u8,
    pub access_code_len: usize,
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
    pub stream_id: u64,
    pub input_epoch: u64,
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
    /// Stable classification; message bytes are diagnostic text only.
    pub error_code: u32,
    pub recoverable: u8,
    pub input_epoch: u64,
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
    /// Original input authorization context, never relabelled at dispatch.
    pub stream_id: u64,
    pub input_epoch: u64,
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
            stream_id: 0,
            input_epoch: 0,
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
    /// Original Noise grant on Connected and Input; zero on other events.
    pub media_generation: u64,
}

pub type DesklinkHostEventCallback = extern "C" fn(*mut c_void, *const DesklinkHostEvent);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DesklinkSavedHostMaterial {
    pub session_id: [u8; 16],
    pub relay_authentication: [u8; 32],
    pub host_verify_key: [u8; 32],
    pub server_name: [c_char; 256],
    pub expires_at_unix_s: u64,
    pub authorization_type: u32,
    pub directory_device_id: u64,
    pub directory_access_code: [u8; 8],
}

pub struct DesklinkHandle {
    runtime: DesklinkRuntime,
}

pub struct DesklinkHostHandle {
    config: DesklinkHostConfigOwned,
    runtime: Mutex<Option<Arc<HostRuntime>>>,
    event_thread: Mutex<Option<JoinHandle<()>>>,
    event_thread_id: Mutex<Option<ThreadId>>,
    lifecycle: Mutex<HostLifecycle>,
    callback: Arc<HostCallbackState>,
}

struct DesklinkHostConfigOwned {
    relay_url: String,
    server_name: String,
    host_device_id: [u8; 16],
    host_secret_key: Zeroizing<[u8; 32]>,
    _log_level: u32,
}

pub(crate) struct SavedHostMaterialOwned {
    pub(crate) session_id: [u8; 16],
    pub(crate) relay_authentication: Zeroizing<[u8; 32]>,
    pub(crate) host_verify_key: [u8; 32],
    pub(crate) server_name: String,
    pub(crate) expires_at_unix_s: u64,
    pub(crate) authorization_type: u32,
    pub(crate) directory: Option<DirectoryConnectionConfigOwned>,
}

struct HostCallbackState {
    callback: Mutex<Option<(DesklinkHostEventCallback, usize)>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HostLifecycle {
    Idle,
    Starting,
    Running,
    Destroying,
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
            media_generation: 0,
        };
        match event {
            HostEvent::State(host_state) => {
                c_event.kind = DesklinkHostEventKind::State;
                c_event.state = map_host_state(host_state);
            }
            HostEvent::Connected { media_generation } => {
                c_event.state = DesklinkHostState::Connected;
                c_event.media_generation = media_generation;
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
            HostEvent::Input {
                media_generation,
                input,
            } => {
                c_event.kind = DesklinkHostEventKind::Input;
                c_event.media_generation = media_generation;
                c_event.input = convert_host_input(input.event);
                c_event.input.stream_id = input.stream_id;
                c_event.input.input_epoch = input.input_epoch;
            }
            HostEvent::Transfer(message) => {
                data = encode_transfer(&message).ok();
                c_event.kind = DesklinkHostEventKind::Transfer;
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
    input_epoch: u64,
}

impl EventMeta {
    pub(crate) const fn for_stream(stream_id: u64) -> Self {
        Self {
            stream_id,
            frame_id: 0,
            config_version: 0,
            width: 0,
            height: 0,
            input_epoch: 0,
        }
    }
}

struct DesklinkRuntime {
    relay_url: String,
    _log_level: u32,
    platform: Platform,
    callback: Option<DesklinkEventCallback>,
    callback_context: *mut c_void,
    session: SessionMachine,
    pairing: Option<PairingOffer>,
    pressed: HashMap<InputContext, PressedInputState>,
    input_sequence: InputSequencer,
    stream_id: u64,
    closed: bool,
    worker: Option<ControllerWorker>,
    saved_host_material: Arc<Mutex<Option<SavedHostMaterialOwned>>>,
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
            error_code: if kind == DesklinkEventKind::Error {
                3
            } else {
                0
            },
            recoverable: 0,
            input_epoch: meta.input_epoch,
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
                input_epoch: 0,
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
                input_epoch: 0,
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

    fn release_all(&mut self) -> DesklinkResult {
        self.release_context(None)
    }

    fn release_context(&mut self, only: Option<InputContext>) -> DesklinkResult {
        let mut releases = Vec::new();
        for (&context, pressed) in &self.pressed {
            if only.is_none_or(|expected| expected == context) {
                releases.extend(
                    pressed
                        .release_events()
                        .into_iter()
                        .map(|event| CapturedInput { context, event }),
                );
            }
        }
        if let Some(worker) = &self.worker {
            if worker.release_all(releases).is_err() {
                return DesklinkResult::InternalError;
            }
        } else {
            for input in releases {
                if let Err(error) = self.dispatch_input(input) {
                    return error;
                }
            }
        }
        self.pressed
            .retain(|context, _| only.is_some_and(|expected| expected != *context));
        self.emit(
            DesklinkEventKind::ReleaseAll,
            &[],
            EventMeta::for_stream(only.map_or(0, |context| context.stream_id)),
        );
        DesklinkResult::Ok
    }

    fn emit_input(&mut self, input: CapturedInput) -> Result<(), DesklinkResult> {
        let envelope = InputEnvelope {
            sequence: self.input_sequence.next_sequence(),
            timestamp_us: now_micros(),
            stream_id: input.context.stream_id,
            input_epoch: input.context.input_epoch,
            event: input.event,
        };
        let bytes = encode_input(&envelope).map_err(|_| DesklinkResult::InvalidArgument)?;
        self.emit(
            DesklinkEventKind::Input,
            &bytes,
            EventMeta {
                stream_id: input.context.stream_id,
                input_epoch: input.context.input_epoch,
                frame_id: 0,
                config_version: 0,
                width: 0,
                height: 0,
            },
        );
        Ok(())
    }

    fn dispatch_input(&mut self, input: CapturedInput) -> Result<(), DesklinkResult> {
        input
            .context
            .validate()
            .map_err(|_| DesklinkResult::InvalidArgument)?;
        if let Some(worker) = &self.worker {
            worker
                .send(ControllerCommand::SendInput(input))
                .map_err(|_| DesklinkResult::InternalError)
        } else {
            self.emit_input(input)
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
    unsafe {
        desklink_create_for_platform(
            config,
            DesklinkPlatform::MacOS as u32,
            callback,
            callback_context,
            out_handle,
        )
    }
}

/// Creates an opaque DeskLink controller runtime for an explicit platform.
///
/// The platform is represented as a `u32` in Rust even though the public C
/// header names the values with an enum. This keeps unknown values defined at
/// the FFI boundary so they can be rejected instead of becoming an invalid
/// Rust enum discriminant.
///
/// # Safety
/// `config` and `out_handle` must be valid writable pointers when non-null,
/// and the callback must remain callable for the lifetime of the handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_create_for_platform(
    config: *const DesklinkConfig,
    platform: u32,
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
    let platform = match DesklinkPlatform::try_from(platform) {
        Ok(platform) => Platform::from(platform),
        Err(()) => return DesklinkResult::InvalidArgument,
    };
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
        platform,
        callback,
        callback_context,
        session: SessionMachine::new(desklink_protocol::DeviceRole::Controller),
        pairing: None,
        pressed: HashMap::new(),
        input_sequence: InputSequencer::new(),
        stream_id: 0,
        closed: false,
        worker: None,
        saved_host_material: Arc::new(Mutex::new(None)),
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
            input_epoch: 0,
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
    clear_saved_material(&runtime.saved_host_material);
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
    clear_saved_material(&runtime.saved_host_material);
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
    if !saved_authorization_valid(
        config.authorization_type,
        config.expires_at_unix_s,
        now_unix_s(),
    ) {
        return DesklinkResult::InvalidArgument;
    }
    let directory = if config.directory_device_id != 0 {
        if desklink_transport::RelayDirectoryLookup::new(
            config.directory_device_id,
            config.directory_access_code,
        )
        .is_err()
        {
            return DesklinkResult::InvalidArgument;
        }
        Some(DirectoryConnectionConfigOwned {
            device_id: config.directory_device_id,
            access_code: config.directory_access_code,
        })
    } else {
        None
    };
    let saved = SavedHostMaterialOwned {
        session_id: config.session_id,
        relay_authentication: Zeroizing::new(config.relay_authentication),
        host_verify_key: config.host_verify_key,
        server_name: server_name.clone(),
        expires_at_unix_s: config.expires_at_unix_s,
        authorization_type: config.authorization_type,
        directory: directory.clone(),
    };
    let Ok(mut material) = runtime.saved_host_material.lock() else {
        return DesklinkResult::InternalError;
    };
    *material = Some(saved);
    drop(material);
    let result = start_secure_worker(
        runtime,
        SecureConnectionConfigOwned {
            server_name,
            session_id: config.session_id,
            relay_authentication: config.relay_authentication,
            controller_device_id: config.controller_device_id,
            controller_secret_key: config.controller_secret_key,
            host_verify_key: config.host_verify_key,
            expires_at_unix_s: (config.authorization_type == 1).then_some(config.expires_at_unix_s),
            authorization_type: config.authorization_type,
            directory,
        },
    );
    if result != DesklinkResult::Ok {
        clear_saved_material(&runtime.saved_host_material);
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
    clear_saved_material(&runtime.saved_host_material);
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
        expires_at_unix_s: invite.expires_at_unix_s(),
        authorization_type: if invite.is_persistent() { 2 } else { 1 },
        directory: None,
    };
    let Ok(mut material) = runtime.saved_host_material.lock() else {
        return DesklinkResult::InternalError;
    };
    *material = Some(saved);
    drop(material);
    let result = start_secure_worker(
        runtime,
        SecureConnectionConfigOwned {
            server_name,
            session_id,
            relay_authentication,
            controller_device_id: config.controller_device_id,
            controller_secret_key: config.controller_secret_key,
            host_verify_key,
            expires_at_unix_s: Some(invite.expires_at_unix_s()).filter(|expiry| *expiry != 0),
            authorization_type: if invite.is_persistent() { 2 } else { 1 },
            directory: None,
        },
    );
    if result != DesklinkResult::Ok {
        clear_saved_material(&runtime.saved_host_material);
    }
    result
}

/// Looks up a host's temporary or fixed access credentials through the relay
/// directory and starts the same authenticated controller runtime used by
/// pairing invitations.
///
/// # Safety
/// `handle` must be a live handle returned by `desklink_create`. `config` must
/// point to readable storage and `server_name` must be a valid NUL-terminated
/// UTF-8 string for the duration of this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_connect_directory(
    handle: *mut DesklinkHandle,
    config: *const DesklinkDirectoryConnectionConfig,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    clear_saved_material(&runtime.saved_host_material);
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
    if config.server_name.is_null() || config.device_id == 0 {
        return DesklinkResult::InvalidArgument;
    }
    let server_name = match unsafe { CStr::from_ptr(config.server_name) }.to_str() {
        Ok(server_name) if !server_name.is_empty() => server_name.to_owned(),
        Ok(_) => return DesklinkResult::InvalidArgument,
        Err(_) => return DesklinkResult::InvalidUtf8,
    };
    if config.access_code.is_null()
        || !(desklink_crypto::MIN_ACCESS_PASSWORD_BYTES
            ..=desklink_crypto::MAX_ACCESS_PASSWORD_BYTES)
            .contains(&config.access_code_len)
    {
        return DesklinkResult::InvalidArgument;
    }
    let password =
        unsafe { std::slice::from_raw_parts(config.access_code, config.access_code_len) };
    // Typed passwords derive the directory code; saved material stores the
    // final code and reaches the worker through the secure-connect path.
    let code = match desklink_crypto::PairingCode::derive_from_password(password) {
        Ok(code) => code,
        Err(_) => return DesklinkResult::InvalidArgument,
    };
    start_secure_worker(
        runtime,
        SecureConnectionConfigOwned {
            server_name,
            session_id: [0; 16],
            relay_authentication: [0; 32],
            controller_device_id: config.controller_device_id,
            controller_secret_key: config.controller_secret_key,
            host_verify_key: [0; 32],
            expires_at_unix_s: None,
            authorization_type: 0,
            directory: Some(DirectoryConnectionConfigOwned {
                device_id: config.device_id,
                access_code: *code.as_bytes(),
            }),
        },
    )
}

fn start_secure_worker(
    runtime: &mut DesklinkRuntime,
    config: SecureConnectionConfigOwned,
) -> DesklinkResult {
    let saved_material = runtime.saved_host_material.clone();
    let material_invalidator: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        clear_saved_material(&saved_material);
    });
    let published_material = runtime.saved_host_material.clone();
    let material_publisher: Arc<dyn Fn(SavedHostMaterialOwned) + Send + Sync> =
        Arc::new(move |material| {
            if let Ok(mut stored) = published_material.lock() {
                *stored = Some(material);
            }
        });
    let worker = match ControllerWorker::start(
        runtime.relay_url.clone(),
        runtime.platform,
        config,
        runtime.callback,
        runtime.callback_context,
        Some(material_invalidator),
        Some(material_publisher),
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
    let release_result = runtime.release_all();
    runtime.stop_worker();
    let _ = runtime.advance(SessionEvent::UserDisconnected);
    runtime.closed = true;
    clear_saved_material(&runtime.saved_host_material);
    release_result
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
    let context = InputContext {
        stream_id: input.stream_id,
        input_epoch: input.input_epoch,
    };
    if context.validate().is_err() {
        return DesklinkResult::InvalidArgument;
    }
    let event = match convert_input(input) {
        Ok(event) => event,
        Err(error) => return error,
    };
    let is_press = matches!(
        &event,
        InputEvent::MouseButton { pressed: true, .. } | InputEvent::Key { pressed: true, .. }
    );
    if is_press && !runtime.pressed.contains_key(&context) && runtime.pressed.len() >= 8 {
        return DesklinkResult::InvalidState;
    }
    let result = runtime.dispatch_input(CapturedInput {
        context,
        event: event.clone(),
    });
    if result.is_ok() {
        if matches!(
            &event,
            InputEvent::MouseButton { pressed: false, .. } | InputEvent::Key { pressed: false, .. }
        ) {
            if let Some(pressed) = runtime.pressed.get_mut(&context) {
                pressed.release(&event);
                if pressed.release_events().is_empty() {
                    runtime.pressed.remove(&context);
                }
            }
        } else if is_press {
            runtime.pressed.entry(context).or_default().press(&event);
        }
    }
    result.map_or_else(|error| error, |_| DesklinkResult::Ok)
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
            .send_action(worker.active_stream_id(), SessionAction::RequestKeyframe)
            .map_or_else(|error| error, |_| DesklinkResult::Ok);
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
            input_epoch: 0,
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
    runtime.release_all()
}

/// Releases only keys/buttons captured on this surface; it never substitutes a
/// newer context or releases a successor epoch's identical keys.
/// # Safety
/// `handle` must be a live, exclusively accessed handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_release_all_for_context(
    handle: *mut DesklinkHandle,
    stream_id: u64,
    input_epoch: u64,
) -> DesklinkResult {
    let context = InputContext {
        stream_id,
        input_epoch,
    };
    if context.validate().is_err() {
        return DesklinkResult::InvalidArgument;
    }
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if let Err(error) = runtime.ensure_active() {
        return error;
    }
    runtime.release_context(Some(context))
}

/// Queues a bounded controller command. command/value are validated before enqueue.
/// Values: display ID; audio 0/1; quality automatic/smooth/balanced/sharp 0..3;
/// profile Main/High 0/1. Success means queued, remote state arrives via Control.
/// # Safety
/// `handle` must be a live, exclusively accessed handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_controller_command(
    handle: *mut DesklinkHandle,
    command: u32,
    value: u32,
) -> DesklinkResult {
    let message = match controller_control_message(command, value) {
        Some(message) => message,
        None => return DesklinkResult::InvalidArgument,
    };
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    let Some(worker) = &runtime.worker else {
        return DesklinkResult::InvalidState;
    };
    if !worker.is_running() {
        return DesklinkResult::InvalidState;
    }
    worker
        .send(ControllerCommand::Control(message))
        .map_or(DesklinkResult::InternalError, |_| DesklinkResult::Ok)
}

fn controller_control_message(command: u32, value: u32) -> Option<ControlMessage> {
    use desklink_protocol::{H264Profile, VideoQualityPreference};
    Some(match command {
        1 => ControlMessage::SelectDisplay { display_id: value },
        2 if value <= 1 => ControlMessage::SetAudioEnabled {
            enabled: value != 0,
        },
        3 => ControlMessage::SetVideoQuality {
            preference: match value {
                0 => VideoQualityPreference::Automatic,
                1 => VideoQualityPreference::Smooth,
                2 => VideoQualityPreference::Balanced,
                3 => VideoQualityPreference::Sharp,
                _ => return None,
            },
        },
        4 => ControlMessage::SetVideoProfile {
            profile: match value {
                0 => H264Profile::Main,
                1 => H264Profile::High,
                _ => return None,
            },
        },
        _ => return None,
    })
}

/// Copies and validates a protocol TransferMessage before bounded enqueue.
/// Callback bytes are borrowed only during the callback; caller bytes only
/// during this function. The queue owns the decoded message afterward.
/// # Safety
/// `handle` must be exclusively accessed; `bytes` must cover `bytes_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_controller_send_transfer(
    handle: *mut DesklinkHandle,
    bytes: *const u8,
    bytes_len: usize,
) -> DesklinkResult {
    if bytes.is_null()
        || bytes_len == 0
        || bytes_len > desklink_protocol::MAX_TRANSFER_MESSAGE_BYTES
    {
        return DesklinkResult::InvalidArgument;
    }
    let message = match desklink_protocol::decode_transfer(unsafe {
        std::slice::from_raw_parts(bytes, bytes_len)
    }) {
        Ok(message) => message,
        Err(_) => return DesklinkResult::InvalidArgument,
    };
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    let Some(worker) = &runtime.worker else {
        return DesklinkResult::InvalidState;
    };
    if !worker.is_running() {
        return DesklinkResult::InvalidState;
    }
    worker
        .send(ControllerCommand::Transfer(message))
        .map_or(DesklinkResult::InternalError, |_| DesklinkResult::Ok)
}

/// Sends input only to the expected active stream. The input context comes
/// from the latest authenticated VideoConfig observed through the session
/// gate, so queued work is bound to its connection/display epoch even when
/// hosts reuse stream ids.
///
/// # Safety
/// `handle` is live and `input` points to a readable DesklinkInput.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_send_input_for_stream(
    handle: *mut DesklinkHandle,
    expected_stream_id: u64,
    input: *const DesklinkInput,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    let Some(input) = (unsafe { input.as_ref() }) else {
        return DesklinkResult::InvalidArgument;
    };
    if let Err(error) = runtime.ensure_active() {
        return error;
    }
    let Some(worker) = &runtime.worker else {
        return DesklinkResult::InvalidState;
    };
    let Some(context) = worker.input_context_for(expected_stream_id) else {
        return DesklinkResult::InvalidState;
    };
    if context.validate().is_err() {
        return DesklinkResult::InvalidArgument;
    }
    let event = match convert_input(input) {
        Ok(event) => event,
        Err(error) => return error,
    };
    let is_press = matches!(
        &event,
        InputEvent::MouseButton { pressed: true, .. } | InputEvent::Key { pressed: true, .. }
    );
    if is_press && !runtime.pressed.contains_key(&context) && runtime.pressed.len() >= 8 {
        return DesklinkResult::InvalidState;
    }
    let result = runtime.dispatch_input(CapturedInput {
        context,
        event: event.clone(),
    });
    if result.is_ok() {
        if matches!(
            &event,
            InputEvent::MouseButton { pressed: false, .. } | InputEvent::Key { pressed: false, .. }
        ) {
            if let Some(pressed) = runtime.pressed.get_mut(&context) {
                pressed.release(&event);
                if pressed.release_events().is_empty() {
                    runtime.pressed.remove(&context);
                }
            }
        } else if is_press {
            runtime.pressed.entry(context).or_default().press(&event);
        }
    }
    result.map_or_else(|error| error, |_| DesklinkResult::Ok)
}

/// Queues one bounded committed text string as ordered Unicode key pairs.
/// This never reads or writes the local clipboard.
///
/// # Safety
/// `handle` is live; `text` references `text_len` readable bytes (or is null
/// when text_len is zero).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_send_text(
    handle: *mut DesklinkHandle,
    expected_stream_id: u64,
    text: *const u8,
    text_len: usize,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    let text = match copy_utf8(text, text_len, MAX_TEXT_BYTES) {
        Ok(text) => text,
        Err(error) => return error,
    };
    if text.is_empty()
        || text
            .chars()
            .any(|scalar| scalar.is_control() && !matches!(scalar, '\r' | '\n' | '\t'))
    {
        return DesklinkResult::InvalidArgument;
    }
    if let Err(error) = session_worker(runtime, expected_stream_id) {
        return error;
    }
    let release = runtime.release_all();
    if release != DesklinkResult::Ok {
        return release;
    }
    send_session_action(
        runtime,
        expected_stream_id,
        SessionAction::Text(text.replace("\r\n", "\n")),
    )
}

/// Requests a host display; an authentic display_list SESSION callback
/// confirms the host's actual selection. Pressed local input is released first.
///
/// # Safety
/// `handle` is a live controller handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_select_display(
    handle: *mut DesklinkHandle,
    expected_stream_id: u64,
    display_id: u32,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if let Err(error) = session_worker(runtime, expected_stream_id) {
        return error;
    }
    let release_result = runtime.release_all();
    if release_result != DesklinkResult::Ok {
        return release_result;
    }
    send_session_action(
        runtime,
        expected_stream_id,
        SessionAction::SelectDisplay(display_id),
    )
}

/// Selects automatic(0), smooth(1), balanced(2), or sharp(3) video quality.
/// Actual applied state is supplied by the host's SESSION callback.
///
/// # Safety
/// `handle` is a live controller handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_set_video_quality(
    handle: *mut DesklinkHandle,
    expected_stream_id: u64,
    preference: u32,
) -> DesklinkResult {
    use desklink_protocol::VideoQualityPreference;
    let preference = match preference {
        0 => VideoQualityPreference::Automatic,
        1 => VideoQualityPreference::Smooth,
        2 => VideoQualityPreference::Balanced,
        3 => VideoQualityPreference::Sharp,
        _ => return DesklinkResult::InvalidArgument,
    };
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    send_session_action(
        runtime,
        expected_stream_id,
        SessionAction::SetVideoQuality(preference),
    )
}

/// Requests remote system sound. Playback starts only after the authenticated
/// host AudioState acknowledgement, reported as SESSION audio_state JSON.
///
/// # Safety
/// `handle` is a live controller handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_set_audio_enabled(
    handle: *mut DesklinkHandle,
    expected_stream_id: u64,
    enabled: u8,
) -> DesklinkResult {
    if enabled > 1 {
        return DesklinkResult::InvalidArgument;
    }
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    send_session_action(
        runtime,
        expected_stream_id,
        SessionAction::SetAudioEnabled(enabled == 1),
    )
}

/// Sends one explicitly selected regular file. The source is opened by the
/// bounded transfer worker; it is never loaded entirely into memory.
///
/// # Safety
/// `handle` is live; `transfer_id` references 16 readable bytes and `path`
/// references `path_len` readable UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_send_file(
    handle: *mut DesklinkHandle,
    expected_stream_id: u64,
    transfer_id: *const u8,
    path: *const u8,
    path_len: usize,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if transfer_id.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    let mut id = [0; 16];
    id.copy_from_slice(unsafe { std::slice::from_raw_parts(transfer_id, 16) });
    if id == [0; 16] {
        return DesklinkResult::InvalidArgument;
    }
    let path = match copy_absolute_path(path, path_len) {
        Ok(path) => path,
        Err(error) => return error,
    };
    send_session_action(
        runtime,
        expected_stream_id,
        SessionAction::File(file_commands::FileAction::Send {
            transfer_id: id,
            path,
        }),
    )
}

/// Asks the peer to select one file and saves it only into the explicitly
/// selected destination directory. Existing files are never overwritten.
///
/// # Safety
/// `handle` is live; `directory` references `directory_len` readable UTF-8 bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_request_remote_file(
    handle: *mut DesklinkHandle,
    expected_stream_id: u64,
    request_id: u64,
    directory: *const u8,
    directory_len: usize,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if request_id == 0 {
        return DesklinkResult::InvalidArgument;
    }
    let destination = match copy_absolute_path(directory, directory_len) {
        Ok(path) => path,
        Err(error) => return error,
    };
    send_session_action(
        runtime,
        expected_stream_id,
        SessionAction::File(file_commands::FileAction::Request {
            request_id,
            destination,
        }),
    )
}

/// Cancels the session's pending selection or file transfer and removes any
/// uncommitted temporary receive file.
///
/// # Safety
/// `handle` is a live controller handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_cancel_file_transfer(
    handle: *mut DesklinkHandle,
    expected_stream_id: u64,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    send_session_action(
        runtime,
        expected_stream_id,
        SessionAction::File(file_commands::FileAction::CancelAll),
    )
}

fn copy_absolute_path(
    pointer: *const u8,
    length: usize,
) -> Result<std::path::PathBuf, DesklinkResult> {
    let text = copy_utf8(pointer, length, 4096)?;
    let path = std::path::PathBuf::from(&text);
    if text.contains('\0') || !path.is_absolute() {
        return Err(DesklinkResult::InvalidArgument);
    }
    Ok(path)
}

/// Explicitly sends UTF-8 text to the peer's clipboard. No automatic local
/// clipboard access occurs. Request ids must increase within a connection.
///
/// # Safety
/// `handle` is live; `text` references `text_len` readable bytes (or is null
/// when text_len is zero).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_send_clipboard_text(
    handle: *mut DesklinkHandle,
    expected_stream_id: u64,
    request_id: u64,
    text: *const u8,
    text_len: usize,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if request_id == 0 {
        return DesklinkResult::InvalidArgument;
    }
    let text = match copy_utf8(text, text_len, desklink_protocol::MAX_CLIPBOARD_TEXT_BYTES) {
        Ok(text) => text,
        Err(error) => return error,
    };
    if text.contains('\0') {
        return DesklinkResult::InvalidArgument;
    }
    send_session_action(
        runtime,
        expected_stream_id,
        SessionAction::SendClipboard { request_id, text },
    )
}

/// Explicitly fetches the peer's clipboard. A correlated CLIPBOARD callback
/// returns text or a bounded failure; unsolicited replies are ignored.
///
/// # Safety
/// `handle` is a live controller handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_request_clipboard_text(
    handle: *mut DesklinkHandle,
    expected_stream_id: u64,
    request_id: u64,
) -> DesklinkResult {
    let Some(runtime) = (unsafe { runtime_mut(handle) }) else {
        return DesklinkResult::InvalidArgument;
    };
    if request_id == 0 {
        return DesklinkResult::InvalidArgument;
    }
    send_session_action(
        runtime,
        expected_stream_id,
        SessionAction::RequestClipboard { request_id },
    )
}

fn session_worker(
    runtime: &DesklinkRuntime,
    expected_stream_id: u64,
) -> Result<&ControllerWorker, DesklinkResult> {
    runtime.ensure_active()?;
    let worker = runtime
        .worker
        .as_ref()
        .ok_or(DesklinkResult::InvalidState)?;
    if expected_stream_id == 0
        || worker.active_stream_id() != expected_stream_id
        || !worker.is_running()
    {
        return Err(DesklinkResult::InvalidState);
    }
    Ok(worker)
}

fn send_session_action(
    runtime: &DesklinkRuntime,
    expected_stream_id: u64,
    action: SessionAction,
) -> DesklinkResult {
    if matches!(action, SessionAction::File(_)) {
        // A display transition releases input without interrupting a file
        // operation on the same authenticated stream. Its independent gate
        // still invalidates on a real stream change or reconnect.
        if let Err(error) = runtime.ensure_active() {
            return error;
        }
        return match runtime
            .worker
            .as_ref()
            .ok_or(DesklinkResult::InvalidState)
            .and_then(|worker| worker.send_action(expected_stream_id, action))
        {
            Ok(()) => DesklinkResult::Ok,
            Err(error) => error,
        };
    }
    match session_worker(runtime, expected_stream_id)
        .and_then(|worker| worker.send_action(expected_stream_id, action))
    {
        Ok(()) => DesklinkResult::Ok,
        Err(error) => error,
    }
}

fn copy_utf8(pointer: *const u8, length: usize, maximum: usize) -> Result<String, DesklinkResult> {
    let bytes = copy_payload(pointer, length, maximum).ok_or(DesklinkResult::InvalidArgument)?;
    String::from_utf8(bytes).map_err(|_| DesklinkResult::InvalidUtf8)
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
    let _ = handle.runtime.release_all();
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
    let Some(worker) = runtime.worker.as_ref() else {
        return DesklinkResult::InvalidState;
    };
    if !worker.is_running() {
        clear_saved_material(&runtime.saved_host_material);
        return DesklinkResult::InvalidState;
    }
    let Ok(material_guard) = runtime.saved_host_material.lock() else {
        return DesklinkResult::InternalError;
    };
    let Some(material) = material_guard.as_ref() else {
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
        expires_at_unix_s: material.expires_at_unix_s,
        authorization_type: material.authorization_type,
        directory_device_id: material
            .directory
            .as_ref()
            .map_or(0, |directory| directory.device_id),
        directory_access_code: material
            .directory
            .as_ref()
            .map_or([0; 8], |directory| directory.access_code),
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
        event_thread_id: Mutex::new(None),
        lifecycle: Mutex::new(HostLifecycle::Idle),
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
    if let Err(error) = reserve_host_start(handle) {
        return error;
    }
    let identity = DeviceIdentity::from_secret_key(
        handle.config.host_device_id,
        &handle.config.host_secret_key,
    );
    let invite = match PairingInvite::new(&identity, now_unix_s(), MAX_PAIRING_TTL_S) {
        Ok(invite) => invite,
        Err(_) => {
            release_host_start(handle);
            return DesklinkResult::InternalError;
        }
    };
    let expires_at = invite.expires_at_unix_s();
    let encoded = match invite.encode() {
        Ok(encoded) => encoded,
        Err(_) => {
            release_host_start(handle);
            return DesklinkResult::InternalError;
        }
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

/// Returns the stable public directory number derived from the host identity.
///
/// # Safety
/// `handle` must be live and `out_device_id` must point to writable storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_host_public_device_id(
    handle: *mut DesklinkHostHandle,
    out_device_id: *mut u64,
) -> DesklinkResult {
    if out_device_id.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    unsafe {
        *out_device_id = 0;
    }
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    unsafe {
        *out_device_id = public_host_device_id(handle.config.host_device_id);
    }
    DesklinkResult::Ok
}

/// Starts an identified host and publishes its signed invitation in the relay
/// directory. Mode 1 is temporary (600 seconds), mode 2 fixed (live connection).
/// The eight-byte access code is never returned, logged or written to disk.
/// This bounded network call must run outside the application main thread.
///
/// # Safety
/// `handle` must be live. Inputs must refer to readable storage of the given
/// length. Outputs must refer to writable storage. Do not destroy the handle
/// concurrently with this call; wait for it to return, then stop and destroy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn desklink_host_start_directory(
    handle: *mut DesklinkHostHandle,
    mode: u32,
    access_code: *const u8,
    access_code_len: usize,
    invite_out: *mut u8,
    invite_capacity: usize,
    invite_len_out: *mut usize,
    expires_at_unix_s_out: *mut u64,
) -> DesklinkResult {
    if invite_len_out.is_null() || expires_at_unix_s_out.is_null() {
        return DesklinkResult::InvalidArgument;
    }
    unsafe {
        *invite_len_out = 0;
        *expires_at_unix_s_out = 0;
    }
    let Some(handle) = host_handle_ref(handle) else {
        return DesklinkResult::InvalidArgument;
    };
    if invite_out.is_null()
        || invite_capacity < PAIRING_INVITE_BYTES
        || !(desklink_crypto::MIN_ACCESS_PASSWORD_BYTES
            ..=desklink_crypto::MAX_ACCESS_PASSWORD_BYTES)
            .contains(&access_code_len)
    {
        return DesklinkResult::InvalidArgument;
    }
    let mode = match mode {
        1 => HostAccessMode::Temporary,
        2 => HostAccessMode::Fixed,
        _ => return DesklinkResult::InvalidArgument,
    };
    let mut password = Zeroizing::new([0u8; desklink_crypto::MAX_ACCESS_PASSWORD_BYTES]);
    let password = unsafe {
        std::ptr::copy_nonoverlapping(access_code, password.as_mut().as_mut_ptr(), access_code_len);
        &password[..access_code_len]
    };
    if let Err(error) = reserve_host_start(handle) {
        return error;
    }
    let result = (|| {
        let config = host_transport_config(handle)?;
        let identity = HostIdentity::from_secret_key(
            handle.config.host_device_id,
            &handle.config.host_secret_key,
        );
        let (runtime, credentials) = HostRuntime::start_directory_from_config(
            config,
            identity,
            mode,
            password,
            now_unix_s(),
        )
        .map_err(|_| DesklinkResult::InternalError)?;
        let result = install_host_runtime(handle, Arc::new(runtime));
        if result != DesklinkResult::Ok {
            return Err(result);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                credentials.invitation.as_bytes().as_ptr(),
                invite_out,
                PAIRING_INVITE_BYTES,
            );
            *invite_len_out = PAIRING_INVITE_BYTES;
            *expires_at_unix_s_out = credentials.expires_at_unix_s;
        }
        Ok(())
    })();
    match result {
        Ok(()) => DesklinkResult::Ok,
        Err(error) => {
            release_host_start(handle);
            error
        }
    }
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
    if let Err(error) = reserve_host_start(handle) {
        return error;
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
    if bytes.is_empty() {
        return DesklinkResult::InvalidArgument;
    }
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
pub unsafe extern "C" fn desklink_host_send_video_config_for_generation(
    handle: *mut DesklinkHostHandle,
    media_generation: u64,
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
    if bytes.is_empty() {
        return DesklinkResult::InvalidArgument;
    }
    send_host_command_for_generation(
        handle,
        media_generation,
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
pub unsafe extern "C" fn desklink_host_send_video_access_unit_for_generation(
    handle: *mut DesklinkHostHandle,
    media_generation: u64,
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
    send_host_command_for_generation(
        handle,
        media_generation,
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
/// `handle` must be live. When `bytes_len` is nonzero, `bytes` must point to
/// that many readable bytes.
pub unsafe extern "C" fn desklink_host_send_cursor_for_generation(
    handle: *mut DesklinkHostHandle,
    media_generation: u64,
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
    send_host_command_for_generation(
        handle,
        media_generation,
        HostCommand::SendCursor { stream_id, bytes },
    )
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
    let Some(runtime) = host_runtime(handle) else {
        return DesklinkResult::InvalidState;
    };
    runtime.stop();
    DesklinkResult::Ok
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
    if let Ok(mut lifecycle) = handle.lifecycle.lock() {
        *lifecycle = HostLifecycle::Destroying;
    }
    if let Some(runtime) = host_runtime(&handle) {
        runtime.stop();
    } else {
        handle
            .callback
            .emit(HostEvent::ReleaseAll, HostState::Stopping);
    }
    let on_callback_thread = handle
        .event_thread_id
        .lock()
        .ok()
        .and_then(|thread_id| *thread_id)
        == Some(thread::current().id());
    let event_thread = handle
        .event_thread
        .lock()
        .ok()
        .and_then(|mut thread| thread.take());
    let runtime = handle
        .runtime
        .lock()
        .ok()
        .and_then(|mut runtime| runtime.take());
    if on_callback_thread {
        let _ = thread::Builder::new()
            .name("desklink-host-destroy".into())
            .spawn(move || {
                if let Some(thread) = event_thread {
                    let _ = thread.join();
                }
                if let Some(runtime) = runtime
                    && let Ok(runtime) = Arc::try_unwrap(runtime)
                {
                    runtime.destroy();
                }
            });
        return;
    }
    handle.callback.clear();
    if let Some(thread) = event_thread {
        let _ = thread.join();
    }
    if let Some(runtime) = runtime
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

fn clear_saved_material(material: &Arc<Mutex<Option<SavedHostMaterialOwned>>>) {
    if let Ok(mut material) = material.lock() {
        *material = None;
    }
}

fn saved_authorization_valid(kind: u32, expires_at: u64, now: u64) -> bool {
    match kind {
        1 => expires_at > now,
        2 => expires_at == 0,
        _ => false,
    }
}

fn reserve_host_start(handle: &DesklinkHostHandle) -> Result<(), DesklinkResult> {
    let Ok(mut lifecycle) = handle.lifecycle.lock() else {
        return Err(DesklinkResult::InternalError);
    };
    if *lifecycle != HostLifecycle::Idle {
        return Err(DesklinkResult::InvalidState);
    }
    *lifecycle = HostLifecycle::Starting;
    Ok(())
}

fn release_host_start(handle: &DesklinkHostHandle) {
    if let Ok(mut lifecycle) = handle.lifecycle.lock()
        && *lifecycle == HostLifecycle::Starting
    {
        *lifecycle = HostLifecycle::Idle;
    }
}

fn mark_host_running(handle: &DesklinkHostHandle) -> Result<(), DesklinkResult> {
    let Ok(mut lifecycle) = handle.lifecycle.lock() else {
        return Err(DesklinkResult::InternalError);
    };
    if *lifecycle != HostLifecycle::Starting {
        return Err(DesklinkResult::InvalidState);
    }
    *lifecycle = HostLifecycle::Running;
    Ok(())
}

fn host_handle_ref<'a>(handle: *mut DesklinkHostHandle) -> Option<&'a DesklinkHostHandle> {
    unsafe { handle.as_ref() }
}

fn start_host_runtime(
    handle: &DesklinkHostHandle,
    session_id: [u8; 16],
    relay_authentication: [u8; 32],
) -> DesklinkResult {
    let result = start_host_runtime_inner(handle, session_id, relay_authentication);
    if result != DesklinkResult::Ok {
        release_host_start(handle);
    }
    result
}

fn start_host_runtime_inner(
    handle: &DesklinkHostHandle,
    session_id: [u8; 16],
    relay_authentication: [u8; 32],
) -> DesklinkResult {
    let transport_config = match host_transport_config(handle) {
        Ok(config) => config,
        Err(error) => return error,
    };
    let identity =
        HostIdentity::from_secret_key(handle.config.host_device_id, &handle.config.host_secret_key);
    let runtime = match HostRuntime::start_from_config(
        transport_config,
        identity,
        SessionId::from_bytes(session_id),
        relay_authentication,
    ) {
        Ok(runtime) => Arc::new(runtime),
        Err(_) => return DesklinkResult::InternalError,
    };
    install_host_runtime(handle, runtime)
}

fn host_transport_config(
    handle: &DesklinkHostHandle,
) -> Result<desklink_transport::QuicClientConfig, DesklinkResult> {
    let relay_addr =
        resolve_relay(&handle.config.relay_url).map_err(|_| DesklinkResult::InvalidArgument)?;
    desklink_transport::QuicClientConfig::new(relay_addr, handle.config.server_name.clone())
        .map_err(|_| DesklinkResult::InternalError)
}

fn install_host_runtime(handle: &DesklinkHostHandle, runtime: Arc<HostRuntime>) -> DesklinkResult {
    let callback = handle.callback.clone();
    let event_runtime = runtime.clone();
    let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
    let event_thread = match thread::Builder::new()
        .name("desklink-host-callback".into())
        .spawn(move || {
            if ready_receiver.recv().is_ok() {
                dispatch_host_events(event_runtime, callback);
            }
        }) {
        Ok(thread) => thread,
        Err(_) => return DesklinkResult::InternalError,
    };
    let Ok(mut runtime_slot) = handle.runtime.lock() else {
        let _ = ready_sender.send(());
        let _ = event_thread.join();
        return DesklinkResult::InternalError;
    };
    let event_thread_id = event_thread.thread().id();
    *runtime_slot = Some(runtime);
    drop(runtime_slot);
    let Ok(mut thread_slot) = handle.event_thread.lock() else {
        let _ = ready_sender.send(());
        let _ = event_thread.join();
        return DesklinkResult::InternalError;
    };
    *thread_slot = Some(event_thread);
    if let Ok(mut thread_id_slot) = handle.event_thread_id.lock() {
        *thread_id_slot = Some(event_thread_id);
    }
    if let Err(error) = mark_host_running(handle) {
        let _ = ready_sender.send(());
        return error;
    }
    let _ = ready_sender.send(());
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
            Ok(event @ HostEvent::Connected { .. }) => callback.emit(event, HostState::Connected),
            Ok(event @ HostEvent::ApprovalRequested { .. }) => {
                callback.emit(event, HostState::WaitingForApproval)
            }
            Ok(event @ HostEvent::Input { .. }) => callback.emit(event, HostState::Connected),
            Ok(HostEvent::Transfer(message)) => {
                // The C ABI has no clipboard/file receiver yet. Reject only
                // requests, through the existing bounded command queue, rather
                // than leaving the peer waiting or closing its control session.
                // Results/cancellation/chunks are ignored to avoid reply loops.
                if let Some(response) = unsupported_transfer_response(&message) {
                    let _ = runtime.send(HostCommand::SendTransfer(response));
                }
                callback.emit(HostEvent::Transfer(message), HostState::Connected);
            }
            Ok(event @ HostEvent::KeyframeRequested) => callback.emit(event, HostState::Connected),
            Ok(event @ HostEvent::ReleaseAll) => callback.emit(event, runtime.state()),
            Ok(event @ HostEvent::Metrics(_)) => callback.emit(event, runtime.state()),
            Err(_) => break,
        }
    }
}

fn unsupported_transfer_response(message: &TransferMessage) -> Option<TransferMessage> {
    match message {
        TransferMessage::ClipboardSet { request_id, .. }
        | TransferMessage::ClipboardRequest { request_id } => {
            Some(TransferMessage::ClipboardResult {
                request_id: *request_id,
                result: TransferResult::Unsupported,
            })
        }
        TransferMessage::FileSelectionRequest { request_id, .. } => {
            Some(TransferMessage::FileSelectionResult {
                request_id: *request_id,
                result: TransferResult::Unsupported,
            })
        }
        TransferMessage::FileOffer { transfer_id, .. } => Some(TransferMessage::FileDecision {
            transfer_id: *transfer_id,
            result: TransferResult::Unsupported,
            resume_offset: 0,
            resume_prefix_hash: None,
        }),
        _ => None,
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

fn send_host_command_for_generation(
    handle: &DesklinkHostHandle,
    media_generation: u64,
    command: HostCommand,
) -> DesklinkResult {
    let Some(runtime) = host_runtime(handle) else {
        return DesklinkResult::StaleGeneration;
    };
    runtime
        .send_for_generation(media_generation, command)
        .map_or_else(map_host_error, |_| DesklinkResult::Ok)
}

fn map_host_error(error: HostError) -> DesklinkResult {
    match error {
        HostError::StaleMediaGeneration => DesklinkResult::StaleGeneration,
        HostError::InvalidAccessCode => DesklinkResult::InvalidArgument,
        HostError::InvalidState
        | HostError::ControllerIdentityMismatch
        | HostError::InvalidControllerCapabilities
        | HostError::ApprovalTimeout
        | HostError::AccessExpired => DesklinkResult::InvalidState,
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
            let (key_code, character) = convert_key_to_ffi(code);
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

fn convert_key_to_ffi(code: KeyCode) -> (u32, u32) {
    match code {
        KeyCode::Character(character) => (0, character as u32),
        KeyCode::Enter => (1, 0),
        KeyCode::Escape => (2, 0),
        KeyCode::Backspace => (3, 0),
        KeyCode::Tab => (4, 0),
        KeyCode::ArrowUp => (5, 0),
        KeyCode::ArrowDown => (6, 0),
        KeyCode::ArrowLeft => (7, 0),
        KeyCode::ArrowRight => (8, 0),
        KeyCode::Delete => (9, 0),
        KeyCode::Insert => (10, 0),
        KeyCode::Home => (11, 0),
        KeyCode::End => (12, 0),
        KeyCode::PageUp => (13, 0),
        KeyCode::PageDown => (14, 0),
        KeyCode::CapsLock => (15, 0),
        KeyCode::Function(number) => (15 + u32::from(number), 0),
        KeyCode::Control => (28, 0),
        KeyCode::Alt => (29, 0),
        KeyCode::Shift => (30, 0),
        KeyCode::Meta => (31, 0),
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
            let code = convert_key(input.key_code, input.character)?;
            if code
                .modifier_mask()
                .is_some_and(|own_modifier| modifiers.contains(own_modifier))
            {
                return Err(DesklinkResult::InvalidArgument);
            }
            Ok(InputEvent::Key {
                code,
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
        9 => Ok(KeyCode::Delete),
        10 => Ok(KeyCode::Insert),
        11 => Ok(KeyCode::Home),
        12 => Ok(KeyCode::End),
        13 => Ok(KeyCode::PageUp),
        14 => Ok(KeyCode::PageDown),
        15 => Ok(KeyCode::CapsLock),
        16..=27 => Ok(KeyCode::Function((key_code - 15) as u8)),
        28 => Ok(KeyCode::Control),
        29 => Ok(KeyCode::Alt),
        30 => Ok(KeyCode::Shift),
        31 => Ok(KeyCode::Meta),
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

    #[test]
    fn delayed_input_c_callback_keeps_original_noise_grant_and_input_context() {
        type Snapshot = (DesklinkHostEventKind, u64, u64, u64, u32, u8);
        extern "C" fn record(context: *mut c_void, event: *const DesklinkHostEvent) {
            let snapshots = unsafe { &mut *context.cast::<Vec<Snapshot>>() };
            let event = unsafe { &*event };
            snapshots.push((
                event.kind,
                event.media_generation,
                event.input.stream_id,
                event.input.input_epoch,
                event.input.button,
                event.input.pressed,
            ));
        }
        let mut snapshots = Vec::<Snapshot>::new();
        let callback =
            HostCallbackState::new(Some(record), (&mut snapshots as *mut Vec<Snapshot>).cast());
        let input = HostEvent::Input {
            media_generation: 101,
            input: desklink_protocol::InputEnvelope {
                sequence: 7,
                timestamp_us: 8,
                stream_id: 9,
                input_epoch: 47,
                event: InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                },
            },
        };
        callback.emit(input.clone(), HostState::Connected);
        callback.emit(
            HostEvent::Connected {
                media_generation: 202,
            },
            HostState::Connected,
        );
        // This invokes the real C callback conversion after successor B became
        // visible. Delayed A must retain A's original grant and nonzero target.
        callback.emit(input, HostState::Connected);
        callback.emit(HostEvent::ReleaseAll, HostState::Connecting);
        assert_eq!(
            snapshots,
            vec![
                (DesklinkHostEventKind::Input, 101, 9, 47, 1, 1),
                (DesklinkHostEventKind::State, 202, 0, 0, 0, 0),
                (DesklinkHostEventKind::Input, 101, 9, 47, 1, 1),
                (DesklinkHostEventKind::ReleaseAll, 0, 0, 0, 0, 0),
            ]
        );
    }

    #[test]
    fn connected_callback_preserves_the_queued_grant_and_zeroes_other_events() {
        extern "C" fn record(context: *mut c_void, event: *const DesklinkHostEvent) {
            let received = unsafe {
                &mut *context.cast::<Vec<(DesklinkHostEventKind, DesklinkHostState, u64)>>()
            };
            let event = unsafe { &*event };
            received.push((event.kind, event.state, event.media_generation));
        }
        let mut received = Vec::new();
        let callback = HostCallbackState::new(
            Some(record),
            (&mut received as *mut Vec<(DesklinkHostEventKind, DesklinkHostState, u64)>).cast(),
        );
        // Dispatch can be delayed past a successor connection. The immutable
        // event carries its original grant, irrespective of the current state.
        callback.emit(
            HostEvent::Connected {
                media_generation: 43,
            },
            HostState::Closed,
        );
        callback.emit(
            HostEvent::Connected {
                media_generation: 44,
            },
            HostState::Connected,
        );
        callback.emit(HostEvent::ReleaseAll, HostState::Connecting);
        assert_eq!(
            received,
            vec![
                (
                    DesklinkHostEventKind::State,
                    DesklinkHostState::Connected,
                    43_u64
                ),
                (
                    DesklinkHostEventKind::State,
                    DesklinkHostState::Connected,
                    44_u64
                ),
                (
                    DesklinkHostEventKind::ReleaseAll,
                    DesklinkHostState::Connecting,
                    0_u64
                ),
            ]
        );
    }

    #[test]
    fn context_specific_release_cannot_clear_a_successor_epochs_identical_key() {
        let relay = std::ffi::CString::new("quic://127.0.0.1:9").unwrap();
        let config = DesklinkConfig {
            relay_url: relay.as_ptr(),
            log_level: 0,
        };
        let mut handle = null_mut();
        unsafe {
            assert_eq!(
                desklink_create(&config, None, null_mut(), &mut handle),
                DesklinkResult::Ok
            );
            let mut key = input(DesklinkInputKind::Key);
            key.key_code = 1;
            key.pressed = 1;
            key.input_epoch = 0;
            assert_eq!(
                desklink_send_input(handle, &key),
                DesklinkResult::InvalidArgument
            );
            key.input_epoch = 10;
            assert_eq!(desklink_send_input(handle, &key), DesklinkResult::Ok);
            key.input_epoch = 20;
            assert_eq!(desklink_send_input(handle, &key), DesklinkResult::Ok);
            assert_eq!((*handle).runtime.pressed.len(), 2);
            assert_eq!(
                desklink_release_all_for_context(handle, 1, 10),
                DesklinkResult::Ok
            );
            let next = InputContext {
                stream_id: 1,
                input_epoch: 20,
            };
            assert_eq!(
                (*handle)
                    .runtime
                    .pressed
                    .get(&next)
                    .unwrap()
                    .release_events()
                    .len(),
                1
            );
            key.input_epoch = 10;
            key.pressed = 0;
            assert_eq!(desklink_send_input(handle, &key), DesklinkResult::Ok);
            assert_eq!((*handle).runtime.pressed.len(), 1);
            assert_eq!(desklink_release_all(handle), DesklinkResult::Ok);
            assert!((*handle).runtime.pressed.is_empty());
            desklink_destroy(handle);
        }
    }

    #[test]
    fn saved_authorization_never_upgrades_unknown_or_expired_material() {
        assert!(!saved_authorization_valid(0, 0, 100));
        assert!(!saved_authorization_valid(1, 0, 100));
        assert!(!saved_authorization_valid(1, 100, 100));
        assert!(saved_authorization_valid(1, 101, 100));
        assert!(saved_authorization_valid(2, 0, 100));
        assert!(!saved_authorization_valid(2, 101, 100));
        assert!(!saved_authorization_valid(3, 0, 100));
    }

    #[test]
    fn controller_commands_validate_values_and_preserve_typed_intent() {
        assert_eq!(
            controller_control_message(1, 42),
            Some(ControlMessage::SelectDisplay { display_id: 42 })
        );
        assert_eq!(
            controller_control_message(2, 0),
            Some(ControlMessage::SetAudioEnabled { enabled: false })
        );
        assert_eq!(
            controller_control_message(2, 1),
            Some(ControlMessage::SetAudioEnabled { enabled: true })
        );
        assert!(controller_control_message(2, 2).is_none());
        for value in 0..4 {
            assert!(controller_control_message(3, value).is_some());
        }
        assert!(controller_control_message(3, 4).is_none());
        assert!(controller_control_message(4, 2).is_none());
        assert!(controller_control_message(0, 0).is_none());
        assert!(controller_control_message(u32::MAX, u32::MAX).is_none());
        unsafe {
            assert_eq!(
                desklink_controller_command(null_mut(), 1, 0),
                DesklinkResult::InvalidArgument
            );
            assert_eq!(
                desklink_controller_send_transfer(null_mut(), null(), 0),
                DesklinkResult::InvalidArgument
            );
            assert_eq!(
                desklink_controller_send_transfer(null_mut(), null(), usize::MAX),
                DesklinkResult::InvalidArgument
            );
        }
    }

    #[test]
    fn c_host_explicitly_rejects_unsupported_transfer_requests_without_reply_loops() {
        for request in [
            TransferMessage::ClipboardSet {
                request_id: 41,
                text: "test".into(),
            },
            TransferMessage::ClipboardRequest { request_id: 41 },
        ] {
            assert_eq!(
                unsupported_transfer_response(&request),
                Some(TransferMessage::ClipboardResult {
                    request_id: 41,
                    result: TransferResult::Unsupported,
                })
            );
        }
        assert_eq!(
            unsupported_transfer_response(&TransferMessage::FileSelectionRequest {
                request_id: 42,
                resume: None,
            }),
            Some(TransferMessage::FileSelectionResult {
                request_id: 42,
                result: TransferResult::Unsupported,
            })
        );
        let response = unsupported_transfer_response(&TransferMessage::FileOffer {
            transfer_id: [43; 16],
            request_id: None,
            name: "test.txt".into(),
            size: 0,
        })
        .unwrap();
        assert_eq!(
            response,
            TransferMessage::FileDecision {
                transfer_id: [43; 16],
                result: TransferResult::Unsupported,
                resume_offset: 0,
                resume_prefix_hash: None,
            }
        );
        for message in [
            response,
            TransferMessage::ClipboardResult {
                request_id: 41,
                result: TransferResult::Unsupported,
            },
            TransferMessage::FileSelectionResult {
                request_id: 42,
                result: TransferResult::Unsupported,
            },
            TransferMessage::FileChunk {
                transfer_id: [43; 16],
                offset: 0,
                bytes: vec![1],
            },
            TransferMessage::Cancel {
                transfer_id: [43; 16],
            },
        ] {
            assert_eq!(unsupported_transfer_response(&message), None);
        }
    }

    fn input(kind: DesklinkInputKind) -> DesklinkInput {
        DesklinkInput {
            stream_id: 1,
            input_epoch: 1,
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

        key.key_code = 28;
        key.modifiers = Modifiers::CONTROL.0;
        assert_eq!(convert_input(&key), Err(DesklinkResult::InvalidArgument));
    }

    #[test]
    fn ffi_special_key_codes_round_trip_across_controller_and_host_abis() {
        let cases = [
            (1, KeyCode::Enter),
            (8, KeyCode::ArrowRight),
            (9, KeyCode::Delete),
            (10, KeyCode::Insert),
            (11, KeyCode::Home),
            (12, KeyCode::End),
            (13, KeyCode::PageUp),
            (14, KeyCode::PageDown),
            (15, KeyCode::CapsLock),
            (16, KeyCode::Function(1)),
            (27, KeyCode::Function(12)),
            (28, KeyCode::Control),
            (29, KeyCode::Alt),
            (30, KeyCode::Shift),
            (31, KeyCode::Meta),
        ];
        for (ffi_code, protocol_code) in cases {
            assert_eq!(convert_key(ffi_code, 0), Ok(protocol_code));
            assert_eq!(convert_key_to_ffi(protocol_code), (ffi_code, 0));
        }
        assert_eq!(convert_key(32, 0), Err(DesklinkResult::InvalidArgument));
    }
}
