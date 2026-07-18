import type { HostSnapshot } from "./types";

export interface HostStatusSummary {
  title: string;
  detail: string;
  tone: "attention" | "working" | "quiet" | "ready";
}

export function hostStatusSummary(snapshot: HostSnapshot): HostStatusSummary {
  if (snapshot.connectionError || snapshot.trustedError || snapshot.fixedPasswordError) {
    return {
      title: "需要处理",
      detail: "打开设置 / 诊断处理本机共享问题",
      tone: "attention",
    };
  }

  if (snapshot.pendingApproval) {
    return {
      title: "等待确认",
      detail: "有设备请求控制这台电脑，请允许或拒绝",
      tone: "working",
    };
  }

  if (
    !snapshot.connection
    || snapshot.readiness === "setup"
    || snapshot.runtime.state === "stopped"
    || snapshot.runtime.state === "notConfigured"
  ) {
    return {
      title: "未开启共享",
      detail: "在“共享此设备”中开启本机共享",
      tone: "quiet",
    };
  }

  if (snapshot.readiness === "attention") {
    return {
      title: "需要处理",
      detail: "打开设置 / 诊断检查本机共享状态",
      tone: "attention",
    };
  }

  switch (snapshot.runtime.state) {
    case "starting":
    case "pairing":
    case "connecting":
    case "reconnecting":
      return {
        title: "连接中",
        detail: "正在准备本机共享",
        tone: "working",
      };
    case "available":
      return {
        title: "本机可被连接",
        detail: "本机共享已准备好",
        tone: "ready",
      };
    case "connected":
      return {
        title: "正在共享本机",
        detail: "远程设备正在查看并控制这台电脑",
        tone: "ready",
      };
  }
}
