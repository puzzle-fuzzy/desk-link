import { randomUUID } from "node:crypto";

import { AccountError } from "./domain/errors";

export function requestId(request: Request): string {
  return request.headers.get("x-request-id")?.trim().slice(0, 96) || randomUUID();
}

export function jsonResponse(
  value: unknown,
  status: number,
  id: string,
  headers: Record<string, string> = {},
): Response {
  return Response.json(value, {
    status,
    headers: {
      "cache-control": "no-store",
      "content-security-policy": "default-src 'none'",
      "x-content-type-options": "nosniff",
      "x-request-id": id,
      ...headers,
    },
  });
}

export function noContent(id: string): Response {
  return new Response(null, {
    status: 204,
    headers: {
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
      "x-request-id": id,
    },
  });
}

export async function readJson(request: Request): Promise<Record<string, unknown>> {
  if (request.headers.get("content-type")?.split(";", 1)[0].trim() !== "application/json") {
    throw new AccountError("invalid_request", 415, "请求必须使用 JSON 格式。");
  }
  let value: unknown;
  try {
    value = await request.json();
  } catch {
    throw new AccountError("invalid_request", 400, "请求内容不是有效的 JSON。");
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new AccountError("invalid_request", 400, "请求内容格式不正确。");
  }
  return value as Record<string, unknown>;
}

export function errorResponse(error: unknown, id: string): Response {
  if (error instanceof AccountError) {
    return jsonResponse({ error: error.code, message: error.message }, error.status, id);
  }
  console.error("account request failed", { requestId: id, error });
  return jsonResponse({ error: "internal", message: "服务暂时不可用，请稍后重试。" }, 500, id);
}

export function corsHeaders(origin: string | null, allowedOrigins: string[]): Record<string, string> {
  if (!origin || !allowedOrigins.includes(origin)) return {};
  return {
    "access-control-allow-origin": origin,
    "access-control-allow-headers": "authorization, content-type, x-request-id",
    "access-control-allow-methods": "GET, POST, DELETE, OPTIONS",
    "vary": "Origin",
  };
}
