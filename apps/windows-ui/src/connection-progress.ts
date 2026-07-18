import type { ControllerRuntimeState } from "./types";

export const CONNECTION_PROGRESS_STEPS = [
  "查找在线设备",
  "建立加密连接",
  "等待主机确认",
  "打开远程桌面",
] as const;

export interface ConnectionProgressPresentation {
  activeStep: number;
  guidance: string;
  delayed: boolean;
}

export function connectionProgressPresentation(
  state: ControllerRuntimeState,
  elapsedSeconds: number,
): ConnectionProgressPresentation {
  const delayed = elapsedSeconds >= 15;
  switch (state) {
    case "finding":
      return {
        activeStep: 0,
        guidance: delayed
          ? "查找时间较长，请确认主机已在线，并核对设备 ID 和访问密码。"
          : "正在通过中继确认设备 ID 和访问密码。",
        delayed,
      };
    case "connecting":
      return {
        activeStep: 1,
        guidance: delayed
          ? "安全连接仍未完成，请保持两台电脑联网，并确认主机没有退出 DeskLink。"
          : "设备已找到，正在建立端到端加密通道。",
        delayed,
      };
    case "waitingApproval":
      return {
        activeStep: 2,
        guidance: delayed
          ? "请查看主机屏幕上的控制请求。如果没有弹出请求，可取消后重新连接。"
          : "请在另一台电脑上确认允许本次远程控制。",
        delayed,
      };
    case "reconnecting":
      return {
        activeStep: 1,
        guidance: delayed
          ? "连接恢复时间较长，请检查主机网络。DeskLink 仍会按计划自动重试。"
          : "网络连接发生变化，DeskLink 正在自动恢复加密会话。",
        delayed,
      };
    case "connected":
      return {
        activeStep: 3,
        guidance: "远程桌面已连接。",
        delayed: false,
      };
    case "idle":
    case "stopped":
      return {
        activeStep: -1,
        guidance: "当前没有正在进行的连接。",
        delayed: false,
      };
  }
}

export function formatConnectionElapsed(elapsedSeconds: number): string {
  const bounded = Math.max(0, Math.floor(elapsedSeconds));
  if (bounded < 1) {
    return "刚刚开始";
  }
  if (bounded < 60) {
    return `已等待 ${bounded} 秒`;
  }
  const minutes = Math.floor(bounded / 60);
  const seconds = bounded % 60;
  return seconds === 0 ? `已等待 ${minutes} 分钟` : `已等待 ${minutes} 分 ${seconds} 秒`;
}
