use super::EncoderError;

pub(super) trait BitrateProperties {
    fn can_modify(&self) -> bool;
    fn read(&self) -> Result<(u32, u32), EncoderError>;
    fn set_mean(&mut self, value: u32) -> Result<(), EncoderError>;
    fn set_max(&mut self, value: u32) -> Result<(), EncoderError>;
}

/// Commit only an exact native readback. A rejected or rounded request is
/// reported as unsupported only after both previous values are verified.
pub(super) fn try_apply(
    properties: &mut impl BitrateProperties,
    bitrate: u32,
) -> Result<bool, EncoderError> {
    if bitrate == 0 {
        return Err(EncoderError::InvalidDimensions);
    }
    if !properties.can_modify() {
        return Ok(false);
    }
    let Ok(previous) = properties.read() else {
        return Ok(false);
    };
    let peak = (u64::from(bitrate) * 3 / 2).min(u64::from(u32::MAX)) as u32;
    let requested = (bitrate, peak);
    if previous == requested {
        return Ok(true);
    }
    // Keep peak >= mean while moving either upwards or downwards.
    let write = if bitrate >= previous.0 {
        properties
            .set_max(peak)
            .and_then(|()| properties.set_mean(bitrate))
    } else {
        properties
            .set_mean(bitrate)
            .and_then(|()| properties.set_max(peak))
    };
    if write.is_ok() && properties.read().is_ok_and(|actual| actual == requested) {
        return Ok(true);
    }
    // Driver failures may follow a partial write. Attempt all restores even if
    // one fails; repeating peak after mean covers either dependency ordering.
    let _ = properties.set_max(previous.1);
    let _ = properties.set_mean(previous.0);
    let _ = properties.set_max(previous.1);
    if properties.read().is_ok_and(|actual| actual == previous) {
        Ok(false)
    } else {
        Err(EncoderError::Native(
            "dynamic bitrate rollback could not be verified".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Codec {
        mean: u32,
        max: u32,
        supported: bool,
        reject_mean_once: bool,
        reject_peak_once: bool,
        reject_restore: bool,
        clamp: bool,
        writes: Vec<(&'static str, u32)>,
        read_count: Cell<u32>,
        fail_first_readback: bool,
    }

    impl Default for Codec {
        fn default() -> Self {
            Self {
                mean: 10_000,
                max: 15_000,
                supported: true,
                reject_mean_once: false,
                reject_peak_once: false,
                reject_restore: false,
                clamp: false,
                writes: Vec::new(),
                read_count: Cell::new(0),
                fail_first_readback: false,
            }
        }
    }

    impl BitrateProperties for Codec {
        fn can_modify(&self) -> bool {
            self.supported
        }
        fn read(&self) -> Result<(u32, u32), EncoderError> {
            self.read_count.set(self.read_count.get() + 1);
            if self.fail_first_readback && self.read_count.get() == 2 {
                return Err(EncoderError::BackendUnavailable);
            }
            Ok((self.mean, self.max))
        }
        fn set_mean(&mut self, value: u32) -> Result<(), EncoderError> {
            self.writes.push(("mean", value));
            if self.reject_restore && value == 10_000 {
                return Err(EncoderError::BackendUnavailable);
            }
            self.mean = if self.clamp && value != 10_000 {
                value - 1
            } else {
                value
            };
            if std::mem::take(&mut self.reject_mean_once) {
                // A failing driver can already have changed the property.
                return Err(EncoderError::BackendUnavailable);
            }
            Ok(())
        }
        fn set_max(&mut self, value: u32) -> Result<(), EncoderError> {
            self.writes.push(("max", value));
            self.max = value;
            if std::mem::take(&mut self.reject_peak_once) {
                return Err(EncoderError::BackendUnavailable);
            }
            Ok(())
        }
    }

    #[test]
    fn commits_only_exact_readback_and_orders_peak_and_mean_safely() {
        let mut codec = Codec::default();
        assert_eq!(try_apply(&mut codec, 20_000), Ok(true));
        assert_eq!((codec.mean, codec.max), (20_000, 30_000));
        assert_eq!(&codec.writes[..2], &[("max", 30_000), ("mean", 20_000)]);
        codec.writes.clear();
        assert_eq!(try_apply(&mut codec, 5_000), Ok(true));
        assert_eq!(&codec.writes[..2], &[("mean", 5_000), ("max", 7_500)]);
    }

    #[test]
    fn unsupported_and_zero_requests_never_write_properties() {
        let mut codec = Codec {
            supported: false,
            ..Codec::default()
        };
        assert_eq!(try_apply(&mut codec, 5_000), Ok(false));
        assert_eq!(
            try_apply(&mut codec, 0),
            Err(EncoderError::InvalidDimensions)
        );
        assert!(codec.writes.is_empty());
    }

    #[test]
    fn rejected_partial_writes_and_driver_clamping_restore_both_values() {
        for mut codec in [
            Codec {
                reject_mean_once: true,
                ..Codec::default()
            },
            Codec {
                reject_peak_once: true,
                ..Codec::default()
            },
            Codec {
                clamp: true,
                ..Codec::default()
            },
            Codec {
                fail_first_readback: true,
                ..Codec::default()
            },
        ] {
            assert_eq!(try_apply(&mut codec, 20_000), Ok(false));
            assert_eq!((codec.mean, codec.max), (10_000, 15_000));
            assert!(codec.read_count.get() >= 2);
        }
    }

    #[test]
    fn failed_restore_requires_recovery_instead_of_claiming_rejection_is_safe() {
        let mut codec = Codec {
            reject_mean_once: true,
            reject_restore: true,
            ..Codec::default()
        };
        assert!(try_apply(&mut codec, 20_000).is_err());
        assert_ne!(codec.mean, 10_000);
    }
}
