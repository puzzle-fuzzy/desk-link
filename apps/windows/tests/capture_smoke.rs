#[cfg(windows)]
#[test]
fn primary_display_capture_reports_non_zero_dimensions() {
    use std::time::Duration;

    use apps_windows::capture::{DesktopCapturer, display_topology};

    let topology = display_topology().expect("display topology");
    let mut capture =
        apps_windows::capture::DxgiDesktopCapturer::new_primary().expect("capture init");
    assert_eq!(capture.desktop_rect(), topology.primary);
    assert!(topology.monitor_count >= 1);
    assert!(topology.virtual_desktop.width >= topology.primary.width);
    assert!(topology.virtual_desktop.height >= topology.primary.height);
    let frame = capture
        .next_frame(Duration::from_millis(500))
        .expect("frame");
    assert!(frame.width > 0);
    assert!(frame.height > 0);
}
