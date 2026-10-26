#[cfg(any(windows, test))]
use std::time::Instant;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use desklink_session::DesktopRect;

#[cfg(windows)]
mod readback;
#[cfg(windows)]
use readback::TextureReadback;

#[derive(Clone, Debug)]
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
    pub pixels: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CaptureError {
    NoDisplay,
    Timeout,
    AccessLost,
    Native(String),
}

pub trait DesktopCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<CapturedFrame, CaptureError>;
    fn dimensions(&self) -> (u32, u32);
    fn desktop_rect(&self) -> DesktopRect;
    fn recover(&mut self) -> Result<(), CaptureError>;
}

#[cfg(windows)]
pub struct DxgiDesktopCapturer {
    backend: WindowsCaptureBackend,
    dimensions: (u32, u32),
    desktop_rect: DesktopRect,
    display_id: u32,
    /// Desktop Duplication only reports changed frames. Keep a one-shot
    /// fallback so a static desktop still produces the first remote frame.
    initial_snapshot_pending: bool,
    dxgi_recovery: DxgiRecoverySchedule,
    readback: TextureReadback,
}

#[cfg(windows)]
enum WindowsCaptureBackend {
    Dxgi(windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication),
    Gdi,
}

/// Limit expensive device recreation to a small burst, then leave the working
/// GDI path alone for a cooldown. A successful constructor is not recovery:
/// only a real acquired DXGI frame is enough to switch back.
#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug)]
struct DxgiRecoverySchedule {
    not_before: Instant,
    failed_attempts: u32,
    restored_at: Option<Instant>,
}

#[cfg(any(windows, test))]
impl DxgiRecoverySchedule {
    const FAILURE_BUDGET: u32 = 6;
    const COOLDOWN: Duration = Duration::from_secs(60);
    const STABILITY_WINDOW: Duration = Duration::from_secs(30);

    fn new(now: Instant) -> Self {
        Self {
            not_before: now + Duration::from_secs(1),
            failed_attempts: 0,
            restored_at: None,
        }
    }

    fn probe_due(&self, now: Instant) -> bool {
        now >= self.not_before
    }

    fn failed_probe(&mut self, now: Instant) {
        self.failed_attempts += 1;
        let delay = if self.failed_attempts >= Self::FAILURE_BUDGET {
            self.failed_attempts = 0;
            Self::COOLDOWN
        } else {
            Duration::from_secs(1_u64 << self.failed_attempts)
        };
        self.not_before = now + delay;
    }

    fn restored(&mut self, now: Instant) {
        self.restored_at = Some(now);
    }

    fn fell_back(&mut self, now: Instant) {
        self.not_before = now
            + if self.restored_at.is_some_and(|restored| {
                now.saturating_duration_since(restored) < Self::STABILITY_WINDOW
            }) {
                Self::COOLDOWN
            } else {
                Duration::from_secs(1)
            };
        self.restored_at = None;
    }
}

#[cfg(not(windows))]
pub struct DxgiDesktopCapturer {
    dimensions: (u32, u32),
    desktop_rect: DesktopRect,
    display_id: u32,
    access_lost: bool,
    initial_snapshot_pending: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayDescriptor {
    pub id: u32,
    pub name: String,
    pub rect: DesktopRect,
    pub primary: bool,
}

/// A display ID belongs to a monitor interface for the lifetime of this process,
/// including periods when that monitor is unplugged. Never recycle an ID for a
/// different monitor: a delayed SelectDisplay must fail rather than select a
/// newly attached output which inherited an enumeration position.
#[cfg(any(windows, test))]
#[derive(Default)]
struct DisplayIdentityRegistry {
    identities: Vec<Vec<u16>>,
}

#[cfg(any(windows, test))]
impl DisplayIdentityRegistry {
    const CAPACITY: usize = 1024;

