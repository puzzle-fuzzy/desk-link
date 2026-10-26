//! The checked-in Apple header must describe the Rust library being linked.
//! Compare every exported enum and compile C layout assertions against Rust's
//! actual size/alignment/offset values, rather than comparing two stale headers.
use desklink_ffi::*;

const RUST: &str = include_str!("../src/lib.rs");
const HEADER: &str = include_str!("../include/desklink.h");
const APPLE_HEADER: &str = include_str!("../../../apps/apple/Sources/DeskLinkC/include/desklink.h");

const ENUMS: &[(&str, &str)] = &[
    ("DesklinkResult", "DESKLINK_"),
    ("DesklinkPlatform", "DESKLINK_PLATFORM_"),
    ("DesklinkState", "DESKLINK_"),
    ("DesklinkEventKind", "DESKLINK_EVENT_"),
    ("DesklinkControllerCommand", "DESKLINK_COMMAND_"),
    ("DesklinkHostEventKind", "DESKLINK_HOST_EVENT_"),
    ("DesklinkHostState", "DESKLINK_HOST_"),
    ("DesklinkInputKind", "DESKLINK_INPUT_"),
];

fn entries(source: &str, declaration: &str) -> Vec<(String, u32)> {
    let body = source
        .split_once(declaration)
        .unwrap()
        .1
        .split_once('}')
        .unwrap()
        .0;
    body.lines()
        .filter_map(|line| {
            let (name, value) = line.trim().split_once('=')?;
            Some((
                name.trim().to_owned(),
                value.trim().trim_end_matches(',').parse().unwrap(),
            ))
        })
        .collect()
}

fn normalized(name: &str) -> String {
    name.chars()
        .filter(|character| *character != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

#[test]
fn checked_in_headers_match_all_rust_abi_enums() {
    assert_eq!(
        HEADER, APPLE_HEADER,
        "the Apple C header must be copied from the FFI header"
    );
    for declaration in RUST.split("pub enum ").skip(1) {
        let name = declaration.split_whitespace().next().unwrap();
        if name.starts_with("Desklink") {
            assert!(
                ENUMS.iter().any(|(known, _)| *known == name),
                "add new ABI enum {name} to the synchronization gate"
            );
        }
    }
    for (name, prefix) in ENUMS {
        let rust = entries(RUST, &format!("pub enum {name} {{"));
        let c = entries(HEADER, &format!("typedef enum {name} {{"));
        let rust: Vec<_> = rust
            .into_iter()
            .map(|(name, value)| (normalized(&name), value))
            .collect();
        let c: Vec<_> = c
            .into_iter()
            .map(|(name, value)| (normalized(name.strip_prefix(prefix).unwrap()), value))
            .collect();
        assert!(!rust.is_empty());
        assert_eq!(
            rust, c,
            "{name} changed in Rust without updating the C ABI header"
        );
    }
}

#[cfg(unix)]
#[test]
fn c_compiler_confirms_callback_and_connection_layouts() {
    use std::io::Write as _;
    use std::{
        fmt::Write,
        mem::{align_of, offset_of, size_of},
        process::{Command, Stdio},
    };
    let mut assertions = HEADER.to_owned();
    for (name, _) in ENUMS {
        for (constant, value) in entries(HEADER, &format!("typedef enum {name} {{")) {
            writeln!(
                assertions,
                "_Static_assert({constant} == {value}, \"{constant}\");"
            )
            .unwrap();
        }
    }
    macro_rules! layout {
        ($type:ty, $($field:ident),+ $(,)?) => {{
            let name = stringify!($type);
            writeln!(assertions, "_Static_assert(sizeof({name}) == {}, \"{name} size\");", size_of::<$type>()).unwrap();
            writeln!(assertions, "_Static_assert(_Alignof({name}) == {}, \"{name} alignment\");", align_of::<$type>()).unwrap();
            $(writeln!(assertions, "_Static_assert(offsetof({name}, {}) == {}, \"{name}.{}\");", stringify!($field), offset_of!($type, $field), stringify!($field)).unwrap();)+
        }};
    }
    layout!(DesklinkConfig, relay_url, log_level);
    layout!(
        DesklinkHostConfig,
        relay_url,
        server_name,
        host_device_id,
        host_secret_key,
        log_level
    );
    layout!(
        DesklinkSecureConnectionConfig,
        server_name,
        session_id,
        relay_authentication,
        controller_device_id,
        controller_secret_key,
        host_verify_key
    );
    layout!(
        DesklinkPairingInviteConnectionConfig,
        server_name,
        invite,
        invite_len,
        controller_device_id,
        controller_secret_key
    );
    layout!(
        DesklinkDirectoryConnectionConfig,
        server_name,
        device_id,
        access_code,
        controller_device_id,
        controller_secret_key
    );
    layout!(
        DesklinkSavedHostMaterial,
        session_id,
        relay_authentication,
        host_verify_key,
        server_name
    );
    layout!(DesklinkPairingInfo, session_id, code, expires_at_unix_s);
    layout!(
        DesklinkInput,
        kind,
        x,
        y,
        wheel_x,
        wheel_y,
        button,
        key_code,
        character,
        pressed,
        modifiers
    );
    layout!(
        DesklinkEvent,
        kind,
        data,
        data_len,
        stream_id,
        frame_id,
        config_version,
        width,
        height,
        state
    );
    layout!(
        DesklinkHostInput,
        kind,
        x,
        y,
        wheel_x,
        wheel_y,
        button,
        key_code,
        character,
        pressed,
        modifiers
    );
    layout!(
        DesklinkHostMetrics,
        sent_video_configs,
        sent_video_packets,
        received_input_events,
        keyframe_requests
    );
    layout!(
        DesklinkHostEvent,
        kind,
        state,
        data,
        data_len,
        controller_device_id,
        controller_verify_key,
        fingerprint,
        fingerprint_len,
        input,
        metrics
    );
    let mut child = Command::new("cc")
        .args(["-std=c11", "-Werror", "-fsyntax-only", "-x", "c", "-"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("a C compiler is required to verify the native ABI");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(assertions.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "C/Rust ABI mismatch: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
