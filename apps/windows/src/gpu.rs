//! Owned, bounded D3D11 surfaces. Normal copy/conversion never maps a texture.
//!
//! A lease is independent of a COM texture reference: Media Foundation callers
//! must retain the NV12 lease until the tracked sample's final-release callback.
//! Event queries cover video processing only on feature level 11 and above.

use std::{
    mem::ManuallyDrop,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use windows::{
    Win32::{
        Foundation::{HMODULE, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::*,
            Dxgi::{
                Common::*, CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_ERROR_NOT_FOUND,
                IDXGIFactory1,
            },
        },
    },
    core::{HRESULT, Interface},
};

pub const MAX_GPU_POOL_CAPACITY: usize = 32;
pub const MAX_GPU_RESIDENT_SLOTS: usize = 64;
pub const MAX_GPU_RESIDENT_BYTES: u64 = 512 * 1024 * 1024;
const COMPLETION_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("GPU surface pool is busy")]
    Busy,
    #[error("unsupported GPU operation: {0}")]
    Unsupported(&'static str),
    #[error("invalid GPU surface dimensions or capacity")]
    InvalidDimensions,
    #[error("GPU surfaces belong to different devices")]
    DeviceMismatch,
    #[error("GPU completion deadline expired")]
    Timeout,
    #[error("GPU {stage} failed ({code:?})")]
    Native { stage: &'static str, code: HRESULT },
}

fn native(stage: &'static str) -> impl FnOnce(windows::core::Error) -> GpuError {
    move |error| GpuError::Native {
        stage,
        code: error.code(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct GpuMetrics {
    pub bgra_copy_bytes: u64,
    pub nv12_gpu_written_bytes: u64,
    pub cpu_readback_bytes: u64,
    pub texture_creations: u64,
    pub query_creations: u64,
    pub vp_input_view_creations: u64,
    pub vp_output_view_creations: u64,
}

static COPY_BYTES: AtomicU64 = AtomicU64::new(0);
static NV12_WRITTEN_BYTES: AtomicU64 = AtomicU64::new(0);
static READBACK_BYTES: AtomicU64 = AtomicU64::new(0);
static TEXTURE_CREATIONS: AtomicU64 = AtomicU64::new(0);
static QUERY_CREATIONS: AtomicU64 = AtomicU64::new(0);
static INPUT_VIEW_CREATIONS: AtomicU64 = AtomicU64::new(0);
static OUTPUT_VIEW_CREATIONS: AtomicU64 = AtomicU64::new(0);

pub fn gpu_metrics() -> GpuMetrics {
    GpuMetrics {
        bgra_copy_bytes: COPY_BYTES.load(Ordering::Relaxed),
        nv12_gpu_written_bytes: NV12_WRITTEN_BYTES.load(Ordering::Relaxed),
        cpu_readback_bytes: READBACK_BYTES.load(Ordering::Relaxed),
        texture_creations: TEXTURE_CREATIONS.load(Ordering::Relaxed),
        query_creations: QUERY_CREATIONS.load(Ordering::Relaxed),
        vp_input_view_creations: INPUT_VIEW_CREATIONS.load(Ordering::Relaxed),
        vp_output_view_creations: OUTPUT_VIEW_CREATIONS.load(Ordering::Relaxed),
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct GpuAdapterInfo {
    pub index: usize,
    pub description: String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_video_memory: u64,
    pub software: bool,
    pub luid_low: u32,
    pub luid_high: i32,
}

pub fn enumerate_adapters() -> Result<Vec<GpuAdapterInfo>, GpuError> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(native("DXGI factory"))?;
    let mut result = Vec::new();
    for index in 0.. {
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(error) if error.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(error) => return Err(native("adapter enumeration")(error)),
        };
        let desc = unsafe { adapter.GetDesc1() }.map_err(native("adapter description"))?;
        let length = desc
            .Description
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(desc.Description.len());
        result.push(GpuAdapterInfo {
            index: index as usize,
            description: String::from_utf16_lossy(&desc.Description[..length]),
            vendor_id: desc.VendorId,
            device_id: desc.DeviceId,
            dedicated_video_memory: desc.DedicatedVideoMemory as u64,
            software: desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
            luid_low: desc.AdapterLuid.LowPart,
            luid_high: desc.AdapterLuid.HighPart,
        });
    }
    Ok(result)
}

/// Creates a video-capable device on the explicitly selected adapter.
pub fn create_device(adapter_index: usize) -> Result<ID3D11Device, GpuError> {
    let index = u32::try_from(adapter_index).map_err(|_| GpuError::InvalidDimensions)?;
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(native("DXGI factory"))?;
    let adapter = unsafe { factory.EnumAdapters1(index) }.map_err(native("adapter selection"))?;
    let mut device = None;
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .map_err(native("device creation"))?;
    let device = device.ok_or(GpuError::Unsupported("empty D3D11 device"))?;
    Device::new(device.clone())?;
    Ok(device)
}

struct Device {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
}

impl Device {
    fn new(device: ID3D11Device) -> Result<Arc<Self>, GpuError> {
        let flags = unsafe { device.GetCreationFlags() };
        let required = (D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT).0;
        if flags & required != required
            || unsafe { device.GetFeatureLevel() }.0 < D3D_FEATURE_LEVEL_11_0.0
        {
            return Err(GpuError::Unsupported(
                "video/BGRA support and feature level 11 required",
            ));
        }
        let context =
            unsafe { device.GetImmediateContext() }.map_err(native("immediate context"))?;
        let multithread: ID3D11Multithread =
            context.cast().map_err(native("multithread protection"))?;
        let _ = unsafe { multithread.SetMultithreadProtected(true) };
        Ok(Arc::new(Self { device, context }))
    }

    fn query(&self) -> Result<ID3D11Query, GpuError> {
        let mut query = None;
        unsafe {
            self.device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut query),
            )
        }
        .map_err(native("event query"))?;
        QUERY_CREATIONS.fetch_add(1, Ordering::Relaxed);
        query.ok_or(GpuError::Unsupported("empty event query"))
    }

    fn completed(&self, query: &ID3D11Query) -> Result<bool, GpuError> {
        let mut completed = 0i32;
        // The generated Result<()> wrapper considers S_FALSE successful. Inspect
        // the HRESULT and BOOL instead; S_FALSE must never release a slot.
        let status = unsafe {
            (self.context.vtable().GetData)(
                self.context.as_raw(),
                query.as_raw(),
                (&mut completed as *mut i32).cast(),
                4,
                D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32,
            )
        };
        event_is_complete(status, completed)
    }

    fn wait(&self, query: &ID3D11Query) -> Result<(), GpuError> {
        unsafe { self.context.Flush() };
        let deadline = Instant::now() + COMPLETION_TIMEOUT;
        while !self.completed(query)? {
            if Instant::now() >= deadline {
                return Err(GpuError::Timeout);
            }
            std::thread::yield_now();
        }
        Ok(())
    }
}

fn event_is_complete(status: HRESULT, completed: i32) -> Result<bool, GpuError> {
    if status.is_err() {
        return Err(GpuError::Native {
            stage: "event completion",
            code: status,
        });
    }
    Ok(status.0 == 0 && completed != 0)
}

// Serializes our multi-call submissions (including publishing the latest fence).
// D3D's multithread protection additionally coordinates individual calls with MF.
static SUBMISSION: Mutex<()> = Mutex::new(());

struct Slot {
    device: Arc<Device>,
    texture: ID3D11Texture2D,
    bytes: u64,
    borrowed: AtomicBool,
    fence: Mutex<Option<ID3D11Query>>,
}

impl Slot {
    fn ready(&self) -> Result<bool, GpuError> {
        let mut fence = lock(&self.fence);
        if let Some(query) = fence.as_ref()
            && !self.device.completed(query)?
        {
            return Ok(false);
        }
        *fence = None;
        Ok(true)
    }
}

// Registry retains resources, their budget, and pending fences across pool drops.
// It prunes only unowned, GPU-complete slots. A hung device therefore consumes a
// bounded quarantine rather than releasing accounting and growing every epoch.
static RESIDENT: OnceLock<Mutex<Vec<Arc<Slot>>>> = OnceLock::new();

fn resident() -> &'static Mutex<Vec<Arc<Slot>>> {
    RESIDENT.get_or_init(|| Mutex::new(Vec::new()))
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct GpuResidentStats {
    pub slots: usize,
    /// Logical image bytes only; excludes driver layout and other resources.
    pub logical_image_bytes: u64,
}

fn prune_retired(registry: &mut Vec<Arc<Slot>>) {
    registry.retain(|slot| {
        if Arc::strong_count(slot) != 1 {
            return true;
        }
        match slot.ready() {
            Ok(true) => false,
            Ok(false) => true,
            Err(_) => unsafe { slot.device.device.GetDeviceRemovedReason() }.is_ok(),
        }
    });
}

pub fn gpu_resident_stats() -> GpuResidentStats {
    let registry = lock(resident());
    GpuResidentStats {
        slots: registry.len(),
        logical_image_bytes: registry.iter().map(|slot| slot.bytes).sum(),
    }
}

/// Non-hot cleanup for a retired encoder/capture generation. It never forces
/// release of a live lease or an incomplete fence, and is not an MF callback.
pub fn trim_retired_gpu_surfaces() -> GpuResidentStats {
    let mut registry = lock(resident());
    prune_retired(&mut registry);
    GpuResidentStats {
        slots: registry.len(),
        logical_image_bytes: registry.iter().map(|slot| slot.bytes).sum(),
    }
}

fn allocate(
    device: &Arc<Device>,
    desc: &D3D11_TEXTURE2D_DESC,
    bytes: u64,
) -> Result<Arc<Slot>, GpuError> {
    let mut registry = lock(resident());
    prune_retired(&mut registry);
    let used: u64 = registry.iter().map(|slot| slot.bytes).sum();
    if registry.len() >= MAX_GPU_RESIDENT_SLOTS
        || bytes > MAX_GPU_RESIDENT_BYTES.saturating_sub(used)
    {
        return Err(GpuError::Busy);
    }
    let texture = texture(&device.device, desc, None)?;
    let slot = Arc::new(Slot {
        device: device.clone(),
        texture,
        bytes,
        borrowed: AtomicBool::new(true),
        fence: Mutex::new(None),
    });
    registry.push(slot.clone());
    Ok(slot)
}

struct Pool {
    device: Arc<Device>,
    desc: D3D11_TEXTURE2D_DESC,
    bytes: u64,
    capacity: usize,
    slots: Mutex<Vec<Arc<Slot>>>,
}

impl Pool {
    fn new(
        device: ID3D11Device,
        width: u32,
        height: u32,
        format: DXGI_FORMAT,
        capacity: usize,
    ) -> Result<Self, GpuError> {
        let bytes = surface_bytes(width, height, format)?;
        if capacity == 0 || capacity > MAX_GPU_POOL_CAPACITY {
            return Err(GpuError::InvalidDimensions);
        }
        Ok(Self {
            device: Device::new(device)?,
            desc: description(width, height, format),
            bytes,
            capacity,
            slots: Mutex::new(Vec::new()),
        })
    }

    fn acquire(&self) -> Result<Lease, GpuError> {
        let mut slots = lock(&self.slots);
        for slot in slots.iter() {
            if slot
                .borrowed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                match slot.ready() {
                    Ok(true) => return Ok(Lease(slot.clone())),
                    Ok(false) => slot.borrowed.store(false, Ordering::Release),
                    Err(error) => {
                        slot.borrowed.store(false, Ordering::Release);
                        return Err(error);
                    }
                }
            }
        }
        if slots.len() >= self.capacity {
            return Err(GpuError::Busy);
        }
        let slot = allocate(&self.device, &self.desc, self.bytes)?;
        slots.push(slot.clone());
        Ok(Lease(slot))
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // A pool is an encoder/capture generation owner, never an MF callback
        // owner. Clear its own references before trimming retired resources.
        self.slots
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        let _ = trim_retired_gpu_surfaces();
    }
}

struct Lease(Arc<Slot>);
impl Drop for Lease {
    fn drop(&mut self) {
        // Runs from an MF allocator callback too: no GPU calls and no pool lock.
        self.0.borrowed.store(false, Ordering::Release);
    }
}

pub struct GpuBgraPool {
    pool: Pool,
}

impl GpuBgraPool {
    pub fn from_device(
        device: ID3D11Device,
        width: u32,
        height: u32,
        capacity: usize,
    ) -> Result<Self, GpuError> {
        Ok(Self {
            pool: Pool::new(device, width, height, DXGI_FORMAT_B8G8R8A8_UNORM, capacity)?,
        })
    }

    pub fn copy_from(
        &self,
        source: &ID3D11Texture2D,
        timestamp_us: u64,
    ) -> Result<GpuBgraFrame, GpuError> {
        let source_device = unsafe { source.GetDevice() }.map_err(native("source device"))?;
        if source_device.as_raw() != self.pool.device.device.as_raw() {
            return Err(GpuError::DeviceMismatch);
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { source.GetDesc(&mut desc) };
        if desc.Width != self.pool.desc.Width
            || desc.Height != self.pool.desc.Height
            || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            || desc.ArraySize != 1
            || desc.MipLevels != 1
            || desc.SampleDesc.Count != 1
        {
            return Err(GpuError::Unsupported("source BGRA texture layout"));
        }
        let lease = self.pool.acquire()?;
        let query = self.pool.device.query()?;
        {
            let _submit = lock(&SUBMISSION);
            unsafe {
                self.pool
                    .device
                    .context
                    .CopyResource(&lease.0.texture, source);
                self.pool.device.context.End(&query);
            }
            *lock(&lease.0.fence) = Some(query.clone());
        }
        COPY_BYTES.fetch_add(self.pool.bytes, Ordering::Relaxed);
        self.pool.device.wait(&query)?;
        Ok(GpuBgraFrame {
            lease: Arc::new(lease),
            width: desc.Width,
            height: desc.Height,
            timestamp_us,
        })
    }
}

#[derive(Clone)]
pub struct GpuBgraFrame {
    lease: Arc<Lease>,
    width: u32,
    height: u32,
    timestamp_us: u64,
}

impl std::fmt::Debug for GpuBgraFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuBgraFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("timestamp_us", &self.timestamp_us)
            .finish_non_exhaustive()
    }
}