    fn id_for(&mut self, identity: &[u16]) -> Result<u32, CaptureError> {
        let end = identity
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(identity.len());
        if end == 0 {
            return Err(CaptureError::Native(
                "monitor interface identity is empty".into(),
            ));
        }
        let identity = identity[..end]
            .iter()
            .map(|unit| {
                if (u16::from(b'a')..=u16::from(b'z')).contains(unit) {
                    *unit - u16::from(b'a' - b'A')
                } else {
                    *unit
                }
            })
            .collect::<Vec<_>>();
        if let Some(index) = self.identities.iter().position(|known| *known == identity) {
            return Ok(index as u32 + 1);
        }
        if self.identities.len() == Self::CAPACITY {
            return Err(CaptureError::Native(
                "monitor interface identity capacity exhausted".into(),
            ));
        }
        self.identities.push(identity);
        Ok(self.identities.len() as u32)
    }
}

#[cfg(windows)]
fn display_id_for_output(
    description: &windows::Win32::Graphics::Dxgi::DXGI_OUTPUT_DESC,
) -> Result<u32, CaptureError> {
    use std::{
        mem::size_of,
        sync::{Mutex, OnceLock},
    };
    use windows::{
        Win32::{
            Graphics::Gdi::{DISPLAY_DEVICEW, EnumDisplayDevicesW},
            UI::WindowsAndMessaging::EDD_GET_DEVICE_INTERFACE_NAME,
        },
        core::PCWSTR,
    };

    static IDENTITIES: OnceLock<Mutex<DisplayIdentityRegistry>> = OnceLock::new();
    let mut monitor = DISPLAY_DEVICEW {
        cb: size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    // EDD_GET_DEVICE_INTERFACE_NAME gives the per-monitor OS interface path;
    // DeviceName by itself is a temporary desktop source such as DISPLAY1.
    // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-enumdisplaydevicesw
    if !unsafe {
        EnumDisplayDevicesW(
            PCWSTR(description.DeviceName.as_ptr()),
            0,
            &mut monitor,
            EDD_GET_DEVICE_INTERFACE_NAME,
        )
    }
    .as_bool()
    {
        return Err(CaptureError::Native(
            "monitor interface identity is unavailable".into(),
        ));
    }
    IDENTITIES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .id_for(&monitor.DeviceID)
}

impl DxgiDesktopCapturer {
    pub fn new_primary() -> Result<Self, CaptureError> {
        #[cfg(windows)]
        {
            match Self::new_dxgi_primary() {
                Ok(capture) => Ok(capture),
                Err(dxgi_error) => available_displays()
                    .ok()
                    .and_then(|displays| displays.into_iter().find(|display| display.primary))
                    .map_or_else(Self::new_gdi_primary, |display| {
                        Self::new_gdi_display(display.id)
                    })
                    .map_err(|gdi_error| {
                    CaptureError::Native(format!(
                        "DXGI desktop capture failed ({dxgi_error:?}); GDI fallback failed ({gdi_error:?})"
                    ))
                }),
            }
        }

        #[cfg(not(windows))]
        Ok(Self {
            dimensions: (1920, 1080),
            desktop_rect: DesktopRect::new(0, 0, 1920, 1080),
            display_id: 0,
            access_lost: false,
            initial_snapshot_pending: true,
        })
    }

    pub fn new_display(display_id: u32) -> Result<Self, CaptureError> {
        #[cfg(windows)]
        {
            match Self::new_dxgi_display(display_id) {
                Ok(capture) => Ok(capture),
                Err(dxgi_error) => Self::new_gdi_display(display_id).map_err(|gdi_error| {
                    CaptureError::Native(format!(
                        "DXGI display capture failed ({dxgi_error:?}); GDI fallback failed ({gdi_error:?})"
                    ))
                }),
            }
        }

        #[cfg(not(windows))]
        {
            if display_id != 0 {
                return Err(CaptureError::NoDisplay);
            }
            Self::new_primary()
        }
    }

    pub const fn display_id(&self) -> u32 {
        self.display_id
    }

    /// Capture an owned image only when recovery needs a frame on an otherwise
    /// idle desktop. Normal frame production remains driven by DXGI changes.
    pub fn snapshot_frame(&mut self) -> Result<CapturedFrame, CaptureError> {
        #[cfg(windows)]
        {
            capture_gdi_frame(self.desktop_rect)
        }
        #[cfg(not(windows))]
        {
            self.next_frame(Duration::from_millis(1))
        }
    }

    #[cfg(windows)]
    fn new_dxgi_primary() -> Result<Self, CaptureError> {
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL};
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
        };
        use windows::Win32::Graphics::Dxgi::IDXGIOutput1;
        use windows::core::Interface;

        let (display_id, adapter, output, description, device) = unsafe {
            let (display_id, adapter, output, description) = find_primary_output()?;
            let mut device: Option<ID3D11Device> = None;
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None::<&[D3D_FEATURE_LEVEL]>,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .map_err(native_error)?;
            (
                display_id,
                adapter,
                output,
                description,
                device
                    .ok_or_else(|| CaptureError::Native("D3D11 device was not created".into()))?,
            )
        };
        let _ = adapter;
        let output1: IDXGIOutput1 = output.cast().map_err(native_error)?;
        let duplication = unsafe { output1.DuplicateOutput(&device) }.map_err(native_error)?;
        let width =
            (description.DesktopCoordinates.right - description.DesktopCoordinates.left) as u32;
        let height =
            (description.DesktopCoordinates.bottom - description.DesktopCoordinates.top) as u32;
        if width == 0 || height == 0 {
            return Err(CaptureError::NoDisplay);
        }
        Ok(Self {
            backend: WindowsCaptureBackend::Dxgi(duplication),
            dimensions: (width, height),
            desktop_rect: DesktopRect::new(
                description.DesktopCoordinates.left,
                description.DesktopCoordinates.top,
                width,
                height,
            ),
            display_id,
            initial_snapshot_pending: true,
            dxgi_recovery: DxgiRecoverySchedule::new(Instant::now()),
            readback: TextureReadback::default(),
        })
    }

