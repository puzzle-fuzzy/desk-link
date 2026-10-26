//! Opt-in validation of actual encoded pixels, independent of VP readback and
//! encoder counters. No desktop input or retained image artifact is involved.

use std::{collections::BTreeMap, mem::ManuallyDrop, ptr, slice};

use desklink_protocol::H264Profile;
use windows::{
    Win32::{
        Foundation::RPC_E_CHANGED_MODE,
        Media::MediaFoundation::*,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize,
        },
    },
    core::Interface,
};

use super::{GpuBgraPool, create_device, create_synthetic_bgra, enumerate_adapters, gpu_metrics};
use crate::encoder::{
    EncodedFrame, EncoderError, H264Encoder, H264EncoderSettings, PixelOrder, convert_to_nv12,
};

type ValidationResult<T> = Result<T, String>;

fn at(stage: &'static str) -> impl FnOnce(windows::core::Error) -> String {
    move |error| format!("{stage}: HRESULT {:#010x}", error.code().0 as u32)
}

struct MfRuntime {
    com_initialized: bool,
}

impl MfRuntime {
    fn start() -> ValidationResult<Self> {
        let status = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let com_initialized = status.is_ok();
        if !com_initialized && status != RPC_E_CHANGED_MODE {
            return Err(format!(
                "decoder CoInitializeEx: HRESULT {:#010x}",
                status.0 as u32
            ));
        }
        if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            if com_initialized {
                unsafe { CoUninitialize() };
            }
            return Err(at("decoder MFStartup")(error));
        }
        Ok(Self { com_initialized })
    }
}

impl Drop for MfRuntime {
    fn drop(&mut self) {
        let _ = unsafe { MFShutdown() };
        if self.com_initialized {
            unsafe { CoUninitialize() };
        }
    }
}

struct DecodedFrame {
    timestamp_us: u64,
    width: u32,
    height: u32,
    coded_dimensions: (u32, u32),
    nv12: Vec<u8>,
}

struct SoftwareDecoder {
    transform: IMFTransform,
    output_type: IMFMediaType,
    _runtime: MfRuntime,
}

impl SoftwareDecoder {
    fn new(width: u32, height: u32) -> ValidationResult<Self> {
        let runtime = MfRuntime::start()?;
        let transform: IMFTransform = unsafe {
            CoCreateInstance(
                &CMSH264DecoderMFT,
                None::<&windows::core::IUnknown>,
                CLSCTX_INPROC_SERVER,
            )
        }
        .map_err(at("create Microsoft H264 decoder"))?;
        let attributes = unsafe { transform.GetAttributes() }.map_err(at("decoder attributes"))?;
        // This is a software verifier, independent of the encoder's D3D device.
        unsafe { attributes.SetUINT32(&CODECAPI_AVDecVideoAcceleration_H264, 0) }
            .map_err(at("disable decoder hardware acceleration"))?;
        let _ = unsafe { attributes.SetUINT32(&MF_LOW_LATENCY, 1) };
        let input = unsafe { MFCreateMediaType() }.map_err(at("decoder input media type"))?;
        unsafe {
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(at("decoder input major type"))?;
            input
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .map_err(at("decoder H264 subtype"))?;
            input
                .SetUINT64(
                    &MF_MT_FRAME_SIZE,
                    (u64::from(width) << 32) | u64::from(height),
                )
                .map_err(at("decoder input dimensions"))?;
            input
                .SetUINT64(&MF_MT_FRAME_RATE, 30_u64 << 32 | 1)
                .map_err(at("decoder input rate"))?;
            input
                .SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(at("decoder input interlace mode"))?;
            transform
                .SetInputType(0, &input, 0)
                .map_err(at("set decoder H264 input type"))?;
        }
        let output_type = Self::choose_nv12(&transform)?;
        unsafe {
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(at("begin decoder streaming"))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(at("start decoder stream"))?;
        }
        Ok(Self {
            transform,
            output_type,
            _runtime: runtime,
        })
    }

    fn choose_nv12(transform: &IMFTransform) -> ValidationResult<IMFMediaType> {
        for index in 0..64 {
            let candidate = unsafe { transform.GetOutputAvailableType(0, index) }
                .map_err(at("enumerate decoder NV12 type"))?;
            if unsafe { candidate.GetGUID(&MF_MT_SUBTYPE) }.map_err(at("decoder output subtype"))?
                == MFVideoFormat_NV12
            {
                unsafe { transform.SetOutputType(0, &candidate, 0) }
                    .map_err(at("set decoder NV12 output"))?;
                return Ok(candidate);
            }
        }
        Err("decoder did not expose NV12 within bounded type enumeration".into())
    }

