import { expect, test } from "bun:test";

import { CONTROLLER_CONNECTION_ENTRIES } from "./controller-workspace";

test("makes connection code primary and device credentials secondary", () => {
  expect(CONTROLLER_CONNECTION_ENTRIES.map((entry) => entry.id)).toEqual([
    "connectionCode",
    "deviceCredentials",
  ]);
  expect(CONTROLLER_CONNECTION_ENTRIES[0]).toMatchObject({
    priority: "primary",
    title: "粘贴连接码",
    action: "开始连接",
  });
  expect(CONTROLLER_CONNECTION_ENTRIES[1]).toMatchObject({
    priority: "secondary",
    title: "使用设备 ID 和密码",
  });
});
