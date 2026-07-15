use std::{
    ffi::{CStr, CString, c_void},
    ptr::{null, null_mut},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
    time::Duration,
};

use desklink_ffi::{
    DesklinkConfig, DesklinkEvent, DesklinkEventCallback, DesklinkEventKind, DesklinkHandle,
    DesklinkInput, DesklinkInputKind, DesklinkPairingInviteConnectionConfig, DesklinkResult,
    DesklinkSecureConnectionConfig, desklink_connect_pairing_invite, desklink_connect_secure,
    desklink_connect_with_code, desklink_create, desklink_destroy, desklink_identity_verify_key,
    desklink_reject, desklink_release_all, desklink_request_keyframe, desklink_send_input,
    desklink_start_pairing,
};

static CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_callback(_context: *mut c_void, _event: *const DesklinkEvent) {
    CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
}

extern "C" fn count_context_callback(context: *mut c_void, _event: *const DesklinkEvent) {
    let counter = unsafe { &*(context.cast::<AtomicUsize>()) };
    counter.fetch_add(1, Ordering::Relaxed);
}

fn config(url: &CString) -> DesklinkConfig {
    DesklinkConfig {
        relay_url: url.as_ptr(),
        log_level: 1,
    }
}

unsafe fn create_handle(callback: Option<DesklinkEventCallback>) -> (*mut DesklinkHandle, CString) {
    let url = CString::new("quic://127.0.0.1:4433").unwrap();
    let mut handle = null_mut();
    assert_eq!(
        unsafe { desklink_create(&config(&url), callback, null_mut(), &mut handle) },
        DesklinkResult::Ok
    );
    (handle, url)
}

#[test]
fn ffi_handle_can_be_created_and_destroyed() {
    let (handle, _url) = unsafe { create_handle(None) };
    assert!(!handle.is_null());
    unsafe { desklink_destroy(handle) };
}

#[test]
fn ffi_rejects_null_arguments_without_allocating_a_handle() {
    let url = CString::new("quic://127.0.0.1:4433").unwrap();
    let mut handle = null_mut();
    assert_eq!(
        unsafe { desklink_create(null(), None, null_mut(), &mut handle) },
        DesklinkResult::InvalidArgument
    );
    assert!(handle.is_null());
    assert_eq!(
        unsafe { desklink_create(&config(&url), None, null_mut(), null_mut()) },
        DesklinkResult::InvalidArgument
    );
}

#[test]
fn ffi_derives_identity_verify_key_and_cancels_connecting_worker_on_destroy() {
    let secret = [91; 32];
    let mut verify_key = [0; 32];
    assert_eq!(
        unsafe { desklink_identity_verify_key(secret.as_ptr(), verify_key.as_mut_ptr()) },
        DesklinkResult::Ok
    );
    let identity = desklink_crypto::DeviceIdentity::from_secret_key([0; 16], &secret);
    assert_eq!(verify_key, *identity.verify_key().as_bytes());

    let callback_count = AtomicUsize::new(0);
    let url = CString::new("quic://127.0.0.1:4433").unwrap();
    let mut handle = null_mut();
    assert_eq!(
        unsafe {
            desklink_create(
                &config(&url),
                Some(count_context_callback),
                (&callback_count as *const AtomicUsize).cast_mut().cast(),
                &mut handle,
            )
        },
        DesklinkResult::Ok
    );
    let server_name = CString::new("localhost").unwrap();
    let secure = DesklinkSecureConnectionConfig {
        server_name: server_name.as_ptr(),
        session_id: [92; 16],
        relay_authentication: [93; 32],
        controller_device_id: [94; 16],
        controller_secret_key: secret,
        host_verify_key: *identity.verify_key().as_bytes(),
    };
    assert_eq!(
        unsafe { desklink_connect_secure(handle, &secure) },
        DesklinkResult::Ok
    );
    unsafe { desklink_destroy(handle) };
    let callbacks_after_destroy = callback_count.load(Ordering::Acquire);
    assert!(callbacks_after_destroy > 0);
    thread::sleep(Duration::from_millis(20));
    assert_eq!(
        callback_count.load(Ordering::Acquire),
        callbacks_after_destroy,
        "the worker must not call Swift after desklink_destroy returns"
    );
}

