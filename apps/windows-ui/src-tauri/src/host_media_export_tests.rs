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
    diagnostics::{DiagnosticLog, diagnostic_u64},
    runtime::{CursorDisableReason, DirectListenerStopReason, HostMediaDiagnostic},
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
    assert_eq!(event["stream_id"], u64::MAX.to_string());
    assert_eq!(event["send_attempts"], "17");
    assert_eq!(event["send_errors"], "1");
    assert_eq!(event["disabled"], true);
    assert_eq!(event["reason"], "capability_or_size");
    assert!(lines[0].contains("\"stream_id\":\"18446744073709551615\""));

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
    assert!(report.contains("\"send_attempts\":\"17\""));
    assert!(report.contains("\"send_errors\":\"1\""));
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
struct HostMediaProducerFixture {
    schema: u32,
    cases: Vec<HostMediaProducerCase>,
}

#[derive(Deserialize)]
struct HostMediaProducerCase {
    name: String,
    configured_stream_id: String,
    log_lines: Vec<String>,
}

fn producer_fixture() -> HostMediaProducerFixture {
    // One fixture comes from the real Rust logger and SignedRequest producer.
    // These full-u64 cases are valid canonical strings. This test performs no
    // HTTP request and does not establish interactive GUI or cloud acceptance.
    let fixture: HostMediaProducerFixture = serde_json::from_str(include_str!(
        "../../../windows/tests/fixtures/diagnostic-u64-producer.json"
    ))
    .unwrap();
    assert_eq!(fixture.schema, 2);
    assert_eq!(
        fixture
            .cases
            .iter()
            .map(|case| case.name.as_str())
            .collect::<Vec<_>>(),
        [
            "safe",
            "two_pow_53",
            "two_pow_53_plus_one",
            "u64_max",
            "all_u64_max",
            "zero_counters",
        ]
    );
    fixture
}

fn canonical_u64_field(event: &serde_json::Value, field: &str, case: &str) -> u64 {
    let text = event[field]
        .as_str()
        .unwrap_or_else(|| panic!("{case}: {field} must be a JSON string"));
    let integer = diagnostic_u64(&event[field])
        .unwrap_or_else(|| panic!("{case}: {field} must be a canonical u64"));
    assert_eq!(text, integer.to_string(), "{case}: {field}");
    integer
}

fn producer_event(case: &HostMediaProducerCase, event_name: &str) -> serde_json::Value {
    let mut matches = case
        .log_lines
        .iter()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["event"].as_str() == Some(event_name));
    let event = matches
        .next()
        .unwrap_or_else(|| panic!("{}: missing {event_name}", case.name));
    assert!(
        matches.next().is_none(),
        "{}: duplicate {event_name}",
        case.name
    );
    assert_eq!(event["schema"], 1, "{}", case.name);
    assert_eq!(event["level"], "warning", "{}", case.name);
    let stream_id = canonical_u64_field(&event, "stream_id", &case.name);
    assert_eq!(
        case.configured_stream_id,
        stream_id.to_string(),
        "{}: configured stream must match the emitted event",
        case.name
    );
    event
}

fn without_timestamp(mut event: serde_json::Value) -> serde_json::Value {
    assert!(
        event
            .as_object_mut()
            .unwrap()
            .remove("timestamp_unix_ms")
            .unwrap()
            .as_u64()
            .unwrap()
            > 0
    );
    event
}

fn report_with_host_line(lines: Vec<String>, report_id: &str) -> String {
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
    let report = build_diagnostic_report(&snapshot, &controller, report_id, 123, &merged);
    // The actual callback line must survive merging and final report redaction,
    // without changing its string-valued identifiers or local counters.
    assert!(report.contains(&merged[0]));
    assert!(report.contains("运行状态: stopped"));
    assert!(report.contains("控制端尚未捕获足够的视频渲染指标。"));
    assert!(!report.contains("\"endpoint\""));
    assert!(!report.contains("\"error\""));
    assert_eq!(snapshot.runtime.state, "stopped");
    report
}

