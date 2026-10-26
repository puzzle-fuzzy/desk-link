use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use windows::{Win32::Graphics::Direct3D11::ID3D11Device, core::Interface};

use super::*;
use crate::gpu::{
    GpuBgraFrame, GpuBgraPool, GpuError, GpuNv12Lease, GpuNv12Pool, create_device,
    create_synthetic_bgra, enumerate_adapters, gpu_metrics,
};

fn fixture(width: u32, height: u32) -> (ID3D11Device, GpuBgraPool, GpuBgraFrame) {
    let adapter = enumerate_adapters()
        .unwrap()
        .into_iter()
        .find(|adapter| !adapter.software)
        .expect("hardware adapter");
    let device = create_device(adapter.index).unwrap();
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
        pixel.copy_from_slice(&[index as u8, (index / 17) as u8, (index / 193) as u8, 255]);
    }
    let texture = create_synthetic_bgra(&device, width, height, &pixels).unwrap();
    let pool = GpuBgraPool::from_device(device.clone(), width, height, 2).unwrap();
    let frame = pool.copy_from(&texture, 33_333).unwrap();
    (device, pool, frame)
}

fn encoder(width: u32, height: u32) -> H264Encoder {
    H264Encoder::new_with_settings(
        width,
        height,
        H264EncoderSettings {
            max_width: width,
            max_height: height,
            bitrate: 2_500_000,
            ..H264EncoderSettings::default()
        },
    )
    .unwrap()
}

#[test]
#[ignore = "explicit synthetic hardware surface encode probe; no desktop capture"]
fn actual_surface_outputs_keep_frame_identity_and_no_cpu_nv12_copy() {
    let (_device, _pool, frame) = fixture(1280, 720);
    let mut encoder = encoder(1280, 720);
    let mut outputs = 0;
    for index in 1..=8 {
        let frame = frame.with_timestamp(index * 33_333);
        match encoder.encode_gpu(&frame, index == 1 || index == 3) {
            Ok(output) => {
                outputs += 1;
                assert!(!output.access_unit.is_empty());
                assert_eq!(output.config_version, encoder.config_version());
                assert_eq!(output.timestamp_us, output.frame_id * 33_333);
                if output.frame_id == 1 || output.frame_id == 3 {
                    assert!(output.keyframe);
                }
                if output.frame_id == 1 {
                    assert!(
                        output
                            .sequence_header
                            .is_some_and(|header| !header.is_empty())
                    );
                }
                let sample = encoder.last_performance_sample().unwrap();
                assert!(sample.gpu_surface_input);
                assert_eq!(sample.output_frame_id, Some(output.frame_id));
                assert_eq!(sample.nv12_written_bytes, 0);
                assert_eq!(sample.mf_input_copy_bytes, 0);
            }
            Err(EncoderError::NeedMoreInput) => {}
            Err(error) => panic!(
                "surface probe failed: {error:?}; status={:?}",
                encoder.gpu_status()
            ),
        }
        assert!(
            encoder.gpu_status().surface_backend_active,
            "software fallback is not hardware success: {:?}",
            encoder.gpu_status()
        );
        assert_eq!(encoder.gpu_status().cpu_fallbacks, 0);
    }
    assert!(outputs > 0);
    assert_eq!(
        encoder.backend_info().unwrap().kind,
        "hardware_mft_d3d11_surface"
    );
    assert_eq!(encoder.gpu_status().manager_accepted, Some(true));
    println!("{}", serde_json::to_string(encoder.gpu_status()).unwrap());
}

struct CountedLease {
    lease: Option<GpuNv12Lease>,
    released: Arc<AtomicUsize>,
}
impl Drop for CountedLease {
    fn drop(&mut self) {
        drop(self.lease.take());
        self.released.fetch_add(1, Ordering::Release);
    }
}

#[test]
#[ignore = "explicit tracked GPU sample COM lifetime/flush/drop regression"]
fn tracked_sample_external_reference_keeps_pool_busy_after_encoder_flush_and_drop() {
    let _runtime = native::NativeRuntime::start().unwrap();
    let (device, _bgra, frame) = fixture(1280, 720);
    let pool = GpuNv12Pool::from_device(device.clone(), 1280, 720, 1280, 720, 1).unwrap();
    let lease = pool.convert(&frame).unwrap();
    let texture = lease.texture().clone();
    let original_texture = texture.as_raw();
    let released = Arc::new(AtomicUsize::new(0));
    let sample = gpu_sample::tracked_nv12_sample(
        &texture,
        0,
        frame.timestamp_us(),
        30,
        true,
        CountedLease {
            lease: Some(lease),
            released: released.clone(),
        },
    )
    .unwrap();
    let external_sample_reference = sample.clone();
    let mut backend = native::MediaFoundationEncoder::for_gpu(
        &device,
        1280,
        720,
        30,
        2_500_000,
        H264Profile::Main,
    )
    .unwrap();
    let output = backend.encode_surface(&sample, true).unwrap();
    assert!(backend.last_input_accepted());
    assert!(
        matches!(pool.convert(&frame), Err(GpuError::Busy)),
        "output availability must not return the sample's lease"
    );
    drop(backend); // Actual MFT flush/end-stream/shutdown occurs here.
    drop(sample);
    drop(_runtime); // No encoder-owned MFStartup guard remains for the late reference.
    assert_eq!(released.load(Ordering::Acquire), 0);
    assert!(matches!(pool.convert(&frame), Err(GpuError::Busy)));
    drop(external_sample_reference);
    let deadline = Instant::now() + Duration::from_secs(2);
    let reclaimed = loop {
        match pool.convert(&frame) {
            Ok(lease) => break lease,
            Err(GpuError::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1))
            }
            Err(error) => panic!("tracked sample did not release pool lease: {error:?}"),
        }
    };
    assert_eq!(released.load(Ordering::Acquire), 1);
    assert_eq!(reclaimed.texture().as_raw(), original_texture);
    println!(
        "tracked sample final-reference recycling verified; output_ready_before_drop={}",
        output.is_some()
    );
}

