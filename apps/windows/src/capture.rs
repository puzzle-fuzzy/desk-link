use std::time::{Duration, SystemTime, UNIX_EPOCH};

use desklink_session::DesktopRect;

#[derive(Clone, Debug)]
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
    pub pixels: Vec<u8>,
    #[cfg(windows)]
    pub texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
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
    duplication: windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication,
    dimensions: (u32, u32),
    desktop_rect: DesktopRect,
}

#[cfg(not(windows))]
pub struct DxgiDesktopCapturer {
    dimensions: (u32, u32),
    desktop_rect: DesktopRect,
    access_lost: bool,
}

impl DxgiDesktopCapturer {
    pub fn new_primary() -> Result<Self, CaptureError> {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HMODULE;
            use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL};
            use windows::Win32::Graphics::Direct3D11::{
                D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11CreateDevice, ID3D11Device,
            };
            use windows::Win32::Graphics::Dxgi::IDXGIOutput1;
            use windows::core::Interface;

            let (adapter, output, description, device) = unsafe {
                let (adapter, output, description) = find_primary_output()?;
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
                    adapter,
                    output,
                    description,
                    device.ok_or_else(|| {
                        CaptureError::Native("D3D11 device was not created".into())
                    })?,
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
                duplication,
                dimensions: (width, height),
                desktop_rect: DesktopRect::new(
                    description.DesktopCoordinates.left,
                    description.DesktopCoordinates.top,
                    width,
                    height,
                ),
            })
        }

        #[cfg(not(windows))]
        Ok(Self {
            dimensions: (1920, 1080),
            desktop_rect: DesktopRect::new(0, 0, 1920, 1080),
            access_lost: false,
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
    let mut fallback: Option<(IDXGIAdapter1, IDXGIOutput, _)> = None;
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
                    return Ok((adapter, output, description));
                }
                if fallback.is_none() {
                    fallback = Some((adapter.clone(), output, description));
                }
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    fallback.ok_or(CaptureError::NoDisplay)
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
            use windows::Win32::Graphics::Dxgi::{DXGI_OUTDUPL_FRAME_INFO, IDXGIResource};
            use windows::core::Interface;

            if timeout.is_zero() {
                return Err(CaptureError::Timeout);
            }
            let timeout_ms = timeout.as_millis().min(u128::from(u32::MAX)) as u32;
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            unsafe {
                self.duplication
                    .AcquireNextFrame(timeout_ms, &mut frame_info, &mut resource)
                    .map_err(map_dxgi_error)?;
            }
            let texture = resource
                .ok_or_else(|| CaptureError::Native("DXGI returned no desktop resource".into()))
                .and_then(|resource| resource.cast().map_err(native_error));
            let release_result = unsafe { self.duplication.ReleaseFrame() };
            let texture = texture?;
            release_result.map_err(map_dxgi_error)?;
            Ok(CapturedFrame {
                width: self.dimensions.0,
                height: self.dimensions.1,
                timestamp_us: now_micros(),
                pixels: Vec::new(),
                texture,
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
            *self = Self::new_primary()?;
        }
        #[cfg(not(windows))]
        {
            self.access_lost = false;
        }
        Ok(())
    }
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
}
