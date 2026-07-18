import { describe, expect, test } from "bun:test";
import { remotePanPosition } from "./remote-pan";

describe("remote desktop local panning", () => {
  const extent = {
    clientWidth: 1_200,
    clientHeight: 700,
    scrollWidth: 1_920,
    scrollHeight: 1_080,
  };

  test("moves the viewport opposite to the pointer drag", () => {
    expect(remotePanPosition({
      clientX: 500,
      clientY: 400,
      scrollLeft: 360,
      scrollTop: 190,
    }, 420, 330, extent)).toEqual({ left: 440, top: 260 });
  });

  test("clamps panning to the available remote canvas", () => {
    expect(remotePanPosition({
      clientX: 500,
      clientY: 400,
      scrollLeft: 700,
      scrollTop: 370,
    }, -500, -600, extent)).toEqual({ left: 720, top: 380 });

    expect(remotePanPosition({
      clientX: 500,
      clientY: 400,
      scrollLeft: 20,
      scrollTop: 10,
    }, 900, 800, extent)).toEqual({ left: 0, top: 0 });
  });

  test("does not scroll when the remote canvas fits the viewport", () => {
    expect(remotePanPosition({
      clientX: 100,
      clientY: 100,
      scrollLeft: 0,
      scrollTop: 0,
    }, 20, 20, {
      clientWidth: 1_920,
      clientHeight: 1_080,
      scrollWidth: 1_920,
      scrollHeight: 1_080,
    })).toEqual({ left: 0, top: 0 });
  });
});