#[test]
#[ignore = "explicit synthetic GPU failure to same-frame software IDR regression"]
fn surface_failure_retries_same_bgra_in_software_and_latches_gpu_off() {
    let (_device, _pool, frame) = fixture(512, 512);
    let mut encoder = encoder(512, 512);
    encoder.encode_gpu(&frame, true).unwrap();
    assert!(encoder.gpu_status().surface_backend_active);
    let previous = encoder.config_version();
    let readback_before = gpu_metrics().cpu_readback_bytes;
    encoder.backend.inject_hardware_failure();
    let next = frame.with_timestamp(66_666);
    let output = encoder.encode_gpu(&next, false).unwrap();
    assert_eq!(output.timestamp_us, next.timestamp_us());
    assert!(output.keyframe);
    assert!(output.config_version > previous);
    assert_eq!(output.config_version, encoder.config_version());
    assert!(!encoder.wants_gpu_input());
    assert!(!encoder.gpu_status().surface_backend_active);
    assert_eq!(encoder.gpu_status().cpu_fallbacks, 1);
    assert_eq!(
        encoder.backend_info().unwrap().kind,
        "microsoft_software_mft"
    );
    assert_eq!(
        encoder.backend_info().unwrap().fallback_reason,
        Some("gpu_surface_failure")
    );
    let sample = encoder.last_performance_sample().unwrap();
    assert!(!sample.gpu_surface_input);
    assert!(sample.nv12_written_bytes > 0 && sample.mf_input_copy_bytes > 0);
    assert!(gpu_metrics().cpu_readback_bytes > readback_before);
}

#[test]
#[ignore = "explicit synthetic delayed GPU output attribution regression"]
fn postponed_surface_output_keeps_original_id_timestamp_and_config() {
    let (_device, _pool, frame) = fixture(512, 512);
    let mut encoder = encoder(512, 512);
    let first = encoder.encode_gpu(&frame, true).unwrap();
    let config = first.config_version;
    encoder.backend.delay_next_surface_output();
    assert_eq!(
        encoder.encode_gpu(&frame.with_timestamp(66_666), false),
        Err(EncoderError::NeedMoreInput)
    );
    assert_eq!(encoder.pending_frames.len(), 1);
    let old = encoder
        .encode_gpu(&frame.with_timestamp(99_999), false)
        .unwrap();
    assert_eq!(old.frame_id, 2);
    assert_eq!(old.timestamp_us, 66_666);
    assert_eq!(old.config_version, config);
    let sample = encoder.last_performance_sample().unwrap();
    assert_eq!(sample.frame_id, 3);
    assert_eq!(sample.output_frame_id, Some(2));
    assert!(sample.gpu_surface_input);
    assert!(encoder.gpu_status().surface_backend_active);
    assert_eq!(encoder.gpu_status().cpu_fallbacks, 0);
}

#[test]
#[ignore = "explicit synthetic GPU configuration-overflow regression"]
fn gpu_fallback_exhaustion_never_reuses_config_or_replaces_backend() {
    let (_device, _pool, frame) = fixture(512, 512);
    let mut encoder = encoder(512, 512);
    encoder.encode_gpu(&frame, true).unwrap();
    let backend = encoder.backend.identity();
    encoder.config_version = u32::MAX;
    encoder.backend.inject_hardware_failure();
    assert_eq!(
        encoder.encode_gpu(&frame.with_timestamp(66_666), false),
        Err(EncoderError::ConfigVersionExhausted)
    );
    assert_eq!(encoder.config_version(), u32::MAX);
    assert_eq!(encoder.backend.identity(), backend);
    assert!(encoder.gpu_status().surface_backend_active);
}

#[test]
#[ignore = "explicit synthetic CPU/GPU mode-switch identity regression"]
fn cpu_frame_after_gpu_switches_real_config_then_gpu_can_bind_again() {
    let (_device, _pool, frame) = fixture(512, 512);
    let mut encoder = encoder(512, 512);
    let first = encoder.encode_gpu(&frame, true).unwrap();
    let cpu = encoder
        .encode(
            CapturedFrame {
                width: 512,
                height: 512,
                timestamp_us: 66_666,
                pixels: vec![0; 512 * 512 * 4],
            },
            false,
        )
        .unwrap();
    assert!(cpu.config_version > first.config_version);
    assert!(cpu.keyframe);
    assert!(
        encoder.gpu_input.is_none(),
        "CPU input must release the no-longer-used NV12 pool owner"
    );
    assert!(!encoder.gpu_status().surface_backend_active);
    assert!(
        encoder.gpu_input.is_none(),
        "CPU mode must release its unused NV12 pool owner"
    );
    assert!(encoder.wants_gpu_input());
    let next = encoder
        .encode_gpu(&frame.with_timestamp(99_999), false)
        .unwrap();
    assert!(next.config_version > cpu.config_version);
    assert_eq!(next.timestamp_us, 99_999);
    assert!(next.keyframe);
    assert!(encoder.gpu_status().surface_backend_active);
}
