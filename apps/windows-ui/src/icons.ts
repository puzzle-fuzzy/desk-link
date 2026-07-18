import {
  ArrowLeft,
  Check,
  CircleAlert,
  CircleCheck,
  CircleHelp,
  CircleX,
  Copy,
  Ellipsis,
  GlobeLock,
  GitFork,
  Info,
  KeyRound,
  Keyboard,
  LoaderCircle,
  LogOut,
  Maximize2,
  Minus,
  Monitor,
  MonitorCheck,
  MonitorUp,
  MousePointer2,
  RefreshCw,
  SendHorizontal,
  Settings2,
  ShieldAlert,
  ShieldCheck,
  ShieldUser,
  Square,
  TriangleAlert,
  X,
  createIcons,
} from "lucide";

const deskLinkIcons = {
  ArrowLeft,
  Check,
  CircleAlert,
  CircleCheck,
  CircleHelp,
  CircleX,
  Copy,
  Ellipsis,
  GlobeLock,
  GitFork,
  Info,
  KeyRound,
  Keyboard,
  LoaderCircle,
  LogOut,
  Maximize2,
  Minus,
  Monitor,
  MonitorCheck,
  MonitorUp,
  MousePointer2,
  RefreshCw,
  SendHorizontal,
  Settings2,
  ShieldAlert,
  ShieldCheck,
  ShieldUser,
  Square,
  TriangleAlert,
  X,
};

export type DeskLinkIconName =
  | "arrow-left"
  | "check"
  | "circle-alert"
  | "circle-check"
  | "circle-help"
  | "circle-x"
  | "copy"
  | "ellipsis"
  | "globe-lock"
  | "git-fork"
  | "info"
  | "key-round"
  | "keyboard"
  | "loader-circle"
  | "log-out"
  | "maximize-2"
  | "minus"
  | "monitor"
  | "monitor-check"
  | "monitor-up"
  | "mouse-pointer-2"
  | "refresh-cw"
  | "send-horizontal"
  | "settings-2"
  | "shield-alert"
  | "shield-check"
  | "shield-user"
  | "square"
  | "triangle-alert"
  | "x";

export function icon(name: DeskLinkIconName, className = ""): string {
  const classes = ["app-icon", className].filter(Boolean).join(" ");
  return `<i data-lucide="${name}" class="${classes}" aria-hidden="true"></i>`;
}

export function renderLucideIcons(root: Element | Document | DocumentFragment = document): void {
  createIcons({
    icons: deskLinkIcons,
    root,
    attrs: {
      "stroke-width": 2,
    },
  });
}
