//! Automatic evidence for the native report builder. This exercises real
//! callback/log I/O and production report helpers, without opening a GUI,
//! starting a session, or writing to current-user logs or Downloads.

use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use apps_windows::{
    diagnostics::DiagnosticLog,
    runtime::{CursorDisableReason, HostMediaDiagnostic},
};
use serde::Deserialize;

use super::{
    HostRuntimeSummary, HostSnapshot, build_diagnostic_checks, build_diagnostic_report,
    controller::ControllerRuntimeSummary, host::host_media_diagnostics_observer,
    local_relay::RelayStatusSummary, merge_recent_diagnostic_lines,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "desklink-host-cursor-export-{}-{nonce}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn inactive_host_snapshot() -> HostSnapshot {
    let runtime = HostRuntimeSummary {
        state: "stopped",
        title: "主机已停止".to_owned(),
        detail: "本地报告测试，没有运行会话。".to_owned(),
        tooltip: "DeskLink：已停止".to_owned(),
    };
    let checks = build_diagnostic_checks(&runtime, None, None, None, None);
    HostSnapshot {
        readiness: "unconfigured",
        title: "尚未配置".to_owned(),
        detail: "本地报告测试".to_owned(),
        runtime,
        connection: None,
        connection_error: None,
        trusted_controllers: Vec::new(),
        trusted_error: None,
        relay_status: RelayStatusSummary {
            mode: "unconfigured",
            state: "inactive",
            title: "没有活动网络".to_owned(),
            detail: "本地报告测试，不连接中继。".to_owned(),
        },
        diagnostic_checks: checks,
        pairing_active: false,
        pending_approval: None,
        fixed_password_enabled: false,
        fixed_password_error: None,
        device_id: None,
        refreshed_at_unix_s: 0,
    }
}

#[test]
fn cursor_callback_persists_exact_fields_and_reaches_the_production_report_builder() {
    let temporary = TemporaryDirectory::new();
    let path = temporary.0.join("host.jsonl");
    let diagnostics = DiagnosticLog::new(&path);
    let reader = diagnostics.clone();
    let observer = host_media_diagnostics_observer(diagnostics);
    assert!(!path.exists());

    observer(HostMediaDiagnostic::CursorSend {
        stream_id: u64::MAX,
        send_attempts: 17,
        send_errors: 1,
        disabled: true,
        reason: CursorDisableReason::CapabilityOrSize,
    });
    // The production callback calls DiagnosticLog::record, including sync_data.
    // Dropping it cannot lose history; the real reader retains a logger clone.
    drop(observer);
    assert!(fs::metadata(&path).unwrap().len() > 0);
    let lines = reader.recent_sanitized_lines().unwrap();
    assert_eq!(lines.len(), 1);
    let event: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    let fields = event
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        fields,
        BTreeSet::from([
            "schema",
            "timestamp_unix_ms",
            "level",
            "event",
            "stream_id",
            "send_attempts",
            "send_errors",
            "disabled",
            "reason",
        ])
    );
    assert_eq!(event["schema"], 1);
    assert!(event["timestamp_unix_ms"].as_u64().unwrap() > 0);
    assert_eq!(event["level"], "warning");
    assert_eq!(event["event"], "host_cursor_send");
    assert_eq!(event["stream_id"].as_u64(), Some(u64::MAX));
    assert_eq!(event["send_attempts"], 17);
    assert_eq!(event["send_errors"], 1);
    assert_eq!(event["disabled"], true);
    assert_eq!(event["reason"], "capability_or_size");
    assert!(lines[0].contains("\"stream_id\":18446744073709551615"));

    let merged = merge_recent_diagnostic_lines(lines, Vec::new());
    assert_eq!(merged.len(), 1);
    assert!(merged[0].starts_with("[主机] "));
    let snapshot = inactive_host_snapshot();
    let controller = ControllerRuntimeSummary {
        state: "idle",
        title: "控制端未连接".to_owned(),
        detail: "本地报告测试".to_owned(),
        stream_id: None,
    };
    let report =
        build_diagnostic_report(&snapshot, &controller, "DL-WIN-CURSOR-TEST", 123, &merged);
    // Assert the actual log line survives both report helpers and final
    // redaction. These are local send counters, with no delivery/loss claims.
    assert!(report.contains(&merged[0]));
    assert!(report.contains("\"send_attempts\":17"));
    assert!(report.contains("\"send_errors\":1"));
    assert!(report.contains("\"disabled\":true"));
    assert!(report.contains("\"reason\":\"capability_or_size\""));
    assert!(!report.contains("\"total_drops\""));
    assert!(!report.contains("\"delivery_rate\""));
    assert!(!report.contains("\"network_loss\""));
    assert!(!report.contains("\"endpoint\""));
    assert!(!report.contains("\"error\""));
    assert_eq!(snapshot.runtime.state, "stopped");
}

