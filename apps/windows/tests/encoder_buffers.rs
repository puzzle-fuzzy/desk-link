use apps_windows::encoder::{EncoderError, PixelOrder, convert_to_nv12, convert_to_nv12_into};

#[test]
fn reusable_nv12_matches_conversion_for_padding_orders_and_resizes() {
    let mut pixels = vec![0_u8; 32 * 4];
    for y in 0..4 {
        for x in 0..6 {
            let offset = y * 32 + x * 4;
            pixels[offset..offset + 4].copy_from_slice(&[
                (x * 41) as u8,
                (y * 61) as u8,
                (x * y * 13) as u8,
                255,
            ]);
        }
    }
    let mut scratch = Vec::with_capacity(8 * 6 * 3 / 2);
    let pointer = scratch.as_ptr();
    let capacity = scratch.capacity();
    for order in [PixelOrder::Bgra, PixelOrder::Rgba] {
        for (width, height) in [(6, 4), (2, 2), (8, 6), (4, 2), (6, 4)] {
            let expected = convert_to_nv12(&pixels, 6, 4, 32, width, height, order).unwrap();
            convert_to_nv12_into(&mut scratch, &pixels, 6, 4, 32, width, height, order).unwrap();
            assert_eq!(scratch, expected);
            assert_eq!(
                scratch.as_ptr(),
                pointer,
                "capacity suffices across resolution changes"
            );
            assert_eq!(scratch.capacity(), capacity);
        }
    }
}

#[test]
fn reusable_buffer_overwrites_old_luma_and_chroma_after_shrink_and_growth() {
    let mut scratch = vec![7_u8; 8 * 6 * 3 / 2];
    let black = vec![0_u8; 8 * 6 * 4];
    let white = vec![255_u8; black.len()];
    for (pixels, width, height, expected_y) in [
        (&black, 8, 6, 16),
        (&white, 2, 2, 235),
        (&white, 8, 6, 235),
        (&black, 4, 2, 16),
    ] {
        convert_to_nv12_into(
            &mut scratch,
            pixels,
            8,
            6,
            8 * 4,
            width,
            height,
            PixelOrder::Bgra,
        )
        .unwrap();
        let y_len = width as usize * height as usize;
        assert!(scratch[..y_len].iter().all(|value| *value == expected_y));
        assert!(scratch[y_len..].iter().all(|value| *value == 128));
        assert_eq!(scratch.len(), y_len * 3 / 2);
    }
}

#[test]
fn invalid_input_or_overflow_keeps_previous_buffer_and_allocation() {
    let mut scratch = vec![91_u8; 6];
    let previous = scratch.clone();
    let pointer = scratch.as_ptr();
    assert_eq!(
        convert_to_nv12_into(&mut scratch, &[0; 15], 2, 2, 8, 2, 2, PixelOrder::Bgra),
        Err(EncoderError::InvalidFrame)
    );
    assert_eq!(
        convert_to_nv12_into(
            &mut scratch,
            &[0; 16],
            2,
            2,
            8,
            u32::MAX - 1,
            u32::MAX - 1,
            PixelOrder::Bgra
        ),
        Err(EncoderError::FrameTooLarge)
    );
    assert_eq!(scratch, previous);
    assert_eq!(scratch.as_ptr(), pointer);
}

#[test]
#[ignore = "offline CPU conversion baseline; run with --release --ignored --nocapture"]
#[allow(clippy::assertions_on_constants)] // The ignored test must reject accidental debug runs.
fn conversion_buffer_reuse_release_baseline() {
    use std::{hint::black_box, time::Instant};
    assert!(
        !cfg!(debug_assertions),
        "use --release for comparable CPU stage measurements"
    );
    let percentiles = |samples: &mut Vec<u128>| {
        samples.sort_unstable();
        let at = |percent: usize| samples[(samples.len() * percent).div_ceil(100) - 1];
        serde_json::json!({"p50": at(50), "p95": at(95), "p99": at(99)})
    };
    for (width, height) in [(1920_u32, 1080_u32), (2560, 1440)] {
        let mut pixels = vec![0_u8; width as usize * height as usize * 4];
        for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
            pixel.copy_from_slice(&[index as u8, (index / 17) as u8, (index / 193) as u8, 255]);
        }
        let mut scratch = Vec::new();
        for _ in 0..5 {
            black_box(
                convert_to_nv12(
                    &pixels,
                    width,
                    height,
                    width as usize * 4,
                    width,
                    height,
                    PixelOrder::Bgra,
                )
                .unwrap(),
            );
            convert_to_nv12_into(
                &mut scratch,
                &pixels,
                width,
                height,
                width as usize * 4,
                width,
                height,
                PixelOrder::Bgra,
            )
            .unwrap();
        }
        let pointer = scratch.as_ptr();
        let capacity = scratch.capacity();
        let mut allocated_ns = Vec::new();
        let mut reused_ns = Vec::new();
        for iteration in 0..30 {
            let allocated;
            let mut run_reused = || {
                let started = Instant::now();
                convert_to_nv12_into(
                    &mut scratch,
                    &pixels,
                    width,
                    height,
                    width as usize * 4,
                    width,
                    height,
                    PixelOrder::Bgra,
                )
                .unwrap();
                started.elapsed().as_nanos()
            };
            let run_allocated = || {
                let started = Instant::now();
                let result = convert_to_nv12(
                    &pixels,
                    width,
                    height,
                    width as usize * 4,
                    width,
                    height,
                    PixelOrder::Bgra,
                )
                .unwrap();
                (result, started.elapsed().as_nanos())
            };
            let allocated_time;
            // Alternate order to avoid always measuring one API after the other.
            if iteration % 2 == 0 {
                (allocated, allocated_time) = run_allocated();
                reused_ns.push(run_reused());
            } else {
                reused_ns.push(run_reused());
                (allocated, allocated_time) = run_allocated();
            }
            allocated_ns.push(allocated_time);
            assert_eq!(
                scratch, allocated,
                "each measured frame must be byte identical"
            );
            assert_eq!(scratch.as_ptr(), pointer);
            assert_eq!(scratch.capacity(), capacity);
            black_box(&scratch);
        }
        println!(
            "{}",
            serde_json::json!({
                "scope": "offline_cpu_nv12_conversion_only", "width": width, "height": height,
                "warmup": 5, "samples_per_api": 30,
                "allocated_ns": percentiles(&mut allocated_ns), "reused_ns": percentiles(&mut reused_ns),
                "exact_output_match": true, "reused_pointer_stable": true,
                "reused_output_buffer_grows_after_warmup": 0,
                "nv12_written_bytes_per_api": scratch.len() * 30,
                "gpu_or_end_to_end_gain_measured": false
            })
        );
    }
}
