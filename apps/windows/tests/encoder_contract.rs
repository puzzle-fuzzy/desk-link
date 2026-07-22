use apps_windows::encoder::{
    EncoderError, H264EncoderSettings, PixelOrder, convert_to_nv12, fit_h264_dimensions,
    fit_h264_dimensions_with_limit,
};
use desklink_protocol::H264Profile;
use std::time::Instant;

#[test]
fn fits_large_frames_inside_level_safe_bounds_without_distortion() {
    assert_eq!(fit_h264_dimensions(2560, 1440), Ok((2560, 1440)));
    assert_eq!(fit_h264_dimensions(3840, 2160), Ok((2560, 1440)));
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
fn default_screen_profile_reserves_text_friendly_bitrate() {
    let settings = H264EncoderSettings::default();
    assert_eq!(
        (settings.max_width, settings.max_height, settings.fps),
        (2560, 1440, 30)
    );
    assert_eq!(settings.bitrate, 18_000_000);
    assert_eq!(settings.profile, H264Profile::Main);
}

#[test]
fn encoder_settings_can_select_high_profile_without_changing_dimensions() {
    let settings = H264EncoderSettings {
        profile: H264Profile::High,
        ..H264EncoderSettings::default()
    };
    assert_eq!(settings.validate().unwrap().profile, H264Profile::High);
}

#[test]
fn experimental_4k_settings_are_explicit_and_not_the_default() {
    let experimental = H264EncoderSettings::experimental_4k();
    let default = H264EncoderSettings::default();
    assert_eq!(
        (experimental.max_width, experimental.max_height),
        (3840, 2160)
    );
    assert_eq!(experimental.bitrate, 40_500_000);
    assert_eq!(experimental.profile, H264Profile::High);
    assert_eq!(
        fit_h264_dimensions_with_limit(3840, 2160, experimental.max_width, experimental.max_height,),
        Ok((3840, 2160))
    );
    assert_ne!((default.max_width, default.max_height), (3840, 2160));
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
fn scaled_luma_keeps_intermediate_glyph_coverage_instead_of_nearest_pixels() {
    let grayscale = [0_u8, 85, 170, 255];
    let mut bgra = vec![0_u8; 4 * 2 * 4];
    for row in 0..2 {
        for (column, value) in grayscale.into_iter().enumerate() {
            let offset = row * 16 + column * 4;
            bgra[offset..offset + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }

    let nv12 = convert_to_nv12(&bgra, 4, 2, 16, 2, 2, PixelOrder::Bgra).unwrap();

    assert_eq!(&nv12[..4], &[53, 199, 53, 199]);
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
    for target_y in (0..target_height).step_by(2) {
        for target_x in (0..target_width).step_by(2) {
            let samples = [
                reference_rgb_for_target(
                    &rgb,
                    source_width,
                    source_height,
                    target_width,
                    target_height,
                    target_x,
                    target_y,
                ),
                reference_rgb_for_target(
                    &rgb,
                    source_width,
                    source_height,
                    target_width,
                    target_height,
                    target_x + 1,
                    target_y,
                ),
                reference_rgb_for_target(
                    &rgb,
                    source_width,
                    source_height,
                    target_width,
                    target_height,
                    target_x,
                    target_y + 1,
                ),
                reference_rgb_for_target(
                    &rgb,
                    source_width,
                    source_height,
                    target_width,
                    target_height,
                    target_x + 1,
                    target_y + 1,
                ),
            ];
            output[target_y * target_width + target_x] =
                luma(samples[0].0, samples[0].1, samples[0].2);
            output[target_y * target_width + target_x + 1] =
                luma(samples[1].0, samples[1].1, samples[1].2);
            output[(target_y + 1) * target_width + target_x] =
                luma(samples[2].0, samples[2].1, samples[2].2);
            output[(target_y + 1) * target_width + target_x + 1] =
                luma(samples[3].0, samples[3].1, samples[3].2);
            let (red, green, blue) =
                samples
                    .into_iter()
                    .fold((0_u32, 0_u32, 0_u32), |(red, green, blue), sample| {
                        (
                            red + u32::from(sample.0),
                            green + u32::from(sample.1),
                            blue + u32::from(sample.2),
                        )
                    });
            let (u, v) = chroma((red / 4) as u8, (green / 4) as u8, (blue / 4) as u8);
            let offset = y_plane_len + target_y / 2 * target_width + target_x;
            output[offset] = u;
            output[offset + 1] = v;
        }
    }
    output
}

fn reference_rgb_for_target(
    rgb: &dyn Fn(usize, usize) -> (u8, u8, u8),
    source_width: usize,
    source_height: usize,
    target_width: usize,
    target_height: usize,
    target_x: usize,
    target_y: usize,
) -> (u8, u8, u8) {
    if source_width == target_width && source_height == target_height {
        return rgb(target_x, target_y);
    }
    let x_denominator = target_width * 2;
    let y_denominator = target_height * 2;
    let x_numerator =
        ((target_x * 2 + 1) * source_width - target_width).min((source_width - 1) * x_denominator);
    let y_numerator = ((target_y * 2 + 1) * source_height - target_height)
        .min((source_height - 1) * y_denominator);
    let x0 = x_numerator / x_denominator;
    let y0 = y_numerator / y_denominator;
    let x1 = (x0 + 1).min(source_width - 1);
    let y1 = (y0 + 1).min(source_height - 1);
    let x_weight = x_numerator % x_denominator;
    let y_weight = y_numerator % y_denominator;
    let top_left = rgb(x0, y0);
    let top_right = rgb(x1, y0);
    let bottom_left = rgb(x0, y1);
    let bottom_right = rgb(x1, y1);
    let blend = |top_left: u8, top_right: u8, bottom_left: u8, bottom_right: u8| {
        let top =
            usize::from(top_left) * (x_denominator - x_weight) + usize::from(top_right) * x_weight;
        let bottom = usize::from(bottom_left) * (x_denominator - x_weight)
            + usize::from(bottom_right) * x_weight;
        ((top * (y_denominator - y_weight) + bottom * y_weight + x_denominator * y_denominator / 2)
            / (x_denominator * y_denominator)) as u8
    };
    (
        blend(top_left.0, top_right.0, bottom_left.0, bottom_right.0),
        blend(top_left.1, top_right.1, bottom_left.1, bottom_right.1),
        blend(top_left.2, top_right.2, bottom_left.2, bottom_right.2),
    )
}
