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
    ];
    const report = analyzeDiagnosticRows(rows, 24, NOW);
    expect(report.summary).toMatchObject({ sessions: 1, healthy: 1, warning: 0, error: 0 });
    expect(report.sessions[0]?.video).toEqual({
      received_packets: 220,
      dropped_packets: 3,
      completed_frames: 110,
      loss_percent: 1.35,
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

  test("reports an expired approval wait without inventing a media failure", () => {
    const rows = [event("controller_waiting_for_approval", -5 * 60_000)];
    const session = analyzeDiagnosticRows(rows, 24, NOW).sessions[0]!;
    expect(session.outcome).toBe("warning");
    expect(session.findings.map((finding) => finding.code)).toEqual(["approval_incomplete"]);
  });

  test("keeps uncorrelated rows out of session conclusions", () => {
    const row = event("host_available", 0, {}, null, "host");
    const report = analyzeDiagnosticRows([row], 24, NOW);
    expect(report.summary.sessions).toBe(0);
    expect(report.uncorrelated_rows).toBe(1);
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
