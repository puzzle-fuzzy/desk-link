use apps_windows::encoder::{
    EncoderError, H264EncoderSettings, PixelOrder, convert_to_nv12, fit_h264_dimensions,
    fit_h264_dimensions_with_limit,
};
use std::time::Instant;

#[test]
fn fits_large_frames_inside_level_safe_bounds_without_distortion() {
    assert_eq!(fit_h264_dimensions(2560, 1440), Ok((1920, 1080)));
    assert_eq!(fit_h264_dimensions(3840, 2160), Ok((1920, 1080)));
    assert_eq!(fit_h264_dimensions(1365, 768), Ok((1364, 768)));
    assert_eq!(
        fit_h264_dimensions(1, 1),
        Err(EncoderError::InvalidDimensions)
    );
}

#[test]
fn quality_limits_preserve_aspect_ratio_and_even_dimensions() {
    assert_eq!(
        fit_h264_dimensions_with_limit(2560, 1440, 1280, 720),
        Ok((1280, 720))
    );
    assert_eq!(
        fit_h264_dimensions_with_limit(1365, 768, 1280, 720),
        Ok((1278, 720))
    );
    assert_eq!(
        fit_h264_dimensions_with_limit(1080, 1920, 1280, 720),
        Ok((404, 720))
    );
}

#[test]
fn encoder_settings_reject_zero_values() {
    for settings in [
        H264EncoderSettings {
            fps: 0,
            ..H264EncoderSettings::default()
        },
        H264EncoderSettings {
            bitrate: 0,
            ..H264EncoderSettings::default()
        },
        H264EncoderSettings {
            max_width: 1,
            ..H264EncoderSettings::default()
        },
    ] {
        assert_eq!(settings.validate(), Err(EncoderError::InvalidDimensions));
    }
}

#[test]
fn converts_black_bgra_to_limited_range_nv12() {
    let bgra = vec![0_u8; 2 * 2 * 4];

    let nv12 = convert_to_nv12(&bgra, 2, 2, 8, 2, 2, PixelOrder::Bgra).unwrap();

    assert_eq!(nv12, vec![16, 16, 16, 16, 128, 128]);
}

#[test]
fn converts_white_rgba_to_limited_range_nv12() {
    let rgba = vec![255_u8; 2 * 2 * 4];

    let nv12 = convert_to_nv12(&rgba, 2, 2, 8, 2, 2, PixelOrder::Rgba).unwrap();

    assert_eq!(nv12, vec![235, 235, 235, 235, 128, 128]);
}

