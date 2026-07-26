import { AccountStore } from "./db/store";
import type { AccountConfig } from "./config";
import { loadAccountConfig } from "./config";
import { AccountError } from "./domain/errors";
import { RateLimiter } from "./domain/rate-limit";
import { errorResponse, corsHeaders, jsonResponse, noContent, readJson, requestId } from "./http";
import { DevMailer } from "./providers/dev-mailer";
import type { Mailer } from "./providers/mailer";
import { HttpMailer } from "./providers/http-mailer";
import { AccountService, publicUser } from "./services/account-service";
import { SessionService } from "./services/session-service";

export interface AccountAppDependencies {
  config?: AccountConfig;
  store?: AccountStore;
  mailer?: Mailer;
  now?: () => number;
  rateLimiter?: RateLimiter;
}

export interface AccountApp {
  fetch(request: Request): Promise<Response>;
  flushOutbox(): Promise<number>;
}

export function createAccountApp(dependencies: AccountAppDependencies = {}): AccountApp {
  const config = dependencies.config ?? loadAccountConfig({ NODE_ENV: "test" });
  const store = dependencies.store ?? new AccountStore(config.databasePath);
  const mailer = dependencies.mailer ?? defaultMailer(config);
  const now = dependencies.now ?? (() => Math.floor(Date.now() / 1000));
  const rateLimiter = dependencies.rateLimiter ?? new RateLimiter();
  const sessions = new SessionService(store, config, now);
  const accounts = new AccountService(store, config, mailer, sessions, now);
  const allowedOrigins = config.corsOrigins;

  return {
    async fetch(request: Request): Promise<Response> {
      const id = requestId(request);
      const origin = request.headers.get("origin");
      const cors = corsHeaders(origin, allowedOrigins);
      try {
        if (request.method === "OPTIONS") {
          return new Response(null, { status: 204, headers: { ...cors, "x-request-id": id } });
        }
        const url = new URL(request.url);
        if (request.method === "GET" && (url.pathname === "/health" || url.pathname === "/ready")) {
          return jsonResponse(
            {
              schema: 1,
              status: url.pathname === "/ready" ? "ready" : "ok",
              service: "desklink-account",
            },
            200,
            id,
            cors,
          );
        }

        if (request.method === "POST" && url.pathname === "/v1/account/register") {
          const body = await readJson(request);
          enforceRateLimit(rateLimiter, request, "register", body, now());
          return jsonResponse(await accounts.register({
            email: body.email,
            password: body.password,
            deviceId: body.deviceId,
            platform: body.platform,
            deviceName: body.deviceName,
          }), 202, id, cors);
        }
        if (request.method === "POST" && url.pathname === "/v1/account/verify-email") {
          const body = await readJson(request);
          enforceRateLimit(rateLimiter, request, "verify-email", body, now());
          return jsonResponse({ user: await accounts.verifyEmail(body.token) }, 200, id, cors);
        }
        if (request.method === "GET" && url.pathname === "/v1/account/verify-email") {
          const user = await accounts.verifyEmail(url.searchParams.get("token"));
          return htmlResponse("邮箱验证成功", `邮箱 ${escapeHtml(user.email)} 已验证成功，现在可以返回 DeskLink 登录。`, id);
        }
        if (request.method === "GET" && url.pathname === "/v1/account/reset-password") {
          return resetPasswordHtml(url.searchParams.get("token"), id);
        }
        if (request.method === "POST" && url.pathname === "/v1/account/verify-email/resend") {
          const body = await readJson(request);
          enforceRateLimit(rateLimiter, request, "resend-verification", body, now());
          await accounts.resendVerification(body.email);
          return jsonResponse({ ok: true }, 202, id, cors);
        }
        if (request.method === "POST" && url.pathname === "/v1/account/login") {
          const body = await readJson(request);
          enforceRateLimit(rateLimiter, request, "login", body, now());
          return jsonResponse(await accounts.login({
            email: body.email,
            password: body.password,
            deviceId: body.deviceId,
            platform: body.platform,
            deviceName: body.deviceName,
          }), 200, id, cors);
        }
        if (request.method === "POST" && url.pathname === "/v1/account/refresh") {
          const body = await readJson(request);
          return jsonResponse({ tokens: sessions.refresh(readString(body.refreshToken, "刷新令牌")) }, 200, id, cors);
        }
        if (request.method === "POST" && url.pathname === "/v1/account/logout") {
          sessions.logout(sessions.authenticate(request));
          return noContent(id);
        }
        if (request.method === "POST" && url.pathname === "/v1/account/password/forgot") {
          const body = await readJson(request);
          enforceRateLimit(rateLimiter, request, "forgot-password", body, now());
          await accounts.requestPasswordReset(body.email);
          return jsonResponse({ ok: true, message: "如果邮箱已注册，重置密码邮件会很快送达。" }, 202, id, cors);
        }
        if (request.method === "POST" && url.pathname === "/v1/account/password/reset") {
          const body = await readJson(request);
          enforceRateLimit(rateLimiter, request, "reset-password", body, now());
          await accounts.resetPassword(body.token, body.password);
          return noContent(id);
        }
        if (request.method === "GET" && url.pathname === "/v1/account/me") {
          const auth = sessions.authenticate(request);
          return jsonResponse({ user: publicUser(auth.user), device: auth.device }, 200, id, cors);
        }
        if (request.method === "GET" && url.pathname === "/v1/account/devices") {
          const auth = sessions.authenticate(request);
          return jsonResponse({ devices: store.listDevices(auth.user.id) }, 200, id, cors);
        }
        const devicePath = /^\/v1\/account\/devices\/([^/]+)$/.exec(url.pathname);
        if (request.method === "DELETE" && devicePath) {
          const auth = sessions.authenticate(request);
          const deviceId = decodeURIComponent(devicePath[1]);
          if (deviceId === auth.device.id) {
            throw new AccountError("invalid_request", 400, "不能从当前设备撤销当前登录，请直接退出登录。");
          }
          if (!store.revokeDevice(auth.user.id, deviceId, now())) {
            throw new AccountError("not_found", 404, "设备不存在。");
          }
          store.recordAuditEvent({
            userId: auth.user.id,
            deviceId,
            action: "account.device_revoked",
            now: now(),
          });
          return noContent(id);
        }
        throw new AccountError("not_found", 404, "请求地址不存在。");
      } catch (error) {
        const response = errorResponse(error, id);
        for (const [key, value] of Object.entries(cors)) response.headers.set(key, value);
        return response;
      }
    },
    flushOutbox: () => accounts.flushOutbox(),
  };
}

