import { describe, expect, test } from "bun:test";

import { analyzeDiagnosticRows, type AnalysisEventRow } from "./analysis";
import { buildDiagnosticHealthReport } from "./health";

const NOW = Date.UTC(2026, 6, 18, 8, 0, 0);

describe("correlated diagnostic analysis", () => {
  test("recognizes a healthy video session and deduplicates cumulative metrics", () => {
    const rows = [
      event("controller_connected", 0),
      event("controller_video_metrics", 10_000, {
        attempt: 1,
        received_video_packets: 100,
        dropped_video_packets: 2,
        completed_frames: 50,
      }),
      event("controller_video_metrics", 20_000, {
        attempt: 1,
        received_video_packets: 220,
        dropped_video_packets: 3,
        completed_frames: 110,
      }),
      event("controller_render_metrics", 20_100, {
        stream_id: 2,
        received_frames: 110,
        submitted_frames: 108,
        displayed_frames: 105,
        malformed_frames: 0,
        decoder_recoveries: 1,
        first_frame_ms: 740,
      }),
    ];
    const report = analyzeDiagnosticRows(rows, 24, NOW);
    expect(report.summary).toMatchObject({ sessions: 1, healthy: 1, warning: 0, error: 0 });
    expect(report.sessions[0]?.video).toEqual({
      received_packets: 220,
      dropped_packets: 3,
      completed_frames: 110,
      delivered_frames: 0,
      video_ipc_overflow_drops: 0,
      video_ipc_keyframe_replacements: 0,
      loss_percent: 1.35,
      input_backpressure_count: 0,
    });
    expect(report.sessions[0]?.render).toEqual({
      received_frames: 110,
      submitted_frames: 108,
      displayed_frames: 105,
      malformed_frames: 0,
      decoder_recoveries: 1,
      video_pull_failures: 0,
      first_frame_ms: 740,
    });
    expect(report.sessions[0]?.findings[0]?.code).toBe("healthy_video");
  });

  test("finds reconnect oscillation, terminal failure and missing frames", () => {
    const rows = [
      event("controller_connected", 0),
      ...[1, 2, 3, 4].map((retry) => event("controller_retry_scheduled", retry * 1_000, {
        attempt: retry,
        retry,
        delay_ms: 1_000,
        reason: "transport_interrupted: connection closed",
      })),
      event("controller_video_metrics", 6_000, {
        attempt: 4,
        received_video_packets: 160,
        dropped_video_packets: 40,
        completed_frames: 0,
      }),
      event("controller_stopped", 7_000, {
        attempt: 5,
        reason: "transport_interrupted: retry budget exhausted",
      }),
    ];
    const session = analyzeDiagnosticRows(rows, 24, NOW).sessions[0]!;
    expect(session.outcome).toBe("error");
    expect(session.stop_reason_kinds).toEqual(["transport_interrupted"]);
    expect(session.findings.map((finding) => finding.code)).toEqual([
      "controller_stopped",
      "reconnect_oscillation",
      "no_completed_frame",
      "high_video_loss",
    ]);
  });

  test("does not call a user-cancelled pre-connection retry loop oscillation", () => {
    const rows = [
      event("controller_connecting", 0),
      ...[1, 2, 3, 4].map((retry) => event("controller_retry_scheduled", retry * 1_000, {
        attempt: retry,
        retry,
        delay_ms: 1_000,
        reason: "session_not_found: join rejected: SessionNotFound",
      })),
      event("controller_cancelled", 5_000),
    ];
    const session = analyzeDiagnosticRows(rows, 24, NOW).sessions[0]!;
    expect(session.outcome).toBe("incomplete");
    expect(session.findings.map((finding) => finding.code)).toEqual([
      "incomplete_evidence",
    ]);
  });

  test("reports an expired approval wait without inventing a media failure", () => {
    const rows = [event("controller_waiting_for_approval", -5 * 60_000)];
    const session = analyzeDiagnosticRows(rows, 24, NOW).sessions[0]!;
    expect(session.outcome).toBe("warning");
    expect(session.findings.map((finding) => finding.code)).toEqual(["approval_incomplete"]);
  });

  test("separates renderer failure, decoder recovery and input congestion", () => {
    const rows = [
      event("controller_connected", 0),
      event("controller_video_metrics", 10_000, {
        attempt: 1,
        received_video_packets: 200,
        dropped_video_packets: 0,
        completed_frames: 80,
        input_backpressure_count: 7,
      }),
      event("controller_render_metrics", 10_100, {
        stream_id: 1,
        received_frames: 40,
        submitted_frames: 38,
        displayed_frames: 0,
        malformed_frames: 0,
        decoder_recoveries: 1,
      }),
      event("controller_render_metrics", 20_100, {
        stream_id: 1,
        received_frames: 80,
        submitted_frames: 76,
        displayed_frames: 0,
        malformed_frames: 2,
        decoder_recoveries: 3,
      }),
    ];
    const session = analyzeDiagnosticRows(rows, 24, NOW).sessions[0]!;
    expect(session.outcome).toBe("error");
    expect(session.findings.map((finding) => finding.code)).toEqual([
      "no_displayed_frame",
      "decoder_instability",
      "input_backpressure",
    ]);
    expect(session.render.displayed_frames).toBe(0);
    expect(session.video.input_backpressure_count).toBe(7);
  });

  test("flags a slow first displayed frame without calling the session broken", () => {
    const rows = [
      event("controller_connected", 0),
      event("controller_video_metrics", 10_000, {
        attempt: 1,
        received_video_packets: 100,
        dropped_video_packets: 0,
        completed_frames: 40,
      }),
      event("controller_render_metrics", 10_100, {
        stream_id: 1,
        received_frames: 40,
        submitted_frames: 40,
        displayed_frames: 38,
        malformed_frames: 0,
        decoder_recoveries: 0,
        first_frame_ms: 6_200,
      }),
    ];
    const session = analyzeDiagnosticRows(rows, 24, NOW).sessions[0]!;
    expect(session.outcome).toBe("warning");
    expect(session.findings.map((finding) => finding.code)).toEqual(["slow_first_frame"]);
  });

  test("detects completed frames that never cross the native video mailbox", () => {
    const rows = [
      event("controller_connected", 0),
      event("controller_video_metrics", 10_000, {
        attempt: 1,
        received_video_packets: 180,
        dropped_video_packets: 0,
        completed_frames: 60,
        delivered_video_frames: 0,
        video_ipc_overflow_drops: 0,
        video_ipc_keyframe_replacements: 0,
      }),
    ];

    const session = analyzeDiagnosticRows(rows, 24, NOW).sessions[0]!;
    expect(session.outcome).toBe("error");
    expect(session.findings.map((finding) => finding.code)).toEqual(["video_ipc_stalled"]);
    expect(session.video.delivered_frames).toBe(0);
  });

  test("uses bounded thresholds for native mailbox pressure and frontend pull retries", () => {
    const lowRows = [
      event("controller_connected", 0),
      event("controller_video_metrics", 10_000, {
        attempt: 1,
        received_video_packets: 150,
        dropped_video_packets: 0,
        completed_frames: 50,
        delivered_video_frames: 48,
        video_ipc_overflow_drops: 2,
        video_ipc_keyframe_replacements: 0,
      }),
      event("controller_render_metrics", 10_100, {
        stream_id: 1,
        received_frames: 48,
        submitted_frames: 48,
        displayed_frames: 48,
        malformed_frames: 0,
        decoder_recoveries: 0,
        video_pull_failures: 2,
      }),
    ];
    const low = analyzeDiagnosticRows(lowRows, 24, NOW).sessions[0]!;
    expect(low.outcome).toBe("healthy");

    const thresholdRows = lowRows.map((row) => ({ ...row }));
    thresholdRows[1] = event("controller_video_metrics", 10_000, {
      attempt: 1,
      received_video_packets: 150,
      dropped_video_packets: 0,
      completed_frames: 50,
      delivered_video_frames: 47,
      video_ipc_overflow_drops: 2,
      video_ipc_keyframe_replacements: 1,
    });
    thresholdRows[2] = event("controller_render_metrics", 10_100, {
      stream_id: 1,
      received_frames: 47,
      submitted_frames: 47,
      displayed_frames: 47,
      malformed_frames: 0,
      decoder_recoveries: 0,
      video_pull_failures: 3,
    });
    const threshold = analyzeDiagnosticRows(thresholdRows, 24, NOW).sessions[0]!;
    expect(threshold.outcome).toBe("warning");
    expect(threshold.findings.map((finding) => finding.code)).toEqual([
      "video_ipc_pressure",
      "video_pull_instability",
    ]);
  });

  test("keeps uncorrelated rows out of session conclusions", () => {
    const row = event("host_available", 0, {}, null, "host");
    const report = analyzeDiagnosticRows([row], 24, NOW);
    expect(report.summary.sessions).toBe(0);
    expect(report.uncorrelated_rows).toBe(1);
  });

  test("a disabled optional cursor lane preserves healthy video evidence and never becomes packet loss or a stopped host", () => {
    const videoRows = [
      event("controller_connected", 0),
      event("controller_video_metrics", 10_000, {
        attempt: 1, received_video_packets: 200, dropped_video_packets: 0,
        completed_frames: 80, delivered_video_frames: 80,
      }),
      event("controller_render_metrics", 10_100, {
        stream_id: 7, received_frames: 80, submitted_frames: 80, displayed_frames: 80,
      }),
    ];
    const baseline = analyzeDiagnosticRows(videoRows, 24, NOW).sessions[0]!;
    const session = analyzeDiagnosticRows([...videoRows, cursorEvent({ send_attempts: 50_000, send_errors: 40_000 })], 24, NOW).sessions[0]!;
    expect(session.video).toEqual(baseline.video);
    expect(session.render).toEqual(baseline.render);
    expect(session.video.loss_percent).toBe(0);
    expect(session.connected_count).toBe(1);
    expect(session.stop_reason_kinds).toEqual([]);
    expect(session.outcome).toBe("warning");
    expect(session.findings.map(finding => finding.code)).toEqual(["healthy_video", "cursor_lane_disabled"]);
    expect(session.findings.find(finding => finding.code === "cursor_lane_disabled")?.severity).toBe("warning");
  });

  test("repeated cursor snapshots or shared stream IDs produce one categorical finding without invented peer totals", () => {
    const report = analyzeDiagnosticRows([
      cursorEvent(), cursorEvent(),
      cursorEvent({ send_attempts: 100, send_errors: 1 }, 1),
      cursorEvent({ stream_id: 7, send_attempts: 1, send_errors: 1 }, 2),
      cursorEvent({ stream_id: 8, send_attempts: 20, send_errors: 1 }, 3),
    ], 24, NOW);
    const session = report.sessions[0]!;
    expect(session.findings.map(finding => finding.code)).toEqual(["cursor_lane_disabled"]);
    expect(report.summary.finding_counts.cursor_lane_disabled).toBe(1);
    expect(session.outcome).toBe("warning");
    expect(session.connected_count).toBe(0);
    expect(session.stop_reason_kinds).toEqual([]);
    expect(session.video).toEqual({ received_packets: 0, dropped_packets: 0, completed_frames: 0,
      delivered_frames: 0, video_ipc_overflow_drops: 0, video_ipc_keyframe_replacements: 0,
      loss_percent: null, input_backpressure_count: 0 });
    expect(session).not.toHaveProperty("cursor_loss_percent");
    expect(session).not.toHaveProperty("cursor_delivery_rate");
    expect(session.findings[0]?.detail).not.toContain("100");
  });

  test("false, mismatched or malformed historical cursor payloads cannot produce a disabled-lane warning", () => {
    const invalidRows = [
      cursorEvent({ disabled: false }), cursorEvent({ disabled: "true" }), cursorEvent({ disabled: 1 }),
      cursorEvent({ reason: "connection_lost" }), cursorEvent({ reason: "capability_or_size: raw driver text" }),
      cursorEvent({ reason: null }), cursorEvent({ event: "host_stopped" }),
      { ...cursorEvent(), source: "controller" as const },
      { ...cursorEvent(), event: "controller_video_metrics" },
      { ...cursorEvent(), payload_json: "not JSON" },
      { ...cursorEvent(), payload_json: "[]" },
      ...["send_attempts", "send_errors", "stream_id"].flatMap(field => [
        cursorEvent({ [field]: "1" }), cursorEvent({ [field]: true }), cursorEvent({ [field]: null }), cursorEvent({ [field]: undefined }),
        cursorEvent({ [field]: -1 }), cursorEvent({ [field]: 1.5 }),
        cursorEvent({ [field]: Number.MAX_SAFE_INTEGER + 1 }), cursorEvent({ [field]: 0 }),
      ]),
      cursorEvent({ send_attempts: 1, send_errors: 2 }),
    ];
    for (const row of invalidRows) {
      const session = analyzeDiagnosticRows([row], 24, NOW).sessions[0]!;
      expect(session.findings.map(finding => finding.code)).not.toContain("cursor_lane_disabled");
      expect(session.findings.map(finding => finding.code)).not.toContain("host_stopped");
    }
  });

  test("raises attention for any error or three warning sessions", () => {
    const errorAnalysis = analyzeDiagnosticRows([
      event("controller_stopped", 0, { reason: "host_unavailable: capture failed" }),
    ], 24, NOW);
    expect(buildDiagnosticHealthReport(errorAnalysis)).toMatchObject({
      status: "attention",
      requires_attention: true,
    });

    const warningRows = ["a", "b", "c"].flatMap((prefix, index) => [
      event(
        "controller_waiting_for_approval",
        -5 * 60_000 + index,
        {},
        prefix.repeat(32),
      ),
    ]);
    const warningHealth = buildDiagnosticHealthReport(
      analyzeDiagnosticRows(warningRows, 24, NOW),
    );
    expect(warningHealth.summary.warning).toBe(3);
    expect(warningHealth.requires_attention).toBeTrue();
  });
});

function cursorEvent(payload: Record<string, unknown> = {}, offsetMs = 10_200): AnalysisEventRow {
  return { ...event("host_cursor_send", offsetMs, {
    level: "warning",
    stream_id: 7, send_attempts: 2, send_errors: 1, disabled: true,
    reason: "capability_or_size", ...payload,
  }, "a".repeat(32), "host"), level: "warning" };
}

function event(
  name: string,
  offsetMs: number,
  payload: Record<string, unknown> = {},
  correlationId: string | null = "a".repeat(32),
  source: "host" | "controller" = "controller",
): AnalysisEventRow {
  return {
    timestamp_unix_ms: NOW + offsetMs,
    correlation_id: correlationId,
    app_version: "0.1.25",
    source,
    level: name.endsWith("stopped") ? "error" : "info",
    event: name,
    payload_json: JSON.stringify({
      schema: 1,
      timestamp_unix_ms: NOW + offsetMs,
      level: "info",
      event: name,
      ...payload,
    }),
  };
}