#[test]
fn scales_using_source_row_pitch_and_rejects_truncated_frames() {
    let mut bgra = vec![0_u8; 4 * 2 * 4 + 8];
    for row in 0..2 {
        for column in 0..4 {
            let offset = row * 24 + column * 4;
            bgra[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }

    assert_eq!(
        convert_to_nv12(&bgra, 4, 2, 24, 2, 2, PixelOrder::Bgra).unwrap(),
        vec![235, 235, 235, 235, 128, 128]
    );
    assert_eq!(
        convert_to_nv12(&bgra[..7], 2, 2, 8, 2, 2, PixelOrder::Bgra),
        Err(EncoderError::InvalidFrame)
    );
}

#[test]
fn optimized_nv12_conversion_matches_the_reference_for_scaled_color_frames() {
    let source_width = 8_usize;
    let source_height = 6_usize;
    let row_pitch = 40_usize;
    for order in [PixelOrder::Bgra, PixelOrder::Rgba] {
        let mut pixels = vec![0_u8; row_pitch * source_height];
        for y in 0..source_height {
            for x in 0..source_width {
                let red = (x * 23 + y * 7) as u8;
                let green = (x * 11 + y * 19) as u8;
                let blue = (x * 5 + y * 29) as u8;
                let offset = y * row_pitch + x * 4;
                let pixel = match order {
                    PixelOrder::Bgra => [blue, green, red, 255],
                    PixelOrder::Rgba => [red, green, blue, 255],
                };
                pixels[offset..offset + 4].copy_from_slice(&pixel);
            }
        }

        assert_eq!(
            convert_to_nv12(
                &pixels,
                source_width as u32,
                source_height as u32,
                row_pitch,
                4,
                4,
                order,
            )
            .unwrap(),
            reference_nv12(&pixels, source_width, source_height, row_pitch, 4, 4, order)
        );
    }
}

#[test]
#[ignore = "manual release-mode hot-path measurement"]
fn reports_1080p_nv12_conversion_speedup() {
    let width = 1920_usize;
    let height = 1080_usize;
    let row_pitch = width * 4;
    let pixels = (0..row_pitch * height)
        .map(|index| index.wrapping_mul(37) as u8)
        .collect::<Vec<_>>();

    let started = Instant::now();
    let optimized = convert_to_nv12(
        std::hint::black_box(&pixels),
        width as u32,
        height as u32,
        row_pitch,
        width as u32,
        height as u32,
        PixelOrder::Bgra,
    )
    .unwrap();
    let optimized_elapsed = started.elapsed();
    let started = Instant::now();
    let reference = reference_nv12(
        std::hint::black_box(&pixels),
        width,
        height,
        row_pitch,
        width,
        height,
        PixelOrder::Bgra,
    );
    let reference_elapsed = started.elapsed();

    assert_eq!(optimized, reference);
    println!(
        "1080p BGRA->NV12: optimized={optimized_elapsed:?}, reference={reference_elapsed:?}, speedup={:.2}x",
        reference_elapsed.as_secs_f64() / optimized_elapsed.as_secs_f64()
    );
}

fn reference_nv12(
    pixels: &[u8],
    source_width: usize,
    source_height: usize,
    row_pitch: usize,
    target_width: usize,
    target_height: usize,
    order: PixelOrder,
) -> Vec<u8> {
    let y_plane_len = target_width * target_height;
    let mut output = vec![0_u8; y_plane_len + y_plane_len / 2];
    let rgb = |x: usize, y: usize| {
        let offset = y * row_pitch + x * 4;
        match order {
            PixelOrder::Bgra => (pixels[offset + 2], pixels[offset + 1], pixels[offset]),
            PixelOrder::Rgba => (pixels[offset], pixels[offset + 1], pixels[offset + 2]),
        }
    };
    let luma = |red: u8, green: u8, blue: u8| {
        ((47 * i32::from(red) + 157 * i32::from(green) + 16 * i32::from(blue) + 128) / 256 + 16)
            .clamp(0, 255) as u8
    };
    let chroma = |red: u8, green: u8, blue: u8| {
        let u = (-26 * i32::from(red) - 87 * i32::from(green) + 113 * i32::from(blue) + 128) / 256
            + 128;
        let v = (112 * i32::from(red) - 102 * i32::from(green) - 10 * i32::from(blue) + 128) / 256
            + 128;
        (u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
    };
    for target_y in 0..target_height {
        let source_y = target_y * source_height / target_height;
        for target_x in 0..target_width {
            let source_x = target_x * source_width / target_width;
            let (red, green, blue) = rgb(source_x, source_y);
            output[target_y * target_width + target_x] = luma(red, green, blue);
        }
    }
    for target_y in (0..target_height).step_by(2) {
        for target_x in (0..target_width).step_by(2) {
            let mut sum = (0_u32, 0_u32, 0_u32);
            for offset_y in 0..2 {
                for offset_x in 0..2 {
                    let source_x = (target_x + offset_x) * source_width / target_width;
                    let source_y = (target_y + offset_y) * source_height / target_height;
                    let sample = rgb(source_x, source_y);
                    sum.0 += u32::from(sample.0);
                    sum.1 += u32::from(sample.1);
                    sum.2 += u32::from(sample.2);
                }
            }
            let (u, v) = chroma((sum.0 / 4) as u8, (sum.1 / 4) as u8, (sum.2 / 4) as u8);
            let offset = y_plane_len + target_y / 2 * target_width + target_x;
            output[offset] = u;
            output[offset + 1] = v;
        }
    }
    output
}