    #[cfg(windows)]
    fn new_dxgi_display(display_id: u32) -> Result<Self, CaptureError> {
        use windows::Win32::Foundation::HMODULE;
        use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL};
        use windows::Win32::Graphics::Direct3D11::{
            D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
        };
        use windows::Win32::Graphics::Dxgi::IDXGIOutput1;
        use windows::core::Interface;

        let (adapter, output, description) = unsafe { find_display_output(display_id)? };
        let mut device: Option<ID3D11Device> = None;
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None::<&[D3D_FEATURE_LEVEL]>,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .map_err(native_error)?;
        let device =
            device.ok_or_else(|| CaptureError::Native("D3D11 device was not created".into()))?;
        let output1: IDXGIOutput1 = output.cast().map_err(native_error)?;
        let duplication = unsafe { output1.DuplicateOutput(&device) }.map_err(native_error)?;
        let width =
            (description.DesktopCoordinates.right - description.DesktopCoordinates.left) as u32;
        let height =
            (description.DesktopCoordinates.bottom - description.DesktopCoordinates.top) as u32;
        if width == 0 || height == 0 {
            return Err(CaptureError::NoDisplay);
        }
        Ok(Self {
            backend: WindowsCaptureBackend::Dxgi(duplication),
            dimensions: (width, height),
            desktop_rect: DesktopRect::new(
                description.DesktopCoordinates.left,
                description.DesktopCoordinates.top,
                width,
                height,
            ),
            display_id,
            initial_snapshot_pending: true,
            dxgi_recovery: DxgiRecoverySchedule::new(Instant::now()),
            readback: TextureReadback::default(),
        })
    }

    #[cfg(windows)]
    fn new_gdi_primary() -> Result<Self, CaptureError> {
        let displays = available_displays()?;
        let primary = displays
            .iter()
            .find(|display| display.primary)
            .or_else(|| displays.first())
            .ok_or(CaptureError::NoDisplay)?;
        Self::new_gdi_display(primary.id)
    }

    #[cfg(windows)]
    fn new_gdi_display(display_id: u32) -> Result<Self, CaptureError> {
        let display = available_displays()?
            .into_iter()
            .find(|display| display.id == display_id)
            .ok_or(CaptureError::NoDisplay)?;
        Ok(Self {
            backend: WindowsCaptureBackend::Gdi,
            dimensions: (display.rect.width, display.rect.height),
            desktop_rect: display.rect,
            display_id,
            initial_snapshot_pending: false,
            dxgi_recovery: DxgiRecoverySchedule::new(Instant::now()),
            readback: TextureReadback::default(),
        })
    }

    #[cfg(not(windows))]
    pub fn mark_access_lost(&mut self) {
        self.access_lost = true;
    }

    #[cfg(windows)]
    fn try_restore_dxgi(&mut self) -> Option<CapturedFrame> {
        let now = Instant::now();
        if !self.dxgi_recovery.probe_due(now) {
            return None;
        }
        let recovered = Self::new_dxgi_display(self.display_id).and_then(|mut candidate| {
            if candidate.desktop_rect != self.desktop_rect {
                // Topology changes need the runtime's coordinated encoder/input
                // reconfiguration. A backend probe must not change coordinates
                // behind that owner, even when it finds a usable duplication.
                return Err(CaptureError::Native(
                    "DXGI recovery awaits coordinated display topology refresh".into(),
                ));
            }
            // Do not mistake the initial GDI snapshot for a successful DXGI probe.
            candidate.initial_snapshot_pending = false;
            let frame = candidate.next_frame(Duration::from_millis(1))?;
            Ok((candidate, frame))
        });
        match recovered {
            Ok((mut candidate, frame)) => {
                candidate.dxgi_recovery = self.dxgi_recovery;
                candidate.dxgi_recovery.restored(Instant::now());
                *self = candidate;
                Some(frame)
            }
            Err(_) => {
                self.dxgi_recovery.failed_probe(Instant::now());
                None
            }
        }
    }
}

