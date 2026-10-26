use crate::capture::CapturedFrame;
use desklink_protocol::H264Profile;

mod bitrate;

const MAX_ENCODE_WIDTH: u32 = 2560;
const MAX_ENCODE_HEIGHT: u32 = 1440;
pub const EXPERIMENTAL_4K_WIDTH: u32 = 3840;
pub const EXPERIMENTAL_4K_HEIGHT: u32 = 2160;
const MAX_PENDING_FRAMES: usize = 2;
// Desktop text is much less forgiving than camera video: even 12 Mbps can
// flatten small glyphs and fine UI lines when a high-DPI display is resized
// for the wire. Keep the existing frame-rate contract, but give the sharp
// preset enough headroom for screen content. Automatic quality still steps
// down when the session is under pressure.
const DEFAULT_VIDEO_BITRATE: u32 = 18_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct H264EncoderSettings {
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub profile: H264Profile,
}

impl Default for H264EncoderSettings {
    fn default() -> Self {
        Self {
            max_width: MAX_ENCODE_WIDTH,
            max_height: MAX_ENCODE_HEIGHT,
            fps: 30,
            bitrate: DEFAULT_VIDEO_BITRATE,
            profile: H264Profile::Main,
        }
    }
}

impl H264EncoderSettings {
    /// Offline/LAN experiment settings. No runtime path selects this preset
    /// automatically; callers must opt in explicitly after transport gating.
    pub fn experimental_4k() -> Self {
        Self {
            max_width: EXPERIMENTAL_4K_WIDTH,
            max_height: EXPERIMENTAL_4K_HEIGHT,
            fps: 30,
            bitrate: 40_500_000,
            profile: H264Profile::High,
        }
    }

    pub fn validate(self) -> Result<Self, EncoderError> {
        if self.max_width < 2 || self.max_height < 2 || self.fps == 0 || self.bitrate == 0 {
            return Err(EncoderError::InvalidDimensions);
        }
        Ok(self)
    }
}

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

/// Local, monotonic measurements for the most recent encoder submission.
/// Byte counters cover the explicit NV12 write and MF input copy only; they
/// are not allocator telemetry or a count of driver/GPU transfers.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct EncoderPerformanceSample {
    pub frame_id: u64,
    pub config_version: u32,
    pub convert_us: u64,
    pub mft_us: u64,
    pub output_residence_us: Option<u64>,
    pub pending_frames: usize,
    pub nv12_written_bytes: usize,
    pub nv12_buffer_grew: bool,
    pub mf_input_copy_bytes: usize,
}