impl GpuBgraFrame {
    pub fn device(&self) -> &ID3D11Device {
        &self.lease.0.device.device
    }
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.lease.0.texture
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn timestamp_us(&self) -> u64 {
        self.timestamp_us
    }
    pub fn with_timestamp(&self, timestamp_us: u64) -> Self {
        Self {
            timestamp_us,
            ..self.clone()
        }
    }
    /// Explicit CPU fallback; never called by the GPU copy/conversion path.
    pub fn readback_bgra(&self) -> Result<Vec<u8>, GpuError> {
        readback(&self.lease.0)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct GpuVideoCapabilities {
    pub source_width: u32,
    pub source_height: u32,
    pub width: u32,
    pub height: u32,
    pub bgra_input: bool,
    pub nv12_output: bool,
    pub feature_level: u32,
    pub matrix: &'static str,
    pub range: &'static str,
}

pub struct GpuNv12Pool {
    pool: Pool,
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    capabilities: GpuVideoCapabilities,
}

impl GpuNv12Pool {
    pub fn from_device(
        device: ID3D11Device,
        source_width: u32,
        source_height: u32,
        width: u32,
        height: u32,
        capacity: usize,
    ) -> Result<Self, GpuError> {
        surface_bytes(source_width, source_height, DXGI_FORMAT_B8G8R8A8_UNORM)?;
        let pool = Pool::new(device, width, height, DXGI_FORMAT_NV12, capacity)?;
        let video_device: ID3D11VideoDevice =
            pool.device.device.cast().map_err(native("video device"))?;
        let video_context: ID3D11VideoContext = pool
            .device
            .context
            .cast()
            .map_err(native("video context"))?;
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            InputWidth: source_width,
            InputHeight: source_height,
            OutputFrameRate: DXGI_RATIONAL {
                Numerator: 60,
                Denominator: 1,
            },
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let enumerator = unsafe { video_device.CreateVideoProcessorEnumerator(&content) }
            .map_err(native("video processor enumeration"))?;
        let input = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_B8G8R8A8_UNORM) }
            .map_err(native("BGRA video format"))?;
        let output = unsafe { enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12) }
            .map_err(native("NV12 video format"))?;
        if input & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0 as u32 == 0
            || output & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0
        {
            return Err(GpuError::Unsupported("BGRA input or NV12 output"));
        }
        let mut caps = D3D11_VIDEO_PROCESSOR_CAPS::default();
        unsafe { enumerator.GetVideoProcessorCaps(&mut caps) }
            .map_err(native("video processor capabilities"))?;
        let mut profile = None;
        for index in 0..caps.RateConversionCapsCount {
            let mut rate = D3D11_VIDEO_PROCESSOR_RATE_CONVERSION_CAPS::default();
            unsafe { enumerator.GetVideoProcessorRateConversionCaps(index, &mut rate) }
                .map_err(native("rate conversion capabilities"))?;
            // These counts describe references for *optimal* deinterlacing /
            // rate conversion, not a mandatory history for progressive 1:1
            // frames. The enumerator already describes progressive input.
            // Prefer the least temporal profile; convert() verifies the real
            // zero-history Blt and returns a typed failure if the driver rejects it.
            let references = u64::from(rate.PastFrames) + u64::from(rate.FutureFrames);
            if profile.is_none_or(|(_, previous)| references < previous) {
                profile = Some((index, references));
            }
        }
        let processor = unsafe {
            video_device.CreateVideoProcessor(
                &enumerator,
                profile
                    .ok_or(GpuError::Unsupported("no video processor profile"))?
                    .0,
            )
        }
        .map_err(native("video processor"))?;
        let capabilities = GpuVideoCapabilities {
            source_width,
            source_height,
            width,
            height,
            bgra_input: true,
            nv12_output: true,
            feature_level: unsafe { pool.device.device.GetFeatureLevel() }.0 as u32,
            matrix: "bt709",
            range: "limited",
        };
        Ok(Self {
            pool,
            video_device,
            video_context,
            enumerator,
            processor,
            capabilities,
        })
    }

    pub fn capabilities(&self) -> GpuVideoCapabilities {
        self.capabilities.clone()
    }

    pub fn convert(&self, frame: &GpuBgraFrame) -> Result<GpuNv12Lease, GpuError> {
        if frame.device().as_raw() != self.pool.device.device.as_raw() {
            return Err(GpuError::DeviceMismatch);
        }
        if frame.dimensions()
            != (
                self.capabilities.source_width,
                self.capabilities.source_height,
            )
        {
            return Err(GpuError::InvalidDimensions);
        }
        let lease = self.pool.acquire()?;
        let input_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let output_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut input = None;
        let mut output = None;
        unsafe {
            self.video_device
                .CreateVideoProcessorInputView(
                    frame.texture(),
                    &self.enumerator,
                    &input_desc,
                    Some(&mut input),
                )
                .map_err(native("video input view"))?;
            INPUT_VIEW_CREATIONS.fetch_add(1, Ordering::Relaxed);
            self.video_device
                .CreateVideoProcessorOutputView(
                    &lease.0.texture,
                    &self.enumerator,
                    &output_desc,
                    Some(&mut output),
                )
                .map_err(native("video output view"))?;
            OUTPUT_VIEW_CREATIONS.fetch_add(1, Ordering::Relaxed);
        }
        let input = input.ok_or(GpuError::Unsupported("empty video input view"))?;
        let output = output.ok_or(GpuError::Unsupported("empty video output view"))?;
        let query = self.pool.device.query()?;
        let source = RECT {
            left: 0,
            top: 0,
            right: frame.width as i32,
            bottom: frame.height as i32,
        };
        let target = RECT {
            left: 0,
            top: 0,
            right: self.capabilities.width as i32,
            bottom: self.capabilities.height as i32,
        };
        // Usage=processing, RGB full range, BT.709 matrix (bit2), matching
        // encoder.rs's existing CPU conversion. Nominal range occupies bits4..5.
        let input_color = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
            _bitfield: 1 | (1 << 2) | (2 << 4),
        };
        let output_color = D3D11_VIDEO_PROCESSOR_COLOR_SPACE {
            _bitfield: 1 | (1 << 2) | (1 << 4),
        };
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(Some(input)),
            ..Default::default()
        };
        let blit = {
            let _submit = lock(&SUBMISSION);
            let result = unsafe {
                self.video_context.VideoProcessorSetStreamFrameFormat(
                    &self.processor,
                    0,
                    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                );
                self.video_context
                    .VideoProcessorSetStreamAutoProcessingMode(&self.processor, 0, false);
                self.video_context.VideoProcessorSetStreamSourceRect(
                    &self.processor,
                    0,
                    true,
                    Some(&source),
                );
                self.video_context.VideoProcessorSetStreamDestRect(
                    &self.processor,
                    0,
                    true,
                    Some(&target),
                );
                self.video_context.VideoProcessorSetOutputTargetRect(
                    &self.processor,
                    true,
                    Some(&target),
                );
                self.video_context.VideoProcessorSetStreamColorSpace(
                    &self.processor,
                    0,
                    &input_color,
                );
                self.video_context
                    .VideoProcessorSetOutputColorSpace(&self.processor, &output_color);
                let result = self.video_context.VideoProcessorBlt(
                    &self.processor,
                    &output,
                    0,
                    std::slice::from_ref(&stream),
                );
                self.pool.device.context.End(&query);
                result
            };
            *lock(&lease.0.fence) = Some(query.clone());
            // Even a timed-out conversion must keep its source quarantined until
            // the GPU has finished reading it, independent of the caller's Drop.
            *lock(&frame.lease.0.fence) = Some(query.clone());
            result
        };
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        blit.map_err(native("video processor blit"))?;
        NV12_WRITTEN_BYTES.fetch_add(self.pool.bytes, Ordering::Relaxed);
        self.pool.device.wait(&query)?;
        Ok(GpuNv12Lease {
            lease,
            width: self.capabilities.width,
            height: self.capabilities.height,
            timestamp_us: frame.timestamp_us,
        })
    }
}

