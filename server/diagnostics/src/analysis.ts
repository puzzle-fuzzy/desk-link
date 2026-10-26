import { canonicalU64, jsonExactUnsigned, parseU64 } from "./diagnostic-u64";

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
    | "video_ipc_stalled"
    | "no_displayed_frame"
    | "high_video_loss"
    | "video_ipc_pressure"
    | "decoder_instability"
    | "video_pull_instability"
    | "input_backpressure"
    | "slow_first_frame"
    | "cursor_lane_disabled"
    | "direct_listener_stopped"
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
    received_packets: number | string;
    dropped_packets: number | string;
    completed_frames: number | string;
    delivered_frames: number | string;
    video_ipc_overflow_drops: number | string;
    video_ipc_keyframe_replacements: number | string;
    loss_percent: number | null;
    input_backpressure_count: number | string;
  };
  render: {
    received_frames: number | string;
    submitted_frames: number | string;
    displayed_frames: number | string;
    malformed_frames: number | string;
    decoder_recoveries: number | string;
    video_pull_failures: number | string;
    first_frame_ms: number | string | null;
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
  received: bigint;
  dropped: bigint;
  completed: bigint;
  delivered: bigint;
  ipcOverflowDrops: bigint;
  ipcKeyframeReplacements: bigint;
  mailboxSamples: number;
  inputBackpressure: bigint;
}

interface RenderStream {
  received: bigint;
  submitted: bigint;
  displayed: bigint;
  malformed: bigint;
  recoveries: bigint;
  pullFailures: bigint;
  firstFrameMs: bigint | null;
  samples: number;
}

