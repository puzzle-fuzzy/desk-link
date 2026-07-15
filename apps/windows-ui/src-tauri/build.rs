fn main() {
    println!("cargo:rerun-if-changed=windows.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let windows =
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("windows.manifest"));
    let attributes = tauri_build::Attributes::new().windows_attributes(windows);
    tauri_build::try_build(attributes).expect("failed to build DeskLink Tauri resources");
}
