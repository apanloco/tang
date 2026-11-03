// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { Profiler } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { FrameLoop } from "../client/frameloop";
import { Clock } from "../client/testing";
import { ClipLight } from "./ClipLight";

afterEach(cleanup);

function arrange(read: () => number | undefined, holdMs?: number) {
  const clock = new Clock();
  const frames = new FrameLoop(clock);
  const view = render(<ClipLight readClips={read} frames={frames} {...(holdMs === undefined ? {} : { holdMs })} />);
  const light = screen.getByRole("img");
  return { clock, frames, view, light, lit: () => light.hasAttribute("data-lit") };
}

describe("the clip light", () => {
  test("says what it is, and is out", () => {
    const { light, lit, clock } = arrange(() => 0);
    expect(light.textContent).toBe("CLIP");
    expect(light.getAttribute("aria-label")).toBe("Clip indicator");
    expect(lit()).toBe(false);
    expect(clock.pending).toBe(1);
  });

  test("is out while the count stays where it is, and the first reading is where it is, not news", () => {
    const { lit, clock } = arrange(() => 7);
    clock.tick(0);
    expect(lit()).toBe(false);
    clock.tick(16);
    clock.tick(5000);
    expect(lit()).toBe(false);
  });

  test("comes on when the count goes up, and goes out two seconds later", () => {
    let clips = 0;
    const { lit, clock } = arrange(() => clips);
    clock.tick(100);
    clips = 1;
    clock.tick(200);
    expect(lit()).toBe(true);
    clock.tick(1000);
    expect(lit()).toBe(true);
    clock.tick(2199);
    expect(lit()).toBe(true);
    clock.tick(2200); // 2000 ms after it went up
    expect(lit()).toBe(false);
  });

  test("stays on for as long as the count keeps going up, from the last time it did", () => {
    let clips = 0;
    const { lit, clock } = arrange(() => clips);
    clock.tick(0);
    clips = 1;
    clock.tick(1000);
    clips = 2;
    clock.tick(2500); // would have gone out at 3000; now at 4500
    clock.tick(3500);
    expect(lit()).toBe(true);
    clock.tick(4499);
    expect(lit()).toBe(true);
    clock.tick(4500);
    expect(lit()).toBe(false);
  });

  test("stays on for as long as it was told to", () => {
    let clips = 0;
    const { lit, clock } = arrange(() => clips, 300);
    clock.tick(0);
    clips = 1;
    clock.tick(1000);
    expect(lit()).toBe(true);
    clock.tick(1299);
    expect(lit()).toBe(true);
    clock.tick(1300);
    expect(lit()).toBe(false);
  });

  test("goes on as it was when there is nothing to read for a while, and does not take the count that comes after as news", () => {
    let clips: number | undefined = 3;
    const { lit, clock } = arrange(() => clips);
    clock.tick(0);
    clips = undefined;
    clock.tick(100);
    clock.tick(200);
    expect(lit()).toBe(false);
    clips = 3;
    clock.tick(300);
    expect(lit()).toBe(false);
    clips = 4;
    clock.tick(400);
    expect(lit()).toBe(true);
  });

  test("takes a count that went up while there was nothing to read as news: it was clipping meanwhile", () => {
    let clips: number | undefined = 3;
    const { lit, clock } = arrange(() => clips);
    clock.tick(0);
    clips = undefined;
    clock.tick(100);
    clips = 5;
    clock.tick(200);
    expect(lit()).toBe(true);
  });

  test("takes a count that is lower than before (a new layout) as where the count is now", () => {
    let clips = 10;
    const { lit, clock } = arrange(() => clips);
    clock.tick(0);
    clips = 0;
    clock.tick(100);
    expect(lit()).toBe(false);
    clips = 1;
    clock.tick(200);
    expect(lit()).toBe(true);
  });

  test("is written only when it changes", () => {
    let clips = 0;
    const { light, clock } = arrange(() => clips);
    const toggle = vi.spyOn(light, "toggleAttribute");
    clock.tick(0);
    clock.tick(16);
    clock.tick(33);
    expect(toggle).toHaveBeenCalledTimes(0);
    clips = 1;
    clock.tick(50);
    clock.tick(66);
    clock.tick(83);
    expect(toggle).toHaveBeenCalledTimes(1);
    clock.tick(5000);
    expect(toggle).toHaveBeenCalledTimes(2);
  });

  test("does not make anything render again", () => {
    let clips = 0;
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    let renders = 0;
    render(
      <Profiler id="clip" onRender={() => (renders += 1)}>
        <ClipLight readClips={() => clips} frames={frames} />
      </Profiler>,
    );
    const mounted = renders;
    for (let frame = 1; frame <= 50; frame += 1) {
      clips += frame % 10 === 0 ? 1 : 0;
      clock.tick(frame * 100);
    }
    expect(renders).toBe(mounted);
  });

  test("holds for the time it is given now, when that changes", () => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    let clips = 0;
    const read = () => clips;
    const { rerender } = render(<ClipLight readClips={read} holdMs={1000} frames={frames} />);
    const light = screen.getByRole("img");
    clock.tick(0);
    rerender(<ClipLight readClips={read} holdMs={100} frames={frames} />);
    clock.tick(10);
    clips = 1;
    clock.tick(50);
    expect(light.hasAttribute("data-lit")).toBe(true);
    clock.tick(149);
    expect(light.hasAttribute("data-lit")).toBe(true);
    clock.tick(150);
    expect(light.hasAttribute("data-lit")).toBe(false);
  });

  test("reads the frames of another loop when it is given one", () => {
    const here = new Clock();
    const there = new Clock();
    let clips = 0;
    const read = () => clips;
    const { rerender } = render(<ClipLight readClips={read} frames={new FrameLoop(here)} />);
    const light = screen.getByRole("img");
    here.tick(0);
    rerender(<ClipLight readClips={read} frames={new FrameLoop(there)} />);
    expect(here.pending).toBe(0);
    expect(there.pending).toBe(1);
    there.tick(0);
    clips = 1;
    there.tick(50);
    expect(light.hasAttribute("data-lit")).toBe(true);
  });

  test("asks for no frames once it is gone, and reads another source when it is given one", () => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const first = vi.fn(() => 0);
    const second = vi.fn(() => 0);
    const { rerender, unmount } = render(<ClipLight readClips={first} frames={frames} />);
    clock.tick(0);
    rerender(<ClipLight readClips={second} frames={frames} />);
    clock.tick(16);
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(1);
    unmount();
    expect(clock.pending).toBe(0);
  });
});
