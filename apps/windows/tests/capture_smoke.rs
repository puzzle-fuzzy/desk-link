#[cfg(windows)]
#[test]
#[ignore = "requires access to the interactive Windows desktop"]
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

#[cfg(windows)]
#[test]
#[ignore = "requires access to every interactive Windows display"]
fn every_attached_display_can_be_selected_by_its_reported_id() {
    use apps_windows::capture::{DesktopCapturer, DxgiDesktopCapturer, available_displays};

    let displays = available_displays().expect("display enumeration");
    assert!(!displays.is_empty());
    assert_eq!(displays.iter().filter(|display| display.primary).count(), 1);
    for display in displays {
        let capture = DxgiDesktopCapturer::new_display(display.id).expect("display capture init");
        assert_eq!(capture.display_id(), display.id);
        assert_eq!(capture.desktop_rect(), display.rect);
    }
}