    fn submit(&mut self, frame: &EncodedFrame) -> ValidationResult<Vec<DecodedFrame>> {
        let mut input = Vec::new();
        // Repeating a header that is already in the AU can make the Microsoft
        // decoder treat it as another stream start and lose the AU timestamp.
        if sps(&frame.access_unit).is_none()
            && frame.keyframe
            && let Some(header) = &frame.sequence_header
            && (header.starts_with(&[0, 0, 1]) || header.starts_with(&[0, 0, 0, 1]))
        {
            input.extend_from_slice(header);
        }
        input.extend_from_slice(&frame.access_unit);
        let sample = sample_with_bytes(&input, frame.timestamp_us)?;
        let mut output = Vec::new();
        match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            Ok(()) => {}
            Err(error) if error.code() == MF_E_NOTACCEPTING => {
                output.extend(self.take_available()?);
                unsafe { self.transform.ProcessInput(0, &sample, 0) }
                    .map_err(at("decoder ProcessInput after drain"))?;
            }
            Err(error) => return Err(at("decoder ProcessInput")(error)),
        }
        output.extend(self.take_available()?);
        Ok(output)
    }

    fn drain(&mut self) -> ValidationResult<Vec<DecodedFrame>> {
        unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
                .map_err(at("decoder end stream"))?;
            self.transform
                .ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
                .map_err(at("decoder drain"))?;
        }
        self.take_available()
    }

    fn take_available(&mut self) -> ValidationResult<Vec<DecodedFrame>> {
        let mut decoded = Vec::new();
        for _ in 0..128 {
            let info = unsafe { self.transform.GetOutputStreamInfo(0) }
                .map_err(at("decoder output stream info"))?;
            let size = unsafe { self.output_type.GetUINT64(&MF_MT_FRAME_SIZE) }
                .map_err(at("decoder output dimensions"))?;
            let (width, height) = ((size >> 32) as u32, size as u32);
            let sample = if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                let sample = unsafe { MFCreateSample() }.map_err(at("decoder output sample"))?;
                let buffer =
                    unsafe { MFCreateMemoryBuffer(info.cbSize.max(width * height * 3 / 2)) }
                        .map_err(at("decoder output allocation"))?;
                unsafe { sample.AddBuffer(&buffer) }.map_err(at("decoder attach output buffer"))?;
                Some(sample)
            } else {
                None
            };
            let mut output = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0;
            let result = unsafe {
                self.transform
                    .ProcessOutput(0, slice::from_mut(&mut output), &mut status)
            };
            let sample = unsafe { ManuallyDrop::take(&mut output.pSample) };
            let events = unsafe { ManuallyDrop::take(&mut output.pEvents) };
            drop(events);
            match result {
                Err(error) if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(decoded),
                Err(error) if error.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.output_type = Self::choose_nv12(&self.transform)?;
                    continue;
                }
                Err(error) => return Err(at("decoder ProcessOutput")(error)),
                Ok(()) => {}
            }
            let sample = sample.ok_or("decoder returned no NV12 output sample")?;
            let timestamp =
                unsafe { sample.GetSampleTime() }.map_err(at("decoder output timestamp"))?;
            let timestamp_us =
                u64::try_from(timestamp).map_err(|_| "decoder output negative timestamp")? / 10;
            let buffer = unsafe { sample.ConvertToContiguousBuffer() }
                .map_err(at("decoder contiguous output"))?;
            let nv12 = copy_nv12(&buffer, &self.output_type, width, height)?;
            let (nv12, visible_width, visible_height) =
                display_aperture_nv12(nv12, &self.output_type, (width, height))?;
            decoded.push(DecodedFrame {
                timestamp_us,
                width: visible_width,
                height: visible_height,
                coded_dimensions: (width, height),
                nv12,
            });
        }
        Err("decoder output/stream-change budget exceeded".into())
    }
}

