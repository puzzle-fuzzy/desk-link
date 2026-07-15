#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(all(not(debug_assertions), not(feature = "custom-protocol")))]
compile_error!(
    "DeskLink release builds must enable the custom-protocol feature so the UI is embedded"
);

fn main() {
    desklink_windows_ui_lib::run();
}