function enforceRateLimit(
  limiter: RateLimiter,
  request: Request,
  scope: string,
  body: Record<string, unknown>,
  now: number,
): void {
  const ip = request.headers.get("x-forwarded-for")?.split(",", 1)[0]?.trim()
    || request.headers.get("cf-connecting-ip")?.trim()
    || "unknown";
  const email = typeof body.email === "string" ? body.email.trim().toLowerCase().slice(0, 320) : "missing";
  const limits: Record<string, [number, number]> = {
    register: [5, 3_600],
    "verify-email": [10, 600],
    "resend-verification": [5, 3_600],
    login: [12, 60],
    "forgot-password": [5, 3_600],
    "reset-password": [10, 3_600],
  };
  const [limit, windowSeconds] = limits[scope] ?? [20, 60];
  const allowed = limiter.allow(`${scope}:ip:${ip}`, limit, windowSeconds, now)
    && limiter.allow(`${scope}:email:${email}`, limit, windowSeconds, now);
  if (!allowed) throw new AccountError("rate_limited", 429, "请求过于频繁，请稍后再试。");
}

function defaultMailer(config: AccountConfig): Mailer {
  if (config.environment === "production") {
    if (!config.mailUrl || !config.mailToken || !config.mailFrom) {
      throw new Error("production mail configuration is incomplete");
    }
    return new HttpMailer(config.mailUrl, config.mailToken, config.mailFrom);
  }
  return new DevMailer();
}

function readString(value: unknown, label: string): string {
  if (typeof value !== "string" || !value.trim()) {
    throw new AccountError("invalid_request", 400, `${label}无效。`);
  }
  return value.trim();
}

function htmlResponse(title: string, message: string, requestIdValue: string): Response {
  return new Response(`<!doctype html><meta charset="utf-8"><title>${escapeHtml(title)}</title><style>body{font-family:-apple-system,BlinkMacSystemFont,sans-serif;background:#f5f5f7;color:#1d1d1f;padding:48px;line-height:1.6}main{max-width:560px;margin:10vh auto;background:#fff;border-radius:24px;padding:32px;box-shadow:0 12px 40px #0001}</style><main><h1>${escapeHtml(title)}</h1><p>${message}</p></main>`, {
    status: 200,
    headers: {
      "cache-control": "no-store",
      "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'",
      "content-type": "text/html; charset=utf-8",
      "x-content-type-options": "nosniff",
      "x-request-id": requestIdValue,
    },
  });
}

function resetPasswordHtml(token: string | null, requestIdValue: string): Response {
  if (!token || token.length < 32 || token.length > 512) {
    return htmlResponse("重置链接无效", "请重新发起找回密码操作。", requestIdValue);
  }
  const safeToken = escapeHtml(token);
  return new Response(`<!doctype html><meta charset="utf-8"><title>重置 DeskLink 密码</title><style>body{font-family:-apple-system,BlinkMacSystemFont,sans-serif;background:#f5f5f7;color:#1d1d1f;padding:48px}main{max-width:560px;margin:10vh auto;background:#fff;border-radius:24px;padding:32px;box-shadow:0 12px 40px #0001}label{display:grid;gap:8px;margin:16px 0;font-weight:600}input{padding:12px;border:1px solid #d2d2d7;border-radius:10px;font:inherit}button{margin-top:8px;padding:12px 18px;border:0;border-radius:10px;background:#1d1d1f;color:#fff;font:inherit;font-weight:600;cursor:pointer}#message{color:#6e6e73;line-height:1.6}</style><main><h1>重置 DeskLink 密码</h1><p id="message">设置一个新的密码（至少 12 个字符）。</p><form id="form"><label>新密码<input id="password" type="password" minlength="12" maxlength="128" required></label><label>确认密码<input id="confirmation" type="password" minlength="12" maxlength="128" required></label><button type="submit">保存新密码</button></form></main><script>const token="${safeToken}";const form=document.getElementById("form");const message=document.getElementById("message");form.addEventListener("submit",async(event)=>{event.preventDefault();const password=document.getElementById("password").value;const confirmation=document.getElementById("confirmation").value;if(password!==confirmation){message.textContent="两次输入的密码不一致。";return}const response=await fetch("/v1/account/password/reset",{method:"POST",headers:{"content-type":"application/json"},body:JSON.stringify({token,password})});const body=await response.json().catch(()=>({}));if(!response.ok){message.textContent=body.message||"重置失败，请重新发起找回密码。";return}form.remove();message.textContent="密码已更新，现在可以返回 DeskLink 登录。";});</script>`, {
    status: 200,
    headers: {
      "cache-control": "no-store",
      "content-security-policy": "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'",
      "content-type": "text/html; charset=utf-8",
      "x-content-type-options": "nosniff",
      "x-request-id": requestIdValue,
    },
  });
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>\"']/g, (character) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  })[character]!);
}
