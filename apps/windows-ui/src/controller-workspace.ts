export interface ControllerConnectionEntry {
  id: "connectionCode" | "deviceCredentials";
  priority: "primary" | "secondary";
  title: string;
  action: string;
}

export const CONTROLLER_CONNECTION_ENTRIES = [
  {
    id: "connectionCode",
    priority: "primary",
    title: "粘贴连接码",
    action: "开始连接",
  },
  {
    id: "deviceCredentials",
    priority: "secondary",
    title: "使用设备 ID 和密码",
    action: "查找并连接设备",
  },
] as const satisfies ReadonlyArray<ControllerConnectionEntry>;