#[cfg(windows)]
unsafe fn find_primary_output() -> Result<
    (
        u32,
        windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
        windows::Win32::Graphics::Dxgi::IDXGIOutput,
        windows::Win32::Graphics::Dxgi::DXGI_OUTPUT_DESC,
    ),
    CaptureError,
> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput,
    };

    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(native_error)?;
    let mut fallback: Option<(u32, IDXGIAdapter1, IDXGIOutput, _)> = None;
    let mut adapter_index = 0;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(native_error(error)),
        };
        let mut output_index = 0;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(native_error(error)),
            };
            let description = unsafe { output.GetDesc() }.map_err(native_error)?;
            if description.AttachedToDesktop.as_bool() {
                let display_id = display_id_for_output(&description)?;
                let coordinates = description.DesktopCoordinates;
                let contains_primary_origin = coordinates.left <= 0
                    && coordinates.top <= 0
                    && coordinates.right > 0
                    && coordinates.bottom > 0;
                if contains_primary_origin {
                    return Ok((display_id, adapter, output, description));
                }
                if fallback.is_none() {
                    fallback = Some((display_id, adapter.clone(), output, description));
                }
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    fallback.ok_or(CaptureError::NoDisplay)
}

#[cfg(windows)]
unsafe fn find_display_output(
    requested_id: u32,
) -> Result<
    (
        windows::Win32::Graphics::Dxgi::IDXGIAdapter1,
        windows::Win32::Graphics::Dxgi::IDXGIOutput,
        windows::Win32::Graphics::Dxgi::DXGI_OUTPUT_DESC,
    ),
    CaptureError,
> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1};

    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(native_error)?;
    let mut adapter_index = 0;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(native_error(error)),
        };
        let mut output_index = 0;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(native_error(error)),
            };
            let description = unsafe { output.GetDesc() }.map_err(native_error)?;
            if description.AttachedToDesktop.as_bool() {
                let display_id = display_id_for_output(&description)?;
                if display_id == requested_id {
                    return Ok((adapter, output, description));
                }
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    Err(CaptureError::NoDisplay)
}

