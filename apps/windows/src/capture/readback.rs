use windows::{
    Win32::Graphics::{
        Direct3D11::{
            D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC,
            D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
        },
        Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB},
    },
    core::Interface,
};

use super::{CaptureError, native_error};

#[derive(Default)]
pub(super) struct TextureReadback {
    cached: Option<CachedReadback>,
    #[cfg(test)]
    creations: usize,
}

struct CachedReadback {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    texture: ID3D11Texture2D,
    description: D3D11_TEXTURE2D_DESC,
}

impl TextureReadback {
    pub(super) fn copy_pixels(
        &mut self,
        texture: &ID3D11Texture2D,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, CaptureError> {
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
        description.Usage = D3D11_USAGE_STAGING;
        description.BindFlags = 0;
        description.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        description.MiscFlags = 0;
        let recreate = self.cached.as_ref().is_none_or(|cached| {
            cached.device.as_raw() != device.as_raw() || cached.description != description
        });
        if recreate {
            let context = unsafe { device.GetImmediateContext() }.map_err(native_error)?;
            let mut staging = None;
            unsafe { device.CreateTexture2D(&description, None, Some(&mut staging)) }
                .map_err(native_error)?;
            self.cached = Some(CachedReadback {
                device,
                context,
                texture: staging.ok_or_else(|| {
                    CaptureError::Native("D3D11 did not return a staging texture".into())
                })?,
                description,
            });
            #[cfg(test)]
            {
                self.creations += 1;
            }
        }
        let cached = self.cached.as_ref().expect("initialized staging texture");
        // The cached destination is owned by this capturer. Map/copy/Unmap
        // still completes before the caller releases the duplication frame;
        // no source desktop surface escapes its Acquire/Release lifetime.
        unsafe { cached.context.CopyResource(&cached.texture, texture) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        unsafe {
            cached
                .context
                .Map(&cached.texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        }
        .map_err(native_error)?;
        let copied = copy_mapped_pixels(&mapped, width, height);
        unsafe { cached.context.Unmap(&cached.texture, 0) };
        copied
    }
}

fn copy_mapped_pixels(
    mapped: &D3D11_MAPPED_SUBRESOURCE,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, CaptureError> {
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
            std::slice::from_raw_parts(mapped.pData.cast::<u8>().add(row * source_pitch), row_bytes)
        };
        pixels[row * row_bytes..(row + 1) * row_bytes].copy_from_slice(source);
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        Foundation::HMODULE,
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_WARP, D3D_FEATURE_LEVEL},
            Direct3D11::{
                D3D11_CREATE_DEVICE_FLAG, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
                D3D11_USAGE_DEFAULT, D3D11CreateDevice,
            },
            Dxgi::{Common::DXGI_SAMPLE_DESC, IDXGIAdapter},
        },
    };

    fn warp_device() -> ID3D11Device {
        let mut device = None;
        unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_FLAG(0),
                None::<&[D3D_FEATURE_LEVEL]>,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .expect("offline WARP D3D11 device");
        device.unwrap()
    }

    fn source_texture(device: &ID3D11Device, width: u32, height: u32, fill: u8) -> ID3D11Texture2D {
        let pixels = vec![fill; width as usize * height as usize * 4];
        let description = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            ..Default::default()
        };
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };
        let mut texture = None;
        unsafe { device.CreateTexture2D(&description, Some(&data), Some(&mut texture)) }
            .expect("synthetic BGRA texture");
        texture.unwrap()
    }

    #[test]
    fn cached_staging_reads_new_content_without_recreating_the_texture() {
        let device = warp_device();
        let source = source_texture(&device, 4, 2, 17);
        let mut readback = TextureReadback::default();
        assert_eq!(readback.copy_pixels(&source, 4, 2).unwrap(), vec![17; 32]);
        let pixels = [29_u8; 32];
        unsafe {
            device.GetImmediateContext().unwrap().UpdateSubresource(
                &source,
                0,
                None,
                pixels.as_ptr().cast(),
                16,
                0,
            );
        }
        assert_eq!(readback.copy_pixels(&source, 4, 2).unwrap(), pixels);
        assert_eq!(readback.creations, 1);
    }

    #[test]
    fn dimensions_and_device_changes_recreate_staging_before_copy() {
        let device = warp_device();
        let mut readback = TextureReadback::default();
        let first = source_texture(&device, 4, 2, 10);
        readback.copy_pixels(&first, 4, 2).unwrap();
        assert!(readback.copy_pixels(&first, 5, 2).is_err());
        assert_eq!(readback.creations, 1);
        let resized = source_texture(&device, 8, 2, 20);
        assert_eq!(readback.copy_pixels(&resized, 8, 2).unwrap(), vec![20; 64]);
        assert_eq!(readback.creations, 2);
        let another_device = warp_device();
        let another_source = source_texture(&another_device, 8, 2, 30);
        assert_eq!(
            readback.copy_pixels(&another_source, 8, 2).unwrap(),
            vec![30; 64]
        );
        assert_eq!(readback.creations, 3);
    }

    #[test]
    fn mapped_padding_is_not_copied_and_bad_pitch_is_rejected() {
        let mut source = [1_u8, 2, 3, 4, 99, 99, 99, 99, 5, 6, 7, 8, 99, 99, 99, 99];
        let mut mapped = D3D11_MAPPED_SUBRESOURCE {
            pData: source.as_mut_ptr().cast(),
            RowPitch: 8,
            DepthPitch: 16,
        };
        assert_eq!(
            copy_mapped_pixels(&mapped, 1, 2).unwrap(),
            [1, 2, 3, 4, 5, 6, 7, 8]
        );
        mapped.RowPitch = 3;
        assert!(copy_mapped_pixels(&mapped, 1, 2).is_err());
    }

    #[test]
    #[ignore = "opt-in offline synthetic WARP readback benchmark; no desktop capture"]
    fn staging_reuse_benchmark() {
        fn report(label: &str, width: u32, height: u32, mut samples: Vec<u128>, creations: usize) {
            samples.sort_unstable();
            let percentile =
                |percent: usize| samples[(samples.len() * percent).div_ceil(100).saturating_sub(1)];
            eprintln!(
                "{label}: {width}x{height} n={} staging_creations={creations} p50_us={} p95_us={} p99_us={} known_cpu_copy_bytes_per_frame={}",
                samples.len(),
                percentile(50),
                percentile(95),
                percentile(99),
                u64::from(width) * u64::from(height) * 4
            );
        }
        let device = warp_device();
        for (width, height) in [(1920, 1080), (2560, 1440)] {
            let source = source_texture(&device, width, height, 17);
            let mut cached = TextureReadback::default();
            for _ in 0..5 {
                cached.copy_pixels(&source, width, height).unwrap();
            }
            let mut fresh_times = Vec::new();
            let mut reused_times = Vec::new();
            for _ in 0..60 {
                let mut fresh = TextureReadback::default();
                let start = std::time::Instant::now();
                std::hint::black_box(fresh.copy_pixels(&source, width, height).unwrap());
                fresh_times.push(start.elapsed().as_micros());
                let start = std::time::Instant::now();
                std::hint::black_box(cached.copy_pixels(&source, width, height).unwrap());
                reused_times.push(start.elapsed().as_micros());
            }
            report("fresh-WARP", width, height, fresh_times, 60);
            report("reuse-WARP", width, height, reused_times, cached.creations);
        }
    }
}
