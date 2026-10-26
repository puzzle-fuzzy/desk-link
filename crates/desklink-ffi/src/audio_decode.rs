//! Bounded remote playback decoding. Audio failures stay independent of video.
use std::{ptr::NonNull, time::Instant};

use desklink_protocol::{
    AUDIO_CHANNELS, AUDIO_FRAME_SAMPLES, AUDIO_SAMPLE_RATE, AudioCodec, AudioPacket,
    MAX_AUDIO_PAYLOAD_BYTES, MAX_OPUS_AUDIO_PAYLOAD_BYTES,
};
use opus_head_sys as opus;

#[derive(Default)]
pub(crate) struct AudioDecoder {
    stream_id: u64,
    enabled: bool,
    last_sequence: Option<u64>,
    timing_anchor: Option<(u64, Instant)>,
    opus: Option<OpusDecoder>,
}

impl AudioDecoder {
    pub fn set_stream(&mut self, stream_id: u64) {
        if self.stream_id != stream_id {
            self.stream_id = stream_id;
            self.last_sequence = None;
            self.timing_anchor = None;
            self.opus = None;
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            self.opus = None;
        }
    }

    pub fn decode(&mut self, packet: &AudioPacket) -> Result<Option<Vec<u8>>, &'static str> {
        if self.stream_id == 0 || packet.stream_id != self.stream_id {
            return Ok(None);
        }
        if self
            .last_sequence
            .is_some_and(|last| packet.sequence <= last)
        {
            return Ok(None);
        }
        if packet.sample_rate != AUDIO_SAMPLE_RATE || packet.channels != AUDIO_CHANNELS {
            return Err("remote audio format is unsupported");
        }
        if self
            .last_sequence
            .is_some_and(|last| packet.sequence != last.saturating_add(1))
        {
            // Prefer the newest 10 ms frame to unbounded concealment/backlog.
            self.opus = None;
        }
        self.last_sequence = Some(packet.sequence);
        let now = Instant::now();
        if let Some((capture, received)) = self.timing_anchor {
            // Relative timing avoids depending on synchronized system clocks.
            // An old network burst must not play seconds after capture.
            let elapsed = now.duration_since(received).as_micros();
            let captured = packet.capture_timestamp_us.saturating_sub(capture) as u128;
            if elapsed > captured.saturating_add(500_000) {
                self.opus = None;
                return Ok(None);
            }
        } else {
            self.timing_anchor = Some((packet.capture_timestamp_us, now));
        }
        if !self.enabled {
            return Ok(None);
        }
        match packet.codec {
            AudioCodec::PcmS16Le if packet.payload.len() == MAX_AUDIO_PAYLOAD_BYTES => {
                // A codec change must not retain a previous Opus predictor.
                self.opus = None;
                Ok(Some(packet.payload.clone()))
            }
            AudioCodec::PcmS16Le => Err("remote PCM frame has an invalid size"),
            AudioCodec::Opus => {
                if packet.payload.is_empty() || packet.payload.len() > MAX_OPUS_AUDIO_PAYLOAD_BYTES
                {
                    return Err("remote Opus frame has an invalid size");
                }
                if self.opus.is_none() {
                    self.opus = Some(OpusDecoder::new()?);
                }
                match self.opus.as_mut().unwrap().decode(&packet.payload) {
                    Ok(pcm) => Ok(Some(pcm)),
                    Err(error) => {
                        self.opus = None;
                        Err(error)
                    }
                }
            }
        }
    }
}

struct OpusDecoder(NonNull<opus::OpusDecoder>);

// The worker exclusively accesses each codec through &mut self. Opus permits
// independent states to move between threads; there is no concurrent access.
unsafe impl Send for OpusDecoder {}

