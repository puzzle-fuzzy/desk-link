fn main() {
    println!("cargo:rerun-if-changed=assets/desklink.rc");
    println!("cargo:rerun-if-changed=assets/desklink.ico");
    println!("cargo:rerun-if-changed=assets/desklink.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    embed_resource::compile_for(
        "assets/desklink.rc",
        ["desklink-windows"],
        embed_resource::NONE,
    )
    .manifest_required()
    .expect("failed to embed DeskLink Windows resources");
}