#[cfg(windows)]
pub fn available_displays() -> Result<Vec<DisplayDescriptor>, CaptureError> {
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND, IDXGIFactory1};

    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(native_error)?;
    let mut displays = Vec::new();
    let mut adapter_index = 0;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(native_error(error)),
        };
        let mut output_index = 0;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(error) => return Err(native_error(error)),
            };
            let description = unsafe { output.GetDesc() }.map_err(native_error)?;
            if description.AttachedToDesktop.as_bool() {
                let coordinates = description.DesktopCoordinates;
                let width = (coordinates.right - coordinates.left) as u32;
                let height = (coordinates.bottom - coordinates.top) as u32;
                if width > 0 && height > 0 {
                    let display_id = display_id_for_output(&description)?;
                    if displays
                        .iter()
                        .any(|display: &DisplayDescriptor| display.id == display_id)
                    {
                        return Err(CaptureError::Native(
                            "multiple outputs share a monitor interface identity".into(),
                        ));
                    }
                    let end = description
                        .DeviceName
                        .iter()
                        .position(|character| *character == 0)
                        .unwrap_or(description.DeviceName.len());
                    displays.push(DisplayDescriptor {
                        id: display_id,
                        name: String::from_utf16_lossy(&description.DeviceName[..end]),
                        rect: DesktopRect::new(coordinates.left, coordinates.top, width, height),
                        primary: coordinates.left <= 0
                            && coordinates.top <= 0
                            && coordinates.right > 0
                            && coordinates.bottom > 0,
                    });
                }
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    if displays.is_empty() {
        Err(CaptureError::NoDisplay)
    } else {
        Ok(displays)
    }
}

#[cfg(not(windows))]
pub fn available_displays() -> Result<Vec<DisplayDescriptor>, CaptureError> {
    Ok(vec![DisplayDescriptor {
        id: 0,
        name: "DISPLAY1".into(),
        rect: DesktopRect::new(0, 0, 1920, 1080),
        primary: true,
    }])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayTopology {
    pub monitor_count: u32,
    pub primary: DesktopRect,
    pub virtual_desktop: DesktopRect,
}

#[cfg(windows)]
pub fn display_topology() -> Result<DisplayTopology, CaptureError> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CMONITORS, SM_CXSCREEN, SM_CXVIRTUALSCREEN, SM_CYSCREEN,
        SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    let monitor_count = unsafe { GetSystemMetrics(SM_CMONITORS) };
    let primary_width = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let primary_height = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let virtual_left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let virtual_top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let virtual_width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let virtual_height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    if monitor_count <= 0
        || primary_width <= 0
        || primary_height <= 0
        || virtual_width <= 0
        || virtual_height <= 0
    {
        return Err(CaptureError::NoDisplay);
    }
    Ok(DisplayTopology {
        monitor_count: monitor_count as u32,
        primary: DesktopRect::new(0, 0, primary_width as u32, primary_height as u32),
        virtual_desktop: DesktopRect::new(
            virtual_left,
            virtual_top,
            virtual_width as u32,
            virtual_height as u32,
        ),
    })
}

#[cfg(not(windows))]
pub fn display_topology() -> Result<DisplayTopology, CaptureError> {
    Ok(DisplayTopology {
        monitor_count: 1,
        primary: DesktopRect::new(0, 0, 1920, 1080),
        virtual_desktop: DesktopRect::new(0, 0, 1920, 1080),
    })
}

