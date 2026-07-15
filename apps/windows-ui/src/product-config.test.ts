import { describe, expect, test } from "bun:test";

import {
  MANAGED_RELAY_ADDRESS,
  MANAGED_RELAY_SERVER_NAME,
  isManagedRelay,
} from "./product-config";

describe("managed relay profile", () => {
  test("uses the deployed DeskLink relay", () => {
    expect(MANAGED_RELAY_ADDRESS).toBe("101.35.246.159:4433");
    expect(MANAGED_RELAY_SERVER_NAME).toBe("turn.p2p.yxswy.com");
  });

  test("matches normalized server names without accepting another endpoint", () => {
    expect(isManagedRelay(` ${MANAGED_RELAY_ADDRESS} `, "TURN.P2P.YXSWY.COM")).toBe(true);
    expect(isManagedRelay("127.0.0.1:4433", MANAGED_RELAY_SERVER_NAME)).toBe(false);
  });
});
