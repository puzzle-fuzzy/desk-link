export type DiagnosticSource = "host" | "controller";
export type DiagnosticLevel = "info" | "warning" | "error";
export type FindingSeverity = "info" | "warning" | "error";

export interface AnalysisEventRow {
  timestamp_unix_ms: number;
  correlation_id: string | null;
  app_version: string;
  source: DiagnosticSource;
  level: DiagnosticLevel;
  event: string;
  payload_json: string;
}

export interface SessionFinding {
  code:
    | "healthy_video"
    | "controller_stopped"
    | "host_stopped"
    | "reconnect_oscillation"
    | "approval_incomplete"
    | "no_completed_frame"
    | "high_video_loss"
    | "incomplete_evidence";
  severity: FindingSeverity;
  title: string;
  detail: string;
}

export interface SessionAnalysis {
  correlation_id: string;
  started_at_unix_ms: number;
  ended_at_unix_ms: number;
  duration_ms: number;
  app_versions: string[];
  sources: DiagnosticSource[];
  event_count: number;
  retry_count: number;
  connected_count: number;
  video: {
    received_packets: number;
    dropped_packets: number;
    completed_frames: number;
    loss_percent: number | null;
  };
  stop_reason_kinds: string[];
  outcome: "healthy" | "warning" | "error" | "incomplete";
  findings: SessionFinding[];
}

export interface DiagnosticAnalysisReport {
  schema: 1;
  generated_at: string;
  hours: number;
  total_rows: number;
  uncorrelated_rows: number;
  summary: {
    sessions: number;
    healthy: number;
    warning: number;
    error: number;
    incomplete: number;
    finding_counts: Record<string, number>;
  };
  sessions: SessionAnalysis[];
}

interface ParsedEvent extends AnalysisEventRow {
  payload: Record<string, unknown>;
}

interface VideoAttempt {
  received: number;
  dropped: number;
  completed: number;
}

const OSCILLATION_RETRY_COUNT = 4;
const HIGH_LOSS_MINIMUM_PACKETS = 100;
const HIGH_LOSS_PERCENT = 10;
const STALE_SESSION_MS = 2 * 60 * 1_000;

export function analyzeDiagnosticRows(
  rows: AnalysisEventRow[],
  hours: number,
  nowUnixMs = Date.now(),
): DiagnosticAnalysisReport {
  const grouped = new Map<string, ParsedEvent[]>();
  let uncorrelatedRows = 0;
  for (const row of rows) {
    if (row.correlation_id === null) {
      uncorrelatedRows += 1;
      continue;
    }
    const event: ParsedEvent = { ...row, payload: parsePayload(row.payload_json) };
    const group = grouped.get(row.correlation_id) ?? [];
    group.push(event);
    grouped.set(row.correlation_id, group);
  }

  const sessions = [...grouped.entries()]
    .map(([correlationId, events]) => analyzeSession(correlationId, events, nowUnixMs))
    .sort((left, right) => right.ended_at_unix_ms - left.ended_at_unix_ms);
  const findingCounts: Record<string, number> = {};
  for (const session of sessions) {
    for (const finding of session.findings) {
      findingCounts[finding.code] = (findingCounts[finding.code] ?? 0) + 1;
    }
  }
  return {
    schema: 1,
    generated_at: new Date(nowUnixMs).toISOString(),
    hours,
    total_rows: rows.length,
    uncorrelated_rows: uncorrelatedRows,
    summary: {
      sessions: sessions.length,
      healthy: sessions.filter((session) => session.outcome === "healthy").length,
      warning: sessions.filter((session) => session.outcome === "warning").length,
      error: sessions.filter((session) => session.outcome === "error").length,
      incomplete: sessions.filter((session) => session.outcome === "incomplete").length,
      finding_counts: Object.fromEntries(
        Object.entries(findingCounts).sort(([left], [right]) => left.localeCompare(right)),
      ),
    },
    sessions,
  };
}

