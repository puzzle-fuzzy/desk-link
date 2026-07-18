use crate::capture::CapturedFrame;

const MAX_ENCODE_WIDTH: u32 = 1920;
const MAX_ENCODE_HEIGHT: u32 = 1080;
const MAX_PENDING_FRAMES: usize = 2;
const DEFAULT_VIDEO_BITRATE: u32 = 4_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelOrder {
    Bgra,
    Rgba,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedFrame {
    pub frame_id: u64,
    pub config_version: u32,
    pub keyframe: bool,
    pub timestamp_us: u64,
    pub access_unit: Vec<u8>,
    pub sequence_header: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EncoderError {
    InvalidDimensions,
    InvalidFrame,
    FrameTooLarge,
    BackendUnavailable,
    NeedMoreInput,
    Native(String),
}

pub fn fit_h264_dimensions(width: u32, height: u32) -> Result<(u32, u32), EncoderError> {
    if width < 2 || height < 2 {
        return Err(EncoderError::InvalidDimensions);
    }

    let (width, height) = if u64::from(width) * u64::from(MAX_ENCODE_HEIGHT)
        >= u64::from(height) * u64::from(MAX_ENCODE_WIDTH)
        && width > MAX_ENCODE_WIDTH
    {
        (
            MAX_ENCODE_WIDTH,
            (u64::from(height) * u64::from(MAX_ENCODE_WIDTH) / u64::from(width)) as u32,
        )
    } else if height > MAX_ENCODE_HEIGHT {
        (
            (u64::from(width) * u64::from(MAX_ENCODE_HEIGHT) / u64::from(height)) as u32,
            MAX_ENCODE_HEIGHT,
        )
    } else {
        (width, height)
    };
    let width = width & !1;
    let height = height & !1;
    if width < 2 || height < 2 {
        return Err(EncoderError::InvalidDimensions);
    }
    Ok((width, height))
}

pub fn convert_to_nv12(
    pixels: &[u8],
    source_width: u32,
    source_height: u32,
    source_row_pitch: usize,
    target_width: u32,
    target_height: u32,
    order: PixelOrder,
) -> Result<Vec<u8>, EncoderError> {
    if source_width == 0
        || source_height == 0
        || target_width < 2
        || target_height < 2
        || !target_width.is_multiple_of(2)
        || !target_height.is_multiple_of(2)
    {
        return Err(EncoderError::InvalidDimensions);
    }

    let source_width = usize::try_from(source_width).map_err(|_| EncoderError::InvalidFrame)?;
    let source_height = usize::try_from(source_height).map_err(|_| EncoderError::InvalidFrame)?;
    let target_width = usize::try_from(target_width).map_err(|_| EncoderError::InvalidFrame)?;
    let target_height = usize::try_from(target_height).map_err(|_| EncoderError::InvalidFrame)?;
    let active_source_bytes = source_width
        .checked_mul(4)
        .ok_or(EncoderError::FrameTooLarge)?;
    if source_row_pitch < active_source_bytes {
        return Err(EncoderError::InvalidFrame);
    }
    let required_source_bytes = source_row_pitch
        .checked_mul(source_height.saturating_sub(1))
        .and_then(|bytes| bytes.checked_add(active_source_bytes))
        .ok_or(EncoderError::FrameTooLarge)?;
    if pixels.len() < required_source_bytes {
        return Err(EncoderError::InvalidFrame);
    }

    let y_plane_len = target_width
        .checked_mul(target_height)
        .ok_or(EncoderError::FrameTooLarge)?;
    let mut nv12 = vec![0_u8; y_plane_len + y_plane_len / 2];
    for target_y in 0..target_height {
        let source_y = target_y * source_height / target_height;
        for target_x in 0..target_width {
            let source_x = target_x * source_width / target_width;
            let (red, green, blue) = rgb_at(pixels, source_row_pitch, source_x, source_y, order);
            nv12[target_y * target_width + target_x] = rgb_to_y(red, green, blue);
        }
    }

    for target_y in (0..target_height).step_by(2) {
        for target_x in (0..target_width).step_by(2) {
            let mut red = 0_u32;
            let mut green = 0_u32;
            let mut blue = 0_u32;
            for offset_y in 0..2 {
                for offset_x in 0..2 {
                    let source_x = (target_x + offset_x) * source_width / target_width;
                    let source_y = (target_y + offset_y) * source_height / target_height;
                    let (sample_red, sample_green, sample_blue) =
                        rgb_at(pixels, source_row_pitch, source_x, source_y, order);
                    red += u32::from(sample_red);
                    green += u32::from(sample_green);
                    blue += u32::from(sample_blue);
                }
            }
            let (u, v) = rgb_to_uv((red / 4) as u8, (green / 4) as u8, (blue / 4) as u8);
            let uv_offset = y_plane_len + target_y / 2 * target_width + target_x;
            nv12[uv_offset] = u;
            nv12[uv_offset + 1] = v;
        }
    }
    Ok(nv12)
}

fn rgb_at(pixels: &[u8], row_pitch: usize, x: usize, y: usize, order: PixelOrder) -> (u8, u8, u8) {
    let offset = y * row_pitch + x * 4;
    match order {
        PixelOrder::Bgra => (pixels[offset + 2], pixels[offset + 1], pixels[offset]),
        PixelOrder::Rgba => (pixels[offset], pixels[offset + 1], pixels[offset + 2]),
    }
}

fn rgb_to_y(red: u8, green: u8, blue: u8) -> u8 {
    let value =
        (47 * i32::from(red) + 157 * i32::from(green) + 16 * i32::from(blue) + 128) / 256 + 16;
    value.clamp(0, 255) as u8
}

fn rgb_to_uv(red: u8, green: u8, blue: u8) -> (u8, u8) {
    let u =
        (-26 * i32::from(red) - 87 * i32::from(green) + 113 * i32::from(blue) + 128) / 256 + 128;
    let v =
        (112 * i32::from(red) - 102 * i32::from(green) - 10 * i32::from(blue) + 128) / 256 + 128;
    (u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
}

pub struct H264Encoder {
    width: u32,
    height: u32,
    fps: u32,
    frame_id: u64,
    config_version: u32,
    #[cfg(windows)]
    pending_frames: std::collections::VecDeque<PendingFrame>,
    #[cfg(windows)]
    backend: native::MediaFoundationEncoder,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
struct PendingFrame {
    frame_id: u64,
    config_version: u32,
    timestamp_us: u64,
}

impl H264Encoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, EncoderError> {
        if fit_h264_dimensions(width, height)? != (width, height) || fps == 0 {
            return Err(EncoderError::InvalidDimensions);
        }
        #[cfg(windows)]
        {
            let backend = native::MediaFoundationEncoder::new(width, height, fps)?;
            Ok(Self {
                width,
                height,
                fps,
                frame_id: 0,
                config_version: 1,
                pending_frames: std::collections::VecDeque::new(),
                backend,
            })
        }

        #[cfg(not(windows))]
        {
            let _ = (width, height, fps);
            return Err(EncoderError::BackendUnavailable);
        }
    }

    pub fn encode(
        &mut self,
        frame: CapturedFrame,
        force_keyframe: bool,
    ) -> Result<EncodedFrame, EncoderError> {
        let target_dimensions = fit_h264_dimensions(frame.width, frame.height)?;
        if target_dimensions != (self.width, self.height) {
            self.rebuild(target_dimensions.0, target_dimensions.1)?;
        }

        #[cfg(windows)]
        {
            let nv12 = native::frame_to_nv12(&frame, self.width, self.height)?;
            let next_frame_id = self.frame_id.wrapping_add(1).max(1);
            let request_keyframe = force_keyframe
                || next_frame_id == 1
                || next_frame_id.is_multiple_of(u64::from(self.fps));
            let output = self
                .backend
                .encode(&nv12, frame.timestamp_us, request_keyframe)?;
            self.frame_id = next_frame_id;
            self.pending_frames.push_back(PendingFrame {
                frame_id: next_frame_id,
                config_version: self.config_version,
                timestamp_us: frame.timestamp_us,
            });
            let Some(output) = output else {
                while self.pending_frames.len() > MAX_PENDING_FRAMES {
                    self.pending_frames.pop_front();
                }
                return Err(EncoderError::NeedMoreInput);
            };
            let pending = if let Some(timestamp_us) = output.timestamp_us {
                self.pending_frames
                    .iter()
                    .position(|pending| pending.timestamp_us == timestamp_us)
                    .and_then(|index| self.pending_frames.remove(index))
            } else {
                self.pending_frames.pop_front()
            };
            while self.pending_frames.len() > MAX_PENDING_FRAMES {
                self.pending_frames.pop_front();
            }
            let Some(pending) = pending else {
                return Err(EncoderError::NeedMoreInput);
            };
            Ok(EncodedFrame {
                frame_id: pending.frame_id,
                config_version: pending.config_version,
                keyframe: output.keyframe,
                timestamp_us: pending.timestamp_us,
                access_unit: output.access_unit,
                sequence_header: output.sequence_header,
            })
        }

        #[cfg(not(windows))]
        {
            let _ = (frame, force_keyframe);
            Err(EncoderError::BackendUnavailable)
        }
    }

    pub fn rebuild(&mut self, width: u32, height: u32) -> Result<(), EncoderError> {
        if fit_h264_dimensions(width, height)? != (width, height) {
            return Err(EncoderError::InvalidDimensions);
        }
        #[cfg(windows)]
        {
            let backend = native::MediaFoundationEncoder::new(width, height, self.fps)?;
            self.backend = backend;
            self.pending_frames.clear();
        }
        self.width = width;
        self.height = height;
        self.config_version = self.config_version.wrapping_add(1).max(1);
        Ok(())
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }
}

#[cfg(windows)]
mod native {
    use std::{mem::ManuallyDrop, ptr, slice};

    use windows::{
        Win32::{
            Foundation::{RPC_E_CHANGED_MODE, VARIANT_FALSE, VARIANT_TRUE},
            Media::MediaFoundation::{
                CMSH264EncoderMFT, CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame,
                CODECAPI_AVLowLatencyMode, ICodecAPI, IMFMediaType, IMFSample, IMFTransform,
                MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_LOW_LATENCY,
                MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
                MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE,
                MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_VERSION, MFCreateMediaType,
                MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFSTARTUP_FULL,
                MFSampleExtension_CleanPoint, MFShutdown, MFStartup, MFT_MESSAGE_COMMAND_FLUSH,
                MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
                MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
                MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_DATA_BUFFER_INCOMPLETE,
                MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFVideoFormat_H264, MFVideoFormat_NV12,
                MFVideoInterlace_Progressive, eAVEncH264VProfile_Main,
            },
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4},
        },
        core::Interface,
    };

    use super::{CapturedFrame, DEFAULT_VIDEO_BITRATE, EncoderError, PixelOrder, convert_to_nv12};

    pub struct EncodedOutput {
        pub access_unit: Vec<u8>,
        pub keyframe: bool,
        pub sequence_header: Option<Vec<u8>>,
        pub timestamp_us: Option<u64>,
    }

    struct NativeRuntime {
        com_initialized: bool,
    }

    impl NativeRuntime {
        fn start() -> Result<Self, EncoderError> {
            let com_result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            let com_initialized = if com_result.is_ok() {
                true
            } else if com_result == RPC_E_CHANGED_MODE {
                false
            } else {
                return Err(EncoderError::Native(format!(
                    "CoInitializeEx failed: {com_result:?}"
                )));
            };
            if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
                if com_initialized {
                    unsafe { CoUninitialize() };
                }
                return Err(native_error("MFStartup", error));
            }
            Ok(Self { com_initialized })
        }
    }

    impl Drop for NativeRuntime {
        fn drop(&mut self) {
            let _ = unsafe { MFShutdown() };
            if self.com_initialized {
                unsafe { CoUninitialize() };
            }
        }
    }

    pub struct MediaFoundationEncoder {
        transform: IMFTransform,
        codec_api: Option<ICodecAPI>,
        output_type: IMFMediaType,
        output_buffer_size: u32,
        width: u32,
        height: u32,
        fps: u32,
        _runtime: NativeRuntime,
    }

    impl MediaFoundationEncoder {
        pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, EncoderError> {
            let runtime = NativeRuntime::start()?;
            let transform: IMFTransform = unsafe {
                CoCreateInstance(
                    &CMSH264EncoderMFT,
                    None::<&windows::core::IUnknown>,
                    CLSCTX_INPROC_SERVER,
                )
            }
            .map_err(|error| native_error("create H.264 MFT", error))?;
            let output_type = create_output_type(width, height, fps)?;
            let input_type = create_input_type(width, height, fps)?;
            let codec_api = transform.cast::<ICodecAPI>().ok();
            unsafe {
                if let Ok(attributes) = transform.GetAttributes() {
                    let _ = attributes.SetUINT32(&MF_LOW_LATENCY, 1);
                }
                transform
                    .SetOutputType(0, &output_type, 0)
                    .map_err(|error| native_error("set H.264 output type", error))?;
                transform
                    .SetInputType(0, &input_type, 0)
                    .map_err(|error| native_error("set NV12 input type", error))?;
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                    .map_err(|error| native_error("begin H.264 streaming", error))?;
                transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                    .map_err(|error| native_error("start H.264 stream", error))?;
            }
            if let Some(codec_api) = &codec_api {
                let low_latency = bool_variant(true);
                let gop_size = u32_variant(fps);
                unsafe {
                    let _ = codec_api.SetValue(&CODECAPI_AVLowLatencyMode, &low_latency);
                    let _ = codec_api.SetValue(&CODECAPI_AVEncMPVGOPSize, &gop_size);
                }
            }
            let stream_info = unsafe { transform.GetOutputStreamInfo(0) }
                .map_err(|error| native_error("get H.264 output stream info", error))?;
            let raw_frame_size = width
                .checked_mul(height)
                .and_then(|pixels| pixels.checked_mul(3))
                .map(|bytes| bytes / 2)
                .ok_or(EncoderError::FrameTooLarge)?;
            Ok(Self {
                transform,
                codec_api,
                output_type,
                output_buffer_size: stream_info.cbSize.max(raw_frame_size),
                width,
                height,
                fps,
                _runtime: runtime,
            })
        }

        pub fn encode(
            &self,
            nv12: &[u8],
            timestamp_us: u64,
            force_keyframe: bool,
        ) -> Result<Option<EncodedOutput>, EncoderError> {
            let expected_len = usize::try_from(self.width)
                .ok()
                .and_then(|width| {
                    usize::try_from(self.height)
                        .ok()
                        .and_then(|height| width.checked_mul(height))
                })
                .and_then(|pixels| pixels.checked_mul(3))
                .map(|bytes| bytes / 2)
                .ok_or(EncoderError::FrameTooLarge)?;
            if nv12.len() != expected_len {
                return Err(EncoderError::InvalidFrame);
            }
            if force_keyframe && let Some(codec_api) = &self.codec_api {
                let force = u32_variant(1);
                let _ = unsafe { codec_api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &force) };
            }
            let sample = create_input_sample(nv12, timestamp_us, self.fps, force_keyframe)?;
            match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
                Ok(()) => self.take_output(),
                Err(error) if error.code() == MF_E_NOTACCEPTING => {
                    let queued_output = self.take_output()?;
                    unsafe { self.transform.ProcessInput(0, &sample, 0) }.map_err(|error| {
                        native_error("submit NV12 sample after draining", error)
                    })?;
                    if queued_output.is_some() {
                        Ok(queued_output)
                    } else {
                        self.take_output()
                    }
                }
                Err(error) => Err(native_error("submit NV12 sample", error)),
            }
        }

        fn take_output(&self) -> Result<Option<EncodedOutput>, EncoderError> {
            let mut access_unit = Vec::new();
            let mut keyframe = false;
            let mut timestamp_us = None;
            loop {
                let Some(part) = self.take_output_part()? else {
                    if access_unit.is_empty() {
                        return Ok(None);
                    }
                    break;
                };
                access_unit.extend_from_slice(&part.bytes);
                keyframe |= part.keyframe;
                timestamp_us = timestamp_us.or(part.timestamp_us);
                if !part.incomplete {
                    break;
                }
            }
            if access_unit.is_empty() {
                return Ok(None);
            }
            Ok(Some(EncodedOutput {
                keyframe: keyframe || contains_idr_nal(&access_unit),
                access_unit,
                sequence_header: read_sequence_header(&self.output_type),
                timestamp_us,
            }))
        }

        fn take_output_part(&self) -> Result<Option<OutputPart>, EncoderError> {
            let stream_info = unsafe { self.transform.GetOutputStreamInfo(0) }
                .map_err(|error| native_error("refresh H.264 output stream info", error))?;
            let sample = if stream_info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0 {
                None
            } else {
                Some(create_output_sample(self.output_buffer_size)?)
            };
            let mut output = MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            };
            let mut status = 0_u32;
            let result = unsafe {
                self.transform
                    .ProcessOutput(0, slice::from_mut(&mut output), &mut status)
            };
            let sample = unsafe { ManuallyDrop::take(&mut output.pSample) };
            let events = unsafe { ManuallyDrop::take(&mut output.pEvents) };
            drop(events);
            if let Err(error) = result {
                if error.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                    return Ok(None);
                }
                return Err(native_error("read H.264 output", error));
            }
            let Some(sample) = sample else {
                return Ok(None);
            };
            let keyframe =
                unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) != 0;
            let timestamp_us = unsafe { sample.GetSampleTime() }
                .ok()
                .and_then(|timestamp| u64::try_from(timestamp).ok())
                .map(|timestamp| timestamp / 10);
            let buffer = unsafe { sample.ConvertToContiguousBuffer() }
                .map_err(|error| native_error("coalesce H.264 output", error))?;
            let length = unsafe { buffer.GetCurrentLength() }
                .map_err(|error| native_error("get H.264 output length", error))?;
            let mut data = ptr::null_mut();
            unsafe { buffer.Lock(&mut data, None, None) }
                .map_err(|error| native_error("lock H.264 output", error))?;
            let bytes = if data.is_null() || length == 0 {
                Vec::new()
            } else {
                unsafe { slice::from_raw_parts(data, length as usize) }.to_vec()
            };
            unsafe { buffer.Unlock() }
                .map_err(|error| native_error("unlock H.264 output", error))?;
            Ok(Some(OutputPart {
                bytes,
                keyframe,
                timestamp_us,
                incomplete: output.dwStatus & MFT_OUTPUT_DATA_BUFFER_INCOMPLETE.0 as u32 != 0,
            }))
        }
    }

    impl Drop for MediaFoundationEncoder {
        fn drop(&mut self) {
            unsafe {
                let _ = self
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
                let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
                let _ = self
                    .transform
                    .ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            }
        }
    }

    struct OutputPart {
        bytes: Vec<u8>,
        keyframe: bool,
        timestamp_us: Option<u64>,
        incomplete: bool,
    }

    pub fn frame_to_nv12(
        frame: &CapturedFrame,
        target_width: u32,
        target_height: u32,
    ) -> Result<Vec<u8>, EncoderError> {
        let row_pitch = usize::try_from(frame.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or(EncoderError::FrameTooLarge)?;
        convert_to_nv12(
            &frame.pixels,
            frame.width,
            frame.height,
            row_pitch,
            target_width,
            target_height,
            PixelOrder::Bgra,
        )
    }

    fn create_output_type(width: u32, height: u32, fps: u32) -> Result<IMFMediaType, EncoderError> {
        let media_type = unsafe { MFCreateMediaType() }
            .map_err(|error| native_error("create H.264 output media type", error))?;
        unsafe { media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video) }
            .map_err(|error| native_error("set H.264 major type", error))?;
        unsafe { media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264) }
            .map_err(|error| native_error("set H.264 subtype", error))?;
        unsafe { media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_ratio(width, height)) }
            .map_err(|error| native_error("set H.264 frame size", error))?;
        unsafe { media_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps, 1)) }
            .map_err(|error| native_error("set H.264 frame rate", error))?;
        unsafe { media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1)) }
            .map_err(|error| native_error("set H.264 pixel aspect ratio", error))?;
        unsafe {
            media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
        }
        .map_err(|error| native_error("set H.264 interlace mode", error))?;
        unsafe { media_type.SetUINT32(&MF_MT_AVG_BITRATE, DEFAULT_VIDEO_BITRATE) }
            .map_err(|error| native_error("set H.264 bitrate", error))?;
        unsafe { media_type.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32) }
            .map_err(|error| native_error("set H.264 profile", error))?;
        Ok(media_type)
    }

    fn create_input_type(width: u32, height: u32, fps: u32) -> Result<IMFMediaType, EncoderError> {
        let media_type = unsafe { MFCreateMediaType() }
            .map_err(|error| native_error("create NV12 input media type", error))?;
        unsafe { media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video) }
            .map_err(|error| native_error("set NV12 major type", error))?;
        unsafe { media_type.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12) }
            .map_err(|error| native_error("set NV12 subtype", error))?;
        unsafe { media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack_ratio(width, height)) }
            .map_err(|error| native_error("set NV12 frame size", error))?;
        unsafe { media_type.SetUINT64(&MF_MT_FRAME_RATE, pack_ratio(fps, 1)) }
            .map_err(|error| native_error("set NV12 frame rate", error))?;
        unsafe { media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_ratio(1, 1)) }
            .map_err(|error| native_error("set NV12 pixel aspect ratio", error))?;
        unsafe {
            media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
        }
        .map_err(|error| native_error("set NV12 interlace mode", error))?;
        Ok(media_type)
    }

    fn create_input_sample(
        nv12: &[u8],
        timestamp_us: u64,
        fps: u32,
        force_keyframe: bool,
    ) -> Result<IMFSample, EncoderError> {
        let length = u32::try_from(nv12.len()).map_err(|_| EncoderError::FrameTooLarge)?;
        let buffer = unsafe { MFCreateMemoryBuffer(length) }
            .map_err(|error| native_error("allocate NV12 media buffer", error))?;
        let mut destination = ptr::null_mut();
        unsafe { buffer.Lock(&mut destination, None, None) }
            .map_err(|error| native_error("lock NV12 media buffer", error))?;
        if destination.is_null() {
            let _ = unsafe { buffer.Unlock() };
            return Err(EncoderError::InvalidFrame);
        }
        unsafe { ptr::copy_nonoverlapping(nv12.as_ptr(), destination, nv12.len()) };
        unsafe { buffer.Unlock() }
            .map_err(|error| native_error("unlock NV12 media buffer", error))?;
        unsafe { buffer.SetCurrentLength(length) }
            .map_err(|error| native_error("commit NV12 media buffer", error))?;
        let sample = unsafe { MFCreateSample() }
            .map_err(|error| native_error("create NV12 media sample", error))?;
        unsafe { sample.AddBuffer(&buffer) }
            .map_err(|error| native_error("attach NV12 media buffer", error))?;
        unsafe {
            sample.SetSampleTime(timestamp_us.saturating_mul(10).min(i64::MAX as u64) as i64)
        }
        .map_err(|error| native_error("set NV12 sample time", error))?;
        unsafe { sample.SetSampleDuration(10_000_000_i64 / i64::from(fps)) }
            .map_err(|error| native_error("set NV12 sample duration", error))?;
        if force_keyframe {
            unsafe { sample.SetUINT32(&MFSampleExtension_CleanPoint, 1) }
                .map_err(|error| native_error("request H.264 keyframe", error))?;
        }
        Ok(sample)
    }

    fn create_output_sample(buffer_size: u32) -> Result<IMFSample, EncoderError> {
        let sample = unsafe { MFCreateSample() }
            .map_err(|error| native_error("create H.264 output sample", error))?;
        let buffer = unsafe { MFCreateMemoryBuffer(buffer_size) }
            .map_err(|error| native_error("allocate H.264 output buffer", error))?;
        unsafe { sample.AddBuffer(&buffer) }
            .map_err(|error| native_error("attach H.264 output buffer", error))?;
        Ok(sample)
    }

    fn read_sequence_header(media_type: &IMFMediaType) -> Option<Vec<u8>> {
        let size = unsafe { media_type.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) }.ok()?;
        if size == 0 {
            return None;
        }
        let mut header = vec![0_u8; size as usize];
        let mut actual = 0_u32;
        unsafe {
            media_type
                .GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut header, Some(&mut actual))
                .ok()?;
        }
        header.truncate(actual as usize);
        (!header.is_empty()).then_some(header)
    }

    fn contains_idr_nal(access_unit: &[u8]) -> bool {
        let mut index = 0;
        while index + 4 <= access_unit.len() {
            let start_code_len = if access_unit[index..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if access_unit[index..].starts_with(&[0, 0, 1]) {
                3
            } else {
                index += 1;
                continue;
            };
            let nal_offset = index + start_code_len;
            if nal_offset < access_unit.len() && access_unit[nal_offset] & 0x1f == 5 {
                return true;
            }
            index = nal_offset.saturating_add(1);
        }
        false
    }

    fn pack_ratio(numerator: u32, denominator: u32) -> u64 {
        (u64::from(numerator) << 32) | u64::from(denominator)
    }

    fn bool_variant(value: bool) -> VARIANT {
        VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                    vt: VT_BOOL,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: VARIANT_0_0_0 {
                        boolVal: if value { VARIANT_TRUE } else { VARIANT_FALSE },
                    },
                }),
            },
        }
    }

    fn u32_variant(value: u32) -> VARIANT {
        VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                    vt: VT_UI4,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: VARIANT_0_0_0 { ulVal: value },
                }),
            },
        }
    }

    fn native_error(context: &str, error: windows::core::Error) -> EncoderError {
        EncoderError::Native(format!("{context}: {error}"))
    }
}