impl DesktopCapturer for DxgiDesktopCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<CapturedFrame, CaptureError> {
        #[cfg(windows)]
        {
            use windows::Win32::Graphics::{
                Direct3D11::ID3D11Texture2D,
                Dxgi::{DXGI_OUTDUPL_FRAME_INFO, IDXGIResource},
            };
            use windows::core::Interface;

            if timeout.is_zero() {
                return Err(CaptureError::Timeout);
            }
            let timeout_ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
            if matches!(&self.backend, WindowsCaptureBackend::Gdi) {
                if let Some(frame) = self.try_restore_dxgi() {
                    return Ok(frame);
                }
                std::thread::sleep(timeout.min(Duration::from_millis(33)));
                return capture_gdi_frame(self.desktop_rect);
            }
            let WindowsCaptureBackend::Dxgi(duplication) = &self.backend else {
                unreachable!()
            };
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            let acquired =
                unsafe { duplication.AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource) };
            if let Err(error) = acquired {
                if error.code() == windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAIT_TIMEOUT
                    && self.initial_snapshot_pending
                {
                    // DXGI emits no frame until the desktop changes. Take a
                    // single GDI snapshot for the initial keyframe, then
                    // return to the low-overhead duplication path.
                    self.initial_snapshot_pending = false;
                    return capture_gdi_frame(self.desktop_rect);
                }
                return Err(map_dxgi_error(error));
            }
            self.initial_snapshot_pending = false;

            // The desktop-duplication surface is only valid until ReleaseFrame. Copy it to
            // process-owned memory before releasing it; retaining the COM texture and reading it
            // later works on some drivers but produces invalid frames or device errors on others.
            let pixels = resource
                .ok_or_else(|| CaptureError::Native("DXGI returned no desktop resource".into()))
                .and_then(|resource| resource.cast::<ID3D11Texture2D>().map_err(native_error))
                .and_then(|texture| {
                    self.readback
                        .copy_pixels(&texture, self.dimensions.0, self.dimensions.1)
                });
            let release_result = unsafe { duplication.ReleaseFrame() };
            let pixels = pixels?;
            release_result.map_err(map_dxgi_error)?;
            Ok(CapturedFrame {
                width: self.dimensions.0,
                height: self.dimensions.1,
                timestamp_us: now_micros(),
                pixels,
            })
        }

        #[cfg(not(windows))]
        {
            if self.access_lost {
                self.access_lost = false;
                return Err(CaptureError::AccessLost);
            }
            if timeout.is_zero() {
                return Err(CaptureError::Timeout);
            }
            Ok(CapturedFrame {
                width: self.dimensions.0,
                height: self.dimensions.1,
                timestamp_us: now_micros(),
                pixels: Vec::new(),
            })
        }
    }

    fn dimensions(&self) -> (u32, u32) {
        self.dimensions
    }

    fn desktop_rect(&self) -> DesktopRect {
        self.desktop_rect
    }

    fn recover(&mut self) -> Result<(), CaptureError> {
        #[cfg(windows)]
        {
            // A DXGI duplication object can be created successfully and still fail on the
            // first acquired frame on hybrid-GPU, locked, RDP, or recently resumed desktops.
            // Retain reliable GDI capture while bounded, backed-off probes try a real DXGI
            // frame. A constructor alone is insufficient evidence that a driver recovered.
            // A failing GDI backend remains terminal for this controller attempt.
            if matches!(&self.backend, WindowsCaptureBackend::Dxgi(_)) {
                let mut fallback = Self::new_gdi_display(self.display_id)?;
                fallback.dxgi_recovery = self.dxgi_recovery;
                fallback.dxgi_recovery.fell_back(Instant::now());
                *self = fallback;
            } else {
                return Err(CaptureError::Native(
                    "GDI desktop capture failed after DXGI fallback".into(),
                ));
            }
        }
        #[cfg(not(windows))]
        {
            self.access_lost = false;
        }
        Ok(())
    }
}