function analyzeSession(
  correlationId: string,
  events: ParsedEvent[],
  nowUnixMs: number,
): SessionAnalysis {
  events.sort((left, right) => left.timestamp_unix_ms - right.timestamp_unix_ms);
  const startedAt = events[0]?.timestamp_unix_ms ?? nowUnixMs;
  const endedAt = events.at(-1)?.timestamp_unix_ms ?? startedAt;
  const count = (name: string) => events.filter((event) => event.event === name).length;
  const retryCount = count("controller_retry_scheduled");
  const connectedCount = count("controller_connected");
  const controllerStopped = events.filter((event) => event.event === "controller_stopped");
  const hostStopped = events.filter((event) => event.event === "host_stopped");
  const videoAttempts = videoByAttempt(events);
  const video = [...videoAttempts.values()].reduce(
    (total, attempt) => ({
      received: total.received + attempt.received,
      dropped: total.dropped + attempt.dropped,
      completed: total.completed + attempt.completed,
    }),
    { received: 0, dropped: 0, completed: 0 },
  );
  const packetTotal = video.received + video.dropped;
  const lossPercent = packetTotal > 0 ? roundPercent(video.dropped, packetTotal) : null;
  const stopReasonKinds = [...controllerStopped, ...hostStopped]
    .map((event) => reasonKind(event.payload.reason))
    .filter((value): value is string => value !== null);
  const findings: SessionFinding[] = [];

  if (controllerStopped.length > 0) {
    findings.push({
      code: "controller_stopped",
      severity: "error",
      title: "控制端已停止",
      detail: stopReasonKinds.length > 0
        ? `停止分类：${[...new Set(stopReasonKinds)].join("、")}`
        : "控制端记录了不可继续恢复的停止事件。",
    });
  }
  if (hostStopped.length > 0) {
    findings.push({
      code: "host_stopped",
      severity: "error",
      title: "主机服务已停止",
      detail: "主机记录了停止事件，需要按同一关联编号检查停止前的恢复阶段。",
    });
  }
  if (retryCount >= OSCILLATION_RETRY_COUNT) {
    findings.push({
      code: "reconnect_oscillation",
      severity: "warning",
      title: "连接发生重试振荡",
      detail: `同一会话已安排 ${retryCount} 次重试，需要检查中继、握手和主机恢复事件的交替顺序。`,
    });
  }
  const approvalIncomplete = count("controller_waiting_for_approval") > 0
    && count("controller_secure_session_ready") === 0
    && (controllerStopped.length > 0 || nowUnixMs - endedAt >= STALE_SESSION_MS);
  if (approvalIncomplete) {
    findings.push({
      code: "approval_incomplete",
      severity: "warning",
      title: "主机确认未完成",
      detail: "控制端进入等待批准，但没有进入安全会话就结束或超过两分钟。",
    });
  }
  const noCompletedFrame = connectedCount > 0
    && video.completed === 0
    && (videoAttempts.size > 0 || nowUnixMs - endedAt >= STALE_SESSION_MS);
  if (noCompletedFrame) {
    findings.push({
      code: "no_completed_frame",
      severity: "error",
      title: "连接后没有完整画面",
      detail: video.received > 0
        ? `已收到 ${video.received} 个视频包，但没有完成一帧，优先检查分片、关键帧和解码恢复。`
        : "连接已建立但没有收到可组成画面的媒体数据，优先检查主机采集与编码。",
    });
  }
  if (packetTotal >= HIGH_LOSS_MINIMUM_PACKETS && lossPercent !== null && lossPercent >= HIGH_LOSS_PERCENT) {
    findings.push({
      code: "high_video_loss",
      severity: "warning",
      title: "视频丢包率过高",
      detail: `累计视频丢包率为 ${lossPercent.toFixed(1)}%，超过 ${HIGH_LOSS_PERCENT}% 阈值。`,
    });
  }

  if (connectedCount > 0 && video.completed > 0 && !findings.some(isAlertFinding)) {
    findings.push({
      code: "healthy_video",
      severity: "info",
      title: "远程画面链路正常",
      detail: `已完成 ${video.completed} 帧，未发现停止、振荡或高丢包。`,
    });
  }
  if (findings.length === 0) {
    findings.push({
      code: "incomplete_evidence",
      severity: "info",
      title: "诊断证据尚不完整",
      detail: "当前事件不足以确认成功或定位失败，等待两端补传后再分析。",
    });
  }

  const outcome = findings.some((finding) => finding.severity === "error")
    ? "error"
    : findings.some((finding) => finding.severity === "warning")
      ? "warning"
      : findings.some((finding) => finding.code === "healthy_video")
        ? "healthy"
        : "incomplete";

  return {
    correlation_id: correlationId,
    started_at_unix_ms: startedAt,
    ended_at_unix_ms: endedAt,
    duration_ms: Math.max(0, endedAt - startedAt),
    app_versions: [...new Set(events.map((event) => event.app_version))].sort(),
    sources: [...new Set(events.map((event) => event.source))].sort(),
    event_count: events.length,
    retry_count: retryCount,
    connected_count: connectedCount,
    video: {
      received_packets: video.received,
      dropped_packets: video.dropped,
      completed_frames: video.completed,
      loss_percent: lossPercent,
    },
    stop_reason_kinds: [...new Set(stopReasonKinds)].sort(),
    outcome,
    findings,
  };
}

function videoByAttempt(events: ParsedEvent[]): Map<number, VideoAttempt> {
  const attempts = new Map<number, VideoAttempt>();
  for (const event of events) {
    if (event.event !== "controller_video_metrics") continue;
    const attemptId = safeInteger(event.payload.attempt) ?? 0;
    const current = attempts.get(attemptId) ?? { received: 0, dropped: 0, completed: 0 };
    attempts.set(attemptId, {
      received: Math.max(current.received, safeInteger(event.payload.received_video_packets) ?? 0),
      dropped: Math.max(current.dropped, safeInteger(event.payload.dropped_video_packets) ?? 0),
      completed: Math.max(current.completed, safeInteger(event.payload.completed_frames) ?? 0),
    });
  }
  return attempts;
}

function parsePayload(value: string): Record<string, unknown> {
  try {
    const parsed: unknown = JSON.parse(value);
    return typeof parsed === "object" && parsed !== null && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : {};
  } catch {
    return {};
  }
}

function safeInteger(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? value
    : null;
}

function reasonKind(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const candidate = value.split(":", 1)[0]?.trim().toLowerCase() ?? "";
  return /^[a-z][a-z0-9_]{1,63}$/.test(candidate) ? candidate : null;
}

function roundPercent(part: number, total: number): number {
  return Math.round((part * 10_000) / total) / 100;
}

function isAlertFinding(finding: SessionFinding): boolean {
  return finding.severity === "warning" || finding.severity === "error";
}
