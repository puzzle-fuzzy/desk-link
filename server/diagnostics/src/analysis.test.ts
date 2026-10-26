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
        cursorEvent({ [field]: "01" }), cursorEvent({ [field]: true }), cursorEvent({ [field]: null }), cursorEvent({ [field]: undefined }),
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

  test("a stopped direct listener keeps healthy video and cursor findings separate without claiming host failure or network loss", () => {
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
    const session = analyzeDiagnosticRows([...videoRows, cursorEvent(), directListenerEvent()], 24, NOW).sessions[0]!;
    expect(session.video).toEqual(baseline.video);
    expect(session.render).toEqual(baseline.render);
    expect(session.connected_count).toBe(1);
    expect(session.stop_reason_kinds).toEqual([]);
    expect(session.outcome).toBe("warning");
    expect(session.findings.map(finding => finding.code)).toEqual(["healthy_video", "cursor_lane_disabled", "direct_listener_stopped"]);
    expect(session.findings.find(finding => finding.code === "direct_listener_stopped")).toMatchObject({ severity: "warning", title: "直连监听器已关闭" });
  });

  test("listener termination with or without a selected direct path produces one fixed warning without inferred fallback success", () => {
    for (const retired of [true, false]) {
      const rows = [directListenerEvent({ retired_active: retired }), directListenerEvent({ retired_active: retired }),
        directListenerEvent({ retired_active: retired, stream_id: 8 }, 1)];
      const report = analyzeDiagnosticRows(rows, 24, NOW);
      const session = report.sessions[0]!;
      expect(session.findings.map(finding => finding.code)).toEqual(["direct_listener_stopped"]);
      expect(report.summary.finding_counts.direct_listener_stopped).toBe(1);
      expect(session.outcome).toBe("warning");
      expect(session.connected_count).toBe(0);
      expect(session.video.loss_percent).toBeNull();
      expect(session.video.received_packets).toBe(0);
      expect(session.video.dropped_packets).toBe(0);
      expect(session.stop_reason_kinds).toEqual([]);
      expect(session.findings[0]?.detail).toContain("是否恢复需结合后续媒体事件确认");
    }
  });

  test("malformed or unrelated historical listener payloads cannot report listener termination", () => {
    const invalidRows = [
      directListenerEvent({ retired_active: "true" }), directListenerEvent({ retired_active: 1 }),
      directListenerEvent({ retired_active: null }), directListenerEvent({ retired_active: undefined }),
      directListenerEvent({ reason: "capability_or_size" }), directListenerEvent({ reason: "listener_closed: synthetic raw error" }),
      directListenerEvent({ reason: null }), directListenerEvent({ event: "host_stopped" }),
      directListenerEvent({ schema: 2 }), directListenerEvent({ level: "info" }),
      { ...directListenerEvent(), source: "controller" as const },
      { ...directListenerEvent(), level: "error" as const },
      { ...directListenerEvent(), event: "controller_video_metrics" },
      { ...directListenerEvent(), payload_json: "not JSON" },
      { ...directListenerEvent(), payload_json: "[]" },
      ...["07", true, null, undefined, -1, 0, 1.5, Number.MAX_SAFE_INTEGER + 1].map(stream_id => directListenerEvent({ stream_id })),
    ];
    for (const row of invalidRows) {
      const session = analyzeDiagnosticRows([row], 24, NOW).sessions[0]!;
      expect(session.findings.map(finding => finding.code)).not.toContain("direct_listener_stopped");
      expect(session.findings.map(finding => finding.code)).not.toContain("host_stopped");
      expect(session.stop_reason_kinds).toEqual([]);
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

  test("canonical render keys merge legacy numeric IDs with decimal strings while adjacent large streams remain distinct", () => {
    const rows = [
      event("controller_render_metrics", 0, { stream_id: 7, received_frames: 10, submitted_frames: 10, displayed_frames: 9 }),
      event("controller_render_metrics", 1, { stream_id: "7", received_frames: "15", submitted_frames: "15", displayed_frames: "14" }),
      event("controller_render_metrics", 2, { stream_id: "9007199254740992", received_frames: "20", submitted_frames: "20", displayed_frames: "18" }),
      event("controller_render_metrics", 3, { stream_id: "9007199254740993", received_frames: "30", submitted_frames: "30", displayed_frames: "29" }),
      // Invalid historical IDs retain the explicit legacy bucket, without
      // swallowing either of the valid large decimal stream identities.
      event("controller_render_metrics", 4, { stream_id: "07", received_frames: 4 }),
      event("controller_render_metrics", 5, { received_frames: 6 }),
    ];
    const session = analyzeDiagnosticRows(rows, 24, NOW).sessions[0]!;
    expect(session.render.received_frames).toBe(71);
    expect(session.render.submitted_frames).toBe(65);
    expect(session.render.displayed_frames).toBe(61);
  });

  test("video u64 snapshots keep exact maxima and cross-attempt totals larger than u64 without exposing BigInt", () => {
    const max = "18446744073709551615";
    const snapshot = {
      stream_id: max, received_video_packets: max, dropped_video_packets: max,
      completed_frames: max, delivered_video_frames: max, video_ipc_overflow_drops: max,
      video_ipc_keyframe_replacements: max, input_backpressure_count: max,
    };
    const rows = [
      event("controller_connected", 0),
      event("controller_video_metrics", 1, { ...snapshot, attempt: 1, received_video_packets: "18446744073709551614" }),
      event("controller_video_metrics", 2, { ...snapshot, attempt: 1 }),
      event("controller_video_metrics", 3, { ...snapshot, attempt: 1 }),
      event("controller_video_metrics", 4, { ...snapshot, attempt: 2 }),
    ];
    const report = analyzeDiagnosticRows(rows, 24, NOW);
    const session = report.sessions[0]!;
    const total = "36893488147419103230";
    expect(session.video).toEqual({
      received_packets: total, dropped_packets: total, completed_frames: total,
      delivered_frames: total, video_ipc_overflow_drops: total,
      video_ipc_keyframe_replacements: total, input_backpressure_count: total, loss_percent: 50,
    });
    expect(session.findings.map(finding => finding.code)).toEqual(["high_video_loss", "video_ipc_pressure", "input_backpressure"]);
    expect(() => JSON.stringify(report)).not.toThrow();
    expect(JSON.parse(JSON.stringify(report)).sessions[0].video.received_packets).toBe(total);
    expect(() => JSON.stringify(buildDiagnosticHealthReport(report))).not.toThrow();
  });

  test("render u64 maxima, totals and first-frame duration stay exact while unchanged u32 counters accept only numbers", () => {
    const max = "18446744073709551615";
    const snapshot = {
      received_frames: max, submitted_frames: max, displayed_frames: max, malformed_frames: max,
      first_frame_ms: max, decoder_recoveries: 4_294_967_295, video_pull_failures: 4_294_967_295,
      coalesced_frame_drops: max, max_frame_gap_ms: max,
    };
    const report = analyzeDiagnosticRows([
      event("controller_render_metrics", 1, { ...snapshot, stream_id: "9007199254740992", received_frames: "18446744073709551614" }),
      event("controller_render_metrics", 2, { ...snapshot, stream_id: "9007199254740992" }),
      event("controller_render_metrics", 3, { ...snapshot, stream_id: "9007199254740993" }),
    ], 24, NOW);
    const session = report.sessions[0]!;
    expect(session.render).toEqual({
      received_frames: "36893488147419103230", submitted_frames: "36893488147419103230",
      displayed_frames: "36893488147419103230", malformed_frames: "36893488147419103230",
      first_frame_ms: max, decoder_recoveries: 8_589_934_590, video_pull_failures: 8_589_934_590,
    });
    expect(session.findings.map(finding => finding.code)).toEqual(["decoder_instability", "video_pull_instability", "slow_first_frame"]);
    expect(session.render).not.toHaveProperty("coalesced_frame_drops");
    expect(session.render).not.toHaveProperty("max_frame_gap_ms");
    const health = JSON.parse(JSON.stringify(buildDiagnosticHealthReport(report)));
    expect(health.alert_sessions[0].render.first_frame_ms).toBe(max);

    const unchangedU32 = analyzeDiagnosticRows([event("controller_render_metrics", 1, {
      stream_id: "7", decoder_recoveries: "3", video_pull_failures: "3",
    })], 24, NOW).sessions[0]!;
    expect(unchangedU32.render.decoder_recoveries).toBe(0);
    expect(unchangedU32.render.video_pull_failures).toBe(0);
    expect(unchangedU32.findings.map(finding => finding.code)).toEqual(["incomplete_evidence"]);
  });

  test("loss percentages use bounded exact rounding and high-loss thresholds compare the original integer ratio", () => {
    const analyze = (received: string, dropped: string) => analyzeDiagnosticRows([
      event("controller_connected", 0),
      event("controller_video_metrics", 1, { attempt: 1, received_video_packets: received,
        dropped_video_packets: dropped, completed_frames: "1", delivered_video_frames: "1" }),
    ], 24, NOW).sessions[0]!;
    const below = analyze("9000000000000000001", "1000000000000000000");
    expect(below.video.loss_percent).toBe(10);
    expect(below.findings.map(finding => finding.code)).toEqual(["healthy_video"]);
    const exact = analyze("9000000000000000000", "1000000000000000000");
    expect(exact.video.loss_percent).toBe(10);
    expect(exact.findings.map(finding => finding.code)).toEqual(["high_video_loss"]);
    const above = analyze("8999999999999999999", "1000000000000000000");
    expect(above.findings.map(finding => finding.code)).toEqual(["high_video_loss"]);
    expect(analyze("9300000000000000000", "300000000000000000").video.loss_percent).toBe(3.13);
    expect(analyze("0", "18446744073709551615").video.loss_percent).toBe(100);
    expect(analyze("18446744073709551615", "1").video.loss_percent).toBe(0);
  });

  test("decimal first-frame and backpressure thresholds retain their exact inclusive boundaries", () => {
    const analyze = (firstFrame: string, pressure: string) => analyzeDiagnosticRows([
      event("controller_connected", 0),
      event("controller_video_metrics", 1, { attempt: 1, completed_frames: "1", input_backpressure_count: pressure }),
      event("controller_render_metrics", 2, { stream_id: "7", received_frames: "1", displayed_frames: "1", first_frame_ms: firstFrame }),
    ], 24, NOW).sessions[0]!;
    expect(analyze("4999", "4").findings.map(finding => finding.code)).toEqual(["healthy_video"]);
    const threshold = analyze("5000", "5");
    expect(threshold.findings.map(finding => finding.code)).toEqual(["input_backpressure", "slow_first_frame"]);
    expect(threshold.render.first_frame_ms).toBe(5000);
    expect(threshold.video.input_backpressure_count).toBe(5);
  });

  test("full-u64 host-media fields compare exactly without inferring cursor delivery or network loss", () => {
    const max = "18446744073709551615";
    for (const [attempts, errors] of [[max, max], ["9007199254740993", "9007199254740992"]]) {
      const session = analyzeDiagnosticRows([
        cursorEvent({ stream_id: max, send_attempts: attempts, send_errors: errors }),
        directListenerEvent({ stream_id: max, retired_active: false }),
      ], 24, NOW).sessions[0]!;
      expect(session.findings.map(finding => finding.code)).toEqual(["cursor_lane_disabled", "direct_listener_stopped"]);
      expect(session.video.loss_percent).toBeNull();
      expect(session.video.dropped_packets).toBe(0);
      expect(session.stop_reason_kinds).toEqual([]);
      expect(() => JSON.stringify(session)).not.toThrow();
    }
    for (const [attempts, errors] of [["18446744073709551614", max], ["9007199254740992", "9007199254740993"]]) {
      const session = analyzeDiagnosticRows([cursorEvent({ stream_id: max, send_attempts: attempts, send_errors: errors })], 24, NOW).sessions[0]!;
      expect(session.findings.map(finding => finding.code)).not.toContain("cursor_lane_disabled");
    }
  });

  test("host-media analysis rejects noncanonical decimal strings rather than coercing them into u64 values", () => {
    const malformed = ["", "01", "+1", "-1", "-0", "1.0", "1e3", " 1", "1 ", "0x1", "1_000", "１２",
      "18446744073709551616", "100000000000000000000"];
    for (const value of malformed) {
      for (const field of ["stream_id", "send_attempts", "send_errors"]) {
        const session = analyzeDiagnosticRows([cursorEvent({ [field]: value })], 24, NOW).sessions[0]!;
        expect(session.findings.map(finding => finding.code)).not.toContain("cursor_lane_disabled");
      }
      const session = analyzeDiagnosticRows([directListenerEvent({ stream_id: value })], 24, NOW).sessions[0]!;
      expect(session.findings.map(finding => finding.code)).not.toContain("direct_listener_stopped");
    }
  });
});

function cursorEvent(payload: Record<string, unknown> = {}, offsetMs = 10_200): AnalysisEventRow {
  return { ...event("host_cursor_send", offsetMs, {
    level: "warning",
    stream_id: 7, send_attempts: 2, send_errors: 1, disabled: true,
    reason: "capability_or_size", ...payload,
  }, "a".repeat(32), "host"), level: "warning" };
}

function directListenerEvent(payload: Record<string, unknown> = {}, offsetMs = 10_300): AnalysisEventRow {
  return { ...event("host_direct_listener_stopped", offsetMs, {
    level: "warning", stream_id: 7, retired_active: true, reason: "listener_closed", ...payload,
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
