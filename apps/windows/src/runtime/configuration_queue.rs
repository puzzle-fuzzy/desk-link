use desklink_protocol::{H264Profile, VideoQualityPreset};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConfigurationKind {
    Display,
    Quality,
    Profile,
}

impl ConfigurationKind {
    fn index(self) -> usize {
        match self {
            Self::Display => 0,
            Self::Quality => 1,
            Self::Profile => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ConfigurationAction {
    SelectDisplay(u32),
    SetVideoQuality(VideoQualityPreset),
    SetVideoProfile(H264Profile),
}

impl ConfigurationAction {
    pub(super) fn kind(self) -> ConfigurationKind {
        match self {
            Self::SelectDisplay(_) => ConfigurationKind::Display,
            Self::SetVideoQuality(_) => ConfigurationKind::Quality,
            Self::SetVideoProfile(_) => ConfigurationKind::Profile,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ConfigurationOperation {
    pub(super) serial: u64,
    pub(super) action: ConfigurationAction,
}

/// Stores only the latest pending choice of each kind. The control loop owns
/// at most one additional popped operation, polling its completion alongside
/// control requests, keyframe requests and cancellation instead of awaiting it
/// inline. Replacing one kind preserves the order of other pending choices.
#[derive(Default)]
pub(super) struct ConfigurationQueue {
    serial: u64,
    latest: [u64; 3],
    pending: [Option<ConfigurationOperation>; 3],
}

impl ConfigurationQueue {
    pub(super) fn request(&mut self, action: ConfigurationAction) -> u64 {
        let serial = self.next_serial();
        let index = action.kind().index();
        self.latest[index] = serial;
        self.pending[index] = Some(ConfigurationOperation { serial, action });
        serial
    }

    pub(super) fn pop_next(&mut self) -> Option<ConfigurationOperation> {
        // Compare ages rather than raw values so MAX -> 1 keeps FIFO order.
        let index = self
            .pending
            .iter()
            .enumerate()
            .filter_map(|(index, operation)| {
                operation.map(|operation| (index, self.serial.wrapping_sub(operation.serial)))
            })
            .max_by_key(|(_, age)| *age)?
            .0;
        self.pending[index].take()
    }

    pub(super) fn is_current(&self, operation: &ConfigurationOperation) -> bool {
        self.latest[operation.action.kind().index()] == operation.serial
    }

    /// Automatic quality can invalidate a previously queued manual preset
    /// without enqueueing a replacement. A late manual completion then cannot
    /// overwrite the user's newer automatic preference.
    pub(super) fn invalidate_kind(&mut self, kind: ConfigurationKind) -> u64 {
        let serial = self.next_serial();
        self.latest[kind.index()] = serial;
        self.pending[kind.index()] = None;
        serial
    }

    pub(super) fn has_pending(&self, kind: ConfigurationKind) -> bool {
        self.pending[kind.index()].is_some()
    }

    pub(super) fn has_pending_display(&self) -> bool {
        self.has_pending(ConfigurationKind::Display)
    }

    fn next_serial(&mut self) -> u64 {
        self.serial = self.serial.wrapping_add(1).max(1);
        self.serial
    }
}

#[cfg(test)]
mod tests {
    use super::{ConfigurationAction as Action, ConfigurationKind as Kind, ConfigurationQueue};
    use desklink_protocol::{H264Profile, VideoQualityPreset};

    #[test]
    fn floods_replace_same_kind_and_keep_only_three_pending_choices() {
        let mut queue = ConfigurationQueue::default();
        let first_serial = queue.request(Action::SelectDisplay(1));
        let in_flight = queue.pop_next().unwrap();
        assert_eq!(in_flight.serial, first_serial);
        for display in 2..=1001 {
            queue.request(Action::SelectDisplay(display));
            queue.request(Action::SetVideoQuality(VideoQualityPreset::Balanced));
            queue.request(Action::SetVideoProfile(H264Profile::High));
            assert_eq!(queue.pending.iter().flatten().count(), 3);
        }
        assert!(!queue.is_current(&in_flight));
        assert_eq!(
            queue.pop_next().unwrap().action,
            Action::SelectDisplay(1001)
        );
        assert_eq!(
            queue.pop_next().unwrap().action,
            Action::SetVideoQuality(VideoQualityPreset::Balanced)
        );
        assert_eq!(
            queue.pop_next().unwrap().action,
            Action::SetVideoProfile(H264Profile::High)
        );
        assert!(queue.pop_next().is_none());
    }

    #[test]
    fn replacing_pending_kind_preserves_cross_kind_request_order() {
        let mut queue = ConfigurationQueue::default();
        queue.request(Action::SelectDisplay(1));
        queue.request(Action::SetVideoProfile(H264Profile::Main));
        queue.request(Action::SetVideoQuality(VideoQualityPreset::Smooth));
        queue.request(Action::SelectDisplay(2));
        assert!(queue.has_pending_display());
        assert_eq!(
            queue.pop_next().unwrap().action,
            Action::SetVideoProfile(H264Profile::Main)
        );
        assert_eq!(
            queue.pop_next().unwrap().action,
            Action::SetVideoQuality(VideoQualityPreset::Smooth)
        );
        assert_eq!(queue.pop_next().unwrap().action, Action::SelectDisplay(2));
        assert!(!queue.has_pending_display());
    }

    #[test]
    fn stale_completion_is_ignored_and_newest_choice_eventually_runs() {
        let mut queue = ConfigurationQueue::default();
        queue.request(Action::SetVideoQuality(VideoQualityPreset::Smooth));
        let in_flight = queue.pop_next().unwrap();
        assert!(queue.is_current(&in_flight));
        queue.request(Action::SetVideoQuality(VideoQualityPreset::Balanced));
        queue.request(Action::SetVideoQuality(VideoQualityPreset::Sharp));
        assert!(!queue.is_current(&in_flight));
        let latest = queue.pop_next().unwrap();
        assert_eq!(
            latest.action,
            Action::SetVideoQuality(VideoQualityPreset::Sharp)
        );
        assert!(queue.is_current(&latest));
        assert!(queue.pop_next().is_none());
    }

    #[test]
    fn automatic_preference_invalidates_pending_and_in_flight_manual_quality() {
        let mut queue = ConfigurationQueue::default();
        queue.request(Action::SetVideoQuality(VideoQualityPreset::Smooth));
        let in_flight = queue.pop_next().unwrap();
        queue.request(Action::SetVideoQuality(VideoQualityPreset::Sharp));
        queue.request(Action::SelectDisplay(42));
        queue.invalidate_kind(Kind::Quality);
        assert!(!queue.is_current(&in_flight));
        assert!(!queue.has_pending(Kind::Quality));
        assert!(queue.has_pending_display());
        assert_eq!(queue.pop_next().unwrap().action, Action::SelectDisplay(42));
        assert!(queue.pop_next().is_none());
    }

    #[test]
    fn serial_wrap_preserves_order_and_replacement_invalidates_max_serial() {
        let mut queue = ConfigurationQueue {
            serial: u64::MAX - 2,
            ..Default::default()
        };
        queue.request(Action::SelectDisplay(7));
        queue.request(Action::SetVideoQuality(VideoQualityPreset::Smooth));
        queue.request(Action::SetVideoProfile(H264Profile::Main));
        assert_eq!(queue.pop_next().unwrap().action, Action::SelectDisplay(7));
        let old = queue.pop_next().unwrap();
        assert_eq!(old.serial, u64::MAX);
        assert!(queue.is_current(&old));
        queue.request(Action::SetVideoQuality(VideoQualityPreset::Sharp));
        assert!(!queue.is_current(&old));
        assert_eq!(
            queue.pop_next().unwrap().action,
            Action::SetVideoProfile(H264Profile::Main)
        );
        assert_eq!(
            queue.pop_next().unwrap().action,
            Action::SetVideoQuality(VideoQualityPreset::Sharp)
        );
    }
}