#[cfg(windows)]
fn capture_gdi_frame(desktop: DesktopRect) -> Result<CapturedFrame, CaptureError> {
    use std::{ffi::c_void, mem::size_of, ptr, slice};

    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC,
        CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HGDIOBJ, ReleaseDC,
        SRCCOPY, SelectObject,
    };

    let width = i32::try_from(desktop.width)
        .map_err(|_| CaptureError::Native("GDI desktop width is too large".into()))?;
    let height = i32::try_from(desktop.height)
        .map_err(|_| CaptureError::Native("GDI desktop height is too large".into()))?;
    let row_bytes = usize::try_from(desktop.width)
        .ok()
        .and_then(|width| width.checked_mul(4))
        .ok_or_else(|| CaptureError::Native("GDI desktop row size overflow".into()))?;
    let total_bytes = row_bytes
        .checked_mul(desktop.height as usize)
        .ok_or_else(|| CaptureError::Native("GDI desktop frame size overflow".into()))?;

    let screen = unsafe { GetDC(None) };
    if screen.0.is_null() {
        return Err(CaptureError::Native(
            "GetDC failed for the Windows desktop".into(),
        ));
    }
    let memory = unsafe { CreateCompatibleDC(Some(screen)) };
    if memory.0.is_null() {
        unsafe {
            ReleaseDC(None, screen);
        }
        return Err(CaptureError::Native(
            "CreateCompatibleDC failed for the Windows desktop".into(),
        ));
    }

    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // A negative height creates a top-down BGRA image, matching DeskLink's input format.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = ptr::null_mut();
    let bitmap = unsafe {
        CreateDIBSection(
            Some(screen),
            &bitmap_info,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
    }
    .map_err(|error| {
        unsafe {
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
        }
        CaptureError::Native(format!("CreateDIBSection failed: {error}"))
    })?;
    let previous = unsafe { SelectObject(memory, HGDIOBJ(bitmap.0)) };
    let captured = (|| {
        if previous.0.is_null() || bits.is_null() {
            return Err(CaptureError::Native(
                "GDI could not select the desktop capture bitmap".into(),
            ));
        }
        unsafe {
            BitBlt(
                memory,
                0,
                0,
                width,
                height,
                Some(screen),
                desktop.left,
                desktop.top,
                SRCCOPY | CAPTUREBLT,
            )
        }
        .map_err(|error| CaptureError::Native(format!("BitBlt failed: {error}")))?;
        Ok(unsafe { slice::from_raw_parts(bits.cast::<u8>(), total_bytes) }.to_vec())
    })();
    unsafe {
        if !previous.0.is_null() {
            let _ = SelectObject(memory, previous);
        }
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(memory);
        ReleaseDC(None, screen);
    }
    captured.map(|pixels| CapturedFrame {
        width: desktop.width,
        height: desktop.height,
        timestamp_us: now_micros(),
        pixels,
    })
}

#[cfg(windows)]
fn native_error(error: windows::core::Error) -> CaptureError {
    CaptureError::Native(error.to_string())
}

