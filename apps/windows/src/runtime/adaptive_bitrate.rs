use desklink_protocol::VideoQualityPreset;

const MIN_FEEDBACK_PACKETS: u64 = 40;
const DEGRADED_LOSS_BASIS_POINTS: u64 = 500;
const SEVERE_LOSS_BASIS_POINTS: u64 = 1_500;
const HEALTHY_LOSS_BASIS_POINTS: u64 = 50;
const DEGRADED_WINDOWS: u8 = 2;
const HEALTHY_WINDOWS: u8 = 12;
const COOLDOWN_WINDOWS: u8 = 8;

/// Changes only bitrate inside the currently applied resolution/fps preset.
/// The caller gates this policy on Automatic preference; render pressure and
/// exhausted/unsupported bitrate control remain the tier policy's decision.
pub(super) struct AdaptiveBitrate {
    ceiling: u32,
    floor: u32,
    current: u32,
    pending: Option<u32>,
    degraded: u8,
    healthy: u8,
    cooldown: u8,
    disabled: bool,
}

impl AdaptiveBitrate {
    pub(super) fn new(preset: VideoQualityPreset) -> Self {
        let ceiling = match preset {
            VideoQualityPreset::Sharp => 18_000_000,
            VideoQualityPreset::Balanced => 2_500_000,
            VideoQualityPreset::Smooth => 1_500_000,
        };
        Self {
            ceiling,
            floor: (ceiling / 3).max(500_000).min(ceiling),
            current: ceiling,
            pending: None,
            degraded: 0,
            healthy: 0,
            cooldown: 0,
            disabled: false,
        }
    }

    /// Call after a preset has actually been applied, not when it is queued.
    pub(super) fn reset(&mut self, preset: VideoQualityPreset) {
        *self = Self::new(preset);
    }

    pub(super) fn reset_to_actual(&mut self, preset: VideoQualityPreset, actual: u32) {
        self.reset(preset);
        self.current = actual;
        self.disabled = actual < self.floor || actual > self.ceiling;
    }

    pub(super) fn current(&self) -> u32 {
        self.current
    }

    pub(super) fn at_floor(&self) -> bool {
        self.current <= self.floor
    }

    pub(super) fn max(&self) -> u32 {
        self.ceiling
    }

    /// Network pressure can be handled here while a bitrate request is pending
    /// or cooling down. Decoder/freshness pressure must bypass this gate so the
    /// resolution/fps policy can respond even while bitrate is above its floor.
    pub(super) fn should_recover_rate_first(
        &self,
        received: u32,
        dropped: u32,
        decode_queue: u16,
        freshness: u16,
    ) -> bool {
        !self.disabled
            && self.current() < self.max()
            && decode_queue <= 2
            && freshness == 0
            && loss_basis_points(received, dropped)
                .is_some_and(|loss| loss <= HEALTHY_LOSS_BASIS_POINTS)
    }

    pub(super) fn can_handle_network_pressure(
        &self,
        received: u32,
        dropped: u32,
        decode_queue: u16,
        freshness: u16,
    ) -> bool {
        !self.disabled
            && !self.at_floor()
            && decode_queue < 5
            && freshness == 0
            && loss_basis_points(received, dropped)
                .is_some_and(|loss| loss >= DEGRADED_LOSS_BASIS_POINTS)
    }

    pub(super) fn observe(
        &mut self,
        received: u32,
        dropped: u32,
        decode_queue: u16,
        freshness: u16,
    ) -> Option<u32> {
        if self.disabled || self.pending.is_some() {
            return None;
        }
        let Some(loss) = loss_basis_points(received, dropped) else {
            self.reset_trend();
            return None;
        };
        if decode_queue >= 5 || freshness > 0 {
            self.reset_trend();
            return None;
        }
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return None;
        }
        let target = if loss >= DEGRADED_LOSS_BASIS_POINTS {
            self.healthy = 0;
            self.degraded = self.degraded.saturating_add(1);
            if self.degraded < DEGRADED_WINDOWS {
                return None;
            }
            let percent = if loss >= SEVERE_LOSS_BASIS_POINTS {
                65
            } else {
                80
            };
            ((u64::from(self.current) * percent / 100) as u32).max(self.floor)
        } else if loss <= HEALTHY_LOSS_BASIS_POINTS && decode_queue <= 2 {
            self.degraded = 0;
            self.healthy = self.healthy.saturating_add(1);
            if self.healthy < HEALTHY_WINDOWS {
                return None;
            }
            self.current
                .saturating_add(self.ceiling / 20)
                .min(self.ceiling)
        } else {
            self.reset_trend();
            return None;
        };
        self.reset_trend();
        if target == self.current {
            return None;
        }
        self.pending = Some(target);
        Some(target)
    }

    /// Native completion supplies the actual bitrate. Desired and actual stay
    /// separate until this acknowledgement; stale replies are gated by the
    /// caller's configuration operation serial before reaching this method.
    pub(super) fn record_applied(&mut self, actual: u32) -> bool {
        if self.pending.is_none() || self.disabled {
            return false;
        }
        if actual < self.floor || actual > self.ceiling {
            self.rejected();
            return false;
        }
        self.current = actual;
        self.pending = None;
        self.cooldown = COOLDOWN_WINDOWS;
        self.reset_trend();
        true
    }

    /// A backend rejecting dynamic bitrate is not retried every feedback tick.
    /// Keep its last actual bitrate and use the established tier fallback.
    pub(super) fn rejected(&mut self) {
        self.disabled = true;
        self.pending = None;
        self.cooldown = 0;
        self.reset_trend();
    }

    fn reset_trend(&mut self) {
        self.degraded = 0;
        self.healthy = 0;
    }
}

