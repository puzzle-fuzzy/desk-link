#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(all(windows, not(debug_assertions), not(feature = "custom-protocol")))]
compile_error!(
    "DeskLink release builds must enable the custom-protocol feature so the UI is embedded"
);

#[cfg(windows)]
fn main() {
    desklink_windows_ui_lib::run();
}

#[cfg(not(windows))]
fn main() {}
