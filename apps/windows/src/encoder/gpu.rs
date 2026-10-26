use std::time::Instant;

use windows::{Win32::Graphics::Direct3D11::ID3D11Device, core::Interface};

use super::{
    CapturedFrame, EncodedFrame, EncoderBackendPreference, EncoderError, EncoderPerformanceSample,
    GpuEncoderFailure, H264Encoder, H264Profile, elapsed_us, fit_h264_dimensions_with_limit,
    gpu_sample, native, next_config_version, validate_output_timestamp,
};
use crate::gpu::{GpuBgraFrame, GpuError, GpuNv12Pool};

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct GpuEncoderStatus {
    pub surface_backend_active: bool,
    /// Last GPU binding; current only while surface_backend_active is true.
    pub adapter_luid: Option<u64>,
    pub selected_mft: Option<String>,
    pub manager_accepted: Option<bool>,
    pub color_matrix: Option<u32>,
    pub nominal_range: Option<u32>,
    pub surface_samples_accepted: u64,
    pub encoded_outputs: u64,
    pub cpu_fallbacks: u64,
    pub last_failure: Option<GpuEncoderFailure>,
}

pub(super) struct GpuInputState {
    device: ID3D11Device,
    source: (u32, u32),
    target: (u32, u32),
    pool: GpuNv12Pool,
}

impl GpuInputState {
    fn matches(&self, frame: &GpuBgraFrame, target: (u32, u32)) -> bool {
        self.device.as_raw() == frame.device().as_raw()
            && self.source == frame.dimensions()
            && self.target == target
    }
}

impl H264Encoder {
    pub fn wants_gpu_input(&self) -> bool {
        self.backend_preference == EncoderBackendPreference::PreferHardware && !self.gpu_failed
    }

    pub fn gpu_status(&self) -> &GpuEncoderStatus {
        &self.gpu_status
    }

    /// The original owned BGRA surface stays alive for a same-frame CPU retry.
    /// Only a successful D3D11 MFT produces gpu_surface_input=true samples.
    pub fn encode_gpu(
        &mut self,
        frame: &GpuBgraFrame,
        force_keyframe: bool,
    ) -> Result<EncodedFrame, EncoderError> {
        if !self.wants_gpu_input() {
            return self.encode(readback(frame)?, force_keyframe);
        }
        match self.encode_gpu_inner(frame, force_keyframe) {
            result @ (Ok(_)
            | Err(EncoderError::NeedMoreInput)
            | Err(EncoderError::ConfigVersionExhausted)) => result,
            Err(error) => {
                let failure = match &error {
                    EncoderError::GpuOperation(failure) => failure.clone(),
                    _ => GpuEncoderFailure {
                        stage: "gpu_input_or_output",
                        hresult: None,
                    },
                };
                self.gpu_status.last_failure = Some(failure);
                self.switch_to_software("gpu_surface_failure", true)?;
                self.gpu_input = None;
                self.gpu_status.cpu_fallbacks = self.gpu_status.cpu_fallbacks.saturating_add(1);
                self.encode(readback(frame)?, true)
            }
        }
    }

