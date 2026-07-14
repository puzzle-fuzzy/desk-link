use std::{
    ffi::{CStr, CString, c_void},
    ptr::{null, null_mut},
    sync::atomic::{AtomicUsize, Ordering},
};

use desklink_ffi::{
    DesklinkConfig, DesklinkEvent, DesklinkEventCallback, DesklinkEventKind, DesklinkHandle,
    DesklinkInput, DesklinkInputKind, DesklinkResult, desklink_connect_with_code, desklink_create,
    desklink_destroy, desklink_reject, desklink_release_all, desklink_request_keyframe,
    desklink_send_input, desklink_start_pairing,
};

static CALLBACK_COUNT: AtomicUsize = AtomicUsize::new(0);

extern "C" fn count_callback(_context: *mut c_void, _event: *const DesklinkEvent) {
    CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
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
    assert_eq!(unsafe { desklink_release_all(handle) }, DesklinkResult::Ok);
    assert_eq!(unsafe { desklink_reject(handle) }, DesklinkResult::Ok);
    assert!(CALLBACK_COUNT.load(Ordering::Relaxed) >= 5);
    let _ = DesklinkEventKind::Pairing;
    unsafe { desklink_destroy(handle) };
}
