use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
}

#[cfg(windows)]
pub struct DxgiDesktopCapturer {
    duplication: windows::Win32::Graphics::Dxgi::IDXGIOutputDuplication,
    dimensions: (u32, u32),
}

#[cfg(not(windows))]
pub struct DxgiDesktopCapturer {
    dimensions: (u32, u32),
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
            use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput1};
            use windows::core::Interface;

            let (factory, adapter, output, device) = unsafe {
                let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(native_error)?;
                let adapter = factory.EnumAdapters1(0).map_err(native_error)?;
                let output = adapter.EnumOutputs(0).map_err(native_error)?;
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
                    factory,
                    adapter,
                    output,
                    device.ok_or_else(|| {
                        CaptureError::Native("D3D11 device was not created".into())
                    })?,
                )
            };
            let _ = (factory, adapter);
            let output1: IDXGIOutput1 = output.cast().map_err(native_error)?;
            let duplication = unsafe { output1.DuplicateOutput(&device) }.map_err(native_error)?;
            let description = unsafe { output.GetDesc() }.map_err(native_error)?;
            let width =
                (description.DesktopCoordinates.right - description.DesktopCoordinates.left) as u32;
            let height =
                (description.DesktopCoordinates.bottom - description.DesktopCoordinates.top) as u32;
            if width == 0 || height == 0 {
                return Err(CaptureError::NoDisplay);
            }
            return Ok(Self {
                duplication,
                dimensions: (width, height),
            });
        }

        #[cfg(not(windows))]
        Ok(Self {
            dimensions: (1920, 1080),
            access_lost: false,
        })
    }

    #[cfg(not(windows))]
    pub fn mark_access_lost(&mut self) {
        self.access_lost = true;
    }
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
                    .map_err(native_error)?;
            }
            let texture = resource
                .ok_or_else(|| CaptureError::Native("DXGI returned no desktop resource".into()))
                .and_then(|resource| resource.cast().map_err(native_error));
            let release_result = unsafe { self.duplication.ReleaseFrame() };
            let texture = texture?;
            release_result.map_err(native_error)?;
            return Ok(CapturedFrame {
                width: self.dimensions.0,
                height: self.dimensions.1,
                timestamp_us: now_micros(),
                pixels: Vec::new(),
                texture,
            });
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
}

#[cfg(windows)]
fn native_error(error: windows::core::Error) -> CaptureError {
    CaptureError::Native(error.to_string())
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