fn display_aperture_nv12(
    nv12: Vec<u8>,
    media_type: &IMFMediaType,
    coded: (u32, u32),
) -> ValidationResult<(Vec<u8>, u32, u32)> {
    for key in [MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_GEOMETRIC_APERTURE] {
        let size = match unsafe { media_type.GetBlobSize(&key) } {
            Ok(size) => size as usize,
            Err(error) if error.code() == MF_E_ATTRIBUTENOTFOUND => continue,
            Err(error) => return Err(at("decoder display aperture size")(error)),
        };
        if size != size_of::<MFVideoArea>() {
            return Err(format!(
                "decoder display aperture blob size {size} != {}",
                size_of::<MFVideoArea>()
            ));
        }
        let mut blob = vec![0; size];
        unsafe { media_type.GetBlob(&key, &mut blob, None) }
            .map_err(at("decoder display aperture"))?;
        let area = unsafe { ptr::read_unaligned(blob.as_ptr().cast::<MFVideoArea>()) };
        if area.OffsetX.fract != 0
            || area.OffsetY.fract != 0
            || area.OffsetX.value < 0
            || area.OffsetY.value < 0
            || area.Area.cx <= 0
            || area.Area.cy <= 0
        {
            return Err(format!(
                "decoder display aperture is not an integral positive rectangle: {area:?}"
            ));
        }
        let rect = (
            area.OffsetX.value as u32,
            area.OffsetY.value as u32,
            area.Area.cx as u32,
            area.Area.cy as u32,
        );
        return Ok((crop_nv12(&nv12, coded, rect)?, rect.2, rect.3));
    }
    Ok((nv12, coded.0, coded.1))
}

fn crop_nv12(
    nv12: &[u8],
    coded: (u32, u32),
    rect: (u32, u32, u32, u32),
) -> ValidationResult<Vec<u8>> {
    let (left, top, width, height) = rect;
    if [left, top, width, height, coded.0, coded.1]
        .iter()
        .any(|value| !value.is_multiple_of(2))
        || width == 0
        || height == 0
        || left.checked_add(width).is_none_or(|right| right > coded.0)
        || top
            .checked_add(height)
            .is_none_or(|bottom| bottom > coded.1)
        || nv12.len() != coded.0 as usize * coded.1 as usize * 3 / 2
    {
        return Err(format!(
            "invalid NV12 display aperture {rect:?} within coded {coded:?}"
        ));
    }
    let mut visible = Vec::with_capacity(width as usize * height as usize * 3 / 2);
    for row in top..top + height {
        let start = row as usize * coded.0 as usize + left as usize;
        visible.extend_from_slice(&nv12[start..start + width as usize]);
    }
    let uv_start = coded.0 as usize * coded.1 as usize;
    for row in top / 2..(top + height) / 2 {
        let start = uv_start + row as usize * coded.0 as usize + left as usize;
        visible.extend_from_slice(&nv12[start..start + width as usize]);
    }
    Ok(visible)
}

impl Drop for SoftwareDecoder {
    fn drop(&mut self) {
        let _ = unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0) };
        let _ = unsafe {
            self.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0)
        };
    }
}

fn sample_with_bytes(bytes: &[u8], timestamp_us: u64) -> ValidationResult<IMFSample> {
    let sample = unsafe { MFCreateSample() }.map_err(at("decoder compressed sample"))?;
    let length = u32::try_from(bytes.len()).map_err(|_| "compressed sample exceeds u32")?;
    let buffer =
        unsafe { MFCreateMemoryBuffer(length) }.map_err(at("decoder compressed buffer"))?;
    let mut data = ptr::null_mut();
    unsafe { buffer.Lock(&mut data, None, None) }.map_err(at("decoder compressed buffer Lock"))?;
    if data.is_null() {
        let _ = unsafe { buffer.Unlock() };
        return Err("decoder compressed buffer was null".into());
    }
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len()) };
    unsafe { buffer.Unlock() }.map_err(at("decoder compressed buffer Unlock"))?;
    unsafe {
        buffer
            .SetCurrentLength(length)
            .map_err(at("decoder compressed length"))?;
        sample
            .AddBuffer(&buffer)
            .map_err(at("decoder attach compressed buffer"))?;
        sample
            .SetSampleTime((timestamp_us * 10) as i64)
            .map_err(at("decoder compressed timestamp"))?;
        sample
            .SetSampleDuration(10_000_000 / 30)
            .map_err(at("decoder compressed duration"))?;
    }
    Ok(sample)
}

