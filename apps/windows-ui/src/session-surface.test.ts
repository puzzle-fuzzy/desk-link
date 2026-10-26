import { expect, test } from "bun:test";
import { retainSessionSurface } from "./session-surface";

test("only the current visible session generation is eligible for shell-only updates", () => {
  expect(retainSessionSurface("1:1", "1:1", true, true)).toBe(true);
  for (const args of [[null,null,true,true], ["1:1","1:2",true,true], ["1:1","1:1",false,true], ["1:1","1:1",true,false]] as const) {
    expect(retainSessionSurface(args[0], args[1], args[2], args[3])).toBe(false);
  }
});
