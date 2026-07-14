use crate::capture::CapturedFrame;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EncodedFrame {
    pub frame_id: u64,
    pub config_version: u32,
    pub keyframe: bool,
    pub timestamp_us: u64,
    pub access_unit: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EncoderError {
    InvalidDimensions,
    FrameTooLarge,
    BackendUnavailable,
}

pub struct H264Encoder {
    width: u32,
    height: u32,
    fps: u32,
    frame_id: u64,
    config_version: u32,
    #[cfg(windows)]
    media_foundation_started: bool,
}

impl H264Encoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, EncoderError> {
        if width == 0 || height == 0 || width > 1920 || height > 1080 || fps == 0 {
            return Err(EncoderError::InvalidDimensions);
        }
        #[cfg(windows)]
        let media_foundation_started =
            unsafe { windows::Win32::Media::MediaFoundation::MFStartup(0x0002_0070, 0).is_ok() };
        #[cfg(windows)]
        if !media_foundation_started {
            return Err(EncoderError::BackendUnavailable);
        }

        Ok(Self {
            width,
            height,
            fps,
            frame_id: 0,
            config_version: 1,
            #[cfg(windows)]
            media_foundation_started,
        })
    }

    pub fn encode(
        &mut self,
        frame: CapturedFrame,
        force_keyframe: bool,
    ) -> Result<EncodedFrame, EncoderError> {
        if frame.width != self.width || frame.height != self.height {
            self.rebuild(frame.width, frame.height)?;
        }
        if frame.pixels.len() > 1920 * 1080 * 4 {
            return Err(EncoderError::FrameTooLarge);
        }
        self.frame_id = self.frame_id.wrapping_add(1).max(1);
        let keyframe =
            force_keyframe || self.frame_id == 1 || self.frame_id % u64::from(self.fps) == 0;
        Ok(EncodedFrame {
            frame_id: self.frame_id,
            config_version: self.config_version,
            keyframe,
            timestamp_us: frame.timestamp_us,
            access_unit: frame.pixels,
        })
    }

    pub fn rebuild(&mut self, width: u32, height: u32) -> Result<(), EncoderError> {
        if width == 0 || height == 0 || width > 1920 || height > 1080 {
            return Err(EncoderError::InvalidDimensions);
        }
        self.width = width;
        self.height = height;
        self.config_version = self.config_version.wrapping_add(1).max(1);
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for H264Encoder {
    fn drop(&mut self) {
        if self.media_foundation_started {
            let _ = unsafe { windows::Win32::Media::MediaFoundation::MFShutdown() };
        }
    }
}
