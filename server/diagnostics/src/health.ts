import type { DiagnosticAnalysisReport, SessionAnalysis } from "./analysis";

export interface DiagnosticHealthReport {
  schema: 1;
  status: "healthy" | "attention" | "empty";
  requires_attention: boolean;
  generated_at: string;
  window_hours: number;
  summary: DiagnosticAnalysisReport["summary"];
  alert_sessions: SessionAnalysis[];
}

export function buildDiagnosticHealthReport(
  analysis: DiagnosticAnalysisReport,
): DiagnosticHealthReport {
  const alertSessions = analysis.sessions
    .filter((session) => session.outcome === "error" || session.outcome === "warning")
    .slice(0, 100);
  const requiresAttention = analysis.summary.error > 0 || analysis.summary.warning >= 3;
  const status = analysis.summary.sessions === 0
    ? "empty"
    : requiresAttention
      ? "attention"
      : "healthy";
  return {
    schema: 1,
    status,
    requires_attention: requiresAttention,
    generated_at: analysis.generated_at,
    window_hours: analysis.hours,
    summary: analysis.summary,
    alert_sessions: alertSessions,
  };
}
