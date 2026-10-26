//! Local synthetic D3D11/MFT evidence. Never acquires a desktop or remote session.

#[cfg(windows)]
mod probe {
    use std::time::Instant;

    use apps_windows::{
        capture::CapturedFrame,
        encoder::{
            EncoderError, H264Encoder, H264EncoderSettings, PixelOrder, convert_to_nv12,
            probe_hardware_encoder_capabilities,
        },
        gpu::{
            GpuAdapterInfo, GpuBgraPool, GpuMetrics, GpuNv12Pool, create_device,
            create_synthetic_bgra, enumerate_adapters, gpu_metrics, readback_nv12,
        },
    };
    use blake2::{Blake2s256, Digest};
    use serde_json::{Value, json};

    #[derive(Clone)]
    struct Options {
        mode: String,
        width: u32,
        height: u32,
        output_width: u32,
        output_height: u32,
        frames: u32,
        warmup: u32,
        adapter_index: Option<usize>,
        encode: bool,
        motion: bool,
        input_path: String,
        experimental_4k: bool,
        workload: String,
    }

    fn options() -> Result<Options, String> {
        let mut value = Options {
            mode: "capability".into(),
            width: 1280,
            height: 720,
            output_width: 0,
            output_height: 0,
            frames: 30,
            warmup: 3,
            adapter_index: None,
            encode: false,
            motion: false,
            input_path: "gpu".into(),
            experimental_4k: false,
            workload: "desktop".into(),
        };
        let mut args = std::env::args().skip(1);
        while let Some(key) = args.next() {
            match key.as_str() {
                "--encode" => value.encode = true,
                "--motion" => value.motion = true,
                "--experimental-4k" => value.experimental_4k = true,
                "--input-path" => value.input_path = args.next().ok_or("missing input path")?,
                "--workload" => value.workload = args.next().ok_or("missing workload")?,
                "--mode" => value.mode = args.next().ok_or("missing mode")?,
                "--adapter-index" => {
                    value.adapter_index = Some(
                        args.next()
                            .ok_or("missing adapter index")?
                            .parse()
                            .map_err(|_| "invalid adapter index")?,
                    )
                }
                "--width" | "--height" | "--output-width" | "--output-height" | "--frames"
                | "--warmup" => {
                    let number = args
                        .next()
                        .ok_or("missing numeric option")?
                        .parse()
                        .map_err(|_| "invalid numeric option")?;
                    match key.as_str() {
                        "--width" => value.width = number,
                        "--height" => value.height = number,
                        "--output-width" => value.output_width = number,
                        "--output-height" => value.output_height = number,
                        "--frames" => value.frames = number,
                        "--warmup" => value.warmup = number,
                        _ => unreachable!(),
                    }
                }
                _ => return Err("unknown probe option".into()),
            }
        }
        if value.output_width == 0 && value.output_height == 0 {
            value.output_width = value.width;
            value.output_height = value.height;
        }
        if !matches!(value.mode.as_str(), "capability" | "benchmark")
            || [
                value.width,
                value.height,
                value.output_width,
                value.output_height,
            ]
            .iter()
            .any(|n| *n < 2 || *n > 8192 || !n.is_multiple_of(2))
            || !(1..=10_000).contains(&value.frames)
            || value.warmup > 1000
            || (value.encode && value.mode != "benchmark")
            || !matches!(value.input_path.as_str(), "gpu" | "cpu")
            || !matches!(value.workload.as_str(), "desktop" | "tiles")
            || ((value.input_path == "cpu" || value.experimental_4k) && !value.encode)
        {
            return Err("invalid mode, even dimensions, frame count or encode option".into());
        }
        Ok(value)
    }

    fn encoder_settings(options: &Options) -> H264EncoderSettings {
        let limits = if options.experimental_4k {
            H264EncoderSettings::experimental_4k()
        } else {
            H264EncoderSettings::default()
        };
        H264EncoderSettings {
            max_width: options.output_width.min(limits.max_width),
            max_height: options.output_height.min(limits.max_height),
            ..limits
        }
    }

    fn elapsed_us(started: Instant) -> u64 {
        started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
    }

