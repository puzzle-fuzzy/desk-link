use std::sync::Mutex;

use windows::{
    Win32::{
        Foundation::E_NOTIMPL,
        Graphics::Direct3D11::ID3D11Texture2D,
        Media::MediaFoundation::{
            IMF2DBuffer, IMFAsyncCallback, IMFAsyncCallback_Impl, IMFAsyncResult, IMFSample,
            IMFTrackedSample, MFCreateDXGISurfaceBuffer, MFCreateVideoSampleFromSurface,
            MFSampleExtension_CleanPoint,
        },
    },
    core::{Interface, Ref, implement},
};

use super::EncoderError;

/// The callback owns the unique pool lease, not the encoder's output queue.
/// A tracked sample invokes it only when every external sample reference has
/// been released, including references held beyond ProcessOutput or flush.
#[implement(IMFAsyncCallback)]
struct ReleaseLease {
    lease: Mutex<Option<Box<dyn Send>>>,
}

impl IMFAsyncCallback_Impl for ReleaseLease_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Invoke(&self, _result: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        let lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        drop(lease);
        Ok(())
    }
}

pub(super) fn tracked_nv12_sample(
    texture: &ID3D11Texture2D,
    subresource: u32,
    timestamp_us: u64,
    fps: u32,
    force_keyframe: bool,
    lease: impl Send + 'static,
) -> Result<IMFSample, EncoderError> {
    if fps == 0 {
        return Err(EncoderError::InvalidDimensions);
    }
    let buffer =
        unsafe { MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, texture, subresource, false) }
            .map_err(|error| error_at("create DXGI surface buffer", error))?;
    let contiguous_length = unsafe {
        buffer
            .cast::<IMF2DBuffer>()
            .and_then(|buffer| buffer.GetContiguousLength())
    }
    .map_err(|error| error_at("get GPU sample contiguous length", error))?;
    unsafe { buffer.SetCurrentLength(contiguous_length) }
        .map_err(|error| error_at("set GPU sample current length", error))?;
    let sample = unsafe { MFCreateVideoSampleFromSurface(None::<&windows::core::IUnknown>) }
        .map_err(|error| error_at("create tracked video sample", error))?;
    unsafe {
        sample
            .AddBuffer(&buffer)
            .map_err(|error| error_at("attach DXGI surface", error))?;
        sample
            .SetSampleTime(timestamp_us.saturating_mul(10).min(i64::MAX as u64) as i64)
            .map_err(|error| error_at("set GPU sample time", error))?;
        sample
            .SetSampleDuration(10_000_000_i64 / i64::from(fps))
            .map_err(|error| error_at("set GPU sample duration", error))?;
        if force_keyframe {
            sample
                .SetUINT32(&MFSampleExtension_CleanPoint, 1)
                .map_err(|error| error_at("set GPU sample IDR hint", error))?;
        }
    }
    let tracked = sample
        .cast::<IMFTrackedSample>()
        .map_err(|error| error_at("tracked sample capability", error))?;
    let allocator: IMFAsyncCallback = ReleaseLease {
        lease: Mutex::new(Some(Box::new(lease))),
    }
    .into();
    unsafe { tracked.SetAllocator(&allocator, None::<&windows::core::IUnknown>) }
        .map_err(|error| error_at("set GPU sample allocator", error))?;
    drop(tracked);
    drop(allocator);
    Ok(sample)
}

fn error_at(stage: &'static str, error: windows::core::Error) -> EncoderError {
    EncoderError::GpuOperation(super::GpuEncoderFailure {
        stage,
        hresult: Some(error.code().0),
    })
}
