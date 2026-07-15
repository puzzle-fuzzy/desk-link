use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/installer.rc");
    println!("cargo:rerun-if-changed=assets/installer.manifest");
    println!("cargo:rerun-if-changed=../../apps/windows/assets/desklink.ico");
    println!("cargo:rerun-if-env-changed=DESKLINK_WINDOWS_UI_PAYLOAD");
    println!("cargo:rerun-if-env-changed=DESKLINK_WINDOWS_UI_PAYLOAD_SHA256");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var_os("CARGO_FEATURE_EMBEDDED_PAYLOAD").is_some() {
        resolve_payload(
            "DESKLINK_WINDOWS_UI_PAYLOAD",
            "DESKLINK_WINDOWS_UI_PAYLOAD_RESOLVED",
            "desklink-ui-payload.empty",
        );
    }
    embed_resource::compile_for(
        "assets/installer.rc",
        ["desklink-installer"],
        embed_resource::NONE,
    )
    .manifest_required()
    .expect("failed to embed DeskLink installer resources");
}

fn resolve_payload(source_variable: &str, resolved_variable: &str, fallback_name: &str) {
    let path = if let Some(path) = env::var_os(source_variable).map(PathBuf::from) {
        assert!(
            path.is_file(),
            "{source_variable} does not point to a readable payload file"
        );
        path
    } else {
        let path = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not provide OUT_DIR"))
            .join(fallback_name);
        fs::write(&path, []).expect("failed to create the empty verification payload");
        println!(
            "cargo:warning={source_variable} is unset; building a non-installable verification payload"
        );
        path
    };
    println!(
        "cargo:rustc-env={resolved_variable}={}",
        path.to_str()
            .expect("DeskLink installer payload path must be valid Unicode")
    );
}
