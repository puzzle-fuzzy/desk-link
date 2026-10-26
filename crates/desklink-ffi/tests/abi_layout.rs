//! Compare the native C compiler's layout with the actual Rust target layout.
use desklink_ffi::*;
use std::{
    fs,
    mem::{align_of, offset_of, size_of},
    process::Command,
};

#[test]
fn c_header_layout_matches_every_public_rust_struct() {
    let mut checks = String::from("#include <desklink.h>\n");
    macro_rules! enum_size { ($($ty:ty),+ $(,)?) => { $(checks.push_str(&format!("_Static_assert(sizeof({}) == {}, \"enum size\");\n", stringify!($ty), size_of::<$ty>())));+ }; }
    enum_size!(
        DesklinkResult,
        DesklinkPlatform,
        DesklinkState,
        DesklinkEventKind,
        DesklinkHostEventKind,
        DesklinkHostState,
        DesklinkInputKind,
        DesklinkControllerCommand
    );
    macro_rules! layout {
        ($ty:ty; $($field:ident),+ $(,)?) => {{
            checks.push_str(&format!("_Static_assert(sizeof({}) == {}, \"size\");\n_Static_assert(_Alignof({}) == {}, \"alignment\");\n", stringify!($ty), size_of::<$ty>(), stringify!($ty), align_of::<$ty>()));
            $(checks.push_str(&format!("_Static_assert(offsetof({}, {}) == {}, \"offset\");\n", stringify!($ty), stringify!($field), offset_of!($ty, $field)));)+
        }};
    }
    layout!(DesklinkConfig; relay_url, log_level);
    layout!(DesklinkHostConfig; relay_url, server_name, host_device_id, host_secret_key, log_level);
    layout!(DesklinkSecureConnectionConfig; server_name, session_id, relay_authentication, controller_device_id, controller_secret_key, host_verify_key, expires_at_unix_s, authorization_type, directory_device_id, directory_access_code);
    layout!(DesklinkPairingInviteConnectionConfig; server_name, invite, invite_len, controller_device_id, controller_secret_key);
    layout!(DesklinkDirectoryConnectionConfig; server_name, device_id, access_code, controller_device_id, controller_secret_key);
    layout!(DesklinkPairingInfo; session_id, code, expires_at_unix_s);
    layout!(DesklinkInput; kind, x, y, wheel_x, wheel_y, button, key_code, character, pressed, modifiers, stream_id, input_epoch);
    layout!(DesklinkEvent; kind, data, data_len, stream_id, frame_id, config_version, width, height, state, error_code, recoverable, input_epoch);
    layout!(DesklinkHostInput; kind, x, y, wheel_x, wheel_y, button, key_code, character, pressed, modifiers);
    layout!(DesklinkHostMetrics; sent_video_configs, sent_video_packets, received_input_events, keyframe_requests);
    layout!(DesklinkHostEvent; kind, state, data, data_len, controller_device_id, controller_verify_key, fingerprint, fingerprint_len, input, metrics, media_generation);
    layout!(DesklinkSavedHostMaterial; session_id, relay_authentication, host_verify_key, server_name, expires_at_unix_s, authorization_type, directory_device_id, directory_access_code);
    checks.push_str("_Static_assert(DESKLINK_OK == 0 && DESKLINK_INVALID_ARGUMENT == 1 && DESKLINK_INVALID_UTF8 == 2 && DESKLINK_INVALID_STATE == 3 && DESKLINK_INTERNAL_ERROR == 4 && DESKLINK_STALE_GENERATION == 5, \"result values\");\n");
    checks.push_str("static DesklinkResult (*config_for_generation)(DesklinkHostHandle *, uint64_t, uint64_t, uint32_t, uint16_t, uint16_t, const uint8_t *, size_t) = desklink_host_send_video_config_for_generation;\n");
    checks.push_str("static DesklinkResult (*access_unit_for_generation)(DesklinkHostHandle *, uint64_t, uint64_t, uint64_t, uint32_t, const uint8_t *, size_t) = desklink_host_send_video_access_unit_for_generation;\n");
    checks.push_str("static DesklinkResult (*cursor_for_generation)(DesklinkHostHandle *, uint64_t, uint64_t, const uint8_t *, size_t) = desklink_host_send_cursor_for_generation;\n");
    let temp = std::env::temp_dir().join(format!("desklink-abi-{}", std::process::id()));
    fs::create_dir_all(&temp).unwrap();
    let source = temp.join("layout.c");
    fs::write(&source, checks).unwrap();
    let include = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("include");
    let output = if cfg!(windows) {
        Command::new("cl")
            .args(["/nologo", "/WX", "/std:c11", "/c"])
            .arg(format!("/I{}", include.display()))
            .arg(&source)
            .arg(format!("/Fo{}", temp.join("layout.obj").display()))
            .output()
    } else {
        Command::new("cc")
            .args(["-std=c11", "-Werror", "-c", "-I"])
            .arg(include)
            .arg(&source)
            .arg("-o")
            .arg(temp.join("layout.o"))
            .output()
    }
    .expect("C compiler is required for ABI layout validation");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(temp).unwrap();
}

#[test]
fn both_generated_headers_are_byte_identical() {
    assert_eq!(
        include_bytes!("../include/desklink.h"),
        include_bytes!("../../../apps/apple/Sources/DeskLinkC/include/desklink.h")
    );
}
