import {
  ArrowLeft,
  Check,
  CircleAlert,
  CircleCheck,
  CircleHelp,
  CircleX,
  ClipboardCopy,
  ClipboardPaste,
  Copy,
  Ellipsis,
  GlobeLock,
  GitFork,
  FileDown,
  FileUp,
  FolderOpen,
  Gauge,
  Hand,
  Info,
  KeyRound,
  Keyboard,
  LoaderCircle,
  LogOut,
  Maximize2,
  Minimize2,
  Minus,
  Monitor,
  MonitorCheck,
  MonitorUp,
  MousePointer2,
  PanelsTopLeft,
  RefreshCw,
  RotateCcw,
  Scan,
  SendHorizontal,
  Share2,
  Settings2,
  ShieldAlert,
  ShieldCheck,
  ShieldUser,
  Square,
  Trash2,
  TriangleAlert,
  Volume2,
  VolumeOff,
  VolumeX,
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
  ClipboardCopy,
  ClipboardPaste,
  Copy,
  Ellipsis,
  GlobeLock,
  GitFork,
  FileDown,
  FileUp,
  FolderOpen,
  Gauge,
  Hand,
  Info,
  KeyRound,
  Keyboard,
  LoaderCircle,
  LogOut,
  Maximize2,
  Minimize2,
  Minus,
  Monitor,
  MonitorCheck,
  MonitorUp,
  MousePointer2,
  PanelsTopLeft,
  RefreshCw,
  RotateCcw,
  Scan,
  SendHorizontal,
  Share2,
  Settings2,
  ShieldAlert,
  ShieldCheck,
  ShieldUser,
  Square,
  Trash2,
  TriangleAlert,
  Volume2,
  VolumeOff,
  VolumeX,
  X,
};

export type DeskLinkIconName =
  | "arrow-left"
  | "check"
  | "circle-alert"
  | "circle-check"
  | "circle-help"
  | "circle-x"
  | "clipboard-copy"
  | "clipboard-paste"
  | "copy"
  | "ellipsis"
  | "globe-lock"
  | "git-fork"
  | "file-down"
  | "file-up"
  | "folder-open"
  | "gauge"
  | "hand"
  | "info"
  | "key-round"
  | "keyboard"
  | "loader-circle"
  | "log-out"
  | "maximize-2"
  | "minimize-2"
  | "minus"
  | "monitor"
  | "monitor-check"
  | "monitor-up"
  | "mouse-pointer-2"
  | "panels-top-left"
  | "refresh-cw"
  | "rotate-ccw"
  | "scan"
  | "send-horizontal"
  | "share-2"
  | "settings-2"
  | "shield-alert"
  | "shield-check"
  | "shield-user"
  | "square"
  | "trash-2"
  | "triangle-alert"
  | "volume-2"
  | "volume-off"
  | "volume-x"
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