#[test]
fn ffi_pairing_invite_rejects_invalid_material_before_starting_worker() {
    let (handle, _url) = unsafe { create_handle(None) };
    let host = desklink_crypto::DeviceIdentity::from_secret_key([31; 16], &[32; 32]);
    let controller_secret_key = [33; 32];
    let server_name = CString::new("localhost").unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let invite = desklink_crypto::PairingInvite::new(&host, now, 60).unwrap();
    let encoded = invite.encode().unwrap();
    let mut tampered = encoded.as_bytes().to_vec();
    tampered[21] ^= 0x80;
    let config = |bytes: &[u8]| DesklinkPairingInviteConnectionConfig {
        server_name: server_name.as_ptr(),
        invite: bytes.as_ptr(),
        invite_len: bytes.len(),
        controller_device_id: [34; 16],
        controller_secret_key,
    };
    assert_eq!(
        unsafe { desklink_connect_pairing_invite(handle, &config(&tampered)) },
        DesklinkResult::InvalidArgument
    );
    assert_eq!(
        unsafe {
            desklink_connect_pairing_invite(
                handle,
                &DesklinkPairingInviteConnectionConfig {
                    invite_len: encoded.as_bytes().len() - 1,
                    ..config(encoded.as_bytes())
                },
            )
        },
        DesklinkResult::InvalidArgument
    );

    let expired = desklink_crypto::PairingInvite::new(&host, now.saturating_sub(2), 1).unwrap();
    let expired = expired.encode().unwrap();
    assert_eq!(
        unsafe { desklink_connect_pairing_invite(handle, &config(expired.as_bytes())) },
        DesklinkResult::InvalidArgument
    );
    unsafe { desklink_destroy(handle) };
}

#[test]
fn ffi_pairing_invite_starts_cancellable_worker_from_authenticated_fields() {
    let (handle, _url) = unsafe { create_handle(None) };
    let host = desklink_crypto::DeviceIdentity::from_secret_key([41; 16], &[42; 32]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let invite = desklink_crypto::PairingInvite::new(&host, now, 60).unwrap();
    let encoded = invite.encode().unwrap();
    let server_name = CString::new("localhost").unwrap();
    let config = DesklinkPairingInviteConnectionConfig {
        server_name: server_name.as_ptr(),
        invite: encoded.as_bytes().as_ptr(),
        invite_len: encoded.as_bytes().len(),
        controller_device_id: [43; 16],
        controller_secret_key: [44; 32],
    };
    assert_eq!(
        unsafe { desklink_connect_pairing_invite(handle, &config) },
        DesklinkResult::Ok
    );
    assert_eq!(
        unsafe { desklink_connect_pairing_invite(handle, &config) },
        DesklinkResult::InvalidState
    );
    unsafe { desklink_destroy(handle) };
}

#[test]
fn ffi_dispatches_pairing_control_and_release_events() {
    CALLBACK_COUNT.store(0, Ordering::Relaxed);
    let (handle, _url) = unsafe { create_handle(Some(count_callback)) };
    let mut pairing = Default::default();
    assert_eq!(
        unsafe { desklink_start_pairing(handle, &mut pairing) },
        DesklinkResult::Ok
    );
    let code = unsafe { CStr::from_ptr(pairing.code.as_ptr()) };
    assert_eq!(code.to_bytes().len(), 8);
    assert_eq!(
        unsafe { desklink_connect_with_code(handle, pairing.code.as_ptr()) },
        DesklinkResult::Ok
    );
    assert_eq!(
        unsafe { desklink_request_keyframe(handle) },
        DesklinkResult::Ok
    );
    let input = DesklinkInput {
        kind: DesklinkInputKind::MouseMove,
        x: 0.5,
        y: 0.25,
        wheel_x: 0,
        wheel_y: 0,
        button: 0,
        key_code: 0,
        character: 0,
        pressed: 0,
        modifiers: 0,
    };
    assert_eq!(
        unsafe { desklink_send_input(handle, &input) },
        DesklinkResult::Ok
    );
    let wheel = DesklinkInput {
        kind: DesklinkInputKind::MouseWheel,
        x: 0.0,
        y: 0.0,
        wheel_x: -120,
        wheel_y: 240,
        button: 0,
        key_code: 0,
        character: 0,
        pressed: 0,
        modifiers: 0,
    };
    assert_eq!(
        unsafe { desklink_send_input(handle, &wheel) },
        DesklinkResult::Ok
    );
    assert_eq!(unsafe { desklink_release_all(handle) }, DesklinkResult::Ok);
    assert_eq!(unsafe { desklink_reject(handle) }, DesklinkResult::Ok);
    assert!(CALLBACK_COUNT.load(Ordering::Relaxed) >= 5);
    let _ = DesklinkEventKind::Pairing;
    unsafe { desklink_destroy(handle) };
}