fn loss_basis_points(received: u32, dropped: u32) -> Option<u64> {
    let total = u64::from(received) + u64::from(dropped);
    (total >= MIN_FEEDBACK_PACKETS).then(|| u64::from(dropped) * 10_000 / total)
}

#[cfg(test)]
mod tests {
    use super::AdaptiveBitrate;
    use desklink_protocol::VideoQualityPreset;

    fn loss(policy: &mut AdaptiveBitrate) -> Option<u32> {
        policy.observe(90, 10, 1, 0)
    }

    fn healthy(policy: &mut AdaptiveBitrate) -> Option<u32> {
        policy.observe(100, 0, 1, 0)
    }

    fn exhaust_cooldown(policy: &mut AdaptiveBitrate) {
        for _ in 0..8 {
            assert_eq!(healthy(policy), None);
        }
    }

    #[test]
    fn native_actual_reset_controls_recovery_and_unsupported_fallback() {
        let mut policy = AdaptiveBitrate::new(VideoQualityPreset::Balanced);
        policy.reset_to_actual(VideoQualityPreset::Balanced, 2_000_000);
        assert_eq!(policy.current(), 2_000_000);
        assert!(policy.should_recover_rate_first(999, 1, 1, 0));
        assert!(!policy.should_recover_rate_first(2, 0, 1, 0));
        policy.rejected();
        assert!(!policy.should_recover_rate_first(1000, 0, 1, 0));
        policy.reset_to_actual(VideoQualityPreset::Balanced, 2_500_000);
        assert!(!policy.should_recover_rate_first(1000, 0, 1, 0));
        policy.reset_to_actual(VideoQualityPreset::Balanced, 1);
        assert_eq!(policy.current(), 1, "never invent a clamped actual value");
        assert!(!policy.should_recover_rate_first(1000, 0, 1, 0));
    }

    #[test]
    fn loss_step_down_waits_for_two_windows_and_native_acknowledgement() {
        let mut policy = AdaptiveBitrate::new(VideoQualityPreset::Balanced);
        assert_eq!(loss(&mut policy), None);
        assert_eq!(loss(&mut policy), Some(2_000_000));
        assert_eq!(policy.current(), 2_500_000);
        for _ in 0..1000 {
            assert_eq!(loss(&mut policy), None);
        }
        assert!(policy.can_handle_network_pressure(90, 10, 1, 0));
        assert!(policy.record_applied(2_000_000));
        assert_eq!(policy.current(), 2_000_000);
        assert!(!policy.record_applied(2_000_000));
        for _ in 0..8 {
            assert_eq!(loss(&mut policy), None);
        }
        assert_eq!(loss(&mut policy), None);
        assert_eq!(loss(&mut policy), Some(1_600_000));
    }

    #[test]
    fn severe_loss_uses_a_bounded_larger_decrease_and_falls_back_at_floor() {
        let mut policy = AdaptiveBitrate::new(VideoQualityPreset::Smooth);
        assert_eq!(policy.observe(80, 20, 1, 0), None);
        assert_eq!(policy.observe(80, 20, 1, 0), Some(975_000));
        assert!(policy.record_applied(975_000));
        exhaust_cooldown(&mut policy);
        policy.observe(80, 20, 1, 0);
        assert_eq!(policy.observe(80, 20, 1, 0), Some(633_750));
        assert!(policy.record_applied(633_750));
        exhaust_cooldown(&mut policy);
        policy.observe(80, 20, 1, 0);
        assert_eq!(policy.observe(80, 20, 1, 0), Some(500_000));
        assert!(policy.record_applied(500_000));
        assert!(policy.at_floor());
        assert!(!policy.can_handle_network_pressure(80, 20, 1, 0));
        exhaust_cooldown(&mut policy);
        for _ in 0..20 {
            assert_eq!(policy.observe(80, 20, 1, 0), None);
        }
    }