fn copy_nv12(
    buffer: &IMFMediaBuffer,
    media_type: &IMFMediaType,
    width: u32,
    height: u32,
) -> ValidationResult<Vec<u8>> {
    let expected = width as usize * height as usize * 3 / 2;
    if let Ok(two_dimensional) = buffer.cast::<IMF2DBuffer>() {
        let length = unsafe { two_dimensional.GetContiguousLength() }
            .map_err(at("decoded NV12 contiguous length"))?;
        if length as usize != expected {
            return Err(format!(
                "decoded NV12 contiguous length {length} != {expected}"
            ));
        }
        let mut nv12 = vec![0; expected];
        unsafe { two_dimensional.ContiguousCopyTo(&mut nv12) }
            .map_err(at("decoded NV12 contiguous copy"))?;
        return Ok(nv12);
    }
    let mut data = ptr::null_mut();
    let mut length = 0;
    unsafe { buffer.Lock(&mut data, None, Some(&mut length)) }.map_err(at("decoded NV12 Lock"))?;
    let stride = unsafe { media_type.GetUINT32(&MF_MT_DEFAULT_STRIDE) }.unwrap_or(width) as i32;
    let result = if stride < width as i32
        || data.is_null()
        || (length as usize) < stride as usize * height as usize * 3 / 2
    {
        Err(format!(
            "decoded NV12 invalid stride/length: stride={stride} length={length}"
        ))
    } else {
        let mut nv12 = vec![0; expected];
        for row in 0..height as usize * 3 / 2 {
            let source =
                unsafe { slice::from_raw_parts(data.add(row * stride as usize), width as usize) };
            nv12[row * width as usize..(row + 1) * width as usize].copy_from_slice(source);
        }
        Ok(nv12)
    };
    unsafe { buffer.Unlock() }.map_err(at("decoded NV12 Unlock"))?;
    result
}

const COLORS: [[u8; 4]; 8] = [
    [0, 0, 255, 255],
    [0, 255, 0, 255],
    [255, 0, 0, 255],
    [0, 255, 255, 255],
    [255, 255, 0, 255],
    [255, 0, 255, 255],
    [240, 240, 240, 255],
    [16, 16, 16, 255],
];

fn tiles(width: u32, height: u32, phase: usize) -> Vec<u8> {
    let mut pixels = vec![0; width as usize * height as usize * 4];
    for y in 0..height as usize {
        for x in 0..width as usize {
            let column = x * 8 / width as usize;
            let row = y * 4 / height as usize;
            pixels[(y * width as usize + x) * 4..(y * width as usize + x + 1) * 4]
                .copy_from_slice(&COLORS[(column + row * 3 + phase) % 8]);
        }
    }
    pixels
}

fn validate_tiles(
    frame: &DecodedFrame,
    expected: &[u8],
    dimensions: (u32, u32),
) -> ValidationResult<()> {
    if (frame.width, frame.height) != dimensions {
        return Err(format!(
            "decoded dimensions {:?} != {dimensions:?}",
            (frame.width, frame.height)
        ));
    }
    let width = frame.width as usize;
    let height = frame.height as usize;
    if frame.nv12.len() != expected.len() {
        return Err("decoded NV12 size differs from reference".into());
    }
    for row in 0..4 {
        for column in 0..8 {
            let x = ((column * 2 + 1) * width / 16) & !1;
            let y = ((row * 2 + 1) * height / 8) & !1;
            // Stay well inside each flat tile, away from chroma/codec edge ringing.
            for (plane, offset) in [
                ("Y", y * width + x),
                ("U", width * height + y / 2 * width + x),
                ("V", width * height + y / 2 * width + x + 1),
            ] {
                if frame.nv12[offset].abs_diff(expected[offset]) > 12 {
                    return Err(format!(
                        "decoded tile row={row} col={column} {plane}: actual={} expected={} timestamp={}",
                        frame.nv12[offset], expected[offset], frame.timestamp_us
                    ));
                }
            }
        }
    }
    Ok(())
}

