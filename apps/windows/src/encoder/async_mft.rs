use super::{AsyncMftCredits, EncoderError};

pub(super) const MAX_PENDING_INPUTS: usize = 16;
const MAX_QUEUED_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const NO_OUTPUT_WATCHDOG_US: u64 = 1_000_000;
const EVENT_WAIT_BUDGET_US: u64 = 100_000;

pub(super) fn event_wait_expired(elapsed_us: u64) -> bool {
    elapsed_us >= EVENT_WAIT_BUDGET_US
}

pub(super) fn queued_bytes_after(current: usize, added: usize) -> Result<usize, EncoderError> {
    current
        .checked_add(added)
        .filter(|bytes| *bytes <= MAX_QUEUED_OUTPUT_BYTES)
        .ok_or(EncoderError::FrameTooLarge)
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Action {
    ReadOutput,
    SubmitInput,
    Yield,
    Wait,
}

#[derive(Default)]
pub(super) struct Scheduler {
    credits: AsyncMftCredits,
    outstanding: usize,
    no_output_since_us: Option<u64>,
}

impl Scheduler {
    pub(super) fn need_input(&mut self) -> Result<(), EncoderError> {
        self.credits.notify_input()
    }

    pub(super) fn have_output(&mut self) -> Result<(), EncoderError> {
        self.credits.notify_output()
    }

    pub(super) fn next(
        &mut self,
        submitted_this_call: bool,
        queued_outputs: usize,
        now_us: u64,
    ) -> Result<Action, EncoderError> {
        // Output can unblock input admission. Consume its notification before
        // waiting for another NeedInput, even when this call has not submitted.
        if self.credits.take_output() {
            if self.outstanding == 0 {
                return Err(EncoderError::Native(
                    "hardware MFT output without outstanding input".into(),
                ));
            }
            return Ok(Action::ReadOutput);
        }
        if self
            .no_output_since_us
            .is_some_and(|started| now_us.saturating_sub(started) >= NO_OUTPUT_WATCHDOG_US)
        {
            return Err(EncoderError::Native(
                "hardware MFT persistent output watchdog exceeded".into(),
            ));
        }
        if !submitted_this_call && self.credits.input != 0 {
            if self.outstanding.saturating_add(queued_outputs) >= MAX_PENDING_INPUTS {
                return Err(EncoderError::Native(
                    "hardware MFT pending input budget exceeded".into(),
                ));
            }
            self.credits.take_input();
            return Ok(Action::SubmitInput);
        }
        if submitted_this_call && (queued_outputs != 0 || self.credits.input != 0) {
            // A legal async MFT may request several inputs before producing
            // output. Leave this credit for the next captured input frame.
            return Ok(Action::Yield);
        }
        Ok(Action::Wait)
    }

    pub(super) fn submitted(&mut self, now_us: u64) {
        if self.outstanding == 0 {
            self.no_output_since_us = Some(now_us);
        }
        self.outstanding += 1;
    }

    pub(super) fn completed(&mut self, now_us: u64) {
        debug_assert!(self.outstanding != 0);
        self.outstanding = self.outstanding.saturating_sub(1);
        self.no_output_since_us = (self.outstanding != 0).then_some(now_us);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn second_need_input_before_first_output_is_legal_and_keeps_old_timestamp() {
        let mut pump = Scheduler::default();
        let mut pending_timestamps = VecDeque::new();
        pump.need_input().unwrap();
        assert_eq!(pump.next(false, 0, 0).unwrap(), Action::SubmitInput);
        pending_timestamps.push_back(11_u64);
        pump.submitted(0);
        pump.need_input().unwrap();
        assert_eq!(pump.next(true, 0, 1).unwrap(), Action::Yield);
        assert_eq!(pump.next(false, 0, 33_333).unwrap(), Action::SubmitInput);
        pending_timestamps.push_back(22);
        pump.submitted(33_333);
        pump.have_output().unwrap();
        assert_eq!(pump.next(true, 0, 33_334).unwrap(), Action::ReadOutput);
        let old_timestamp = pending_timestamps.pop_front().unwrap();
        pump.completed(33_334);
        assert_eq!(pump.next(true, 1, 33_334).unwrap(), Action::Yield);
        assert_eq!(old_timestamp, 11);
        assert_eq!(pending_timestamps, VecDeque::from([22]));
        assert_eq!(pump.outstanding, 1);
    }

    #[test]
    fn queued_output_is_drained_before_waiting_for_current_input_credit() {
        let mut pump = Scheduler::default();
        pump.need_input().unwrap();
        assert_eq!(pump.next(false, 0, 0).unwrap(), Action::SubmitInput);
        pump.submitted(0);
        pump.need_input().unwrap();
        assert_eq!(pump.next(true, 0, 1).unwrap(), Action::Yield);
        pump.have_output().unwrap();
        assert_eq!(pump.next(false, 0, 33_333).unwrap(), Action::ReadOutput);
        pump.completed(33_333);
        // Having an old output does not authorize losing the new input.
        assert_eq!(pump.next(false, 1, 33_333).unwrap(), Action::SubmitInput);
        pump.submitted(33_333);
        assert_eq!(pump.next(true, 1, 33_333).unwrap(), Action::Yield);
    }

    #[test]
    fn repeated_need_input_cannot_reset_watchdog_or_exceed_pending_budget() {
        let mut pump = Scheduler::default();
        for index in 0..MAX_PENDING_INPUTS {
            pump.need_input().unwrap();
            assert_eq!(
                pump.next(false, 0, index as u64).unwrap(),
                Action::SubmitInput
            );
            pump.submitted(index as u64);
        }
        pump.need_input().unwrap();
        assert!(pump.next(false, 0, 17).is_err());

        let mut pump = Scheduler::default();
        pump.need_input().unwrap();
        pump.next(false, 0, 0).unwrap();
        pump.submitted(0);
        pump.need_input().unwrap();
        assert!(pump.next(true, 0, NO_OUTPUT_WATCHDOG_US).is_err());
    }

    #[test]
    fn event_floods_and_unsolicited_outputs_fail_with_fixed_limits() {
        let mut pump = Scheduler::default();
        for _ in 0..128 {
            pump.need_input().unwrap();
        }
        assert!(pump.need_input().is_err());
        let mut pump = Scheduler::default();
        pump.have_output().unwrap();
        assert!(pump.next(false, 0, 0).is_err());
    }

    #[test]
    fn no_event_wait_has_an_exact_finite_deadline() {
        assert!(!event_wait_expired(99_999));
        assert!(event_wait_expired(100_000));
        assert!(event_wait_expired(u64::MAX));
    }

    #[test]
    fn output_byte_budget_checks_exact_boundary_and_overflow_before_copy() {
        assert_eq!(
            queued_bytes_after(MAX_QUEUED_OUTPUT_BYTES - 1, 1),
            Ok(MAX_QUEUED_OUTPUT_BYTES)
        );
        assert!(queued_bytes_after(MAX_QUEUED_OUTPUT_BYTES, 1).is_err());
        assert!(queued_bytes_after(usize::MAX, 1).is_err());
        assert!(queued_bytes_after(0, MAX_QUEUED_OUTPUT_BYTES + 1).is_err());
    }
}
