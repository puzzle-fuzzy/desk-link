#[cfg(windows)]
#[test]
#[ignore = "requires access to the interactive Windows desktop"]
fn captured_desktop_frame_encodes_to_h264() {
    use std::time::Duration;

    use apps_windows::{
        capture::{CaptureError, DesktopCapturer, DxgiDesktopCapturer},
        encoder::{EncoderError, H264Encoder, fit_h264_dimensions},
    };

    let mut capture = DxgiDesktopCapturer::new_primary().expect("capture init");
    let (source_width, source_height) = capture.dimensions();
    let (width, height) =
        fit_h264_dimensions(source_width, source_height).expect("supported desktop dimensions");
    let mut encoder = H264Encoder::new(width, height, 30).expect("Media Foundation encoder init");

    let mut submitted = 0_u64;
    let forced_keyframe_id = 3_u64;
    for _ in 0..60 {
        let frame = match capture.next_frame(Duration::from_millis(500)) {
            Ok(frame) => frame,
            Err(CaptureError::Timeout) => continue,
            Err(error) => panic!("capture failed: {error:?}"),
        };
        let request_keyframe = submitted + 1 == forced_keyframe_id;
        match encoder.encode(frame, request_keyframe) {
            Ok(encoded) => {
                submitted += 1;
                assert!(!encoded.access_unit.is_empty());
                if encoded.frame_id == 1 {
                    let sequence_header = encoded
                        .sequence_header
                        .as_ref()
                        .expect("the first access unit must expose decoder configuration");
                    println!(
                        "H.264 sequence header prefix: {:02X?}",
                        &sequence_header[..sequence_header.len().min(32)]
                    );
                    println!(
                        "H.264 access unit prefix: {:02X?}",
                        &encoded.access_unit[..encoded.access_unit.len().min(32)]
                    );
                    assert!(
                        encoded.keyframe,
                        "the first access unit must be random-access"
                    );
                    assert!(!sequence_header.is_empty());
                }
                if encoded.frame_id == forced_keyframe_id {
                    assert!(
                        encoded.keyframe,
                        "the explicitly requested frame must be an IDR"
                    );
                    return;
                }
            }
            Err(EncoderError::NeedMoreInput) => submitted += 1,
            Err(error) => panic!("encode failed: {error:?}"),
        }
    }

    panic!("encoder produced no forced H.264 keyframe after 60 captured frames");
}

#[cfg(windows)]
#[test]
#[ignore = "manual 4K Media Foundation capture and encode capability probe"]
fn experimental_4k_media_foundation_captures_and_encodes() {
    use std::time::Duration;

    use apps_windows::{
        capture::{CaptureError, DesktopCapturer, DxgiDesktopCapturer},
        encoder::{EncoderError, H264Encoder, H264EncoderSettings},
    };

    let mut capture = DxgiDesktopCapturer::new_primary().expect("capture init");
    let mut encoder =
        H264Encoder::new_with_settings(3840, 2160, H264EncoderSettings::experimental_4k())
            .expect("4K Media Foundation encoder init");
    assert_eq!(encoder.dimensions(), (3840, 2160));

    let mut encoded_frames = 0_u32;
    let mut keyframes = 0_u32;
    for _ in 0..60 {
        let frame = match capture.next_frame(Duration::from_millis(500)) {
            Ok(frame) => frame,
            Err(CaptureError::Timeout) => continue,
            Err(error) => panic!("capture failed: {error:?}"),
        };
        // Keep the probe useful on a 1080p/1440p development monitor too: the
        // encoder must receive a 4K input, otherwise it legitimately rebuilds
        // itself back to the captured desktop size on the first frame.
        let frame = upscale_bgra_to_4k(frame);
        match encoder.encode(frame, encoded_frames == 0) {
            Ok(encoded) => {
                encoded_frames = encoded_frames.saturating_add(1);
                keyframes = keyframes.saturating_add(u32::from(encoded.keyframe));
                assert!(!encoded.access_unit.is_empty());
                if encoded_frames == 1 {
                    assert!(
                        encoded.keyframe,
                        "the first 4K access unit must be random-access"
                    );
                    assert!(
                        encoded
                            .sequence_header
                            .as_ref()
                            .is_some_and(|header| !header.is_empty()),
                        "the first 4K access unit must expose decoder configuration"
                    );
                }
            }
            Err(EncoderError::NeedMoreInput) => {}
            Err(error) => panic!("4K encode failed: {error:?}"),
        }
        if encoded_frames >= 10 {
            break;
        }
    }

    assert!(encoded_frames > 0, "4K encoder produced no access unit");
    assert!(keyframes > 0, "4K encoder produced no keyframe");
    println!(
        "4K encoder probe passed: profile={:?}, dimensions={:?}, encoded_frames={encoded_frames}, keyframes={keyframes}",
        encoder.profile(),
        encoder.dimensions(),
    );
}

#[cfg(windows)]
fn upscale_bgra_to_4k(
    frame: apps_windows::capture::CapturedFrame,
) -> apps_windows::capture::CapturedFrame {
    const TARGET_WIDTH: usize = 3840;
    const TARGET_HEIGHT: usize = 2160;
    let source_width = usize::try_from(frame.width).expect("source width fits usize");
    let source_height = usize::try_from(frame.height).expect("source height fits usize");
    assert!(source_width > 0 && source_height > 0);
    let source_stride = source_width
        .checked_mul(4)
        .expect("source stride fits usize");
    let source_len = source_stride
        .checked_mul(source_height)
        .expect("source frame fits usize");
    assert!(frame.pixels.len() >= source_len);

    let mut pixels = vec![0_u8; TARGET_WIDTH * TARGET_HEIGHT * 4];
    for target_y in 0..TARGET_HEIGHT {
        let source_y = target_y * source_height / TARGET_HEIGHT;
        for target_x in 0..TARGET_WIDTH {
            let source_x = target_x * source_width / TARGET_WIDTH;
            let source_offset = source_y * source_stride + source_x * 4;
            let target_offset = (target_y * TARGET_WIDTH + target_x) * 4;
            pixels[target_offset..target_offset + 4]
                .copy_from_slice(&frame.pixels[source_offset..source_offset + 4]);
        }
    }
    apps_windows::capture::CapturedFrame {
        width: TARGET_WIDTH as u32,
        height: TARGET_HEIGHT as u32,
        timestamp_us: frame.timestamp_us,
        pixels,
    }
}
