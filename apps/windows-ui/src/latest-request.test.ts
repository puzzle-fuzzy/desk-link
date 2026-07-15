import { describe, expect, test } from "bun:test";

import { LatestRequest } from "./latest-request";

describe("latest async request gate", () => {
  test("accepts only the newest request", () => {
    const gate = new LatestRequest();
    const first = gate.begin();
    const second = gate.begin();

    expect(gate.isCurrent(first)).toBe(false);
    expect(gate.isCurrent(second)).toBe(true);
  });

  test("never reactivates an older request", () => {
    const gate = new LatestRequest();
    const request = gate.begin();
    gate.begin();
    gate.begin();

    expect(gate.isCurrent(request)).toBe(false);
  });
});