fn sps(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 < bytes.len() {
        let prefix = if bytes[index..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[index..].starts_with(&[0, 0, 1]) {
            3
        } else {
            index += 1;
            continue;
        };
        starts.push((index, index + prefix));
        index += prefix;
    }
    starts.iter().enumerate().find_map(|(index, (_, nal))| {
        (bytes.get(*nal).is_some_and(|value| value & 0x1f == 7)).then(|| {
            bytes[*nal..starts.get(index + 1).map_or(bytes.len(), |entry| entry.0)].to_vec()
        })
    })
}

#[test]
fn synthetic_reference_rejects_vertical_horizontal_uv_swaps_and_old_phase() {
    let pixels = tiles(512, 512, 0);
    let reference =
        convert_to_nv12(&pixels, 512, 512, 512 * 4, 512, 512, PixelOrder::Bgra).unwrap();
    let mut frame = DecodedFrame {
        timestamp_us: 1,
        width: 512,
        height: 512,
        coded_dimensions: (512, 512),
        nv12: reference.clone(),
    };
    validate_tiles(&frame, &reference, (512, 512)).unwrap();
    for row in 0..512 {
        frame.nv12[row * 512..(row + 1) * 512].reverse();
    }
    assert!(validate_tiles(&frame, &reference, (512, 512)).is_err());
    frame.nv12 = reference.clone();
    for row in 0..256 {
        for x in 0..512 {
            frame.nv12.swap(row * 512 + x, (511 - row) * 512 + x);
        }
    }
    assert!(validate_tiles(&frame, &reference, (512, 512)).is_err());
    frame.nv12 = reference.clone();
    for pair in frame.nv12[512 * 512..].chunks_exact_mut(2) {
        pair.swap(0, 1);
    }
    assert!(validate_tiles(&frame, &reference, (512, 512)).is_err());
    frame.nv12 = convert_to_nv12(
        &tiles(512, 512, 3),
        512,
        512,
        512 * 4,
        512,
        512,
        PixelOrder::Bgra,
    )
    .unwrap();
    assert!(validate_tiles(&frame, &reference, (512, 512)).is_err());
}

#[test]
fn decoder_display_aperture_crop_preserves_luma_and_interleaved_uv() {
    let padded: Vec<u8> = (0..24).collect();
    assert_eq!(
        crop_nv12(&padded, (4, 4), (2, 2, 2, 2)).unwrap(),
        [10, 11, 14, 15, 22, 23]
    );
    assert!(crop_nv12(&padded, (4, 4), (2, 2, 4, 2)).is_err());
    assert!(crop_nv12(&padded, (4, 4), (1, 0, 2, 2)).is_err());
}

#[test]
#[ignore = "opt-in actual GPU H264 encode -> software decode pixels; synthetic only"]
fn actual_gpu_h264_bitstream_decodes_two_phases_main_high_and_reconfigured_dimensions() {
    let _runtime = MfRuntime::start().unwrap();
    let adapter = enumerate_adapters()
        .unwrap()
        .into_iter()
        .find(|adapter| !adapter.software)
        .expect("hardware adapter required");
    let device = create_device(adapter.index).unwrap();
    for profile in [H264Profile::Main, H264Profile::High] {
        let mut encoder = H264Encoder::new_with_settings(
            512,
            512,
            H264EncoderSettings {
                max_width: 512,
                max_height: 512,
                profile,
                ..Default::default()
            },
        )
        .unwrap();
        let mut previous_version = 0;
        let mut previous_sps = None;
        let mut timestamp_us = 1_000_000;
        for dimensions in [(512, 512), (640, 384)] {
            if dimensions != (512, 512) {
                encoder
                    .reconfigure_for_source(
                        dimensions.0,
                        dimensions.1,
                        H264EncoderSettings {
                            max_width: dimensions.0,
                            max_height: dimensions.1,
                            profile,
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            let pool =
                GpuBgraPool::from_device(device.clone(), dimensions.0, dimensions.1, 4).unwrap();
            let mut decoder = SoftwareDecoder::new(dimensions.0, dimensions.1).unwrap();
            let mut references = BTreeMap::new();
            let mut decoded_phases = [0_usize; 2];
            let mut current_sps = None;
            let mut current_version = None;
            let before = gpu_metrics().cpu_readback_bytes;
            for phase_index in 0..2 {
                let phase = phase_index * 3;
                let pixels = tiles(dimensions.0, dimensions.1, phase);
                let reference = convert_to_nv12(
                    &pixels,
                    dimensions.0,
                    dimensions.1,
                    dimensions.0 as usize * 4,
                    dimensions.0,
                    dimensions.1,
                    PixelOrder::Bgra,
                )
                .unwrap();
                let source =
                    create_synthetic_bgra(&device, dimensions.0, dimensions.1, &pixels).unwrap();
                for index in 0..16 {
                    timestamp_us += 33_333;
                    references.insert(timestamp_us, (phase_index, reference.clone()));
                    let frame = pool.copy_from(&source, timestamp_us).unwrap();
                    let encoded = match encoder.encode_gpu(&frame, index == 0) {
                        Ok(encoded) => encoded,
                        Err(EncoderError::NeedMoreInput) => continue,
                        Err(error) => panic!("actual GPU encoding failed: {error:?}"),
                    };
                    assert!(
                        encoder.last_performance_sample().unwrap().gpu_surface_input,
                        "software fallback cannot prove GPU content"
                    );
                    if current_version.is_none() {
                        assert!(encoded.keyframe, "new configuration must begin with an IDR");
                        assert!(encoded.config_version > previous_version);
                        current_version = Some(encoded.config_version);
                    }
                    assert_eq!(current_version, Some(encoded.config_version));
                    current_sps = current_sps
                        .or_else(|| encoded.sequence_header.as_deref().and_then(sps))
                        .or_else(|| sps(&encoded.access_unit));
                    for decoded in decoder.submit(&encoded).unwrap() {
                        let (phase_index, reference) = references.get(&decoded.timestamp_us).unwrap_or_else(|| panic!("decoder timestamp {} outside submitted input range {:?}..{:?}; encoded timestamp={}", decoded.timestamp_us, references.first_key_value().map(|entry| entry.0), references.last_key_value().map(|entry| entry.0), encoded.timestamp_us));
                        validate_tiles(&decoded, reference, dimensions).unwrap();
                        decoded_phases[*phase_index] += 1;
                    }
                }
            }
            for decoded in decoder.drain().unwrap() {
                let (phase_index, reference) = references
                    .get(&decoded.timestamp_us)
                    .expect("drained decoder output must retain input timestamp");
                validate_tiles(&decoded, reference, dimensions).unwrap();
                decoded_phases[*phase_index] += 1;
            }
            assert!(
                decoded_phases.iter().all(|count| *count >= 8),
                "both content updates must decode: {decoded_phases:?}"
            );
            assert_eq!(
                gpu_metrics().cpu_readback_bytes,
                before,
                "GPU encode -> compressed software decode must not read back raw GPU input"
            );
            assert_eq!(encoder.gpu_status().cpu_fallbacks, 0);
            assert_eq!(
                encoder.profile(),
                profile,
                "requested profile must really encode"
            );
            let current_sps = current_sps.expect("actual bitstream must contain SPS");
            let profile_idc = match profile {
                H264Profile::Main => 77,
                H264Profile::High => 100,
            };
            assert_eq!(
                current_sps.get(1),
                Some(&profile_idc),
                "SPS must contain the requested H264 profile, independently of encoder metadata"
            );
            if let Some(previous) = previous_sps {
                assert_ne!(
                    current_sps, previous,
                    "resized bitstream must contain a new SPS"
                );
            }
            previous_sps = Some(current_sps);
            previous_version = current_version.expect("GPU encoder must produce a configuration");
            println!(
                "gpu_encoded_pixels profile={profile:?} dimensions={dimensions:?} decoded_phases={decoded_phases:?} config={previous_version} raw_gpu_readback=0"
            );
        }
    }
}

#[test]
#[ignore = "opt-in representative 1080p/1440p/explicit-4K GPU encode -> software decoded pixels; synthetic only"]
fn representative_gpu_h264_bitstreams_decode_correct_pixels_at_1080p_1440p_and_explicit_4k() {
    let _runtime = MfRuntime::start().unwrap();
    let adapter = enumerate_adapters()
        .unwrap()
        .into_iter()
        .find(|adapter| !adapter.software)
        .expect("hardware adapter required");
    let device = create_device(adapter.index).unwrap();
    for (dimensions, settings) in [
        ((1920, 1080), H264EncoderSettings::default()),
        ((2560, 1440), H264EncoderSettings::default()),
        ((3840, 2160), H264EncoderSettings::experimental_4k()),
    ] {
        let mut encoder =
            H264Encoder::new_with_settings(dimensions.0, dimensions.1, settings).unwrap();
        assert_eq!(
            encoder.dimensions(),
            dimensions,
            "representative mode must not silently downscale"
        );
        let pool = GpuBgraPool::from_device(device.clone(), dimensions.0, dimensions.1, 4).unwrap();
        let mut decoder =
            SoftwareDecoder::new(dimensions.0, dimensions.1).unwrap_or_else(|error| {
                panic!("software decoder creation for {dimensions:?}: {error}")
            });
        let mut references = BTreeMap::new();
        let mut decoded_phases = [0_usize; 2];
        let mut parameter_set = None;
        let mut config_version = None;
        let mut coded_dimensions = None;
        let mut timestamp_us = 1_000_000;
        let before = gpu_metrics().cpu_readback_bytes;
        for phase_index in 0..2 {
            let pixels = tiles(dimensions.0, dimensions.1, phase_index * 3);
            let reference = convert_to_nv12(
                &pixels,
                dimensions.0,
                dimensions.1,
                dimensions.0 as usize * 4,
                dimensions.0,
                dimensions.1,
                PixelOrder::Bgra,
            )
            .unwrap();
            let source =
                create_synthetic_bgra(&device, dimensions.0, dimensions.1, &pixels).unwrap();
            for index in 0..4 {
                timestamp_us += 33_333;
                references.insert(timestamp_us, (phase_index, reference.clone()));
                let frame = pool.copy_from(&source, timestamp_us).unwrap();
                let encoded = match encoder.encode_gpu(&frame, index == 0) {
                    Ok(encoded) => encoded,
                    Err(EncoderError::NeedMoreInput) => continue,
                    Err(error) => panic!("GPU encoding at {dimensions:?}: {error:?}"),
                };
                assert!(
                    encoder.last_performance_sample().unwrap().gpu_surface_input,
                    "CPU fallback cannot prove representative GPU content"
                );
                if config_version.is_none() {
                    assert!(
                        encoded.keyframe,
                        "representative bitstream must begin with IDR"
                    );
                    config_version = Some(encoded.config_version);
                }
                assert_eq!(Some(encoded.config_version), config_version);
                parameter_set = parameter_set
                    .or_else(|| encoded.sequence_header.as_deref().and_then(sps))
                    .or_else(|| sps(&encoded.access_unit));
                for decoded in decoder.submit(&encoded).unwrap_or_else(|error| {
                    panic!("software decoder submission at {dimensions:?}: {error}")
                }) {
                    let coded = coded_dimensions.get_or_insert(decoded.coded_dimensions);
                    assert_eq!(*coded, decoded.coded_dimensions);
                    let (phase_index, reference) = references
                        .get(&decoded.timestamp_us)
                        .expect("decoded timestamp must strictly identify submitted phase");
                    validate_tiles(&decoded, reference, dimensions).unwrap_or_else(|error| {
                        panic!("decoded content at {dimensions:?}: {error}")
                    });
                    decoded_phases[*phase_index] += 1;
                }
            }
        }
        for decoded in decoder
            .drain()
            .unwrap_or_else(|error| panic!("software decoder drain at {dimensions:?}: {error}"))
        {
            let coded = coded_dimensions.get_or_insert(decoded.coded_dimensions);
            assert_eq!(*coded, decoded.coded_dimensions);
            let (phase_index, reference) = references
                .get(&decoded.timestamp_us)
                .expect("drained decoded timestamp must strictly identify submitted phase");
            validate_tiles(&decoded, reference, dimensions)
                .unwrap_or_else(|error| panic!("drained content at {dimensions:?}: {error}"));
            decoded_phases[*phase_index] += 1;
        }
        assert!(
            decoded_phases.iter().all(|count| *count >= 2),
            "representative dimensions {dimensions:?} must decode both updates: {decoded_phases:?}"
        );
        let expected_profile_idc = match settings.profile {
            H264Profile::Main => 77,
            H264Profile::High => 100,
        };
        assert_eq!(
            parameter_set
                .expect("actual representative SPS required")
                .get(1),
            Some(&expected_profile_idc)
        );
        assert_eq!(encoder.gpu_status().cpu_fallbacks, 0);
        assert_eq!(
            gpu_metrics().cpu_readback_bytes,
            before,
            "representative GPU input must not be read back to CPU"
        );
        println!(
            "representative_gpu_encoded_pixels visible_dimensions={dimensions:?} coded_dimensions={:?} profile={:?} bitrate={} decoded_phases={decoded_phases:?} config={} raw_gpu_readback=0",
            coded_dimensions.unwrap(),
            settings.profile,
            settings.bitrate,
            config_version.unwrap()
        );
    }
}