pub struct GpuNv12Lease {
    lease: Lease,
    width: u32,
    height: u32,
    timestamp_us: u64,
}

impl GpuNv12Lease {
    pub fn device(&self) -> &ID3D11Device {
        &self.lease.0.device.device
    }
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.lease.0.texture
    }
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn timestamp_us(&self) -> u64 {
        self.timestamp_us
    }
    pub fn subresource_index(&self) -> u32 {
        0
    }
}

fn surface_bytes(width: u32, height: u32, format: DXGI_FORMAT) -> Result<u64, GpuError> {
    if width == 0
        || height == 0
        || width > 16384
        || height > 16384
        || (format == DXGI_FORMAT_NV12 && (!width.is_multiple_of(2) || !height.is_multiple_of(2)))
    {
        return Err(GpuError::InvalidDimensions);
    }
    let pixels = u64::from(width) * u64::from(height);
    Ok(if format == DXGI_FORMAT_NV12 {
        pixels * 3 / 2
    } else {
        pixels * 4
    })
}

fn description(width: u32, height: u32, format: DXGI_FORMAT) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
        ..Default::default()
    }
}

fn texture(
    device: &ID3D11Device,
    desc: &D3D11_TEXTURE2D_DESC,
    data: Option<*const D3D11_SUBRESOURCE_DATA>,
) -> Result<ID3D11Texture2D, GpuError> {
    let mut texture = None;
    unsafe { device.CreateTexture2D(desc, data, Some(&mut texture)) }
        .map_err(native("texture allocation"))?;
    TEXTURE_CREATIONS.fetch_add(1, Ordering::Relaxed);
    texture.ok_or(GpuError::Unsupported("empty texture"))
}