    fn percentiles(samples: &[u64]) -> Value {
        if samples.is_empty() {
            return Value::Null;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_unstable();
        let at = |percent: usize| sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)];
        json!({"count": sorted.len(), "p50": at(50), "p95": at(95), "p99": at(99), "max": sorted.last()})
    }

    fn metrics_delta(before: GpuMetrics, after: GpuMetrics) -> Value {
        json!({
            "bgra_gpu_copy_bytes": after.bgra_copy_bytes.saturating_sub(before.bgra_copy_bytes),
            "cpu_readback_bytes": after.cpu_readback_bytes.saturating_sub(before.cpu_readback_bytes),
            "texture_creations": after.texture_creations.saturating_sub(before.texture_creations),
            "query_creations": after.query_creations.saturating_sub(before.query_creations),
            "vp_input_view_creations": after.vp_input_view_creations.saturating_sub(before.vp_input_view_creations),
            "vp_output_view_creations": after.vp_output_view_creations.saturating_sub(before.vp_output_view_creations),
            "nv12_gpu_written_bytes": after.nv12_gpu_written_bytes.saturating_sub(before.nv12_gpu_written_bytes),
            "counter_scope": "explicit_project_api_operations_not_driver_or_heap_telemetry",
        })
    }

    fn pattern(width: u32, height: u32, phase: usize) -> Vec<u8> {
        const COLORS: [[u8; 4]; 8] = [
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [0, 0, 255, 255],
            [0, 255, 0, 255],
            [255, 0, 0, 255],
            [0, 255, 255, 255],
            [255, 255, 0, 255],
            [255, 0, 255, 255],
        ];
        let mut pixels = vec![0; width as usize * height as usize * 4];
        for y in 0..height as usize {
            for x in 0..width as usize {
                // Every colour boundary aligns to a chroma 2x2 block. Different
                // rows make vertical flips distinguishable from valid output.
                let column = (x / 2) * 8 / (width as usize / 2);
                let row = (y / 2) * 4 / (height as usize / 2);
                let color = COLORS[(column + row * 3 + phase) % COLORS.len()];
                pixels[(y * width as usize + x) * 4..][..4].copy_from_slice(&color);
            }
        }
        pixels
    }

    fn desktop_pattern(width: u32, height: u32, phase: usize) -> Vec<u8> {
        let (width, height) = (width as usize, height as usize);
        let mut pixels = vec![0; width * height * 4];
        let sidebar = width / 5;
        for y in 0..height {
            for x in 0..width {
                let mut color = [
                    (28 + x * 90 / width) as u8,
                    (24 + y * 70 / height) as u8,
                    (20 + (x + y) * 50 / (width + height)) as u8,
                    255,
                ];
                if x >= 12 && x + 12 < width && y >= 12 && y + 12 < height {
                    color = [238, 239, 241, 255];
                    if y < 44 {
                        color = [210, 215, 220, 255];
                        if x > sidebar && x % 29 < 17 && y % 18 < 3 {
                            color = [36, 40, 43, 255];
                        }
                    } else if x < sidebar {
                        color = [221, 226, 231, 255];
                        if x > 24 && x + 12 < sidebar && y % 37 < 9 {
                            color = [120, 135, 151, 255];
                        }
                    } else {
                        // Only the content panel scrolls; chrome stays fixed.
                        let scroll_y = y + phase * 4;
                        let local_x = x - sidebar;
                        if (scroll_y / 72).is_multiple_of(2) {
                            color = [246, 245, 243, 255];
                        }
                        if local_x % 192 >= 150 && scroll_y % 72 < 48 {
                            color = [
                                (x % 251) as u8,
                                (scroll_y % 239) as u8,
                                ((x + scroll_y) % 241) as u8,
                                255,
                            ];
                        } else if local_x % 192 < 140 && scroll_y % 24 < 16 {
                            let glyph = (local_x / 9 + scroll_y / 24 * 13) as u32;
                            let gx = local_x % 9;
                            let gy = scroll_y % 24;
                            let strokes = gx == 1
                                || gy == 1
                                || gy == 8
                                || (gx < 7
                                    && gy < 14
                                    && (glyph.rotate_left(gy as u32) >> gx) & 1 != 0);
                            if strokes {
                                color = [25, 28, 31, 255];
                            }
                        }
                    }
                }
                pixels[(y * width + x) * 4..][..4].copy_from_slice(&color);
            }
        }
        pixels
    }

    fn benchmark_pattern(options: &Options, phase: usize) -> Vec<u8> {
        if options.workload == "desktop" {
            desktop_pattern(options.width, options.height, phase)
        } else {
            pattern(options.width, options.height, phase)
        }
    }

    fn compare_nv12(actual: &[u8], expected: &[u8], width: u32, height: u32) -> Value {
        let expected_length = width as usize * height as usize * 3 / 2;
        if actual.len() != expected_length || expected.len() != expected_length {
            return json!({"passed": false, "reason": "packed_nv12_length", "actual_bytes": actual.len(), "expected_bytes": expected_length});
        }
        // Check 3x3 interior positions in every tile. Boundary chroma filters
        // differ by GPU vendor; those edges are not a pixel-exact quality test.
        let (width, height) = (width as usize, height as usize);
        let mut compared = 0_u64;
        let mut failures = 0_u64;
        let mut maximum_delta = 0_u8;
        for row in 0..4 {
            for column in 0..8 {
                for fy in [1, 2, 3] {
                    for fx in [1, 2, 3] {
                        let x = (((column * 4 + fx) * width / 32) & !1).min(width - 2);
                        let y = (((row * 4 + fy) * height / 16) & !1).min(height - 2);
                        for offset in [
                            y * width + x,
                            width * height + (y / 2) * width + x,
                            width * height + (y / 2) * width + x + 1,
                        ] {
                            let delta = actual[offset].abs_diff(expected[offset]);
                            maximum_delta = maximum_delta.max(delta);
                            compared += 1;
                            failures += u64::from(delta > 2);
                        }
                    }
                }
            }
        }
        json!({"passed": failures == 0, "checked_components": compared, "out_of_tolerance": failures,
            "maximum_delta": maximum_delta, "tolerance_lsb": 2,
            "scope": "8x4_synthetic_tile_interiors_y_u_v_not_boundary_filter_quality"})
    }

    fn validate(
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
        options: &Options,
    ) -> Result<Value, String> {
        if options.output_width < 32 || options.output_height < 16 {
            return Err("validation needs output width>=32 and height>=16".into());
        }
        let before = gpu_metrics();
        let bgra = GpuBgraPool::from_device(device.clone(), options.width, options.height, 2)
            .map_err(|e| format!("{e:?}"))?;
        let nv12 = GpuNv12Pool::from_device(
            device.clone(),
            options.width,
            options.height,
            options.output_width,
            options.output_height,
            2,
        )
        .map_err(|e| format!("{e:?}"))?;
        let mut cases = Vec::new();
        for phase in [0, 3] {
            let pixels = pattern(options.width, options.height, phase);
            let source = create_synthetic_bgra(device, options.width, options.height, &pixels)
                .map_err(|e| format!("{e:?}"))?;
            let frame = bgra
                .copy_from(&source, phase as u64 + 1)
                .map_err(|e| format!("{e:?}"))?;
            drop(source);
            let owned_bgra_matches = frame.readback_bgra().map_err(|e| format!("{e:?}"))? == pixels;
            let reference = convert_to_nv12(
                &pixels,
                options.width,
                options.height,
                options.width as usize * 4,
                options.output_width,
                options.output_height,
                PixelOrder::Bgra,
            )
            .map_err(|e| format!("{e:?}"))?;
            let normal_before = gpu_metrics();
            let surface = nv12.convert(&frame).map_err(|e| format!("{e:?}"))?;
            let conversion_readback_bytes = gpu_metrics()
                .cpu_readback_bytes
                .saturating_sub(normal_before.cpu_readback_bytes);
            let actual = readback_nv12(&surface).map_err(|e| format!("{e:?}"))?;
            let comparison = compare_nv12(
                &actual,
                &reference,
                options.output_width,
                options.output_height,
            );
            cases.push(json!({"phase": phase, "owned_bgra_matches_after_source_drop": owned_bgra_matches,
                "normal_conversion_cpu_readback_bytes": conversion_readback_bytes, "nv12": comparison}));
        }
        Ok(
            json!({"passed": cases.iter().all(|case| case["owned_bgra_matches_after_source_drop"] == true && case["normal_conversion_cpu_readback_bytes"] == 0 && case["nv12"]["passed"] == true),
            "reference": "existing_convert_to_nv12_cpu_coefficients_bt709_limited",
            "source": "synthetic_8x4_bgra_tiles_two_phases", "cases": cases,
            "validation_metrics": metrics_delta(before, gpu_metrics()), "readback_is_validation_only": true}),
        )
    }

    fn benchmark(adapter: &GpuAdapterInfo, options: &Options) -> Result<Value, String> {
        let device = create_device(adapter.index).map_err(|e| format!("{e:?}"))?;
        let validation = validate(&device, options)?;
        if validation["passed"] != true {
            return Ok(json!({"status": "validation_failed", "validation": validation}));
        }
        let cpu_input = options.input_path == "cpu";
        let bgra = GpuBgraPool::from_device(device.clone(), options.width, options.height, 4)
            .map_err(|e| format!("{e:?}"))?;
        let nv12 = GpuNv12Pool::from_device(
            device.clone(),
            options.width,
            options.height,
            options.output_width,
            options.output_height,
            8,
        )
        .map_err(|e| format!("{e:?}"))?;
        let mut pixels = benchmark_pattern(options, 0);
        let fixture_hash = Blake2s256::digest(&pixels)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut source = if cpu_input {
            None
        } else {
            Some(
                create_synthetic_bgra(&device, options.width, options.height, &pixels)
                    .map_err(|e| format!("{e:?}"))?,
            )
        };
        let settings = encoder_settings(options);
        let mut encoder = if options.encode {
            Some(
                H264Encoder::new_with_settings(options.width, options.height, settings)
                    .map_err(|e| format!("{e:?}"))?,
            )
        } else {
            None
        };
        let mut copy_us = Vec::new();
        let mut cpu_fixture_clone_us = Vec::new();
        let mut processing_us = Vec::new();
        let mut pipeline_us = Vec::new();
        let mut fixture_update_us = Vec::new();
        let mut samples = Vec::new();
        let mut frame_metrics = Vec::new();
        let mut outputs = 0_u64;
        let mut encoded_bytes = 0_u64;
        let mut fixture_upload_bytes = 0_u64;
        let mut measured_before = gpu_metrics();
        let bgra_bytes = u64::from(options.width) * u64::from(options.height) * 4;
        for index in 0..options.frames + options.warmup {
            if index == options.warmup {
                measured_before = gpu_metrics();
            }
            let measured = index >= options.warmup;
            let pipeline_started = Instant::now();
            if options.motion {
                let started = Instant::now();
                pixels = benchmark_pattern(options, index as usize);
                if !cpu_input {
                    source = Some(
                        create_synthetic_bgra(&device, options.width, options.height, &pixels)
                            .map_err(|e| format!("{e:?}"))?,
                    );
                }
                if measured {
                    fixture_update_us.push(elapsed_us(started));
                    if !cpu_input {
                        fixture_upload_bytes += bgra_bytes;
                    }
                }
            }
            let metrics_before = gpu_metrics();
            let started = Instant::now();
            let timestamp_us = u64::from(index) * 33_333;
            let (gpu_frame, cpu_frame) = if cpu_input {
                let frame = CapturedFrame {
                    width: options.width,
                    height: options.height,
                    timestamp_us,
                    pixels: pixels.clone(),
                };
                if measured {
                    cpu_fixture_clone_us.push(elapsed_us(started));
                }
                (None, Some(frame))
            } else {
                let frame = bgra
                    .copy_from(source.as_ref().expect("GPU fixture texture"), timestamp_us)
                    .map_err(|e| format!("{e:?}"))?;
                if measured {
                    copy_us.push(elapsed_us(started));
                }
                (Some(frame), None)
            };
            let started = Instant::now();
            if let Some(encoder) = &mut encoder {
                let result = if let Some(frame) = cpu_frame {
                    encoder.encode(frame, index == 0)
                } else {
                    encoder.encode_gpu(gpu_frame.as_ref().expect("GPU frame"), index == 0)
                };
                match result {
                    Ok(output) => {
                        if measured {
                            outputs += 1;
                            encoded_bytes += output.access_unit.len() as u64;
                        }
                    }
                    Err(EncoderError::NeedMoreInput) => {}
                    Err(error) => return Err(format!("{error:?}")),
                }
                if measured {
                    samples.push(
                        serde_json::to_value(encoder.last_performance_sample())
                            .map_err(|e| format!("{e:?}"))?,
                    );
                }
            } else {
                let _surface = nv12
                    .convert(gpu_frame.as_ref().expect("GPU frame"))
                    .map_err(|e| format!("{e:?}"))?;
            }
            if measured {
                processing_us.push(elapsed_us(started));
                pipeline_us.push(elapsed_us(pipeline_started));
                frame_metrics.push(metrics_delta(metrics_before, gpu_metrics()));
            }
        }
        let metrics = metrics_delta(measured_before, gpu_metrics());
        let actual_dimensions = encoder.as_ref().map(H264Encoder::dimensions);
        let experimental_configuration_confirmed = options.experimental_4k
            && encoder.as_ref().is_some_and(|encoder| {
                encoder.dimensions() == (3840, 2160)
                    && encoder.profile() == desklink_protocol::H264Profile::High
                    && encoder.bitrate() == 40_500_000
            });
        let status = if options.encode && outputs == 0 {
            "no_encoded_output"
        } else if options.experimental_4k
            && options.output_width == 3840
            && options.output_height == 2160
            && !experimental_configuration_confirmed
        {
            "experimental_4k_configuration_rejected"
        } else {
            "ok"
        };
        let mut report = json!({"status": status, "validation": validation,
            "source_width": options.width, "source_height": options.height,
            "requested_width": options.output_width, "requested_height": options.output_height,
            "output_dimensions": actual_dimensions,
            "requested_dimensions_matched": actual_dimensions.map(|dims| dims == (options.output_width, options.output_height)),
            "requested_encoder_settings": {"max_width": settings.max_width, "max_height": settings.max_height,
                "fps": settings.fps, "bitrate": settings.bitrate, "profile": format!("{:?}", settings.profile)},
            "experimental_4k_enabled": options.experimental_4k,
            "experimental_4k_configuration_confirmed": if options.experimental_4k { Some(experimental_configuration_confirmed) } else { None },
            "input_path": options.input_path,
            "benchmark_workload": options.workload,
            "benchmark_motion": options.motion,
            "pattern_description": if options.workload == "desktop" { "deterministic_glyph_like_strokes_panels_gradients_content_scroll_with_fixed_chrome_not_real_desktop" } else { "flat_8x4_color_tiles_phase_rotation_for_path_validation" },
            "fixture_initial_blake2s256": fixture_hash,
            "fixture_phase_schedule": if options.motion { "frame_index_content_scroll_or_tile_rotation" } else { "static_phase_zero" },
            "cpu_input_scope": if cpu_input { Some("synthetic_owned_BGRA_not_a_desktop_readback_or_software_only_backend") } else { None },
            "profile": encoder.as_ref().map(|e| format!("{:?}", e.profile())),
            "actual_bitrate": encoder.as_ref().map(H264Encoder::bitrate),
            "backend": encoder.as_ref().and_then(H264Encoder::backend_info),
            "gpu_encoder_status": encoder.as_ref().map(|encoder| encoder.gpu_status()),
            "gpu_surface_encoding_confirmed": encoder.as_ref().filter(|_| !cpu_input).map(|encoder| {
                let status = encoder.gpu_status();
                status.surface_backend_active && status.manager_accepted == Some(true) && status.surface_samples_accepted > 0
                    && status.encoded_outputs > 0 && status.cpu_fallbacks == 0
            }),
            "encoding_enabled": options.encode,
            "video_capabilities": nv12.capabilities(), "measured_frames": options.frames, "warmup_frames": options.warmup,
            "mode": if options.encode { "encoder_input_path_comparison_with_explicit_fallback_reporting" } else { "gpu_bgra_copy_and_video_processor" },
        });
        let measurements = json!({
            "copy_call_with_gpu_completion_wait_cpu_wall_us": percentiles(&copy_us),
            "cpu_fixture_clone_cpu_wall_us": percentiles(&cpu_fixture_clone_us),
            "processing_call_cpu_wall_us": percentiles(&processing_us),
            "submission_pipeline_cpu_wall_us_including_fixture_updates_and_input_ownership": percentiles(&pipeline_us),
            "fixture_update_cpu_wall_us": percentiles(&fixture_update_us),
            "fixture_cpu_upload_bytes_measured": fixture_upload_bytes,
            "fixture_cpu_upload_bytes_initial": if cpu_input { 0 } else { bgra_bytes },
            "fixture_cpu_clone_bytes_measured": if cpu_input { u64::from(options.frames) * bgra_bytes } else { 0 },
            "completed_outputs": outputs, "encoded_bytes": encoded_bytes,
            "output_count_scope": "outputs_returned_during_measurement_calls_may_reference_warmup_inputs_not_a_drained_input_cohort_or_FPS",
            "pending_after_last_call": encoder.as_ref().and_then(H264Encoder::last_performance_sample).map(|sample| sample.pending_frames),
            "pending_scope": "last_submission_sample_only_no_tail_drain_performed",
            "measured_gpu_metrics_including_motion_fixture_resources": metrics,
            "per_frame_normal_path_gpu_metrics": frame_metrics,
            "encoder_samples": samples,
            "gpu_execution_time_us": Value::Null, "encoder_gpu_time_us": Value::Null,
            "driver_copy_bytes": Value::Null, "heap_allocation_count": Value::Null,
            "time_scope": "local_cpu_wall_calls_with_completion_wait_not_pure_gpu_time",
            "normal_path_readback_is_counted_even_when_fallback_occurs": true,
            "quality_equivalence_verified": false,
            "comparison_scope": "local_stage_comparison_only_CPU_and_GPU_sampling_kernels_and_decoded_quality_require_separate_validation",
        });
        if let Value::Object(fields) = measurements {
            report
                .as_object_mut()
                .expect("probe report object")
                .extend(fields);
        }
        Ok(report)
    }

    pub(super) fn run() -> Result<Value, String> {
        let options = options()?;
        let adapters = enumerate_adapters().map_err(|e| format!("{e:?}"))?;
        let mfts = match probe_hardware_encoder_capabilities() {
            Ok(capabilities) => json!({"status": "ok", "candidates": capabilities,
                "evidence": "read_only_activation_async_d3d11_attributes_only", "manager_acceptance": Value::Null, "surface_input_acceptance": Value::Null}),
            Err(error) => {
                json!({"status": "failed", "stage": "mft_attribute_inventory", "error": format!("{error:?}")})
            }
        };
        let mut cases = Vec::new();
        for adapter in &adapters {
            if options.mode == "benchmark" && adapter.software && options.adapter_index.is_none() {
                continue;
            }
            if options
                .adapter_index
                .is_some_and(|index| index != adapter.index)
            {
                continue;
            }
            let result = if options.mode == "benchmark" {
                benchmark(adapter, &options)
            } else {
                create_device(adapter.index).map_err(|e| format!("{e:?}")).map(|device| {
                    let mut sizes = vec![(512, 512), (1280, 720), (1920, 1080), (2560, 1440)];
                    if !sizes.contains(&(options.width, options.height)) { sizes.push((options.width, options.height)); }
                    let formats = sizes.into_iter().map(|(width, height)| {
                        match GpuNv12Pool::from_device(device.clone(), width, height, width, height, 1) {
                            Ok(pool) => json!({"status": "supported", "capabilities": pool.capabilities(), "conversion_performed": false}),
                            Err(error) => json!({"status": "unsupported", "width": width, "height": height, "stage": "video_processor_configuration", "error": format!("{error:?}")}),
                        }
                    }).collect::<Vec<_>>();
                    json!({"status": "ok", "video_processor_sizes": formats})
                })
            };
            cases.push(json!({"adapter_index": adapter.index, "result": match result { Ok(value) => value, Err(error) => json!({"status": "failed", "stage": "adapter_probe", "error": error}) }}));
        }
        if cases.is_empty() {
            return Err("requested adapter was not enumerated".into());
        }
        Ok(
            json!({"schema_version": 1, "scope": "offline_synthetic_gpu_probe", "mode": options.mode,
            "desktop_capture": false, "remote_session": false, "input_injection": false,
            "adapters": adapters, "mft_capabilities": mfts, "cases": cases,
            "color_contract": "existing_cpu_bt709_limited_coefficients", "gpu_time_us": Value::Null}),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn synthetic_tiles_distinguish_rows_and_phases_and_keep_chroma_alignment() {
            let first = pattern(64, 32, 0);
            let second = pattern(64, 32, 3);
            assert_ne!(first, second);
            assert_ne!(&first[..4], &first[16 * 64 * 4..][..4]);
            for y in (0..32).step_by(2) {
                for x in (0..64).step_by(2) {
                    let offset = (y * 64 + x) * 4;
                    assert_eq!(&first[offset..offset + 4], &first[offset + 4..offset + 8]);
                    assert_eq!(
                        &first[offset..offset + 4],
                        &first[offset + 64 * 4..offset + 64 * 4 + 4]
                    );
                    assert_eq!(first[offset + 3], 255);
                }
            }
        }

        #[test]
        fn validation_rejects_uv_swap_and_luma_corruption() {
            let source = pattern(64, 32, 0);
            let expected = convert_to_nv12(&source, 64, 32, 256, 64, 32, PixelOrder::Bgra).unwrap();
            assert_eq!(compare_nv12(&expected, &expected, 64, 32)["passed"], true);
            let mut swapped = expected.clone();
            for pair in swapped[64 * 32..].chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
            assert_eq!(compare_nv12(&swapped, &expected, 64, 32)["passed"], false);
            let mut corrupt = expected.clone();
            corrupt[0..64 * 32].fill(0);
            assert_eq!(compare_nv12(&corrupt, &expected, 64, 32)["passed"], false);
            assert_eq!(
                compare_nv12(&corrupt[..5], &expected, 64, 32)["passed"],
                false
            );
        }

        #[test]
        fn four_k_requires_explicit_settings_for_both_cpu_and_gpu_input_paths() {
            let mut options = Options {
                mode: "benchmark".into(),
                width: 3840,
                height: 2160,
                output_width: 3840,
                output_height: 2160,
                frames: 30,
                warmup: 3,
                adapter_index: Some(0),
                encode: true,
                motion: false,
                input_path: "gpu".into(),
                experimental_4k: false,
                workload: "desktop".into(),
            };
            let normal = encoder_settings(&options);
            assert_eq!((normal.max_width, normal.max_height), (2560, 1440));
            assert_eq!(normal.profile, desklink_protocol::H264Profile::Main);
            options.experimental_4k = true;
            for input_path in ["gpu", "cpu"] {
                options.input_path = input_path.into();
                let settings = encoder_settings(&options);
                assert_eq!((settings.max_width, settings.max_height), (3840, 2160));
                assert_eq!(settings.profile, desklink_protocol::H264Profile::High);
                assert_eq!(settings.bitrate, 40_500_000);
                assert_eq!(settings.fps, 30);
            }
        }

        #[test]
        fn synthetic_desktop_motion_changes_content_but_preserves_fixed_chrome() {
            let first = desktop_pattern(512, 512, 0);
            let next = desktop_pattern(512, 512, 1);
            assert_ne!(first, next);
            assert_eq!(&first[..512 * 44 * 4], &next[..512 * 44 * 4]);
            for y in 0..512 {
                assert_eq!(
                    &first[y * 512 * 4..(y * 512 + 100) * 4],
                    &next[y * 512 * 4..(y * 512 + 100) * 4]
                );
            }
            assert!(first.chunks_exact(4).all(|pixel| pixel[3] == 255));
            assert_eq!(first, desktop_pattern(512, 512, 0));
        }
    }
}

fn main() {
    #[cfg(windows)]
    match probe::run() {
        Ok(report) => println!("{report}"),
        Err(error) => {
            println!(
                "{}",
                serde_json::json!({"schema_version": 1, "status": "failed", "error": error})
            );
            std::process::exit(1);
        }
    }
    #[cfg(not(windows))]
    {
        println!(
            "{}",
            serde_json::json!({"status": "unsupported", "reason": "Windows D3D11 required"})
        );
        std::process::exit(1);
    }
}
