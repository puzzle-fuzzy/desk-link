//! Offline synthetic benchmark: never captures the desktop or opens a session.
use std::{hint::black_box, time::Instant};

use apps_windows::{
    capture::CapturedFrame,
    encoder::{
        EncoderBackendPreference, EncoderError, H264Encoder, H264EncoderSettings, PixelOrder,
        convert_to_nv12,
    },
};
use serde_json::{Value, json};

fn percentiles(samples: &[u64]) -> Value {
    if samples.is_empty() {
        return Value::Null;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let at = |percent: usize| ordered[(ordered.len() * percent).div_ceil(100).saturating_sub(1)];
    json!({"count": ordered.len(), "p50": at(50), "p95": at(95), "p99": at(99), "max": ordered.last()})
}

fn number(args: &[String], key: &str, default: u32) -> u32 {
    args.iter()
        .position(|arg| arg == key)
        .map(|index| {
            args.get(index + 1)
                .and_then(|value| value.parse().ok())
                .expect("numeric option value")
        })
        .unwrap_or(default)
}

fn micros(start: Instant) -> u64 {
    start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let width = number(&args, "--width", 1920);
    let height = number(&args, "--height", 1080);
    let frames = number(&args, "--frames", 120);
    let warmup = number(&args, "--warmup", 10);
    assert!((2..=3840).contains(&width) && width.is_multiple_of(2));
    assert!((2..=2160).contains(&height) && height.is_multiple_of(2));
    assert!((1..=10_000).contains(&frames) && warmup <= 1_000);
    let encode = args.iter().any(|arg| arg == "--encode");
    let software = args.iter().any(|arg| arg == "--software");
    assert!(!software || encode, "--software requires --encode");
    let motion = args.iter().any(|arg| arg == "--motion");
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    // Deterministic high-contrast glyph-like edges plus colour gradients.
    // This is a reproducible synthetic workload, not a visual-quality score.
    for y in 0..height as usize {
        for x in 0..width as usize {
            let offset = (y * width as usize + x) * 4;
            let edge = if x % 16 < 2 || y % 24 < 2 { 0 } else { 255 };
            pixels[offset..offset + 4].copy_from_slice(&[edge, x as u8, y as u8, 255]);
        }
    }
    let mut encoder = encode.then(|| {
        H264Encoder::new_with_settings_and_backend(
            width,
            height,
            H264EncoderSettings {
                max_width: width,
                max_height: height,
                ..H264EncoderSettings::default()
            },
            if software {
                EncoderBackendPreference::SoftwareOnly
            } else {
                EncoderBackendPreference::PreferHardware
            },
        )
        .expect("synthetic Media Foundation encoder initialization")
    });
    let mut convert_us = Vec::with_capacity(frames as usize);
    let mut mft_us = Vec::with_capacity(frames as usize);
    let mut residence_us = Vec::with_capacity(frames as usize);
    let mut encoded_bytes = 0_u64;
    let mut completed = 0_u32;
    let mut nv12_written_bytes = 0_u64;
    let mut mf_input_copy_bytes = 0_u64;
    let started = Instant::now();
    for frame_index in 0..frames + warmup {
        if motion {
            let offset = (frame_index as usize % (pixels.len() / 4)) * 4;
            pixels[offset] = pixels[offset].wrapping_add(127);
        }
        let measured = frame_index >= warmup;
        if let Some(encoder) = &mut encoder {
            // Fixture cloning is outside the production stage timers.
            let frame = CapturedFrame {
                width,
                height,
                timestamp_us: u64::from(frame_index) * 33_333,
                pixels: pixels.clone(),
            };
            match encoder.encode(frame, frame_index == 0) {
                Ok(output) => {
                    if measured {
                        completed += 1;
                        encoded_bytes += output.access_unit.len() as u64;
                    }
                }
                Err(EncoderError::NeedMoreInput) => {}
                Err(error) => panic!("synthetic encode failed: {error:?}"),
            }
            if measured {
                let sample = encoder
                    .last_performance_sample()
                    .expect("submission sample");
                convert_us.push(sample.convert_us);
                mft_us.push(sample.mft_us);
                residence_us.extend(sample.output_residence_us);
                nv12_written_bytes += sample.nv12_written_bytes as u64;
                mf_input_copy_bytes += sample.mf_input_copy_bytes as u64;
            }
        } else {
            let convert_started = Instant::now();
            let nv12 = convert_to_nv12(
                &pixels,
                width,
                height,
                width as usize * 4,
                width,
                height,
                PixelOrder::Bgra,
            )
            .expect("BGRA conversion");
            let elapsed = micros(convert_started);
            if measured {
                convert_us.push(elapsed);
                nv12_written_bytes += nv12.len() as u64;
            }
            black_box(nv12);
        }
    }
    println!(
        "{}",
        json!({
            "schema_version": 1,
            "scope": "offline_synthetic_local_stages",
            "clock": "local_monotonic_instant",
            "width": width, "height": height, "frames": frames, "warmup": warmup,
            "workload": if motion { "synthetic_changed_pixel" } else { "synthetic_static_edges" },
            "encode_enabled": encode,
            "profile": encoder.as_ref().map(|encoder| format!("{:?}", encoder.profile())),
        "backend": encoder.as_ref().and_then(|encoder| encoder.backend_info()),
            "total_elapsed_with_warmup_us": micros(started),
            "convert_us": percentiles(&convert_us), "mft_us": percentiles(&mft_us),
            "encoder_output_residence_us": percentiles(&residence_us),
            "completed_outputs": completed, "encoded_bytes": encoded_bytes,
            "known_nv12_written_bytes": nv12_written_bytes,
            "known_mf_input_copy_bytes": mf_input_copy_bytes,
            "allocation_rate": Value::Null, "driver_copy_bytes": Value::Null,
            "capture_us": Value::Null, "transport_us": Value::Null,
            "decode_us": Value::Null, "display_us": Value::Null,
            "input_to_display_us": Value::Null, "gpu_time_us": Value::Null,
            "power_watts": Value::Null, "temperature_c": Value::Null
        })
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_percentiles_cover_tails_and_empty_samples() {
        assert_eq!(percentiles(&[]), Value::Null);
        assert_eq!(percentiles(&[7])["p99"], 7);
        let samples = (1..=100).rev().collect::<Vec<_>>();
        let report = percentiles(&samples);
        assert_eq!(report["p50"], 50);
        assert_eq!(report["p95"], 95);
        assert_eq!(report["p99"], 99);
        assert_eq!(report["max"], 100);
    }
}
