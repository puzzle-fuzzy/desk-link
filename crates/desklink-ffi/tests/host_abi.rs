use std::{
    ffi::{CString, c_void},
    ptr::{null, null_mut},
    sync::{Mutex, OnceLock},
};

use desklink_ffi::{
    DesklinkConfig, DesklinkHostConfig, DesklinkHostEvent, DesklinkHostEventCallback,
    DesklinkHostEventKind, DesklinkHostHandle, DesklinkResult, desklink_host_approve,
    desklink_host_create, desklink_host_destroy, desklink_host_release_all,
    desklink_host_send_cursor, desklink_host_send_cursor_for_generation,
    desklink_host_send_video_access_unit, desklink_host_send_video_access_unit_for_generation,
    desklink_host_send_video_config, desklink_host_send_video_config_for_generation,
    desklink_host_start_from_invite, desklink_host_start_pairing,
};

static CALLBACK_EVENTS: OnceLock<Mutex<Vec<DesklinkHostEventKind>>> = OnceLock::new();

extern "C" fn record_event(_context: *mut c_void, event: *const DesklinkHostEvent) {
    let event = unsafe { &*event };
    CALLBACK_EVENTS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .push(event.kind);
}

fn host_config(relay_url: &CString, server_name: &CString) -> DesklinkHostConfig {
    DesklinkHostConfig {
        relay_url: relay_url.as_ptr(),
        server_name: server_name.as_ptr(),
        host_device_id: [7; 16],
        host_secret_key: [9; 32],
        log_level: 1,
    }
}

unsafe fn create_host() -> (*mut DesklinkHostHandle, CString, CString) {
    let relay_url = CString::new("quic://127.0.0.1:1").unwrap();
    let server_name = CString::new("localhost").unwrap();
    let mut handle = null_mut();
    assert_eq!(
        unsafe {
            desklink_host_create(
                &host_config(&relay_url, &server_name),
                Some(record_event as DesklinkHostEventCallback),
                null_mut(),
                &mut handle,
            )
        },
        DesklinkResult::Ok
    );
    (handle, relay_url, server_name)
}

#[test]
fn host_abi_rejects_null_and_wrong_invite_lengths() {
    assert_eq!(
        unsafe { desklink_host_create(null(), None, null_mut(), null_mut()) },
        DesklinkResult::InvalidArgument
    );

    let (handle, _relay_url, _server_name) = unsafe { create_host() };
    let mut invite = [0; desklink_crypto::PAIRING_INVITE_BYTES];
    let mut invite_len = 0;
    let mut expires_at = 0;
    assert_eq!(
        unsafe {
            desklink_host_start_pairing(
                handle,
                invite.as_mut_ptr(),
                invite.len() - 1,
                &mut invite_len,
                &mut expires_at,
            )
        },
        DesklinkResult::InvalidArgument
    );
    assert_eq!(
        unsafe { desklink_host_start_from_invite(handle, invite.as_ptr(), invite.len() - 1) },
        DesklinkResult::InvalidArgument
    );
    unsafe { desklink_host_destroy(handle) };
}

#[test]
fn host_abi_validates_invites_approval_and_media_arguments() {
    let (handle, _relay_url, _server_name) = unsafe { create_host() };
    let invalid_invite = [0; desklink_crypto::PAIRING_INVITE_BYTES];
    assert_eq!(
        unsafe {
            desklink_host_start_from_invite(handle, invalid_invite.as_ptr(), invalid_invite.len())
        },
        DesklinkResult::InvalidArgument
    );
    assert_eq!(
        unsafe { desklink_host_approve(handle, null(), [3; 32].as_ptr()) },
        DesklinkResult::InvalidArgument
    );
    assert_eq!(
        unsafe { desklink_host_send_video_config(handle, 1, 1, 1, 1, null(), 1) },
        DesklinkResult::InvalidArgument
    );
    assert_eq!(
        unsafe { desklink_host_send_video_config(handle, 1, 1, 1, 1, null(), 0) },
        DesklinkResult::InvalidArgument
    );
    assert_eq!(
        unsafe { desklink_host_send_video_access_unit(handle, 1, 1, 1, null(), 1) },
        DesklinkResult::InvalidArgument
    );
    assert_eq!(
        unsafe { desklink_host_send_cursor(handle, 1, null(), 1) },
        DesklinkResult::InvalidArgument
    );
    unsafe { desklink_host_destroy(handle) };
}

