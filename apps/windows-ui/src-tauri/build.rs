fn main() {
    println!("cargo:rerun-if-changed=windows.manifest");
    let windows =
        tauri_build::WindowsAttributes::new().app_manifest(include_str!("windows.manifest"));
    let attributes = tauri_build::Attributes::new().windows_attributes(windows);
    tauri_build::try_build(attributes).expect("failed to build DeskLink Tauri resources");
}
