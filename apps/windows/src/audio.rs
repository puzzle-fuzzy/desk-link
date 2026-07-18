use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use desklink_protocol::{
    AUDIO_FRAME_SAMPLES, AUDIO_SAMPLE_RATE, MAX_AUDIO_PAYLOAD_BYTES, MAX_OPUS_AUDIO_PAYLOAD_BYTES,
};
use tokio::sync::{Notify, watch};

pub const AUDIO_QUEUE_CAPACITY: usize = 8;
pub const OPUS_TARGET_BITRATE: i32 = 64_000;
const CAPTURE_RETRY_DELAY: Duration = Duration::from_secs(2);

#[cfg(windows)]
mod opus_codec {
    use std::{ffi::CStr, ptr::NonNull};

    use opus_head_sys as ffi;

    use super::{
        AUDIO_FRAME_SAMPLES, AUDIO_SAMPLE_RATE, MAX_AUDIO_PAYLOAD_BYTES,
        MAX_OPUS_AUDIO_PAYLOAD_BYTES, OPUS_TARGET_BITRATE,
    };

    pub struct RemoteAudioEncoder {
        inner: NonNull<ffi::OpusEncoder>,
        pcm: [i16; AUDIO_FRAME_SAMPLES],
        encoded: [u8; MAX_OPUS_AUDIO_PAYLOAD_BYTES],
    }

    // Each codec instance is exclusively accessed through &mut self. libopus
    // permits independent state objects to move between threads.
    unsafe impl Send for RemoteAudioEncoder {}

    impl RemoteAudioEncoder {
        pub fn new() -> Result<Self, String> {
            let mut error = ffi::OPUS_OK as i32;
            let inner = unsafe {
                ffi::opus_encoder_create(
                    AUDIO_SAMPLE_RATE as i32,
                    1,
                    ffi::OPUS_APPLICATION_AUDIO as i32,
                    &mut error,
                )
            };
            check_codec_result("opus_encoder_create", error)?;
            let inner = NonNull::new(inner)
                .ok_or_else(|| "opus_encoder_create returned a null state".to_owned())?;
            let mut encoder = Self {
                inner,
                pcm: [0; AUDIO_FRAME_SAMPLES],
                encoded: [0; MAX_OPUS_AUDIO_PAYLOAD_BYTES],
            };
            encoder.set_control(
                ffi::OPUS_SET_BITRATE_REQUEST,
                OPUS_TARGET_BITRATE,
                "OPUS_SET_BITRATE",
            )?;
            encoder.set_control(ffi::OPUS_SET_VBR_REQUEST, 1, "OPUS_SET_VBR")?;
            encoder.set_control(
                ffi::OPUS_SET_VBR_CONSTRAINT_REQUEST,
                1,
                "OPUS_SET_VBR_CONSTRAINT",
            )?;
            encoder.set_control(ffi::OPUS_SET_COMPLEXITY_REQUEST, 6, "OPUS_SET_COMPLEXITY")?;
            encoder.set_control(ffi::OPUS_SET_INBAND_FEC_REQUEST, 1, "OPUS_SET_INBAND_FEC")?;
            encoder.set_control(
                ffi::OPUS_SET_PACKET_LOSS_PERC_REQUEST,
                5,
                "OPUS_SET_PACKET_LOSS_PERC",
            )?;
            encoder.set_control(ffi::OPUS_SET_DTX_REQUEST, 0, "OPUS_SET_DTX")?;
            Ok(encoder)
        }

        pub fn encode_pcm_s16_le(&mut self, payload: &[u8]) -> Result<Vec<u8>, String> {
            if payload.len() != MAX_AUDIO_PAYLOAD_BYTES {
                return Err(format!(
                    "expected {MAX_AUDIO_PAYLOAD_BYTES} PCM bytes, got {}",
                    payload.len()
                ));
            }
            for (sample, bytes) in self.pcm.iter_mut().zip(payload.chunks_exact(2)) {
                *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
            }
            let written = unsafe {
                ffi::opus_encode(
                    self.inner.as_ptr(),
                    self.pcm.as_ptr(),
                    AUDIO_FRAME_SAMPLES as i32,
                    self.encoded.as_mut_ptr(),
                    MAX_OPUS_AUDIO_PAYLOAD_BYTES as i32,
                )
            };
            check_codec_result("opus_encode", written)?;
            let written = written as usize;
            if written == 0 || written > MAX_OPUS_AUDIO_PAYLOAD_BYTES {
                return Err("Opus produced an invalid packet length".to_owned());
            }
            Ok(self.encoded[..written].to_vec())
        }

