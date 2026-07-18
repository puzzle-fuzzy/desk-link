use apps_windows::encoder::{
    EncoderError, H264EncoderSettings, PixelOrder, convert_to_nv12, fit_h264_dimensions,
    fit_h264_dimensions_with_limit,
};

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