#[derive(Deserialize)]
struct CursorProducerFixture {
    producer_commit: String,
    cases: Vec<CursorProducerCase>,
}

#[derive(Deserialize)]
struct CursorProducerCase {
    name: String,
    line: String,
}

#[test]
fn cursor_observer_matches_the_committed_producer_cases_with_exact_u64_values() {
    // Byte-for-byte copy from the services contract b1118ad465703493255123e23b6f522662c3d603.
    // This is producer/log compatibility, not a server-acceptance test. In
    // particular, u64_overflow is a services rejection case: the GUI must still
    // retain its original Rust u64 values, without rounding, clamping or hiding.
    let fixture: CursorProducerFixture = serde_json::from_str(include_str!(
        "../tests/fixtures/host-media-cursor-producer.json"
    ))
    .unwrap();
    assert_eq!(
        fixture.producer_commit,
        "a599246535b73f57e0425cbbfd18c7156e050bba"
    );
    assert_eq!(
        fixture
            .cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<Vec<_>>(),
        ["disabled", "enabled", "max_safe", "u64_overflow"]
    );
    let temporary = TemporaryDirectory::new();
    for case in fixture.cases {
        let mut expected: serde_json::Value = serde_json::from_str(&case.line).unwrap();
        assert_eq!(expected["schema"].as_u64(), Some(1), "{}", case.name);
        assert_eq!(expected["level"].as_str(), Some("warning"), "{}", case.name);
        assert_eq!(
            expected["event"].as_str(),
            Some("host_cursor_send"),
            "{}",
            case.name
        );
        assert_eq!(
            expected["reason"].as_str(),
            Some("capability_or_size"),
            "{}",
            case.name
        );
        let stream_id = expected["stream_id"].as_u64().unwrap();
        let send_attempts = expected["send_attempts"].as_u64().unwrap();
        let send_errors = expected["send_errors"].as_u64().unwrap();
        let disabled = expected["disabled"].as_bool().unwrap();
        let diagnostics = DiagnosticLog::new(temporary.0.join(format!("{}.jsonl", case.name)));
        let observer = host_media_diagnostics_observer(diagnostics.clone());
        observer(HostMediaDiagnostic::CursorSend {
            stream_id,
            send_attempts,
            send_errors,
            disabled,
            reason: CursorDisableReason::CapabilityOrSize,
        });
        drop(observer);
        let lines = diagnostics.recent_sanitized_lines().unwrap();
        assert_eq!(lines.len(), 1, "{}", case.name);
        let mut actual: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(
            actual["stream_id"].as_u64(),
            Some(stream_id),
            "{}",
            case.name
        );
        assert_eq!(
            actual["send_attempts"].as_u64(),
            Some(send_attempts),
            "{}",
            case.name
        );
        assert_eq!(
            actual["send_errors"].as_u64(),
            Some(send_errors),
            "{}",
            case.name
        );
        assert_eq!(
            actual["disabled"].as_bool(),
            Some(disabled),
            "{}",
            case.name
        );
        assert_eq!(actual["schema"].as_u64(), Some(1), "{}", case.name);
        assert_eq!(actual["level"].as_str(), Some("warning"), "{}", case.name);
        assert_eq!(
            actual["event"].as_str(),
            Some("host_cursor_send"),
            "{}",
            case.name
        );
        assert_eq!(
            actual["reason"].as_str(),
            Some("capability_or_size"),
            "{}",
            case.name
        );
        if case.name == "u64_overflow" {
            assert_eq!(
                (stream_id, send_attempts, send_errors),
                (u64::MAX, u64::MAX, u64::MAX)
            );
            assert!(lines[0].contains("\"stream_id\":18446744073709551615"));
            assert!(lines[0].contains("\"send_attempts\":18446744073709551615"));
            assert!(lines[0].contains("\"send_errors\":18446744073709551615"));
        }
        // Timestamp is the only dynamic field. Value equality below retains
        // every other key and its JSON type, rejecting extra or omitted fields.
        assert!(
            actual
                .as_object_mut()
                .unwrap()
                .remove("timestamp_unix_ms")
                .unwrap()
                .as_u64()
                .unwrap()
                > 0
        );
        assert!(
            expected
                .as_object_mut()
                .unwrap()
                .remove("timestamp_unix_ms")
                .unwrap()
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(actual, expected, "producer fixture case {}", case.name);
    }
}
