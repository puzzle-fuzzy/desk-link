fn main() {
    println!("cargo:rerun-if-changed=assets/installer.rc");
    println!("cargo:rerun-if-changed=assets/installer.manifest");
    println!("cargo:rerun-if-changed=../../apps/windows/assets/desklink.ico");
    println!("cargo:rerun-if-env-changed=DESKLINK_WINDOWS_PAYLOAD");
    println!("cargo:rerun-if-env-changed=DESKLINK_WINDOWS_PAYLOAD_SHA256");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    embed_resource::compile_for(
        "assets/installer.rc",
        ["desklink-installer"],
        embed_resource::NONE,
    )
    .manifest_required()
    .expect("failed to embed DeskLink installer resources");
}