#[cfg(windows)]
fn map_dxgi_error(error: windows::core::Error) -> CaptureError {
    use windows::Win32::Graphics::Dxgi::{
        DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET,
        DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_WAIT_TIMEOUT,
    };

    match error.code() {
        DXGI_ERROR_WAIT_TIMEOUT => CaptureError::Timeout,
        DXGI_ERROR_ACCESS_LOST
        | DXGI_ERROR_DEVICE_REMOVED
        | DXGI_ERROR_DEVICE_RESET
        | DXGI_ERROR_SESSION_DISCONNECTED => CaptureError::AccessLost,
        _ => native_error(error),
    }
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration
                .as_secs()
                .saturating_mul(1_000_000)
                .saturating_add(u64::from(duration.subsec_micros()))
        })
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    fn identity(name: &str) -> Vec<u16> {
        name.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[test]
    fn output_ids_survive_enumeration_reordering_and_primary_changes() {
        let mut registry = DisplayIdentityRegistry::default();
        let a = identity("monitor-interface-a");
        let b = identity("monitor-interface-b");
        let initial = [registry.id_for(&a).unwrap(), registry.id_for(&b).unwrap()];
        let reordered = [registry.id_for(&b).unwrap(), registry.id_for(&a).unwrap()];
        assert_eq!(reordered, [initial[1], initial[0]]);
        assert_ne!(initial[0], initial[1]);
        assert_ne!(initial[0], 0);
    }

    #[test]
    fn detached_output_id_is_not_reused_and_reinsert_keeps_its_identity() {
        let mut registry = DisplayIdentityRegistry::default();
        let a = registry.id_for(&identity("monitor-a")).unwrap();
        // A disappears while B is enumerated on its former source/position.
        let b = registry.id_for(&identity("monitor-b")).unwrap();
        assert_ne!(a, b);
        assert_eq!(registry.id_for(&identity("MONITOR-A")).unwrap(), a);
        assert_eq!(registry.identities.len(), 2);
    }

    #[test]
    fn identity_exhaustion_rejects_new_outputs_without_reassigning_known_ones() {
        let mut registry = DisplayIdentityRegistry::default();
        assert!(registry.id_for(&[0]).is_err());
        for index in 0..DisplayIdentityRegistry::CAPACITY {
            assert_eq!(
                registry
                    .id_for(&identity(&format!("monitor-{index}")))
                    .unwrap(),
                index as u32 + 1
            );
        }
        assert!(registry.id_for(&identity("another-monitor")).is_err());
        assert_eq!(registry.id_for(&identity("monitor-0")).unwrap(), 1);
        assert_eq!(registry.identities.len(), DisplayIdentityRegistry::CAPACITY);
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;

    #[test]
    fn dxgi_probe_failures_back_off_and_enter_a_bounded_burst_cooldown() {
        let start = Instant::now();
        let mut schedule = DxgiRecoverySchedule::new(start);
        assert!(!schedule.probe_due(start));
        let mut now = start + Duration::from_secs(1);
        for delay in [2, 4, 8, 16, 32, 60] {
            assert!(schedule.probe_due(now));
            schedule.failed_probe(now);
            assert!(!schedule.probe_due(now + Duration::from_secs(delay - 1)));
            now += Duration::from_secs(delay);
        }
        // The next bounded retry burst is available after the quiet cooldown;
        // a desktop restored much later need not start a new remote session.
        assert!(schedule.probe_due(now));
        schedule.failed_probe(now);
        assert!(!schedule.probe_due(now + Duration::from_secs(1)));
        assert!(schedule.probe_due(now + Duration::from_secs(2)));
    }

    #[test]
    fn unstable_restoration_cannot_oscillate_at_the_normal_probe_rate() {
        let start = Instant::now();
        let mut schedule = DxgiRecoverySchedule::new(start);
        schedule.restored(start);
        let failed_at = start + Duration::from_secs(2);
        schedule.fell_back(failed_at);
        assert!(!schedule.probe_due(failed_at + Duration::from_secs(59)));
        assert!(schedule.probe_due(failed_at + Duration::from_secs(60)));
    }

    #[test]
    fn stable_backend_failure_can_begin_a_new_probe_burst() {
        let start = Instant::now();
        let mut schedule = DxgiRecoverySchedule::new(start);
        schedule.restored(start);
        let failed_at = start + DxgiRecoverySchedule::STABILITY_WINDOW;
        schedule.fell_back(failed_at);
        assert!(!schedule.probe_due(failed_at));
        assert!(schedule.probe_due(failed_at + Duration::from_secs(1)));
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Dxgi::{DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT};

    #[test]
    fn dxgi_timeout_and_access_loss_have_stable_recovery_errors() {
        assert_eq!(
            map_dxgi_error(windows::core::Error::from_hresult(DXGI_ERROR_WAIT_TIMEOUT)),
            CaptureError::Timeout
        );
        assert_eq!(
            map_dxgi_error(windows::core::Error::from_hresult(DXGI_ERROR_ACCESS_LOST)),
            CaptureError::AccessLost
        );
    }

    #[test]
    #[ignore = "requires access to the interactive Windows desktop"]
    fn gdi_fallback_returns_an_owned_bgra_frame() {
        let desktop = display_topology().unwrap().primary;
        let frame = capture_gdi_frame(desktop).unwrap();
        assert_eq!((frame.width, frame.height), (desktop.width, desktop.height));
        assert_eq!(
            frame.pixels.len(),
            desktop.width as usize * desktop.height as usize * 4
        );
        assert_ne!(frame.pixels.as_ptr(), std::ptr::null());
    }
}
