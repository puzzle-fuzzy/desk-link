use std::time::{Duration, SystemTime, UNIX_EPOCH};

use desklink_session::DesktopRect;

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
}

#[cfg(windows)]
enum WindowsCaptureBackend {
    Dxgi(windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication),
    Gdi,
}

#[cfg(not(windows))]
pub struct DxgiDesktopCapturer {
    dimensions: (u32, u32),
    desktop_rect: DesktopRect,
    display_id: u32,
    access_lost: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DisplayDescriptor {
    pub id: u32,
    pub name: String,
    pub rect: DesktopRect,
    pub primary: bool,
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
        })
    }

    #[cfg(windows)]
    fn new_gdi_primary() -> Result<Self, CaptureError> {
        let topology = display_topology()?;
        Ok(Self {
            backend: WindowsCaptureBackend::Gdi,
            dimensions: (topology.primary.width, topology.primary.height),
            desktop_rect: topology.primary,
            display_id: 0,
        })
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
        })
    }

    #[cfg(not(windows))]
    pub fn mark_access_lost(&mut self) {
        self.access_lost = true;
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
    let mut display_id = 0;
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
                display_id = display_id.saturating_add(1);
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
    let mut display_id = 0;
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
                if display_id == requested_id {
                    return Ok((adapter, output, description));
                }
                display_id = display_id.saturating_add(1);
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
                    let end = description
                        .DeviceName
                        .iter()
                        .position(|character| *character == 0)
                        .unwrap_or(description.DeviceName.len());
                    displays.push(DisplayDescriptor {
                        id: displays.len() as u32,
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
            let WindowsCaptureBackend::Dxgi(duplication) = &self.backend else {
                std::thread::sleep(timeout.min(Duration::from_millis(33)));
                return capture_gdi_frame(self.desktop_rect);
            };
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            unsafe {
                duplication
                    .AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource)
                    .map_err(map_dxgi_error)?;
            }

            // The desktop-duplication surface is only valid until ReleaseFrame. Copy it to
            // process-owned memory before releasing it; retaining the COM texture and reading it
            // later works on some drivers but produces invalid frames or device errors on others.
            let pixels = resource
                .ok_or_else(|| CaptureError::Native("DXGI returned no desktop resource".into()))
                .and_then(|resource| resource.cast::<ID3D11Texture2D>().map_err(native_error))
                .and_then(|texture| {
                    copy_texture_pixels(&texture, self.dimensions.0, self.dimensions.1)
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
            // Retrying `new_primary` would select DXGI again and loop forever. Once a live DXGI
            // backend fails, deliberately downgrade this runtime to the slower but much more
            // compatible GDI path. A failing GDI backend is terminal for this controller attempt.
            if matches!(&self.backend, WindowsCaptureBackend::Dxgi(_)) {
                *self = Self::new_gdi_display(self.display_id)?;
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
fn copy_texture_pixels(
    texture: &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, CaptureError> {
    use std::slice;

    use windows::Win32::Graphics::{
        Direct3D11::{
            D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_STAGING, ID3D11Texture2D,
        },
        Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB},
    };

    let mut description = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut description) };
    if description.Width != width || description.Height != height {
        return Err(CaptureError::Native(
            "DXGI desktop texture dimensions changed unexpectedly".into(),
        ));
    }
    if !matches!(
        description.Format,
        DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
    ) {
        return Err(CaptureError::Native(format!(
            "unsupported DXGI desktop texture format: {:?}",
            description.Format
        )));
    }

    let device = unsafe { texture.GetDevice() }.map_err(native_error)?;
    let context = unsafe { device.GetImmediateContext() }.map_err(native_error)?;
    description.Usage = D3D11_USAGE_STAGING;
    description.BindFlags = 0;
    description.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    description.MiscFlags = 0;
    let mut staging: Option<ID3D11Texture2D> = None;
    unsafe { device.CreateTexture2D(&description, None, Some(&mut staging)) }
        .map_err(native_error)?;
    let staging = staging
        .ok_or_else(|| CaptureError::Native("D3D11 did not return a staging texture".into()))?;
    unsafe { context.CopyResource(&staging, texture) };

    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe { context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
        .map_err(native_error)?;
    let copied = (|| {
        if mapped.pData.is_null() {
            return Err(CaptureError::Native(
                "D3D11 returned an empty desktop surface".into(),
            ));
        }
        let row_bytes = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| CaptureError::Native("desktop row size overflow".into()))?;
        let height = usize::try_from(height)
            .map_err(|_| CaptureError::Native("desktop height overflow".into()))?;
        let source_pitch = mapped.RowPitch as usize;
        if source_pitch < row_bytes {
            return Err(CaptureError::Native(
                "D3D11 desktop row pitch is smaller than the active image".into(),
            ));
        }
        let total_bytes = row_bytes
            .checked_mul(height)
            .ok_or_else(|| CaptureError::Native("desktop frame size overflow".into()))?;
        let mut pixels = vec![0_u8; total_bytes];
        for row in 0..height {
            let source = unsafe {
                slice::from_raw_parts(mapped.pData.cast::<u8>().add(row * source_pitch), row_bytes)
            };
            let destination = &mut pixels[row * row_bytes..(row + 1) * row_bytes];
            destination.copy_from_slice(source);
        }
        Ok(pixels)
    })();
    unsafe { context.Unmap(&staging, 0) };
    copied
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