impl OpusDecoder {
    fn new() -> Result<Self, &'static str> {
        let mut error = 0;
        let state = unsafe {
            opus::opus_decoder_create(AUDIO_SAMPLE_RATE as i32, AUDIO_CHANNELS as i32, &mut error)
        };
        let state = NonNull::new(state).ok_or("remote audio decoder could not start")?;
        if error != opus::OPUS_OK as i32 {
            unsafe { opus::opus_decoder_destroy(state.as_ptr()) };
            return Err("remote audio decoder could not start");
        }
        Ok(Self(state))
    }

    fn decode(&mut self, payload: &[u8]) -> Result<Vec<u8>, &'static str> {
        let samples = unsafe {
            opus::opus_packet_get_nb_samples(
                payload.as_ptr(),
                payload.len() as i32,
                AUDIO_SAMPLE_RATE as i32,
            )
        };
        if samples != AUDIO_FRAME_SAMPLES as i32 {
            return Err("remote Opus frame has an invalid duration");
        }
        let mut pcm = [0_i16; AUDIO_FRAME_SAMPLES];
        let decoded = unsafe {
            opus::opus_decode(
                self.0.as_ptr(),
                payload.as_ptr(),
                payload.len() as i32,
                pcm.as_mut_ptr(),
                AUDIO_FRAME_SAMPLES as i32,
                0,
            )
        };
        if decoded != AUDIO_FRAME_SAMPLES as i32 {
            return Err("remote Opus frame could not be decoded");
        }
        Ok(pcm.into_iter().flat_map(i16::to_le_bytes).collect())
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        unsafe { opus::opus_decoder_destroy(self.0.as_ptr()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(sequence: u64, codec: AudioCodec, payload: Vec<u8>) -> AudioPacket {
        AudioPacket {
            protocol_version: desklink_protocol::PROTOCOL_VERSION,
            stream_id: 7,
            sequence,
            capture_timestamp_us: sequence * 10_000,
            codec,
            sample_rate: AUDIO_SAMPLE_RATE,
            channels: AUDIO_CHANNELS,
            payload,
        }
    }

    fn opus_packet() -> Vec<u8> {
        let mut error = 0;
        let encoder = unsafe {
            opus::opus_encoder_create(
                AUDIO_SAMPLE_RATE as i32,
                1,
                opus::OPUS_APPLICATION_AUDIO as i32,
                &mut error,
            )
        };
        assert_eq!(error, 0);
        assert!(!encoder.is_null());
        let pcm = std::array::from_fn::<_, AUDIO_FRAME_SAMPLES, _>(|index| {
            ((index as f64 * std::f64::consts::TAU * 440.0 / 48_000.0).sin() * 12_000.0) as i16
        });
        let mut payload = vec![0; MAX_OPUS_AUDIO_PAYLOAD_BYTES];
        let bytes = unsafe {
            opus::opus_encode(
                encoder,
                pcm.as_ptr(),
                AUDIO_FRAME_SAMPLES as i32,
                payload.as_mut_ptr(),
                payload.len() as i32,
            )
        };
        unsafe { opus::opus_encoder_destroy(encoder) };
        assert!(bytes > 0);
        payload.truncate(bytes as usize);
        payload
    }

    #[test]
    fn actual_opus_frames_decode_to_fixed_nonzero_pcm_and_recover_after_a_gap() {
        let mut decoder = AudioDecoder::default();
        decoder.set_stream(7);
        decoder.set_enabled(true);
        for sequence in [1, 2, 10] {
            let pcm = decoder
                .decode(&packet(sequence, AudioCodec::Opus, opus_packet()))
                .unwrap()
                .unwrap();
            assert_eq!(pcm.len(), MAX_AUDIO_PAYLOAD_BYTES);
            assert!(pcm.iter().any(|byte| *byte != 0));
        }
    }

    #[test]
    fn muted_wrong_stream_and_out_of_order_audio_never_reaches_playback() {
        let mut decoder = AudioDecoder::default();
        let frame = packet(9, AudioCodec::PcmS16Le, vec![1; MAX_AUDIO_PAYLOAD_BYTES]);
        decoder.set_stream(7);
        assert_eq!(decoder.decode(&frame).unwrap(), None);
        decoder.set_enabled(true);
        let frame = packet(10, frame.codec, frame.payload);
        assert_eq!(decoder.decode(&frame).unwrap().unwrap(), frame.payload);
        assert_eq!(decoder.decode(&frame).unwrap(), None);
        assert_eq!(
            decoder
                .decode(&packet(8, frame.codec, frame.payload.clone()))
                .unwrap(),
            None
        );
        decoder.set_stream(8);
        assert_eq!(decoder.decode(&frame).unwrap(), None);
        decoder.set_stream(7);
        assert!(decoder.decode(&frame).unwrap().is_some());
        decoder.set_enabled(false);
        assert_eq!(decoder.decode(&frame).unwrap(), None);
        decoder.set_enabled(true);
        assert_eq!(
            decoder.decode(&frame).unwrap(),
            None,
            "mute cannot reset the sequence replay floor"
        );
        decoder.timing_anchor = Some((
            frame.capture_timestamp_us,
            Instant::now() - std::time::Duration::from_secs(2),
        ));
        assert_eq!(
            decoder
                .decode(&packet(11, frame.codec, frame.payload))
                .unwrap(),
            None,
            "a delayed network burst must not play old sound"
        );
    }

    #[test]
    fn malformed_audio_is_bounded_and_valid_audio_can_resume() {
        let mut decoder = AudioDecoder::default();
        decoder.set_stream(7);
        decoder.set_enabled(true);
        assert!(
            decoder
                .decode(&packet(1, AudioCodec::PcmS16Le, vec![0; 2]))
                .is_err()
        );
        assert!(
            decoder
                .decode(&packet(2, AudioCodec::Opus, vec![0; 513]))
                .is_err()
        );
        assert!(
            decoder
                .decode(&packet(3, AudioCodec::Opus, vec![0xff]))
                .is_err()
        );
        let mut wrong = packet(4, AudioCodec::PcmS16Le, vec![0; MAX_AUDIO_PAYLOAD_BYTES]);
        wrong.channels = 2;
        assert!(decoder.decode(&wrong).is_err());
        assert!(
            decoder
                .decode(&packet(5, AudioCodec::Opus, opus_packet()))
                .unwrap()
                .is_some()
        );
    }
}