#[test]
fn cursor_observer_matches_the_shared_producer_fixture_with_exact_u64_strings() {
    let fixture = producer_fixture();
    let temporary = TemporaryDirectory::new();
    for case in fixture.cases {
        let expected = producer_event(&case, "host_cursor_send");
        assert_eq!(expected["reason"], "capability_or_size", "{}", case.name);
        let stream_id = canonical_u64_field(&expected, "stream_id", &case.name);
        let send_attempts = canonical_u64_field(&expected, "send_attempts", &case.name);
        let send_errors = canonical_u64_field(&expected, "send_errors", &case.name);
        let disabled = expected["disabled"].as_bool().unwrap();
        match case.name.as_str() {
            "safe" => assert_eq!(stream_id, (1_u64 << 53) - 1),
            "two_pow_53" => assert_eq!(stream_id, 1_u64 << 53),
            "two_pow_53_plus_one" => assert_eq!(stream_id, (1_u64 << 53) + 1),
            "u64_max" => assert_eq!(stream_id, u64::MAX),
            "all_u64_max" => assert_eq!((send_attempts, send_errors), (u64::MAX, u64::MAX)),
            "zero_counters" => assert_eq!((send_attempts, send_errors), (0, 0)),
            _ => unreachable!(),
        }
        let path = temporary.0.join(format!("cursor-{}.jsonl", case.name));
        let diagnostics = DiagnosticLog::new(&path);
        let observer = host_media_diagnostics_observer(diagnostics.clone());
        assert!(!path.exists());
        observer(HostMediaDiagnostic::CursorSend {
            stream_id,
            send_attempts,
            send_errors,
            disabled,
            reason: CursorDisableReason::CapabilityOrSize,
        });
        drop(observer);
        assert!(fs::metadata(&path).unwrap().len() > 0);
        let lines = diagnostics.recent_sanitized_lines().unwrap();
        assert_eq!(lines.len(), 1, "{}", case.name);
        let actual: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(
            (
                canonical_u64_field(&actual, "stream_id", &case.name),
                canonical_u64_field(&actual, "send_attempts", &case.name),
                canonical_u64_field(&actual, "send_errors", &case.name),
            ),
            (stream_id, send_attempts, send_errors),
            "{}",
            case.name
        );
        // Timestamp is the sole dynamic field. Value equality checks every
        // remaining key and its JSON type, including reason and disabled.
        assert_eq!(
            without_timestamp(actual),
            without_timestamp(expected),
            "producer fixture case {}",
            case.name
        );
        let report = report_with_host_line(lines, "DL-WIN-CURSOR-PRODUCER-TEST");
        assert!(report.contains(&format!("\"stream_id\":\"{stream_id}\"")));
        assert!(report.contains(&format!("\"send_attempts\":\"{send_attempts}\"")));
        assert!(report.contains(&format!("\"send_errors\":\"{send_errors}\"")));
        assert!(report.contains(&format!("\"disabled\":{disabled}")));
        assert!(report.contains("\"reason\":\"capability_or_size\""));
        assert!(!report.contains("\"total_drops\""));
        assert!(!report.contains("\"delivery_rate\""));
        assert!(!report.contains("\"network_loss\""));
    }
}

#[test]
fn listener_observer_matches_the_shared_producer_fixture_and_preserves_report_strings() {
    let fixture = producer_fixture();
    let temporary = TemporaryDirectory::new();
    for case in fixture.cases {
        let expected = producer_event(&case, "host_direct_listener_stopped");
        assert_eq!(expected["reason"], "listener_closed", "{}", case.name);
        let stream_id = canonical_u64_field(&expected, "stream_id", &case.name);
        let retired_active = expected["retired_active"].as_bool().unwrap();
        let path = temporary.0.join(format!("listener-{}.jsonl", case.name));
        let diagnostics = DiagnosticLog::new(&path);
        let observer = host_media_diagnostics_observer(diagnostics.clone());
        assert!(!path.exists());
        observer(HostMediaDiagnostic::DirectListenerStopped {
            stream_id,
            retired_active,
            reason: DirectListenerStopReason::ListenerClosed,
        });
        drop(observer);
        assert!(fs::metadata(&path).unwrap().len() > 0);
        let lines = diagnostics.recent_sanitized_lines().unwrap();
        assert_eq!(lines.len(), 1, "{}", case.name);
        let actual: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(
            actual
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "schema",
                "timestamp_unix_ms",
                "level",
                "event",
                "stream_id",
                "retired_active",
                "reason",
            ]),
            "{}",
            case.name
        );
        assert_eq!(
            canonical_u64_field(&actual, "stream_id", &case.name),
            stream_id,
            "{}",
            case.name
        );
        // Listener closure remains a historical event, with no claim that a
        // current session recovered. Only the dynamic timestamp may differ.
        assert_eq!(
            without_timestamp(actual),
            without_timestamp(expected),
            "producer fixture case {}",
            case.name
        );
        let report = report_with_host_line(lines, "DL-WIN-LISTENER-PRODUCER-TEST");
        assert!(report.contains(&format!("\"stream_id\":\"{stream_id}\"")));
        assert!(report.contains(&format!("\"retired_active\":{retired_active}")));
        assert!(report.contains("\"reason\":\"listener_closed\""));
        assert!(!report.contains("host_direct_listener_recovered"));
        assert!(!report.contains("\"healthy\""));
        assert!(!report.contains("\"recovered\""));
    }
}
