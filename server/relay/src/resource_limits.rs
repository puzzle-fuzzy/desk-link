//! Explicit payload-buffer budgets. These are not an RSS bound: allocator,
//! stream/packet metadata, TLS, tasks and the OS also consume memory.

use desklink_transport::{MAX_DATAGRAM_BYTES, MAX_JOIN_ENVELOPE_BYTES, MAX_RELIABLE_MESSAGE_BYTES};

use crate::{
    MAX_ADMISSION_REJECTION_TASKS, MAX_RELIABLE_FORWARD_TASKS_PER_CONNECTION, RelayConfig,
    RelayError, media_scheduler::MEDIA_BUDGETS,
};

pub const DEFAULT_RELAY_BUFFER_BUDGET_BYTES: u64 = 160 * 1024 * 1024;
pub const DEFAULT_RELAY_MAX_CONNECTIONS: usize = 64;
pub const DEFAULT_RELAY_MAX_SESSIONS: usize = 64;
pub(super) const MAX_PENDING_INCOMING: usize = 64;
pub(super) const INCOMING_BUFFER_BYTES: u64 = 16 * 1024;
pub(super) const TOTAL_INCOMING_BUFFER_BYTES: u64 = 1024 * 1024;
const CRYPTO_BUFFER_BYTES: usize = 16 * 1024;
const MODELED_ACTIVE_CONNECTION_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelayResourceBudget {
    pub max_incoming_uni_streams: u32,
    pub max_incoming_bidi_streams: u32,
    pub stream_receive_window_bytes: u64,
    pub connection_receive_window_bytes: u64,
    pub send_window_bytes: u64,
    pub datagram_receive_buffer_bytes: usize,
    pub datagram_send_buffer_bytes: usize,
    pub per_admitted_connection_buffer_bytes: u64,
    pub modeled_total_buffer_bytes: u64,
    pub declared_buffer_budget_bytes: u64,
}

impl RelayResourceBudget {
    pub(super) fn for_config(config: &RelayConfig) -> Result<Self, RelayError> {
        let mut budget = Self {
            max_incoming_uni_streams: 0,
            max_incoming_bidi_streams: (MAX_RELIABLE_FORWARD_TASKS_PER_CONNECTION + 1) as u32,
            stream_receive_window_bytes: (MAX_RELIABLE_MESSAGE_BYTES + 13) as u64,
            connection_receive_window_bytes: 128 * 1024,
            send_window_bytes: 128 * 1024,
            datagram_receive_buffer_bytes: 32 * 1024,
            datagram_send_buffer_bytes: 64 * 1024,
            per_admitted_connection_buffer_bytes: MODELED_ACTIVE_CONNECTION_BYTES,
            modeled_total_buffer_bytes: 0,
            declared_buffer_budget_bytes: config.buffer_budget_bytes,
        };
        let quic_buffers = budget.connection_receive_window_bytes
            + budget.send_window_bytes
            + budget.datagram_receive_buffer_bytes as u64
            + budget.datagram_send_buffer_bytes as u64
            + CRYPTO_BUFFER_BYTES as u64;
        let media_buffers: usize = MEDIA_BUDGETS
            .iter()
            .map(|lane| (lane.capacity + 1) * (MAX_DATAGRAM_BYTES + 9))
            .sum();
        let application_buffers = MAX_RELIABLE_FORWARD_TASKS_PER_CONNECTION
            * MAX_RELIABLE_MESSAGE_BYTES
            + media_buffers
            + MAX_JOIN_ENVELOPE_BYTES
            + 4;
        if quic_buffers + application_buffers as u64 > MODELED_ACTIVE_CONNECTION_BYTES {
            return Err(RelayError::InvalidConfig(
                "per-connection buffer model needs updating".to_owned(),
            ));
        }
        let modeled = config.max_connections as u128 * MODELED_ACTIVE_CONNECTION_BYTES as u128
            + MAX_ADMISSION_REJECTION_TASKS as u128 * quic_buffers as u128
            + TOTAL_INCOMING_BUFFER_BYTES as u128;
        budget.modeled_total_buffer_bytes = u64::try_from(modeled)
            .map_err(|_| RelayError::InvalidConfig("relay buffer budget overflows".to_owned()))?;
        if budget.modeled_total_buffer_bytes > config.buffer_budget_bytes {
            return Err(RelayError::InvalidConfig(format!(
                "modeled relay buffers require {} bytes but the declared budget is {}; reduce max_connections or explicitly provision a larger budget and container",
                budget.modeled_total_buffer_bytes, config.buffer_budget_bytes,
            )));
        }
        Ok(budget)
    }