fn elapsed_us(started: std::time::Instant) -> u64 {
    started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod backend_tests {
    use super::*;

    #[test]
    fn asynchronous_calls_require_matching_bounded_event_credits() {
        let mut credits = AsyncMftCredits::default();
        assert!(!credits.take_input());
        assert!(!credits.take_output());
        credits.notify_input().unwrap();
        assert!(!credits.take_output());
        assert!(credits.take_input());
        assert!(!credits.take_input());
        for _ in 0..128 {
            credits.notify_output().unwrap();
        }
        assert!(credits.notify_output().is_err());
        for _ in 0..128 {
            assert!(credits.take_output());
        }
        assert!(!credits.take_output());
        credits.notify_output().unwrap();
        assert!(credits.take_output());
    }

    #[cfg(windows)]
    #[test]
    fn software_bitrate_change_preserves_stream_metadata_and_reference_state() {
        let mut encoder = H264Encoder::new_with_settings_and_backend(
            64,
            64,
            H264EncoderSettings::default(),
            EncoderBackendPreference::SoftwareOnly,
        )
        .unwrap();
        let frame = |index: u64| CapturedFrame {
            width: 64,
            height: 64,
            timestamp_us: index * 33_333,
            pixels: vec![index as u8; 64 * 64 * 4],
        };
        let first = encoder.encode(frame(1), true).unwrap();
        let config = encoder.config_version;
        let fps = encoder.fps;
        let profile = encoder.profile;
        let previous_bitrate = encoder.bitrate();
        let pending = encoder.pending_frames.len();
        let accepted = encoder
            .try_set_bitrate(previous_bitrate / 2)
            .expect("either accepted or safely unsupported");
        assert_eq!(
            encoder.bitrate(),
            if accepted {
                previous_bitrate / 2
            } else {
                previous_bitrate
            }
        );
        assert_eq!(encoder.config_version, config);
        assert_eq!(encoder.dimensions(), (64, 64));
        assert_eq!(encoder.fps, fps);
        assert_eq!(encoder.profile, profile);
        assert_eq!(encoder.pending_frames.len(), pending);
        let stable_bitrate = encoder.bitrate();
        assert_eq!(
            encoder.try_set_bitrate(0),
            Err(EncoderError::InvalidDimensions)
        );
        assert_eq!(encoder.bitrate(), stable_bitrate);
        let second = encoder.encode(frame(2), false).unwrap();
        assert_eq!(second.config_version, first.config_version);
        assert_eq!(second.frame_id, first.frame_id + 1);
        assert!(!second.access_unit.is_empty());
        println!("software MFT dynamic bitrate accepted={accepted}; stream generation preserved");
    }

    #[cfg(windows)]
    #[test]
    fn software_submissions_reuse_nv12_storage_after_warmup() {
        let mut encoder = H264Encoder::new_with_settings_and_backend(
            64,
            64,
            H264EncoderSettings::default(),
            EncoderBackendPreference::SoftwareOnly,
        )
        .unwrap();
        let frame = |index: u64| CapturedFrame {
            width: 64,
            height: 64,
            timestamp_us: index * 33_333,
            pixels: vec![index as u8; 64 * 64 * 4],
        };
        encoder.encode(frame(1), true).unwrap();
        assert!(encoder.last_performance_sample().unwrap().nv12_buffer_grew);
        let pointer = encoder.nv12_buffer.as_ptr();
        let capacity = encoder.nv12_buffer.capacity();
        for index in 2..=4 {
            encoder.encode(frame(index), false).unwrap();
            assert!(!encoder.last_performance_sample().unwrap().nv12_buffer_grew);
            assert_eq!(encoder.nv12_buffer.as_ptr(), pointer);
            assert_eq!(encoder.nv12_buffer.capacity(), capacity);
        }
    }

    #[cfg(windows)]
    #[test]
    fn synthetic_hardware_failure_retries_same_frame_as_fresh_software_idr() {
        let mut encoder = H264Encoder::new_with_settings_and_backend(
            64,
            64,
            H264EncoderSettings::default(),
            EncoderBackendPreference::SoftwareOnly,
        )
        .expect("software MFT init");
        assert_eq!(
            encoder.backend_info().unwrap().kind,
            "microsoft_software_mft"
        );
        let old_config = encoder.config_version;
        encoder.backend.inject_hardware_failure();
        let output = encoder
            .encode(
                CapturedFrame {
                    width: 64,
                    height: 64,
                    timestamp_us: 123_000,
                    pixels: vec![0; 64 * 64 * 4],
                },
                false,
            )
            .expect("software retry output");
        assert!(output.keyframe);
        assert!(!output.access_unit.is_empty());
        assert!(
            output
                .sequence_header
                .is_some_and(|header| !header.is_empty())
        );
        assert_eq!(output.timestamp_us, 123_000);
        assert_ne!(output.config_version, old_config);
        assert_eq!(output.config_version, encoder.config_version);
        assert_eq!(
            encoder.backend_info().unwrap().fallback_reason,
            Some("hardware_runtime_failure")
        );
        assert_eq!(
            encoder.backend_preference,
            EncoderBackendPreference::SoftwareOnly
        );
        let sample = encoder.last_performance_sample().unwrap();
        assert_eq!(sample.config_version, output.config_version);
        assert_eq!(sample.mf_input_copy_bytes, 64 * 64 * 3 / 2);
        assert!(sample.output_residence_us.is_some());
        assert!(encoder.pending_frames.is_empty());
        encoder.rebuild(64, 64).unwrap();
        assert!(encoder.last_performance_sample().is_none());
        assert_eq!(
            encoder.backend_info().unwrap().kind,
            "microsoft_software_mft"
        );
    }
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

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct EncoderBackendInfo {
    /// Hardware MFTs still receive CPU NV12 here. This is not a GPU surface path.
    pub kind: &'static str,
    pub name: String,
    pub hardware_candidates: usize,
    pub rejected_candidates: usize,
    pub fallback_reason: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EncoderBackendPreference {
    PreferHardware,
    SoftwareOnly,
}

/// Each asynchronous MFT notification authorizes exactly one matching call.
/// Keep credits bounded even if a defective driver floods the event queue.
#[cfg(any(windows, test))]
#[derive(Default)]
struct AsyncMftCredits {
    input: u32,
    output: u32,
}

#[cfg(any(windows, test))]
impl AsyncMftCredits {
    fn notify_input(&mut self) -> Result<(), EncoderError> {
        Self::increment(&mut self.input)
    }

    fn notify_output(&mut self) -> Result<(), EncoderError> {
        Self::increment(&mut self.output)
    }

    fn increment(credit: &mut u32) -> Result<(), EncoderError> {
        if *credit >= 128 {
            return Err(EncoderError::Native(
                "asynchronous MFT event budget exceeded".into(),
            ));
        }
        *credit += 1;
        Ok(())
    }

    fn take_input(&mut self) -> bool {
        Self::take(&mut self.input)
    }

    fn take_output(&mut self) -> bool {
        Self::take(&mut self.output)
    }

    fn take(credit: &mut u32) -> bool {
        if *credit == 0 {
            return false;
        }
        *credit -= 1;
        true
    }
}

pub fn fit_h264_dimensions(width: u32, height: u32) -> Result<(u32, u32), EncoderError> {
    fit_h264_dimensions_with_limit(width, height, MAX_ENCODE_WIDTH, MAX_ENCODE_HEIGHT)
}

pub fn fit_h264_dimensions_with_limit(
    width: u32,
    height: u32,
    max_width: u32,
    max_height: u32,
) -> Result<(u32, u32), EncoderError> {
    if width < 2 || height < 2 || max_width < 2 || max_height < 2 {
        return Err(EncoderError::InvalidDimensions);
    }

    let (width, height) = if u64::from(width) * u64::from(max_height)
        >= u64::from(height) * u64::from(max_width)
        && width > max_width
    {
        (
            max_width,
            (u64::from(height) * u64::from(max_width) / u64::from(width)) as u32,
        )
    } else if height > max_height {
        (
            (u64::from(width) * u64::from(max_height) / u64::from(height)) as u32,
            max_height,
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
    let mut nv12 = Vec::new();
    convert_to_nv12_into(
        &mut nv12,
        pixels,
        source_width,
        source_height,
        source_row_pitch,
        target_width,
        target_height,
        order,
    )?;
    Ok(nv12)
}

/// Reuses the caller's allocation. Every active Y/UV byte is overwritten;
/// validation errors leave the caller's previous buffer intact.
#[allow(clippy::too_many_arguments)]
pub fn convert_to_nv12_into(
    nv12: &mut Vec<u8>,
    pixels: &[u8],
    source_width: u32,
    source_height: u32,
    source_row_pitch: usize,
    target_width: u32,
    target_height: u32,
    order: PixelOrder,
) -> Result<(), EncoderError> {
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
    let nv12_len = y_plane_len
        .checked_add(y_plane_len / 2)
        .filter(|length| *length <= isize::MAX as usize)
        .ok_or(EncoderError::FrameTooLarge)?;
    nv12.resize(nv12_len, 0);
    // The 1:1 path uses offsets directly, without coordinate lookup arrays or
    // per-pixel division. When a high-DPI source must be resized, use
    // bilinear samples instead of nearest-neighbour picks: preserving the
    // coverage of thin glyphs is noticeably clearer than selecting one source
    // pixel for each output pixel.
    let needs_resample = source_width != target_width || source_height != target_height;
    for target_y in (0..target_height).step_by(2) {
        for target_x in (0..target_width).step_by(2) {
            let samples = [
                rgb_for_target(
                    pixels,
                    source_width,
                    source_height,
                    source_row_pitch,
                    target_width,
                    target_height,
                    target_x,
                    target_y,
                    order,
                    needs_resample,
                ),
                rgb_for_target(
                    pixels,
                    source_width,
                    source_height,
                    source_row_pitch,
                    target_width,
                    target_height,
                    target_x + 1,
                    target_y,
                    order,
                    needs_resample,
                ),
                rgb_for_target(
                    pixels,
                    source_width,
                    source_height,
                    source_row_pitch,
                    target_width,
                    target_height,
                    target_x,
                    target_y + 1,
                    order,
                    needs_resample,
                ),
                rgb_for_target(
                    pixels,
                    source_width,
                    source_height,
                    source_row_pitch,
                    target_width,
                    target_height,
                    target_x + 1,
                    target_y + 1,
                    order,
                    needs_resample,
                ),
            ];
            let y_offset_0 = target_y * target_width + target_x;
            let y_offset_1 = y_offset_0 + target_width;
            nv12[y_offset_0] = rgb_to_y(samples[0].0, samples[0].1, samples[0].2);
            nv12[y_offset_0 + 1] = rgb_to_y(samples[1].0, samples[1].1, samples[1].2);
            nv12[y_offset_1] = rgb_to_y(samples[2].0, samples[2].1, samples[2].2);
            nv12[y_offset_1 + 1] = rgb_to_y(samples[3].0, samples[3].1, samples[3].2);
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
            let (u, v) = rgb_to_uv((red / 4) as u8, (green / 4) as u8, (blue / 4) as u8);
            let uv_offset = y_plane_len + target_y / 2 * target_width + target_x;
            nv12[uv_offset] = u;
            nv12[uv_offset + 1] = v;
        }
    }
    Ok(())
}

fn rgb_at_offset(pixels: &[u8], offset: usize, order: PixelOrder) -> (u8, u8, u8) {
    match order {
        PixelOrder::Bgra => (pixels[offset + 2], pixels[offset + 1], pixels[offset]),
        PixelOrder::Rgba => (pixels[offset], pixels[offset + 1], pixels[offset + 2]),
    }
}

#[allow(clippy::too_many_arguments)]
#[inline(always)]
fn rgb_for_target(
    pixels: &[u8],
    source_width: usize,
    source_height: usize,
    source_row_pitch: usize,
    target_width: usize,
    target_height: usize,
    target_x: usize,
    target_y: usize,
    order: PixelOrder,
    needs_resample: bool,
) -> (u8, u8, u8) {
    if !needs_resample {
        return rgb_at_offset(pixels, target_y * source_row_pitch + target_x * 4, order);
    }

    let x_denominator = target_width.saturating_mul(2).max(1);
    let y_denominator = target_height.saturating_mul(2).max(1);
    let x_numerator = ((target_x.saturating_mul(2).saturating_add(1)).saturating_mul(source_width))
        .saturating_sub(target_width)
        .min(source_width.saturating_sub(1).saturating_mul(x_denominator));
    let y_numerator = ((target_y.saturating_mul(2).saturating_add(1))
        .saturating_mul(source_height))
    .saturating_sub(target_height)
    .min(
        source_height
            .saturating_sub(1)
            .saturating_mul(y_denominator),
    );
    let x0 = x_numerator / x_denominator;
    let y0 = y_numerator / y_denominator;
    let x1 = (x0 + 1).min(source_width - 1);
    let y1 = (y0 + 1).min(source_height - 1);
    let x_weight = x_numerator % x_denominator;
    let y_weight = y_numerator % y_denominator;
    let top_left = rgb_at_offset(pixels, y0 * source_row_pitch + x0 * 4, order);
    let top_right = rgb_at_offset(pixels, y0 * source_row_pitch + x1 * 4, order);
    let bottom_left = rgb_at_offset(pixels, y1 * source_row_pitch + x0 * 4, order);
    let bottom_right = rgb_at_offset(pixels, y1 * source_row_pitch + x1 * 4, order);
    (
        bilinear_channel(
            top_left.0,
            top_right.0,
            bottom_left.0,
            bottom_right.0,
            x_weight,
            y_weight,
            x_denominator,
            y_denominator,
        ),
        bilinear_channel(
            top_left.1,
            top_right.1,
            bottom_left.1,
            bottom_right.1,
            x_weight,
            y_weight,
            x_denominator,
            y_denominator,
        ),
        bilinear_channel(
            top_left.2,
            top_right.2,
            bottom_left.2,
            bottom_right.2,
            x_weight,
            y_weight,
            x_denominator,
            y_denominator,
        ),
    )
}

#[allow(clippy::too_many_arguments)]
fn bilinear_channel(
    top_left: u8,
    top_right: u8,
    bottom_left: u8,
    bottom_right: u8,
    x_weight: usize,
    y_weight: usize,
    x_denominator: usize,
    y_denominator: usize,
) -> u8 {
    let top =
        usize::from(top_left) * (x_denominator - x_weight) + usize::from(top_right) * x_weight;
    let bottom = usize::from(bottom_left) * (x_denominator - x_weight)
        + usize::from(bottom_right) * x_weight;
    let numerator = top * (y_denominator - y_weight) + bottom * y_weight;
    ((numerator + x_denominator * y_denominator / 2) / (x_denominator * y_denominator)) as u8
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
    bitrate: u32,
    max_width: u32,
    max_height: u32,
    frame_id: u64,
    config_version: u32,
    profile: H264Profile,
    backend_preference: EncoderBackendPreference,
    nv12_buffer: Vec<u8>,
    last_performance_sample: Option<EncoderPerformanceSample>,
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
    submitted_at: std::time::Instant,
}

impl H264Encoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, EncoderError> {
        let settings = H264EncoderSettings {
            fps,
            ..H264EncoderSettings::default()
        }
        .validate()?;
        if fit_h264_dimensions(width, height)? != (width, height) {
            return Err(EncoderError::InvalidDimensions);
        }
        Self::new_with_settings(width, height, settings)
    }

    pub fn new_with_settings(
        source_width: u32,
        source_height: u32,
        settings: H264EncoderSettings,
    ) -> Result<Self, EncoderError> {
        Self::new_with_settings_and_backend(
            source_width,
            source_height,
            settings,
            EncoderBackendPreference::PreferHardware,
        )
    }

    pub fn new_with_settings_and_backend(
        source_width: u32,
        source_height: u32,
        settings: H264EncoderSettings,
        backend_preference: EncoderBackendPreference,
    ) -> Result<Self, EncoderError> {
        let settings = settings.validate()?;
        let (width, height) = fit_h264_dimensions_with_limit(
            source_width,
            source_height,
            settings.max_width,
            settings.max_height,
        )?;
        if width == 0 || height == 0 {
            return Err(EncoderError::InvalidDimensions);
        }
        #[cfg(windows)]
        {
            let (backend, profile) = match native::MediaFoundationEncoder::with_preference(
                width,
                height,
                settings.fps,
                settings.bitrate,
                settings.profile,
                backend_preference,
            ) {
                Ok(backend) => (backend, settings.profile),
                Err(_error) if settings.profile == H264Profile::High => (
                    native::MediaFoundationEncoder::with_preference(
                        width,
                        height,
                        settings.fps,
                        settings.bitrate,
                        H264Profile::Main,
                        backend_preference,
                    )?,
                    H264Profile::Main,
                ),
                Err(error) => return Err(error),
            };
            Ok(Self {
                width,
                height,
                fps: settings.fps,
                bitrate: settings.bitrate,
                max_width: settings.max_width,
                max_height: settings.max_height,
                frame_id: 0,
                config_version: 1,
                profile,
                backend_preference,
                nv12_buffer: Vec::new(),
                last_performance_sample: None,
                pending_frames: std::collections::VecDeque::new(),
                backend,
            })
        }

        #[cfg(not(windows))]
        {
            let _ = (width, height, settings, backend_preference);
            Err(EncoderError::BackendUnavailable)
        }
    }

    pub fn encode(
        &mut self,
        frame: CapturedFrame,
        force_keyframe: bool,
    ) -> Result<EncodedFrame, EncoderError> {
        self.last_performance_sample = None;
        let target_dimensions = fit_h264_dimensions_with_limit(
            frame.width,
            frame.height,
            self.max_width,
            self.max_height,
        )?;
        if target_dimensions != (self.width, self.height) {
            self.rebuild(target_dimensions.0, target_dimensions.1)?;
        }

        #[cfg(windows)]
        {
            let submitted_at = std::time::Instant::now();
            let previous_nv12_capacity = self.nv12_buffer.capacity();
            native::frame_to_nv12_into(&frame, self.width, self.height, &mut self.nv12_buffer)?;
            let nv12 = &self.nv12_buffer;
            let convert_us = elapsed_us(submitted_at);
            let next_frame_id = self.frame_id.wrapping_add(1).max(1);
            let request_keyframe = force_keyframe || next_frame_id == 1;
            let mft_started = std::time::Instant::now();
            let mut failed_input_copy_bytes = 0_usize;
            let output = match self
                .backend
                .encode(nv12, frame.timestamp_us, request_keyframe)
            {
                Ok(output) => output,
                Err(_) if self.backend.is_hardware() => {
                    failed_input_copy_bytes = self.backend.last_input_copy_bytes();
                    let previous = self.backend.info().clone();
                    let (mut replacement, profile) = match native::MediaFoundationEncoder::software(
                        self.width,
                        self.height,
                        self.fps,
                        self.bitrate,
                        self.profile,
                    ) {
                        Ok(backend) => (backend, self.profile),
                        Err(_) if self.profile == H264Profile::High => (
                            native::MediaFoundationEncoder::software(
                                self.width,
                                self.height,
                                self.fps,
                                self.bitrate,
                                H264Profile::Main,
                            )?,
                            H264Profile::Main,
                        ),
                        Err(error) => return Err(error),
                    };
                    replacement.record_runtime_fallback(previous);
                    // A backend failure invalidates all reference pictures. Bind
                    // the retry to a fresh config and force an IDR of this frame.
                    self.backend = replacement;
                    self.backend_preference = EncoderBackendPreference::SoftwareOnly;
                    self.profile = profile;
                    self.config_version = self.config_version.wrapping_add(1).max(1);
                    self.pending_frames.clear();
                    self.backend.encode(nv12, frame.timestamp_us, true)?
                }
                Err(error) => return Err(error),
            };
            self.last_performance_sample = Some(EncoderPerformanceSample {
                frame_id: next_frame_id,
                config_version: self.config_version,
                convert_us,
                mft_us: elapsed_us(mft_started),
                output_residence_us: None,
                pending_frames: 0,
                nv12_written_bytes: nv12.len(),
                nv12_buffer_grew: self.nv12_buffer.capacity() > previous_nv12_capacity,
                mf_input_copy_bytes: failed_input_copy_bytes
                    .saturating_add(self.backend.last_input_copy_bytes()),
            });
            self.frame_id = next_frame_id;
            self.pending_frames.push_back(PendingFrame {
                frame_id: next_frame_id,
                config_version: self.config_version,
                timestamp_us: frame.timestamp_us,
                submitted_at,
            });
            let Some(output) = output else {
                while self.pending_frames.len() > MAX_PENDING_FRAMES {
                    self.pending_frames.pop_front();
                }
                if let Some(sample) = &mut self.last_performance_sample {
                    sample.pending_frames = self.pending_frames.len();
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
            if let Some(sample) = &mut self.last_performance_sample {
                sample.pending_frames = self.pending_frames.len();
                sample.output_residence_us = Some(elapsed_us(pending.submitted_at));
            }
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
        if fit_h264_dimensions_with_limit(width, height, self.max_width, self.max_height)?
            != (width, height)
        {
            return Err(EncoderError::InvalidDimensions);
        }
        #[cfg(windows)]
        {
            let (backend, profile) = match native::MediaFoundationEncoder::with_preference(
                width,
                height,
                self.fps,
                self.bitrate,
                self.profile,
                self.backend_preference,
            ) {
                Ok(backend) => (backend, self.profile),
                Err(_error) if self.profile == H264Profile::High => (
                    native::MediaFoundationEncoder::with_preference(
                        width,
                        height,
                        self.fps,
                        self.bitrate,
                        H264Profile::Main,
                        self.backend_preference,
                    )?,
                    H264Profile::Main,
                ),
                Err(error) => return Err(error),
            };
            self.backend = backend;
            self.profile = profile;
            self.pending_frames.clear();
            self.last_performance_sample = None;
        }
        self.width = width;
        self.height = height;
        self.config_version = self.config_version.wrapping_add(1).max(1);
        Ok(())
    }

    pub fn reconfigure_for_source(
        &mut self,
        source_width: u32,
        source_height: u32,
        settings: H264EncoderSettings,
    ) -> Result<(), EncoderError> {
        let settings = settings.validate()?;
        let (width, height) = fit_h264_dimensions_with_limit(
            source_width,
            source_height,
            settings.max_width,
            settings.max_height,
        )?;
        #[cfg(windows)]
        {
            let (backend, profile) = match native::MediaFoundationEncoder::with_preference(
                width,
                height,
                settings.fps,
                settings.bitrate,
                settings.profile,
                self.backend_preference,
            ) {
                Ok(backend) => (backend, settings.profile),
                Err(_error) if settings.profile == H264Profile::High => (
                    native::MediaFoundationEncoder::with_preference(
                        width,
                        height,
                        settings.fps,
                        settings.bitrate,
                        H264Profile::Main,
                        self.backend_preference,
                    )?,
                    H264Profile::Main,
                ),
                Err(error) => return Err(error),
            };
            self.backend = backend;
            self.profile = profile;
            self.pending_frames.clear();
            self.last_performance_sample = None;
        }
        self.width = width;
        self.height = height;
        self.fps = settings.fps;
        self.bitrate = settings.bitrate;
        self.max_width = settings.max_width;
        self.max_height = settings.max_height;
        self.config_version = self.config_version.wrapping_add(1).max(1);
        Ok(())
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn profile(&self) -> H264Profile {
        self.profile
    }

    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// Updates native rate control without resetting reference pictures. False
    /// means unsupported without writes, or rejection with both previous values
    /// restored and verified. Failed rollback requires rebuilding old settings.
    pub fn try_set_bitrate(&mut self, bitrate: u32) -> Result<bool, EncoderError> {
        if bitrate == 0 {
            return Err(EncoderError::InvalidDimensions);
        }
        #[cfg(windows)]
        {
            if self.backend.try_set_bitrate(bitrate)? {
                self.bitrate = bitrate;
                return Ok(true);
            }
            Ok(false)
        }
        #[cfg(not(windows))]
        {
            Ok(false)
        }
    }

    pub fn last_performance_sample(&self) -> Option<EncoderPerformanceSample> {
        self.last_performance_sample
    }

    pub fn backend_info(&self) -> Option<&EncoderBackendInfo> {
        #[cfg(windows)]
        {
            Some(self.backend.info())
        }
        #[cfg(not(windows))]
        {
            None
        }
    }
}

#[cfg(windows)]
mod native {
    use std::{
        mem::ManuallyDrop,
        ptr, slice,
        time::{Duration, Instant},
    };

    use desklink_protocol::H264Profile;

    use windows::{
        Win32::{
            Foundation::{RPC_E_CHANGED_MODE, VARIANT_FALSE, VARIANT_TRUE},
            Media::MediaFoundation::{
                CMSH264EncoderMFT, CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate,
                CODECAPI_AVEncCommonQualityVsSpeed, CODECAPI_AVEncCommonRateControlMode,
                CODECAPI_AVEncH264CABACEnable, CODECAPI_AVEncMPVDefaultBPictureCount,
                CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame,
                CODECAPI_AVLowLatencyMode, ICodecAPI, IMFActivate, IMFMediaEventGenerator,
                IMFMediaType, IMFSample, IMFShutdown, IMFTransform, METransformHaveOutput,
                METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE, MF_E_NOTACCEPTING,
                MF_E_TRANSFORM_NEED_MORE_INPUT, MF_EVENT_FLAG_NO_WAIT, MF_LOW_LATENCY,
                MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
                MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE,
                MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC,
                MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION, MFCreateMediaType, MFCreateMemoryBuffer,
                MFCreateSample, MFMediaType_Video, MFSTARTUP_FULL, MFSampleExtension_CleanPoint,
                MFShutdown, MFStartup, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
                MFT_ENUM_FLAG_SORTANDFILTER, MFT_FRIENDLY_NAME_Attribute,
                MFT_MESSAGE_COMMAND_FLUSH, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
                MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING,
                MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
                MFT_OUTPUT_DATA_BUFFER_INCOMPLETE, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
                MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_NV12,
                MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_PeakConstrainedVBR,
                eAVEncH264VProfile_High, eAVEncH264VProfile_Main,
            },
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
            System::Variant::{
                VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_UI4, VariantClear,
            },
        },
        core::Interface,
    };

    use super::{
        AsyncMftCredits, CapturedFrame, DEFAULT_VIDEO_BITRATE, EncoderBackendInfo,
        EncoderBackendPreference, EncoderError, PixelOrder, convert_to_nv12_into,
    };

    const ASYNC_FRAME_BUDGET: Duration = Duration::from_millis(100);
    const MAX_HARDWARE_CANDIDATES: usize = 16;

    struct NativeBitrateProperties<'a>(&'a ICodecAPI);

    impl super::bitrate::BitrateProperties for NativeBitrateProperties<'_> {
        fn can_modify(&self) -> bool {
            // S_FALSE is a successful HRESULT but explicitly means not
            // modifiable here, so .is_ok() would be incorrect.
            unsafe {
                self.0.IsModifiable(&CODECAPI_AVEncCommonMeanBitRate).0 == 0
                    && self.0.IsModifiable(&CODECAPI_AVEncCommonMaxBitRate).0 == 0
            }
        }

        fn read(&self) -> Result<(u32, u32), EncoderError> {
            Ok((
                read_codec_u32(self.0, &CODECAPI_AVEncCommonMeanBitRate)?,
                read_codec_u32(self.0, &CODECAPI_AVEncCommonMaxBitRate)?,
            ))
        }

        fn set_mean(&mut self, value: u32) -> Result<(), EncoderError> {
            unsafe {
                self.0
                    .SetValue(&CODECAPI_AVEncCommonMeanBitRate, &u32_variant(value))
            }
            .map_err(|error| native_error("set dynamic mean bitrate", error))
        }

        fn set_max(&mut self, value: u32) -> Result<(), EncoderError> {
            unsafe {
                self.0
                    .SetValue(&CODECAPI_AVEncCommonMaxBitRate, &u32_variant(value))
            }
            .map_err(|error| native_error("set dynamic peak bitrate", error))
        }
    }

    fn read_codec_u32(codec: &ICodecAPI, key: &windows::core::GUID) -> Result<u32, EncoderError> {
        let mut value = unsafe { codec.GetValue(key) }
            .map_err(|error| native_error("read codec bitrate", error))?;
        let result = if unsafe { value.Anonymous.Anonymous.vt } == VT_UI4 {
            Ok(unsafe { value.Anonymous.Anonymous.Anonymous.ulVal })
        } else {
            Err(EncoderError::Native(
                "codec bitrate is not a uint32 property".into(),
            ))
        };
        let _ = unsafe { VariantClear(&mut value) };
        result
    }

    fn hardware_candidates() -> Result<Vec<IMFActivate>, EncoderError> {
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let mut entries = ptr::null_mut();
        let mut count = 0;
        let result = unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut entries,
                &mut count,
            )
        };
        let candidates = if entries.is_null() {
            Vec::new()
        } else {
            // MFTEnumEx allocates COM references and the pointer array separately.
            // Move each reference into Rust ownership before freeing the array.
            let values = unsafe { slice::from_raw_parts_mut(entries, count as usize) };
            let candidates = values.iter_mut().filter_map(Option::take).collect();
            unsafe { CoTaskMemFree(Some(entries.cast())) };
            candidates
        };
        result.map_err(|error| native_error("enumerate hardware H.264 MFTs", error))?;
        Ok(candidates)
    }

    fn activation_name(activation: &IMFActivate) -> String {
        let length = unsafe { activation.GetStringLength(&MFT_FRIENDLY_NAME_Attribute) }
            .unwrap_or(0)
            .min(512);
        let mut name = vec![0_u16; length as usize + 1];
        if unsafe { activation.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut name, None) }.is_ok() {
            let end = name
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(name.len());
            String::from_utf16_lossy(&name[..end])
        } else {
            "hardware H.264 MFT".into()
        }
    }

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
        info: EncoderBackendInfo,
        events: Option<IMFMediaEventGenerator>,
        credits: AsyncMftCredits,
        last_input_copy_bytes: usize,
        #[cfg(test)]
        simulated_hardware_failure: bool,
        _runtime: NativeRuntime,
    }

    impl MediaFoundationEncoder {
        pub fn with_preference(
            width: u32,
            height: u32,
            fps: u32,
            bitrate: u32,
            profile: H264Profile,
            preference: EncoderBackendPreference,
        ) -> Result<Self, EncoderError> {
            match preference {
                EncoderBackendPreference::PreferHardware => {
                    Self::new(width, height, fps, bitrate, profile)
                }
                EncoderBackendPreference::SoftwareOnly => {
                    Self::software(width, height, fps, bitrate, profile)
                }
            }
        }

        pub fn new(
            width: u32,
            height: u32,
            fps: u32,
            bitrate: u32,
            profile: H264Profile,
        ) -> Result<Self, EncoderError> {
            let _runtime = NativeRuntime::start()?;
            let candidates = hardware_candidates();
            let (candidate_count, fallback_reason) = match &candidates {
                Ok(candidates) if candidates.is_empty() => (0, "no_hardware_candidates"),
                Ok(candidates) => (candidates.len(), "hardware_candidates_rejected"),
                Err(_) => (0, "hardware_enumeration_failed"),
            };
            let mut rejected = 0;
            if let Ok(candidates) = candidates {
                for activation in candidates.iter().take(MAX_HARDWARE_CANDIDATES) {
                    let info = EncoderBackendInfo {
                        kind: "hardware_mft_system_memory",
                        name: activation_name(activation),
                        hardware_candidates: candidate_count,
                        rejected_candidates: rejected,
                        fallback_reason: None,
                    };
                    if let Ok(transform) = unsafe { activation.ActivateObject::<IMFTransform>() } {
                        match Self::configure(transform, info, width, height, fps, bitrate, profile)
                        {
                            Ok(backend) => return Ok(backend),
                            Err(_) => {
                                let _ = unsafe { activation.ShutdownObject() };
                            }
                        }
                    }
                    rejected += 1;
                }
            }
            let mut backend = Self::software(width, height, fps, bitrate, profile)?;
            backend.info.hardware_candidates = candidate_count;
            backend.info.rejected_candidates = rejected;
            backend.info.fallback_reason = Some(fallback_reason);
            Ok(backend)
        }

        pub fn software(
            width: u32,
            height: u32,
            fps: u32,
            bitrate: u32,
            profile: H264Profile,
        ) -> Result<Self, EncoderError> {
            let _runtime = NativeRuntime::start()?;
            let transform: IMFTransform = unsafe {
                CoCreateInstance(
                    &CMSH264EncoderMFT,
                    None::<&windows::core::IUnknown>,
                    CLSCTX_INPROC_SERVER,
                )
            }
            .map_err(|error| native_error("create H.264 MFT", error))?;
            let info = EncoderBackendInfo {
                kind: "microsoft_software_mft",
                name: "Microsoft H.264 Encoder MFT".into(),
                hardware_candidates: 0,
                rejected_candidates: 0,
                fallback_reason: None,
            };
            Self::configure(transform, info, width, height, fps, bitrate, profile)
        }

        fn configure(
            transform: IMFTransform,
            info: EncoderBackendInfo,
            width: u32,
            height: u32,
            fps: u32,
            bitrate: u32,
            profile: H264Profile,
        ) -> Result<Self, EncoderError> {
            let runtime = NativeRuntime::start()?;
            let events = if info.kind == "hardware_mft_system_memory" {
                let attributes = unsafe { transform.GetAttributes() }
                    .map_err(|error| native_error("hardware MFT attributes", error))?;
                if unsafe { attributes.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) != 1 {
                    return Err(EncoderError::Native(
                        "hardware MFT did not declare asynchronous processing".into(),
                    ));
                }
                unsafe { attributes.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }
                    .map_err(|error| native_error("unlock hardware MFT", error))?;
                Some(
                    transform
                        .cast::<IMFMediaEventGenerator>()
                        .map_err(|error| native_error("hardware MFT event generator", error))?,
                )
            } else {
                None
            };
            let output_type = create_output_type(width, height, fps, bitrate, profile)?;
            let input_type = create_input_type(width, height, fps)?;
            let codec_api = transform.cast::<ICodecAPI>().ok();
            if let Some(codec_api) = &codec_api {
                configure_screen_quality(codec_api, bitrate);
            }
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
                let mean_bitrate = u32_variant(bitrate);
                // Allow short desktop-change bursts (window moves, scrolling,
                // text redraws) without forcing the encoder to flatten them
                // into the mean bitrate. Unsupported codec properties are
                // intentionally best-effort because OEM MFTs differ.
                let max_bitrate = u32_variant(bitrate.saturating_mul(3) / 2);
                let b_frames = u32_variant(0);
                unsafe {
                    let _ = codec_api.SetValue(&CODECAPI_AVLowLatencyMode, &low_latency);
                    let _ = codec_api.SetValue(&CODECAPI_AVEncMPVGOPSize, &gop_size);
                    let _ = codec_api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &mean_bitrate);
                    let _ = codec_api.SetValue(&CODECAPI_AVEncCommonMaxBitRate, &max_bitrate);
                    let _ = codec_api.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &b_frames);
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
                info,
                events,
                credits: AsyncMftCredits::default(),
                last_input_copy_bytes: 0,
                #[cfg(test)]
                simulated_hardware_failure: false,
                _runtime: runtime,
            })
        }

        pub fn encode(
            &mut self,
            nv12: &[u8],
            timestamp_us: u64,
            force_keyframe: bool,
        ) -> Result<Option<EncodedOutput>, EncoderError> {
            self.last_input_copy_bytes = 0;
            #[cfg(test)]
            if self.simulated_hardware_failure {
                return Err(EncoderError::Native("synthetic hardware failure".into()));
            }
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
            let sample = create_input_sample(
                nv12,
                timestamp_us,
                self.fps,
                force_keyframe,
                &mut self.last_input_copy_bytes,
            )?;
            if self.events.is_some() {
                return self.encode_async(&sample);
            }
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

        pub fn info(&self) -> &EncoderBackendInfo {
            &self.info
        }

        pub fn try_set_bitrate(&mut self, bitrate: u32) -> Result<bool, EncoderError> {
            let Some(codec) = &self.codec_api else {
                return Ok(false);
            };
            super::bitrate::try_apply(&mut NativeBitrateProperties(codec), bitrate)
        }

        pub fn last_input_copy_bytes(&self) -> usize {
            self.last_input_copy_bytes
        }

        pub fn is_hardware(&self) -> bool {
            #[cfg(test)]
            if self.simulated_hardware_failure {
                return true;
            }
            self.events.is_some()
        }

        #[cfg(test)]
        pub fn inject_hardware_failure(&mut self) {
            self.simulated_hardware_failure = true;
        }

        pub fn record_runtime_fallback(&mut self, previous: EncoderBackendInfo) {
            self.info.hardware_candidates = previous.hardware_candidates;
            self.info.rejected_candidates = previous.rejected_candidates.saturating_add(1);
            self.info.fallback_reason = Some("hardware_runtime_failure");
        }

        fn pump_events(&mut self) -> Result<(), EncoderError> {
            let Some(events) = &self.events else {
                return Ok(());
            };
            for _ in 0..32 {
                let event = match unsafe { events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                    Ok(event) => event,
                    Err(error) if error.code() == MF_E_NO_EVENTS_AVAILABLE => break,
                    Err(error) => return Err(native_error("read hardware MFT event", error)),
                };
                unsafe { event.GetStatus() }
                    .and_then(|status| status.ok())
                    .map_err(|error| native_error("hardware MFT event status", error))?;
                let event_type = unsafe { event.GetType() }
                    .map_err(|error| native_error("hardware MFT event type", error))?;
                if event_type == METransformNeedInput.0 as u32 {
                    self.credits.notify_input()?;
                } else if event_type == METransformHaveOutput.0 as u32 {
                    self.credits.notify_output()?;
                }
            }
            Ok(())
        }

        fn encode_async(
            &mut self,
            sample: &IMFSample,
        ) -> Result<Option<EncodedOutput>, EncoderError> {
            let started = Instant::now();
            loop {
                self.pump_events()?;
                if self.credits.take_input() {
                    break;
                }
                Self::wait_for_async_progress(started)?;
            }
            unsafe { self.transform.ProcessInput(0, sample, 0) }
                .map_err(|error| native_error("submit hardware NV12 sample", error))?;
            let mut access_unit = Vec::new();
            let mut keyframe = false;
            let mut timestamp_us = None;
            loop {
                self.pump_events()?;
                if !self.credits.take_output() {
                    Self::wait_for_async_progress(started)?;
                    continue;
                }
                // An async ProcessOutput call is permitted only after a
                // matching HaveOutput event, including incomplete fragments.
                let part = self.take_output_part()?.ok_or_else(|| {
                    EncoderError::Native("hardware MFT signalled output without a sample".into())
                })?;
                access_unit.extend_from_slice(&part.bytes);
                keyframe |= part.keyframe;
                timestamp_us = timestamp_us.or(part.timestamp_us);
                if !part.incomplete {
                    if access_unit.is_empty() {
                        return Err(EncoderError::Native(
                            "hardware MFT produced an empty access unit".into(),
                        ));
                    }
                    return Ok(Some(EncodedOutput {
                        keyframe: keyframe || contains_idr_nal(&access_unit),
                        access_unit,
                        sequence_header: self.sequence_header(),
                        timestamp_us,
                    }));
                }
                if started.elapsed() >= ASYNC_FRAME_BUDGET {
                    return Err(EncoderError::Native(
                        "hardware MFT output deadline exceeded".into(),
                    ));
                }
            }
        }

        fn wait_for_async_progress(started: Instant) -> Result<(), EncoderError> {
            if started.elapsed() >= ASYNC_FRAME_BUDGET {
                return Err(EncoderError::Native(
                    "hardware MFT progress deadline exceeded".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
            Ok(())
        }

        fn sequence_header(&self) -> Option<Vec<u8>> {
            unsafe { self.transform.GetOutputCurrentType(0) }
                .ok()
                .and_then(|media_type| read_sequence_header(&media_type))
                .or_else(|| read_sequence_header(&self.output_type))
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
                sequence_header: self.sequence_header(),
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
                if self.events.is_some()
                    && let Ok(shutdown) = self.transform.cast::<IMFShutdown>()
                {
                    let _ = shutdown.Shutdown();
                }
            }
        }
    }

    struct OutputPart {
        bytes: Vec<u8>,
        keyframe: bool,
        timestamp_us: Option<u64>,
        incomplete: bool,
    }

    pub fn frame_to_nv12_into(
        frame: &CapturedFrame,
        target_width: u32,
        target_height: u32,
        nv12: &mut Vec<u8>,
    ) -> Result<(), EncoderError> {
        let row_pitch = usize::try_from(frame.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or(EncoderError::FrameTooLarge)?;
        convert_to_nv12_into(
            nv12,
            &frame.pixels,
            frame.width,
            frame.height,
            row_pitch,
            target_width,
            target_height,
            PixelOrder::Bgra,
        )
    }

    fn create_output_type(
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        profile: H264Profile,
    ) -> Result<IMFMediaType, EncoderError> {
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
        unsafe { media_type.SetUINT32(&MF_MT_AVG_BITRATE, bitrate) }
            .map_err(|error| native_error("set H.264 bitrate", error))?;
        let profile_value = match profile {
            H264Profile::Main => eAVEncH264VProfile_Main.0,
            H264Profile::High => eAVEncH264VProfile_High.0,
        };
        unsafe { media_type.SetUINT32(&MF_MT_MPEG2_PROFILE, profile_value as u32) }
            .map_err(|error| native_error("set H.264 profile", error))?;
        Ok(media_type)
    }

    fn configure_screen_quality(codec_api: &ICodecAPI, bitrate: u32) {
        if bitrate < DEFAULT_VIDEO_BITRATE {
            return;
        }

        // Desktop text benefits from a constrained VBR burst, CABAC and a
        // higher encoder-complexity target. These are optional codec API
        // properties, so OEM encoders that do not expose one simply retain
        // their safe defaults.
        let rate_control = u32_variant(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32);
        let quality_vs_speed = u32_variant(95);
        let cabac = bool_variant(true);
        unsafe {
            let _ = codec_api.SetValue(&CODECAPI_AVEncCommonRateControlMode, &rate_control);
            let _ = codec_api.SetValue(&CODECAPI_AVEncCommonQualityVsSpeed, &quality_vs_speed);
            let _ = codec_api.SetValue(&CODECAPI_AVEncH264CABACEnable, &cabac);
        }
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
        copied_bytes: &mut usize,
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
        *copied_bytes = nv12.len();
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