        fn set_control(
            &mut self,
            request: u32,
            value: i32,
            operation: &'static str,
        ) -> Result<(), String> {
            let result =
                unsafe { ffi::opus_encoder_ctl(self.inner.as_ptr(), request as i32, value) };
            check_codec_result(operation, result)
        }
    }

    impl Drop for RemoteAudioEncoder {
        fn drop(&mut self) {
            unsafe { ffi::opus_encoder_destroy(self.inner.as_ptr()) };
        }
    }

    pub struct RemoteAudioDecoder {
        inner: NonNull<ffi::OpusDecoder>,
        pcm: [i16; AUDIO_FRAME_SAMPLES],
    }

    unsafe impl Send for RemoteAudioDecoder {}

    impl RemoteAudioDecoder {
        pub fn new() -> Result<Self, String> {
            let mut error = ffi::OPUS_OK as i32;
            let inner =
                unsafe { ffi::opus_decoder_create(AUDIO_SAMPLE_RATE as i32, 1, &mut error) };
            check_codec_result("opus_decoder_create", error)?;
            Ok(Self {
                inner: NonNull::new(inner)
                    .ok_or_else(|| "opus_decoder_create returned a null state".to_owned())?,
                pcm: [0; AUDIO_FRAME_SAMPLES],
            })
        }

        pub fn reset(&mut self) -> Result<(), String> {
            let result =
                unsafe { ffi::opus_decoder_ctl(self.inner.as_ptr(), ffi::OPUS_RESET_STATE as i32) };
            check_codec_result("OPUS_RESET_STATE", result)
        }

        pub fn decode(&mut self, payload: &[u8], fec: bool) -> Result<Vec<u8>, String> {
            if payload.len() > MAX_OPUS_AUDIO_PAYLOAD_BYTES {
                return Err("Opus packet exceeds the accepted audio limit".to_owned());
            }
            let decoded = unsafe {
                ffi::opus_decode(
                    self.inner.as_ptr(),
                    payload.as_ptr(),
                    payload.len() as i32,
                    self.pcm.as_mut_ptr(),
                    AUDIO_FRAME_SAMPLES as i32,
                    i32::from(fec),
                )
            };
            check_codec_result("opus_decode", decoded)?;
            let decoded = decoded as usize;
            if decoded != AUDIO_FRAME_SAMPLES {
                return Err(format!(
                    "expected {AUDIO_FRAME_SAMPLES} decoded samples, got {decoded}"
                ));
            }
            let mut bytes = Vec::with_capacity(MAX_AUDIO_PAYLOAD_BYTES);
            for sample in &self.pcm[..decoded] {
                bytes.extend_from_slice(&sample.to_le_bytes());
            }
            Ok(bytes)
        }
    }

    impl Drop for RemoteAudioDecoder {
        fn drop(&mut self) {
            unsafe { ffi::opus_decoder_destroy(self.inner.as_ptr()) };
        }
    }

    fn check_codec_result(operation: &'static str, code: i32) -> Result<(), String> {
        if code >= 0 {
            return Ok(());
        }
        let reason = unsafe {
            let pointer = ffi::opus_strerror(code);
            (!pointer.is_null()).then(|| CStr::from_ptr(pointer).to_string_lossy().into_owned())
        }
        .unwrap_or_else(|| "unknown libopus error".to_owned());
        Err(format!("{operation}: {reason} ({code})"))
    }
}

#[cfg(not(windows))]
mod opus_codec {
    pub struct RemoteAudioEncoder;

    impl RemoteAudioEncoder {
        pub fn new() -> Result<Self, String> {
            Err("Opus system audio is available only on Windows".to_owned())
        }