const OSCILLATION_RETRY_COUNT = 4;
const HIGH_LOSS_MINIMUM_PACKETS = 100n;
const HIGH_LOSS_PERCENT = 10n;
const VIDEO_IPC_PRESSURE_WARNING = 3n;
const DECODER_RECOVERY_WARNING = 3n;
const VIDEO_PULL_FAILURE_WARNING = 3n;
const INPUT_BACKPRESSURE_WARNING = 5n;
const SLOW_FIRST_FRAME_MS = 5_000n;
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
  const cancelledCount = count("controller_cancelled");
  const controllerStopped = events.filter((event) => event.event === "controller_stopped");
  const hostStopped = events.filter((event) => event.event === "host_stopped");
  const videoAttempts = videoByAttempt(events);
  const renderStreams = renderByStream(events);
  const video = [...videoAttempts.values()].reduce(
    (total, attempt) => ({
      received: total.received + attempt.received,
      dropped: total.dropped + attempt.dropped,
      completed: total.completed + attempt.completed,
      delivered: total.delivered + attempt.delivered,
      ipcOverflowDrops: total.ipcOverflowDrops + attempt.ipcOverflowDrops,
      ipcKeyframeReplacements:
        total.ipcKeyframeReplacements + attempt.ipcKeyframeReplacements,
      mailboxSamples: total.mailboxSamples + attempt.mailboxSamples,
      inputBackpressure: total.inputBackpressure + attempt.inputBackpressure,
    }),
    {
      received: 0n,
      dropped: 0n,
      completed: 0n,
      delivered: 0n,
      ipcOverflowDrops: 0n,
      ipcKeyframeReplacements: 0n,
      mailboxSamples: 0,
      inputBackpressure: 0n,
    },
  );
  const render = [...renderStreams.values()].reduce(
    (total, stream) => ({
      received: total.received + stream.received,
      submitted: total.submitted + stream.submitted,
      displayed: total.displayed + stream.displayed,
      malformed: total.malformed + stream.malformed,
      recoveries: total.recoveries + stream.recoveries,
      pullFailures: total.pullFailures + stream.pullFailures,
      firstFrameMs: maximumOptional(total.firstFrameMs, stream.firstFrameMs),
      samples: total.samples + stream.samples,
    }),
    {
      received: 0n,
      submitted: 0n,
      displayed: 0n,
      malformed: 0n,
      recoveries: 0n,
      pullFailures: 0n,
      firstFrameMs: null as bigint | null,
      samples: 0,
    },
  );
  const packetTotal = video.received + video.dropped;
  const lossPercent = packetTotal > 0n ? roundPercent(video.dropped, packetTotal) : null;
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
  const userCancelledBeforeConnection = cancelledCount > 0
    && connectedCount === 0
    && controllerStopped.length === 0
    && hostStopped.length === 0;
  if (retryCount >= OSCILLATION_RETRY_COUNT && !userCancelledBeforeConnection) {
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
    && video.completed === 0n
    && (videoAttempts.size > 0 || nowUnixMs - endedAt >= STALE_SESSION_MS);
  if (noCompletedFrame) {
    findings.push({
      code: "no_completed_frame",
      severity: "error",
      title: "连接后没有完整画面",
      detail: video.received > 0n
        ? `已收到 ${video.received} 个视频包，但没有完成一帧，优先检查分片、关键帧和解码恢复。`
        : "连接已建立但没有收到可组成画面的媒体数据，优先检查主机采集与编码。",
    });
  }
  const stalledVideoIpcAttempt = [...videoAttempts.entries()].find(([, attempt]) => (
    attempt.completed > 0n
    && attempt.mailboxSamples > 0
    && attempt.delivered === 0n
  ));
  if (stalledVideoIpcAttempt !== undefined) {
    const [attempt, metrics] = stalledVideoIpcAttempt;
    findings.push({
      code: "video_ipc_stalled",
      severity: "error",
      title: "远程画面未送达界面",
      detail: `第 ${attempt} 次连接已完成 ${metrics.completed} 帧，但本机视频邮箱没有交付画面，优先检查桌面端视频 IPC。`,
    });
  }
  if (video.completed > 0n && render.received > 0n && render.displayed === 0n && render.samples >= 2) {
    findings.push({
      code: "no_displayed_frame",
      severity: "error",
      title: "控制端没有显示远程画面",
      detail: `传输端已完成 ${video.completed} 帧、界面已收到 ${render.received} 帧，但连续诊断仍未显示画面。`,
    });
  }
  if (packetTotal >= HIGH_LOSS_MINIMUM_PACKETS && lossPercent !== null
    && video.dropped * 100n >= packetTotal * HIGH_LOSS_PERCENT) {
    findings.push({
      code: "high_video_loss",
      severity: "warning",
      title: "视频丢包率过高",
      detail: `累计视频丢包率为 ${lossPercent.toFixed(1)}%，超过 ${HIGH_LOSS_PERCENT}% 阈值。`,
    });
  }
  const videoIpcPressure = video.ipcOverflowDrops + video.ipcKeyframeReplacements;
  if (videoIpcPressure >= VIDEO_IPC_PRESSURE_WARNING) {
    findings.push({
      code: "video_ipc_pressure",
      severity: "warning",
      title: "本机画面交付出现积压",
      detail: `视频邮箱丢弃 ${video.ipcOverflowDrops} 帧、替换关键帧 ${video.ipcKeyframeReplacements} 次，控制端可能暂时来不及取走画面。`,
    });
  }
  if (render.recoveries >= DECODER_RECOVERY_WARNING || render.malformed >= 60n) {
    findings.push({
      code: "decoder_instability",
      severity: "warning",
      title: "本机视频解码不稳定",
      detail: `解码器恢复 ${render.recoveries} 次，异常视频帧 ${render.malformed} 个。`,
    });
  }
  if (render.pullFailures >= VIDEO_PULL_FAILURE_WARNING) {
    findings.push({
      code: "video_pull_instability",
      severity: "warning",
      title: "本机画面读取不稳定",
      detail: `界面读取远程画面失败 ${render.pullFailures} 次，桌面端已自动退避重试。`,
    });
  }
  if (video.inputBackpressure >= INPUT_BACKPRESSURE_WARNING) {
    findings.push({
      code: "input_backpressure",
      severity: "warning",
      title: "远程输入队列出现拥塞",
      detail: `输入队列累计等待 ${video.inputBackpressure} 次，需要检查控制事件频率或控制通道延迟。`,
    });
  }
  if (render.firstFrameMs !== null && render.firstFrameMs >= SLOW_FIRST_FRAME_MS) {
    findings.push({
      code: "slow_first_frame",
      severity: "warning",
      title: "远程画面首帧较慢",
      detail: `从收到视频配置到显示第一帧耗时 ${render.firstFrameMs} 毫秒。`,
    });
  }

  if (
    connectedCount > 0
    && video.completed > 0n
    && (render.samples === 0 || render.displayed > 0n)
    && !findings.some(isAlertFinding)
  ) {
    findings.push({
      code: "healthy_video",
      severity: "info",
      title: "远程画面链路正常",
      detail: `已完成 ${video.completed} 帧，未发现停止、振荡或高丢包。`,
    });
  }
  if (events.some(isDisabledCursorLane)) {
    // A stream ID does not identify a peer. Keep one categorical observation,
    // without summing snapshots or interpreting local send errors as loss.
    findings.push({
      code: "cursor_lane_disabled",
      severity: "warning",
      title: "可选光标通道已停用",
      detail: "主机因本地能力或大小限制停用了可选光标通道；该事件不表示视频、输入或远程连接已中断。",
    });
  }
  if (events.some(isStoppedDirectListener)) {
    findings.push({
      code: "direct_listener_stopped",
      severity: "warning",
      title: "直连监听器已关闭",
      detail: "主机直连监听器已关闭，媒体发送按中继回退策略处理；是否恢复需结合后续媒体事件确认，该事件本身不表示主机或远程会话已停止。",
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
      received_packets: jsonExactUnsigned(video.received),
      dropped_packets: jsonExactUnsigned(video.dropped),
      completed_frames: jsonExactUnsigned(video.completed),
      delivered_frames: jsonExactUnsigned(video.delivered),
      video_ipc_overflow_drops: jsonExactUnsigned(video.ipcOverflowDrops),
      video_ipc_keyframe_replacements: jsonExactUnsigned(video.ipcKeyframeReplacements),
      loss_percent: lossPercent,
      input_backpressure_count: jsonExactUnsigned(video.inputBackpressure),
    },
    render: {
      received_frames: jsonExactUnsigned(render.received),
      submitted_frames: jsonExactUnsigned(render.submitted),
      displayed_frames: jsonExactUnsigned(render.displayed),
      malformed_frames: jsonExactUnsigned(render.malformed),
      decoder_recoveries: jsonExactUnsigned(render.recoveries),
      video_pull_failures: jsonExactUnsigned(render.pullFailures),
      first_frame_ms: render.firstFrameMs === null ? null : jsonExactUnsigned(render.firstFrameMs),
    },
    stop_reason_kinds: [...new Set(stopReasonKinds)].sort(),
    outcome,
    findings,
  };
}

function isDisabledCursorLane(event: ParsedEvent): boolean {
  if (event.source !== "host" || event.event !== "host_cursor_send"
    || event.payload.event !== "host_cursor_send" || event.payload.disabled !== true
    || event.payload.reason !== "capability_or_size") return false;
  const streamId = parseU64(event.payload.stream_id);
  const attempts = parseU64(event.payload.send_attempts);
  const errors = parseU64(event.payload.send_errors);
  return streamId !== null && streamId > 0n && attempts !== null
    && errors !== null && errors > 0n && errors <= attempts;
}

function isStoppedDirectListener(event: ParsedEvent): boolean {
  const streamId = parseU64(event.payload.stream_id);
  return event.source === "host" && event.level === "warning"
    && event.event === "host_direct_listener_stopped"
    && event.payload.schema === 1 && event.payload.level === "warning"
    && event.payload.event === "host_direct_listener_stopped"
    && event.payload.reason === "listener_closed"
    && typeof event.payload.retired_active === "boolean"
    && streamId !== null && streamId > 0n;
}

function videoByAttempt(events: ParsedEvent[]): Map<number, VideoAttempt> {
  const attempts = new Map<number, VideoAttempt>();
  for (const event of events) {
    if (event.event !== "controller_video_metrics") continue;
    const attemptId = safeInteger(event.payload.attempt) ?? 0;
    const current = attempts.get(attemptId) ?? {
      received: 0n,
      dropped: 0n,
      completed: 0n,
      delivered: 0n,
      ipcOverflowDrops: 0n,
      ipcKeyframeReplacements: 0n,
      mailboxSamples: 0,
      inputBackpressure: 0n,
    };
    const delivered = parseU64(event.payload.delivered_video_frames);
    const ipcOverflowDrops = parseU64(event.payload.video_ipc_overflow_drops);
    const ipcKeyframeReplacements = parseU64(event.payload.video_ipc_keyframe_replacements);
    const hasMailboxSample = delivered !== null
      || ipcOverflowDrops !== null
      || ipcKeyframeReplacements !== null;
    attempts.set(attemptId, {
      received: maximum(current.received, parseU64(event.payload.received_video_packets) ?? 0n),
      dropped: maximum(current.dropped, parseU64(event.payload.dropped_video_packets) ?? 0n),
      completed: maximum(current.completed, parseU64(event.payload.completed_frames) ?? 0n),
      delivered: maximum(current.delivered, delivered ?? 0n),
      ipcOverflowDrops: maximum(current.ipcOverflowDrops, ipcOverflowDrops ?? 0n),
      ipcKeyframeReplacements: maximum(
        current.ipcKeyframeReplacements,
        ipcKeyframeReplacements ?? 0n,
      ),
      mailboxSamples: Math.max(current.mailboxSamples, hasMailboxSample ? 1 : 0),
      inputBackpressure: maximum(
        current.inputBackpressure,
        parseU64(event.payload.input_backpressure_count) ?? 0n,
      ),
    });
  }
  return attempts;
}

function renderByStream(events: ParsedEvent[]): Map<string, RenderStream> {
  const streams = new Map<string, RenderStream>();
  for (const event of events) {
    if (event.event !== "controller_render_metrics") continue;
    // Canonical strings preserve adjacent u64 IDs, while old numeric 7 and new
    // string "7" share one stream. Invalid legacy IDs keep an explicit bucket.
    const canonicalStreamId = canonicalU64(event.payload.stream_id);
    const streamId = canonicalStreamId !== null && canonicalStreamId !== "0" ? canonicalStreamId : "legacy";
    const current = streams.get(streamId) ?? {
      received: 0n,
      submitted: 0n,
      displayed: 0n,
      malformed: 0n,
      recoveries: 0n,
      pullFailures: 0n,
      firstFrameMs: null,
      samples: 0,
    };
    streams.set(streamId, {
      received: maximum(current.received, parseU64(event.payload.received_frames) ?? 0n),
      submitted: maximum(current.submitted, parseU64(event.payload.submitted_frames) ?? 0n),
      displayed: maximum(current.displayed, parseU64(event.payload.displayed_frames) ?? 0n),
      malformed: maximum(current.malformed, parseU64(event.payload.malformed_frames) ?? 0n),
      // These producer fields remain u32 JSON numbers, not decimal strings.
      recoveries: maximum(current.recoveries, BigInt(safeInteger(event.payload.decoder_recoveries) ?? 0)),
      pullFailures: maximum(
        current.pullFailures,
        BigInt(safeInteger(event.payload.video_pull_failures) ?? 0),
      ),
      firstFrameMs: maximumOptional(current.firstFrameMs, parseU64(event.payload.first_frame_ms)),
      samples: current.samples + 1,
    });
  }
  return streams;
}

function maximum(left: bigint, right: bigint): bigint {
  return left > right ? left : right;
}

function maximumOptional(left: bigint | null, right: bigint | null): bigint | null {
  if (left === null) return right;
  if (right === null) return left;
  return maximum(left, right);
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

function roundPercent(part: bigint, total: bigint): number {
  // part is dropped packets and total is received + dropped, so only this
  // rounded, bounded value (0..10000 basis points) is converted to Number.
  const basisPoints = (part * 10_000n + total / 2n) / total;
  return Number(basisPoints) / 100;
}

function isAlertFinding(finding: SessionFinding): boolean {
  return finding.severity === "warning" || finding.severity === "error";
}
