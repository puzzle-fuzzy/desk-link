#[cfg(windows)]
#[test]
fn primary_display_capture_reports_non_zero_dimensions() {
    use std::time::Duration;

    use apps_windows::capture::DesktopCapturer;

    let mut capture =
        apps_windows::capture::DxgiDesktopCapturer::new_primary().expect("capture init");
    let frame = capture
        .next_frame(Duration::from_millis(500))
        .expect("frame");
    assert!(frame.width > 0);
    assert!(frame.height > 0);
}