/// Synthetic probe upload. Production capture does not call this helper.
pub fn create_synthetic_bgra(
    device: &ID3D11Device,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Result<ID3D11Texture2D, GpuError> {
    if surface_bytes(width, height, DXGI_FORMAT_B8G8R8A8_UNORM)? != pixels.len() as u64 {
        return Err(GpuError::InvalidDimensions);
    }
    let data = D3D11_SUBRESOURCE_DATA {
        pSysMem: pixels.as_ptr().cast(),
        SysMemPitch: width * 4,
        SysMemSlicePitch: 0,
    };
    texture(
        device,
        &description(width, height, DXGI_FORMAT_B8G8R8A8_UNORM),
        Some(&data),
    )
}

/// Validation-only readback, tightly packed Y followed by interleaved UV.
pub fn readback_nv12(lease: &GpuNv12Lease) -> Result<Vec<u8>, GpuError> {
    readback(&lease.lease.0)
}

fn readback(slot: &Slot) -> Result<Vec<u8>, GpuError> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { slot.texture.GetDesc(&mut desc) };
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
    desc.MiscFlags = 0;
    let staging = texture(&slot.device.device, &desc, None)?;
    let _submit = lock(&SUBMISSION);
    unsafe { slot.device.context.CopyResource(&staging, &slot.texture) };
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    unsafe {
        slot.device
            .context
            .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
    }
    .map_err(native("explicit CPU readback"))?;
    let result = copy_mapped(&mapped, desc.Width, desc.Height, desc.Format);
    unsafe { slot.device.context.Unmap(&staging, 0) };
    if let Ok(bytes) = &result {
        READBACK_BYTES.fetch_add(bytes.len() as u64, Ordering::Relaxed);
    }
    result
}

fn copy_mapped(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
) -> Result<Vec<u8>, GpuError> {
    let row_bytes = width as usize * if format == DXGI_FORMAT_NV12 { 1 } else { 4 };
    let rows = height as usize * if format == DXGI_FORMAT_NV12 { 3 } else { 2 } / 2;
    if mapped.pData.is_null() || (mapped.RowPitch as usize) < row_bytes {
        return Err(GpuError::Unsupported("invalid mapped row pitch"));
    }
    let mut pixels = vec![0; rows * row_bytes];
    for row in 0..rows {
        let source = unsafe {
            std::slice::from_raw_parts(
                mapped
                    .pData
                    .cast::<u8>()
                    .add(row * mapped.RowPitch as usize),
                row_bytes,
            )
        };
        pixels[row * row_bytes..(row + 1) * row_bytes].copy_from_slice(source);
    }
    Ok(pixels)
}

#[cfg(test)]
mod bitstream_validation;
#[cfg(test)]
mod tests;