    pub(super) fn configure(&self, transport: &mut quinn::TransportConfig) {
        transport
            .max_concurrent_uni_streams(self.max_incoming_uni_streams.into())
            .max_concurrent_bidi_streams(self.max_incoming_bidi_streams.into())
            .stream_receive_window(
                quinn::VarInt::from_u64(self.stream_receive_window_bytes)
                    .expect("bounded stream window"),
            )
            .receive_window(
                quinn::VarInt::from_u64(self.connection_receive_window_bytes)
                    .expect("bounded connection window"),
            )
            .send_window(self.send_window_bytes)
            .datagram_receive_buffer_size(Some(self.datagram_receive_buffer_bytes))
            .datagram_send_buffer_size(self.datagram_send_buffer_bytes)
            .crypto_buffer_size(CRYPTO_BUFFER_BYTES);
    }
}

/// Read-only Linux process/cgroup-v2 observations; missing values stay unknown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RelayMemoryObservation {
    pub process_resident_bytes: Option<u64>,
    pub cgroup_current_bytes: Option<u64>,
    pub cgroup_peak_bytes: Option<u64>,
    pub cgroup_limit_bytes: Option<u64>,
    pub cgroup_oom_kills: Option<u64>,
}

impl RelayMemoryObservation {
    pub fn read() -> Self {
        #[cfg(target_os = "linux")]
        {
            fn value(path: &str) -> Option<u64> {
                std::fs::read_to_string(path).ok()?.trim().parse().ok()
            }
            let status = std::fs::read_to_string("/proc/self/status").ok();
            let events = std::fs::read_to_string("/sys/fs/cgroup/memory.events").ok();
            Self {
                process_resident_bytes: status.as_deref().and_then(|text| {
                    let line = text.lines().find(|line| line.starts_with("VmRSS:"))?;
                    line.split_whitespace()
                        .nth(1)?
                        .parse::<u64>()
                        .ok()?
                        .checked_mul(1024)
                }),
                cgroup_current_bytes: value("/sys/fs/cgroup/memory.current"),
                cgroup_peak_bytes: value("/sys/fs/cgroup/memory.peak"),
                cgroup_limit_bytes: value("/sys/fs/cgroup/memory.max"),
                cgroup_oom_kills: events.as_deref().and_then(|text| {
                    text.lines()
                        .find_map(|line| line.strip_prefix("oom_kill ")?.parse().ok())
                }),
            }
        }
        #[cfg(not(target_os = "linux"))]
        Self::default()
    }
}

impl std::fmt::Display for RelayMemoryObservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn number(value: Option<u64>) -> String {
            value.map_or_else(|| "null".to_owned(), |value| value.to_string())
        }
        write!(
            formatter,
            "{{\"schema\":1,\"process_resident_bytes\":{},\"cgroup_current_bytes\":{},\"cgroup_peak_bytes\":{},\"cgroup_limit_bytes\":{},\"cgroup_oom_kills\":{}}}",
            number(self.process_resident_bytes),
            number(self.cgroup_current_bytes),
            number(self.cgroup_peak_bytes),
            number(self.cgroup_limit_bytes),
            number(self.cgroup_oom_kills),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_budget_covers_admission_rejection_and_endpoint_buffers() {
        let budget = RelayResourceBudget::for_config(&RelayConfig::default()).unwrap();
        assert_eq!(budget.modeled_total_buffer_bytes, 152 * 1024 * 1024);
        assert!(budget.modeled_total_buffer_bytes <= budget.declared_buffer_budget_bytes);
        assert!(
            RelayResourceBudget::for_config(&RelayConfig {
                max_connections: 512,
                ..RelayConfig::default()
            })
            .is_err()
        );
        assert!(
            RelayResourceBudget::for_config(&RelayConfig {
                max_connections: usize::MAX,
                buffer_budget_bytes: u64::MAX,
                ..RelayConfig::default()
            })
            .is_err()
        );
    }
}