    fn encode_gpu_inner(
        &mut self,
        frame: &GpuBgraFrame,
        mut force_keyframe: bool,
    ) -> Result<EncodedFrame, EncoderError> {
        self.last_performance_sample = None;
        let submitted_at = Instant::now();
        let target = fit_h264_dimensions_with_limit(
            frame.width(),
            frame.height(),
            self.max_width,
            self.max_height,
        )?;
        if !self.backend.uses_gpu_surfaces()
            || !self
                .gpu_input
                .as_ref()
                .is_some_and(|input| input.matches(frame, target))
        {
            let config_version = next_config_version(self.config_version)?;
            let pool = GpuNv12Pool::from_device(
                frame.device().clone(),
                frame.width(),
                frame.height(),
                target.0,
                target.1,
                18,
            )
            .map_err(gpu_failure)?;
            let (backend, profile) = match native::MediaFoundationEncoder::for_gpu(
                frame.device(),
                target.0,
                target.1,
                self.fps,
                self.bitrate,
                self.profile,
            ) {
                Ok(backend) => (backend, self.profile),
                Err(_) if self.profile == H264Profile::High => (
                    native::MediaFoundationEncoder::for_gpu(
                        frame.device(),
                        target.0,
                        target.1,
                        self.fps,
                        self.bitrate,
                        H264Profile::Main,
                    )?,
                    H264Profile::Main,
                ),
                Err(error) => return Err(error),
            };
            self.gpu_status.adapter_luid = backend.gpu_adapter_luid();
            self.gpu_status.selected_mft = Some(backend.info().name.clone());
            self.gpu_status.manager_accepted = Some(true);
            self.gpu_status.surface_backend_active = true;
            (self.gpu_status.color_matrix, self.gpu_status.nominal_range) =
                backend.color_metadata();
            self.backend = backend;
            self.profile = profile;
            self.width = target.0;
            self.height = target.1;
            self.config_version = config_version;
            self.pending_frames.clear();
            self.gpu_input = Some(GpuInputState {
                device: frame.device().clone(),
                source: frame.dimensions(),
                target,
                pool,
            });
            force_keyframe = true;
        }
        let lease = self
            .gpu_input
            .as_ref()
            .expect("GPU pool configured")
            .pool
            .convert(frame)
            .map_err(gpu_failure)?;
        let texture = lease.texture().clone();
        let sample = gpu_sample::tracked_nv12_sample(
            &texture,
            lease.subresource_index(),
            frame.timestamp_us(),
            self.fps,
            force_keyframe,
            lease,
        )?;
        let convert_us = elapsed_us(submitted_at);
        let mft_started = Instant::now();
        let output = self.backend.encode_surface(&sample, force_keyframe);
        if self.backend.last_input_accepted() {
            self.gpu_status.surface_samples_accepted =
                self.gpu_status.surface_samples_accepted.saturating_add(1);
        }
        drop(sample); // TrackedSample still owns lease while MFT holds any reference.
        let output = output?;
        if let Some(output) = &output {
            validate_output_timestamp(
                output.timestamp_us,
                frame.timestamp_us(),
                &self.pending_frames,
                true,
            )?;
            self.gpu_status.encoded_outputs = self.gpu_status.encoded_outputs.saturating_add(1);
            (self.gpu_status.color_matrix, self.gpu_status.nominal_range) =
                self.backend.color_metadata();
        }
        let next_frame_id = self.frame_id.wrapping_add(1).max(1);
        self.finish_submission(
            frame.timestamp_us(),
            submitted_at,
            next_frame_id,
            output,
            EncoderPerformanceSample {
                frame_id: next_frame_id,
                output_frame_id: None,
                config_version: self.config_version,
                convert_us,
                mft_us: elapsed_us(mft_started),
                output_residence_us: None,
                pending_frames: 0,
                nv12_written_bytes: 0,
                nv12_buffer_grew: false,
                mf_input_copy_bytes: 0,
                gpu_surface_input: true,
            },
        )
    }

    pub(super) fn ensure_cpu_input_mode(&mut self) -> Result<bool, EncoderError> {
        if !self.backend.uses_gpu_surfaces() {
            return Ok(false);
        }
        self.switch_to_software("cpu_input_after_gpu", false)?;
        Ok(true)
    }

    fn switch_to_software(
        &mut self,
        reason: &'static str,
        latch: bool,
    ) -> Result<(), EncoderError> {
        let config_version = next_config_version(self.config_version)?;
        let previous = self.backend.info().clone();
        let (mut backend, profile) = match native::MediaFoundationEncoder::software(
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
        backend.record_fallback(previous, reason);
        self.backend = backend;
        self.gpu_status.surface_backend_active = false;
        self.gpu_input = None;
        self.profile = profile;
        self.config_version = config_version;
        self.pending_frames.clear();
        self.last_performance_sample = None;
        if latch {
            self.gpu_failed = true;
            self.backend_preference = EncoderBackendPreference::SoftwareOnly;
        }
        Ok(())
    }
}

fn gpu_failure(error: GpuError) -> EncoderError {
    let failure = match error {
        GpuError::Native { stage, code } => GpuEncoderFailure {
            stage,
            hresult: Some(code.0),
        },
        GpuError::Busy => GpuEncoderFailure {
            stage: "gpu_pool_busy",
            hresult: None,
        },
        GpuError::Timeout => GpuEncoderFailure {
            stage: "gpu_completion_timeout",
            hresult: None,
        },
        GpuError::DeviceMismatch => GpuEncoderFailure {
            stage: "gpu_device_mismatch",
            hresult: None,
        },
        GpuError::InvalidDimensions => GpuEncoderFailure {
            stage: "gpu_dimensions",
            hresult: None,
        },
        GpuError::Unsupported(stage) => GpuEncoderFailure {
            stage,
            hresult: None,
        },
    };
    EncoderError::GpuOperation(failure)
}

fn readback(frame: &GpuBgraFrame) -> Result<CapturedFrame, EncoderError> {
    let pixels = frame
        .readback_bgra()
        .map_err(|error| EncoderError::Native(format!("GPU fallback readback failed: {error}")))?;
    Ok(CapturedFrame {
        width: frame.width(),
        height: frame.height(),
        timestamp_us: frame.timestamp_us(),
        pixels,
    })
}