    #[test]
    fn healthy_step_up_requires_twelve_windows_and_respects_the_ceiling() {
        let mut policy = AdaptiveBitrate::new(VideoQualityPreset::Balanced);
        loss(&mut policy);
        loss(&mut policy);
        policy.record_applied(2_000_000);
        exhaust_cooldown(&mut policy);
        for _ in 0..11 {
            assert_eq!(healthy(&mut policy), None);
        }
        assert_eq!(healthy(&mut policy), Some(2_125_000));
        assert_eq!(policy.current(), 2_000_000);
        assert!(policy.record_applied(2_490_000));
        exhaust_cooldown(&mut policy);
        for _ in 0..11 {
            assert_eq!(healthy(&mut policy), None);
        }
        assert_eq!(healthy(&mut policy), Some(2_500_000));
        assert!(policy.record_applied(2_500_000));
        assert_eq!(policy.current(), policy.max());
        for _ in 0..100 {
            assert_eq!(healthy(&mut policy), None);
        }
    }

    #[test]
    fn sparse_feedback_and_renderer_pressure_do_not_change_bitrate_or_consume_cooldown() {
        let mut policy = AdaptiveBitrate::new(VideoQualityPreset::Sharp);
        assert_eq!(loss(&mut policy), None);
        assert_eq!(policy.observe(20, 19, 1, 0), None);
        assert_eq!(loss(&mut policy), None);
        assert_eq!(policy.observe(90, 10, 5, 0), None);
        assert_eq!(loss(&mut policy), None);
        assert_eq!(policy.observe(90, 10, 1, 1), None);
        assert!(!policy.can_handle_network_pressure(90, 10, 5, 0));
        assert!(!policy.can_handle_network_pressure(90, 10, 1, 1));
        assert_eq!(loss(&mut policy), None);
        assert_eq!(loss(&mut policy), Some(14_400_000));
        assert!(policy.record_applied(14_400_000));
        for _ in 0..100 {
            assert_eq!(policy.observe(20, 19, 1, 0), None);
        }
        for _ in 0..8 {
            assert_eq!(loss(&mut policy), None);
        }
        assert_eq!(loss(&mut policy), None);
        assert_eq!(loss(&mut policy), Some(11_520_000));
    }

    #[test]
    fn rejection_disables_retries_until_a_real_preset_reset() {
        let mut policy = AdaptiveBitrate::new(VideoQualityPreset::Sharp);
        loss(&mut policy);
        loss(&mut policy);
        policy.rejected();
        assert_eq!(policy.current(), 18_000_000);
        assert!(!policy.can_handle_network_pressure(90, 10, 1, 0));
        for _ in 0..100 {
            assert_eq!(loss(&mut policy), None);
        }
        assert!(!policy.record_applied(14_400_000));
        policy.reset(VideoQualityPreset::Balanced);
        assert_eq!(policy.current(), 2_500_000);
        assert_eq!(loss(&mut policy), None);
        assert_eq!(loss(&mut policy), Some(2_000_000));
    }

    #[test]
    fn preset_bounds_and_counter_overflow_cannot_produce_an_invalid_target() {
        for (preset, maximum, floor) in [
            (VideoQualityPreset::Sharp, 18_000_000, 6_000_000),
            (VideoQualityPreset::Balanced, 2_500_000, 833_333),
            (VideoQualityPreset::Smooth, 1_500_000, 500_000),
        ] {
            let mut policy = AdaptiveBitrate::new(preset);
            assert_eq!(policy.max(), maximum);
            assert_eq!(policy.floor, floor);
            for _ in 0..200 {
                if let Some(target) = policy.observe(u32::MAX, u32::MAX, 0, 0) {
                    assert!((floor..=maximum).contains(&target));
                    assert!(policy.record_applied(target));
                }
            }
            assert!(policy.at_floor());
        }
    }

    #[test]
    fn neutral_feedback_breaks_a_degradation_streak_and_invalid_ack_disables_policy() {
        let mut policy = AdaptiveBitrate::new(VideoQualityPreset::Balanced);
        loss(&mut policy);
        assert_eq!(policy.observe(99, 1, 1, 0), None);
        assert_eq!(loss(&mut policy), None);
        assert_eq!(loss(&mut policy), Some(2_000_000));
        assert!(!policy.record_applied(0));
        assert_eq!(policy.current(), 2_500_000);
        assert!(!policy.can_handle_network_pressure(90, 10, 1, 0));
    }
}