#[test]
fn scoped_media_abi_preserves_signatures_and_validates_arguments() {
    let _: unsafe extern "C" fn(
        *mut DesklinkHostHandle,
        u64,
        u64,
        u32,
        u16,
        u16,
        *const u8,
        usize,
    ) -> DesklinkResult = desklink_host_send_video_config_for_generation;
    let _: unsafe extern "C" fn(
        *mut DesklinkHostHandle,
        u64,
        u64,
        u64,
        u32,
        *const u8,
        usize,
    ) -> DesklinkResult = desklink_host_send_video_access_unit_for_generation;
    let _: unsafe extern "C" fn(
        *mut DesklinkHostHandle,
        u64,
        u64,
        *const u8,
        usize,
    ) -> DesklinkResult = desklink_host_send_cursor_for_generation;
    assert_eq!(DesklinkResult::Ok as i32, 0);
    assert_eq!(DesklinkResult::InvalidArgument as i32, 1);
    assert_eq!(DesklinkResult::InvalidUtf8 as i32, 2);
    assert_eq!(DesklinkResult::InvalidState as i32, 3);
    assert_eq!(DesklinkResult::InternalError as i32, 4);
    assert_eq!(DesklinkResult::StaleGeneration as i32, 5);
    let (handle, _relay_url, _server_name) = unsafe { create_host() };
    let payload = [0, 0, 1, 0x67];
    for invalid_handle in [null_mut(), handle] {
        assert_eq!(
            unsafe {
                desklink_host_send_video_config_for_generation(
                    invalid_handle,
                    1,
                    1,
                    1,
                    1,
                    1,
                    null(),
                    1,
                )
            },
            DesklinkResult::InvalidArgument
        );
        assert_eq!(
            unsafe {
                desklink_host_send_video_access_unit_for_generation(
                    invalid_handle,
                    1,
                    1,
                    1,
                    1,
                    null(),
                    1,
                )
            },
            DesklinkResult::InvalidArgument
        );
        assert_eq!(
            unsafe { desklink_host_send_cursor_for_generation(invalid_handle, 1, 1, null(), 1) },
            DesklinkResult::InvalidArgument
        );
    }
    assert_eq!(
        unsafe { desklink_host_send_video_config_for_generation(handle, 1, 1, 1, 1, 1, null(), 0) },
        DesklinkResult::InvalidArgument
    );
    assert_eq!(
        unsafe {
            desklink_host_send_video_access_unit_for_generation(handle, 1, 1, 1, 1, null(), 0)
        },
        DesklinkResult::InvalidArgument
    );
    assert_eq!(
        unsafe { desklink_host_send_cursor_for_generation(handle, 1, 1, null(), 0) },
        DesklinkResult::InvalidArgument
    );
    // A live idle handle owns no producer grant; valid payloads fail closed
    // without starting a transport or manufacturing a generation.
    for generation in [0, 1] {
        assert_eq!(
            unsafe {
                desklink_host_send_video_config_for_generation(
                    handle,
                    generation,
                    1,
                    1,
                    1,
                    1,
                    payload.as_ptr(),
                    payload.len(),
                )
            },
            DesklinkResult::StaleGeneration
        );
        assert_eq!(
            unsafe {
                desklink_host_send_video_access_unit_for_generation(
                    handle,
                    generation,
                    1,
                    1,
                    1,
                    payload.as_ptr(),
                    payload.len(),
                )
            },
            DesklinkResult::StaleGeneration
        );
        assert_eq!(
            unsafe {
                desklink_host_send_cursor_for_generation(
                    handle,
                    generation,
                    1,
                    payload.as_ptr(),
                    payload.len(),
                )
            },
            DesklinkResult::StaleGeneration
        );
    }
    unsafe { desklink_host_destroy(handle) };
}

#[test]
fn host_abi_destroy_emits_release_all() {
    CALLBACK_EVENTS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap()
        .clear();
    let (handle, _relay_url, _server_name) = unsafe { create_host() };
    assert_eq!(
        unsafe { desklink_host_release_all(handle) },
        DesklinkResult::Ok
    );
    unsafe { desklink_host_destroy(handle) };
    assert!(
        CALLBACK_EVENTS
            .get()
            .unwrap()
            .lock()
            .unwrap()
            .contains(&DesklinkHostEventKind::ReleaseAll)
    );
}

#[test]
fn controller_saved_material_is_not_exported_before_authenticated_connection() {
    let relay_url = CString::new("quic://127.0.0.1:1").unwrap();
    let config = DesklinkConfig {
        relay_url: relay_url.as_ptr(),
        log_level: 0,
    };
    let mut handle = null_mut();
    assert_eq!(
        unsafe { desklink_ffi::desklink_create(&config, None, null_mut(), &mut handle) },
        DesklinkResult::Ok
    );
    let mut material = desklink_ffi::DesklinkSavedHostMaterial {
        session_id: [0; 16],
        relay_authentication: [0; 32],
        host_verify_key: [0; 32],
        server_name: [0; 256],
        expires_at_unix_s: 0,
        authorization_type: 0,
        directory_device_id: 0,
        directory_access_code: [0; 8],
    };
    assert_eq!(
        unsafe {
            desklink_ffi::desklink_controller_copy_saved_host_material(handle, &mut material)
        },
        DesklinkResult::InvalidState
    );
    unsafe { desklink_ffi::desklink_destroy(handle) };
}
