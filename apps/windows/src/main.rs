#[cfg(windows)]
use apps_windows::capture::DesktopCapturer;
use apps_windows::{
    capture::DxgiDesktopCapturer,
    encoder::H264Encoder,
    input::{InputInjector, VirtualDesktop},
    window::HostApprovalWindow,
};

#[cfg(windows)]
fn main() {
    let capture = DxgiDesktopCapturer::new_primary().expect("primary display");
    let (width, height) = capture.dimensions();
    let _encoder = H264Encoder::new(width.min(1920), height.min(1080), 30).expect("H.264 encoder");
    let _input = InputInjector::new(VirtualDesktop {
        rect: desklink_session::DesktopRect::new(0, 0, width, height),
    });
    let _approval = HostApprovalWindow::new();
}

#[cfg(not(windows))]
fn main() {
    let _ = (
        DxgiDesktopCapturer::new_primary,
        H264Encoder::new,
        InputInjector::new,
        VirtualDesktop::map,
        HostApprovalWindow::new,
    );
}
