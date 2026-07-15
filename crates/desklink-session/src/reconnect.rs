use std::time::Duration;

use thiserror::Error;

pub const DEFAULT_RECONNECT_BASE_DELAY: Duration = Duration::from_millis(250);
pub const DEFAULT_RECONNECT_MAX_DELAY: Duration = Duration::from_secs(8);
pub const DEFAULT_RECONNECT_RETRIES: u32 = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectPolicy {
    base_delay: Duration,
    max_delay: Duration,
    max_retries: u32,
}

impl ReconnectPolicy {
    pub fn new(
        base_delay: Duration,
        max_delay: Duration,
        max_retries: u32,
    ) -> Result<Self, ReconnectPolicyError> {
        if base_delay.is_zero() {
            return Err(ReconnectPolicyError::ZeroBaseDelay);
        }
        if max_delay < base_delay {
            return Err(ReconnectPolicyError::MaxBelowBase);
        }
        if max_retries == 0 {
            return Err(ReconnectPolicyError::ZeroRetries);
        }
        Ok(Self {
            base_delay,
            max_delay,
            max_retries,
        })
    }

    pub const fn base_delay(&self) -> Duration {
        self.base_delay
    }

    pub const fn max_delay(&self) -> Duration {
        self.max_delay
    }

    pub const fn max_retries(&self) -> u32 {
        self.max_retries
    }
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            base_delay: DEFAULT_RECONNECT_BASE_DELAY,
            max_delay: DEFAULT_RECONNECT_MAX_DELAY,
            max_retries: DEFAULT_RECONNECT_RETRIES,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconnectDecision {
    RetryAfter { retry: u32, delay: Duration },
    Exhausted,
    SessionExpired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReconnectSchedule {
    policy: ReconnectPolicy,
    retries_used: u32,
    expires_at_unix_s: Option<u64>,
}

impl ReconnectSchedule {
    pub const fn new(policy: ReconnectPolicy, expires_at_unix_s: Option<u64>) -> Self {
        Self {
            policy,
            retries_used: 0,
            expires_at_unix_s,
        }
    }

    pub fn next(&mut self, now_unix_s: u64) -> ReconnectDecision {
        let remaining = match self.expires_at_unix_s {
            Some(expires_at) if now_unix_s >= expires_at => {
                return ReconnectDecision::SessionExpired;
            }
            Some(expires_at) => Some(Duration::from_secs(expires_at - now_unix_s)),
            None => None,
        };
        if self.retries_used >= self.policy.max_retries {
            return ReconnectDecision::Exhausted;
        }
        let factor = 1_u32 << self.retries_used.min(31);
        let mut delay = self
            .policy
            .base_delay
            .saturating_mul(factor)
            .min(self.policy.max_delay);
        if let Some(remaining) = remaining {
            delay = delay.min(remaining);
        }
        self.retries_used += 1;
        ReconnectDecision::RetryAfter {
            retry: self.retries_used,
            delay,
        }
    }

    pub const fn retries_used(&self) -> u32 {
        self.retries_used
    }

    pub const fn max_retries(&self) -> u32 {
        self.policy.max_retries
    }

    pub fn reset(&mut self) {
        self.retries_used = 0;
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ReconnectPolicyError {
    #[error("reconnect base delay must be nonzero")]
    ZeroBaseDelay,
    #[error("reconnect maximum delay must not be below the base delay")]
    MaxBelowBase,
    #[error("reconnect retry budget must be nonzero")]
    ZeroRetries,
}