        pub fn encode_pcm_s16_le(&mut self, _payload: &[u8]) -> Result<Vec<u8>, String> {
            Err("Opus system audio is available only on Windows".to_owned())
        }
    }

    pub struct RemoteAudioDecoder;

    impl RemoteAudioDecoder {
        pub fn new() -> Result<Self, String> {
            Err("Opus system audio is available only on Windows".to_owned())
        }

        pub fn reset(&mut self) -> Result<(), String> {
            Err("Opus system audio is available only on Windows".to_owned())
        }

        pub fn decode(&mut self, _payload: &[u8], _fec: bool) -> Result<Vec<u8>, String> {
            Err("Opus system audio is available only on Windows".to_owned())
        }
    }
}

pub use opus_codec::{RemoteAudioDecoder, RemoteAudioEncoder};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapturedAudioFrame {
    pub capture_timestamp_us: u64,
    pub payload: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioCaptureState {
    Starting,
    Available,
    Unavailable,
}

#[derive(Debug)]
pub struct AudioFrameQueue {
    frames: VecDeque<CapturedAudioFrame>,
    capacity: usize,
}

impl AudioFrameQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
        }
    }

    pub fn push_drop_oldest(&mut self, frame: CapturedAudioFrame) {
        if self.frames.len() == self.capacity {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
    }

    pub fn drain(&mut self) -> Vec<CapturedAudioFrame> {
        self.frames.drain(..).collect()
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

impl Default for AudioFrameQueue {
    fn default() -> Self {
        Self::new(AUDIO_QUEUE_CAPACITY)
    }
}

pub fn run_loopback_capture(
    queue: Arc<Mutex<AudioFrameQueue>>,
    notify: Arc<Notify>,
    shutdown: Arc<AtomicBool>,
    enabled: Arc<AtomicBool>,
    state: watch::Sender<AudioCaptureState>,
) {
    while !shutdown.load(Ordering::Acquire) {
        state.send_replace(AudioCaptureState::Starting);
        let result = platform::capture_until_stopped(
            queue.clone(),
            notify.clone(),
            shutdown.clone(),
            enabled.clone(),
            state.clone(),
        );
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        state.send_replace(AudioCaptureState::Unavailable);
        if result.is_err() && wait_for_shutdown(&shutdown, CAPTURE_RETRY_DELAY) {
            break;
        }
    }
    state.send_replace(AudioCaptureState::Unavailable);
    if let Ok(mut queue) = queue.lock() {
        queue.clear();
    }
    notify.notify_waiters();
}

fn wait_for_shutdown(shutdown: &AtomicBool, duration: Duration) -> bool {
    let steps = duration.as_millis().div_ceil(100) as usize;
    for _ in 0..steps {
        if shutdown.load(Ordering::Acquire) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    shutdown.load(Ordering::Acquire)
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros()
        .try_into()
        .unwrap_or(u64::MAX)
        .max(1)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeSampleFormat {
    Unsigned8,
    Signed16,
    Signed24,
    Signed32,
    Float32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeAudioFormat {
    sample_rate: u32,
    channels: u16,
    block_align: u16,
    sample_format: NativeSampleFormat,
}

impl NativeAudioFormat {
    fn sample_bytes(self) -> usize {
        match self.sample_format {
            NativeSampleFormat::Unsigned8 => 1,
            NativeSampleFormat::Signed16 => 2,
            NativeSampleFormat::Signed24 => 3,
            NativeSampleFormat::Signed32 | NativeSampleFormat::Float32 => 4,
        }
    }

    fn validate(self) -> Result<Self, String> {
        let required = usize::from(self.channels)
            .checked_mul(self.sample_bytes())
            .ok_or_else(|| "audio channel layout overflowed".to_owned())?;
        if self.sample_rate == 0 || self.channels == 0 || usize::from(self.block_align) < required {
            return Err("audio mix format is invalid".to_owned());
        }
        Ok(self)
    }
}

struct AudioConverter {
    format: NativeAudioFormat,
    source: Vec<f32>,
    source_position: f64,
    pcm: Vec<i16>,
}

impl AudioConverter {
    fn new(format: NativeAudioFormat) -> Result<Self, String> {
        Ok(Self {
            format: format.validate()?,
            source: Vec::new(),
            source_position: 0.0,
            pcm: Vec::with_capacity(AUDIO_FRAME_SAMPLES * 2),
        })
    }

    fn reset(&mut self) {
        self.source.clear();
        self.pcm.clear();
        self.source_position = 0.0;
    }

    fn push_interleaved(
        &mut self,
        bytes: &[u8],
        frame_count: usize,
    ) -> Result<Vec<CapturedAudioFrame>, String> {
        let required = frame_count
            .checked_mul(usize::from(self.format.block_align))
            .ok_or_else(|| "audio buffer size overflowed".to_owned())?;
        if bytes.len() < required {
            return Err("audio buffer was shorter than its frame count".to_owned());
        }

        for frame in 0..frame_count {
            let base = frame * usize::from(self.format.block_align);
            let mut mono = 0.0_f32;
            for channel in 0..usize::from(self.format.channels) {
                let offset = base + channel * self.format.sample_bytes();
                mono += decode_sample(
                    self.format.sample_format,
                    &bytes[offset..offset + self.format.sample_bytes()],
                );
            }
            self.source.push(mono / f32::from(self.format.channels));
        }

        let step = f64::from(self.format.sample_rate) / f64::from(AUDIO_SAMPLE_RATE);
        while self.source_position + 1.0 < self.source.len() as f64 {
            let index = self.source_position.floor() as usize;
            let fraction = (self.source_position - index as f64) as f32;
            let sample =
                self.source[index] + (self.source[index + 1] - self.source[index]) * fraction;
            self.pcm
                .push((sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16);
            self.source_position += step;
        }

        let consumed = (self.source_position.floor() as usize).min(self.source.len());
        if consumed > 0 {
            self.source.drain(..consumed);
            self.source_position -= consumed as f64;
        }

        let mut frames = Vec::new();
        while self.pcm.len() >= AUDIO_FRAME_SAMPLES {
            let mut payload = Vec::with_capacity(AUDIO_FRAME_SAMPLES * 2);
            for sample in self.pcm.drain(..AUDIO_FRAME_SAMPLES) {
                payload.extend_from_slice(&sample.to_le_bytes());
            }
            frames.push(CapturedAudioFrame {
                capture_timestamp_us: now_micros(),
                payload,
            });
        }
        Ok(frames)
    }
}

fn decode_sample(format: NativeSampleFormat, bytes: &[u8]) -> f32 {
    match format {
        NativeSampleFormat::Unsigned8 => (f32::from(bytes[0]) - 128.0) / 128.0,
        NativeSampleFormat::Signed16 => {
            f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0
        }
        NativeSampleFormat::Signed24 => {
            let value = i32::from_le_bytes([
                bytes[0],
                bytes[1],
                bytes[2],
                if bytes[2] & 0x80 == 0 { 0 } else { 0xff },
            ]);
            value as f32 / 8_388_608.0
        }
        NativeSampleFormat::Signed32 => {
            i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32 / 2_147_483_648.0
        }
        NativeSampleFormat::Float32 => {
            f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]).clamp(-1.0, 1.0)
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        ffi::c_void,
        ptr, slice,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use tokio::sync::{Notify, watch};
    use windows::{
        Win32::{
            Media::Audio::{
                AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
                IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
                WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eConsole, eRender,
            },
            System::Com::{
                CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
        },
        core::{GUID, IUnknown},
    };

    use super::{
        AudioCaptureState, AudioConverter, AudioFrameQueue, NativeAudioFormat, NativeSampleFormat,
    };

    const WAVE_FORMAT_PCM: u16 = 1;
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xfffe;
    const KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
    const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID =
        GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

    pub(super) fn capture_until_stopped(
        queue: Arc<Mutex<AudioFrameQueue>>,
        notify: Arc<Notify>,
        shutdown: Arc<AtomicBool>,
        enabled: Arc<AtomicBool>,
        state: watch::Sender<AudioCaptureState>,
    ) -> Result<(), String> {
        let _com = ComApartment::initialize()?;
        let mut capture = LoopbackCapture::start()?;
        state.send_replace(AudioCaptureState::Available);
        let mut was_enabled = enabled.load(Ordering::Acquire);

        while !shutdown.load(Ordering::Acquire) {
            let is_enabled = enabled.load(Ordering::Acquire);
            if is_enabled != was_enabled {
                capture.converter.reset();
                if let Ok(mut queue) = queue.lock() {
                    queue.clear();
                }
                was_enabled = is_enabled;
            }

            let mut processed = false;
            loop {
                let packet_frames = unsafe { capture.capture.GetNextPacketSize() }
                    .map_err(|error| native_error("read audio packet size", error))?;
                if packet_frames == 0 {
                    break;
                }
                processed = true;

                let mut data = ptr::null_mut();
                let mut frames = 0_u32;
                let mut flags = 0_u32;
                unsafe {
                    capture
                        .capture
                        .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                }
                .map_err(|error| native_error("read loopback audio", error))?;

                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                let converted = if is_enabled && !silent && frames > 0 {
                    let byte_count = usize::try_from(frames)
                        .ok()
                        .and_then(|count| {
                            count.checked_mul(usize::from(capture.format.block_align))
                        })
                        .ok_or_else(|| "audio packet size overflowed".to_owned())?;
                    let bytes = unsafe { slice::from_raw_parts(data, byte_count) };
                    capture.converter.push_interleaved(bytes, frames as usize)
                } else {
                    capture.converter.reset();
                    Ok(Vec::new())
                };

                let release = unsafe { capture.capture.ReleaseBuffer(frames) }
                    .map_err(|error| native_error("release loopback audio", error));
                let converted = converted?;
                release?;

                if !converted.is_empty() {
                    if let Ok(mut queue) = queue.lock() {
                        for frame in converted {
                            queue.push_drop_oldest(frame);
                        }
                    }
                    notify.notify_one();
                }
            }

            if !processed {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        Ok(())
    }

    struct ComApartment;

    impl ComApartment {
        fn initialize() -> Result<Self, String> {
            let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            result
                .ok()
                .map_err(|error| native_error("initialize audio COM apartment", error))?;
            Ok(Self)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    struct MixFormat(*mut WAVEFORMATEX);

    impl Drop for MixFormat {
        fn drop(&mut self) {
            unsafe { CoTaskMemFree(Some(self.0.cast::<c_void>())) };
        }
    }

    struct LoopbackCapture {
        client: IAudioClient,
        capture: IAudioCaptureClient,
        format: NativeAudioFormat,
        converter: AudioConverter,
    }

    impl LoopbackCapture {
        fn start() -> Result<Self, String> {
            let enumerator: IMMDeviceEnumerator =
                unsafe { CoCreateInstance(&MMDeviceEnumerator, None::<&IUnknown>, CLSCTX_ALL) }
                    .map_err(|error| native_error("open Windows audio devices", error))?;
            let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
                .map_err(|error| native_error("open default Windows output", error))?;
            let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
                .map_err(|error| native_error("activate Windows output capture", error))?;
            let mix_format = MixFormat(
                unsafe { client.GetMixFormat() }
                    .map_err(|error| native_error("read Windows output format", error))?,
            );
            let format = unsafe { parse_mix_format(mix_format.0) }?;
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    1_000_000,
                    0,
                    mix_format.0,
                    None,
                )
            }
            .map_err(|error| native_error("initialize Windows loopback capture", error))?;
            let capture: IAudioCaptureClient = unsafe { client.GetService() }
                .map_err(|error| native_error("open Windows loopback buffer", error))?;
            unsafe { client.Start() }
                .map_err(|error| native_error("start Windows loopback capture", error))?;
            Ok(Self {
                client,
                capture,
                format,
                converter: AudioConverter::new(format)?,
            })
        }
    }

    impl Drop for LoopbackCapture {
        fn drop(&mut self) {
            let _ = unsafe { self.client.Stop() };
        }
    }

    unsafe fn parse_mix_format(format: *const WAVEFORMATEX) -> Result<NativeAudioFormat, String> {
        if format.is_null() {
            return Err("Windows returned an empty audio mix format".to_owned());
        }
        let format_tag = unsafe { ptr::read_unaligned(ptr::addr_of!((*format).wFormatTag)) };
        let channels = unsafe { ptr::read_unaligned(ptr::addr_of!((*format).nChannels)) };
        let sample_rate = unsafe { ptr::read_unaligned(ptr::addr_of!((*format).nSamplesPerSec)) };
        let block_align = unsafe { ptr::read_unaligned(ptr::addr_of!((*format).nBlockAlign)) };
        let bits = unsafe { ptr::read_unaligned(ptr::addr_of!((*format).wBitsPerSample)) };
        let sample_format = match format_tag {
            WAVE_FORMAT_PCM => pcm_sample_format(bits)?,
            WAVE_FORMAT_IEEE_FLOAT if bits == 32 => NativeSampleFormat::Float32,
            WAVE_FORMAT_EXTENSIBLE => {
                let extensible = format.cast::<WAVEFORMATEXTENSIBLE>();
                let sub_format =
                    unsafe { ptr::read_unaligned(ptr::addr_of!((*extensible).SubFormat)) };
                if sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT && bits == 32 {
                    NativeSampleFormat::Float32
                } else if sub_format == KSDATAFORMAT_SUBTYPE_PCM {
                    pcm_sample_format(bits)?
                } else {
                    return Err("Windows output uses an unsupported audio sample format".to_owned());
                }
            }
            _ => return Err("Windows output uses an unsupported audio mix format".to_owned()),
        };
        NativeAudioFormat {
            sample_rate,
            channels,
            block_align,
            sample_format,
        }
        .validate()
    }

    fn pcm_sample_format(bits: u16) -> Result<NativeSampleFormat, String> {
        match bits {
            8 => Ok(NativeSampleFormat::Unsigned8),
            16 => Ok(NativeSampleFormat::Signed16),
            24 => Ok(NativeSampleFormat::Signed24),
            32 => Ok(NativeSampleFormat::Signed32),
            _ => Err("Windows output uses an unsupported PCM bit depth".to_owned()),
        }
    }

    fn native_error(operation: &str, error: windows::core::Error) -> String {
        format!("{operation}: {error}")
    }
}

#[cfg(not(windows))]
mod platform {
    use std::sync::{Arc, Mutex, atomic::AtomicBool};

    use tokio::sync::{Notify, watch};

    use super::{AudioCaptureState, AudioFrameQueue};

    pub(super) fn capture_until_stopped(
        _queue: Arc<Mutex<AudioFrameQueue>>,
        _notify: Arc<Notify>,
        _shutdown: Arc<AtomicBool>,
        _enabled: Arc<AtomicBool>,
        _state: watch::Sender<AudioCaptureState>,
    ) -> Result<(), String> {
        Err("system audio capture is available only on Windows".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_drops_oldest_audio_instead_of_accumulating_latency() {
        let mut queue = AudioFrameQueue::new(2);
        for marker in [1_u8, 2, 3] {
            queue.push_drop_oldest(CapturedAudioFrame {
                capture_timestamp_us: u64::from(marker),
                payload: vec![marker],
            });
        }
        assert_eq!(
            queue
                .drain()
                .into_iter()
                .map(|frame| frame.payload[0])
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn stereo_float_is_downmixed_into_fixed_ten_millisecond_packets() {
        let format = NativeAudioFormat {
            sample_rate: AUDIO_SAMPLE_RATE,
            channels: 2,
            block_align: 8,
            sample_format: NativeSampleFormat::Float32,
        };
        let mut converter = AudioConverter::new(format).unwrap();
        let mut bytes = Vec::new();
        for _ in 0..=AUDIO_FRAME_SAMPLES {
            bytes.extend_from_slice(&0.75_f32.to_le_bytes());
            bytes.extend_from_slice(&0.25_f32.to_le_bytes());
        }
        let frames = converter
            .push_interleaved(&bytes, AUDIO_FRAME_SAMPLES + 1)
            .unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload.len(), AUDIO_FRAME_SAMPLES * 2);
        let first = i16::from_le_bytes([frames[0].payload[0], frames[0].payload[1]]);
        assert!((first - 16_384).abs() <= 1);
    }

    #[test]
    fn resampler_converts_44100_hz_to_48000_hz_without_unbounded_buffering() {
        let format = NativeAudioFormat {
            sample_rate: 44_100,
            channels: 1,
            block_align: 2,
            sample_format: NativeSampleFormat::Signed16,
        };
        let mut converter = AudioConverter::new(format).unwrap();
        let bytes = vec![0_u8; 4_412 * 2];
        let frames = converter.push_interleaved(&bytes, 4_412).unwrap();
        assert_eq!(frames.len(), 10);
        assert!(converter.source.len() <= 2);
        assert!(converter.pcm.len() < AUDIO_FRAME_SAMPLES);
    }

    #[test]
    fn pcm_sample_decoders_preserve_sign_and_silence() {
        assert_eq!(decode_sample(NativeSampleFormat::Unsigned8, &[128]), 0.0);
        assert!(decode_sample(NativeSampleFormat::Signed16, &[0, 128]) <= -1.0);
        assert!(decode_sample(NativeSampleFormat::Signed24, &[0, 0, 128]) <= -1.0);
        assert!(decode_sample(NativeSampleFormat::Signed32, &[0, 0, 0, 128]) <= -1.0);
    }

    #[cfg(windows)]
    #[test]
    fn opus_round_trip_is_fixed_duration_and_far_smaller_than_pcm() {
        let mut encoder = RemoteAudioEncoder::new().expect("create encoder");
        let mut decoder = RemoteAudioDecoder::new().expect("create decoder");
        let mut encoded_bytes = 0_usize;
        let mut decoded_energy = 0_i64;

        for frame_index in 0..24 {
            let mut pcm = Vec::with_capacity(MAX_AUDIO_PAYLOAD_BYTES);
            for sample_index in 0..AUDIO_FRAME_SAMPLES {
                let elapsed = (frame_index * AUDIO_FRAME_SAMPLES + sample_index) as f32
                    / AUDIO_SAMPLE_RATE as f32;
                let sample = (elapsed * 440.0 * std::f32::consts::TAU).sin() * 8_000.0;
                pcm.extend_from_slice(&(sample as i16).to_le_bytes());
            }
            let encoded = encoder.encode_pcm_s16_le(&pcm).expect("encode frame");
            assert!(encoded.len() <= MAX_OPUS_AUDIO_PAYLOAD_BYTES);
            encoded_bytes += encoded.len();

            let decoded = decoder.decode(&encoded, false).expect("decode frame");
            assert_eq!(decoded.len(), MAX_AUDIO_PAYLOAD_BYTES);
            if frame_index >= 2 {
                decoded_energy += decoded
                    .chunks_exact(2)
                    .map(|bytes| {
                        i64::from(i32::from(i16::from_le_bytes([bytes[0], bytes[1]])).abs())
                    })
                    .sum::<i64>();
            }
        }

        assert!(encoded_bytes < 24 * MAX_AUDIO_PAYLOAD_BYTES / 4);
        assert!(decoded_energy > 0);
    }

    #[cfg(windows)]
    #[test]
    fn opus_decoder_can_recover_one_missing_frame_with_inband_fec() {
        let mut encoder = RemoteAudioEncoder::new().expect("create encoder");
        let mut decoder = RemoteAudioDecoder::new().expect("create decoder");
        let silence = vec![0_u8; MAX_AUDIO_PAYLOAD_BYTES];
        let first = encoder.encode_pcm_s16_le(&silence).expect("encode first");
        let second = encoder.encode_pcm_s16_le(&silence).expect("encode second");

        decoder.decode(&first, false).expect("prime decoder");
        assert_eq!(
            decoder.decode(&second, true).expect("decode fec").len(),
            MAX_AUDIO_PAYLOAD_BYTES
        );
        assert_eq!(
            decoder
                .decode(&second, false)
                .expect("decode current")
                .len(),
            MAX_AUDIO_PAYLOAD_BYTES
        );
    }
}
