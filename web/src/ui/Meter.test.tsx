// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { FrameLoop } from "../client/frameloop";
import { Clock } from "../client/testing";
import { Meter } from "./Meter";

afterEach(cleanup);

function arrange(readPeak: () => number | undefined, readClips?: () => number | undefined) {
  const clock = new Clock();
  const frames = new FrameLoop(clock);
  const view = render(<Meter label="Master" readPeak={readPeak} frames={frames} {...(readClips ? { readClips } : {})} />);
  const bar = screen.getByRole("meter");
  const fill = view.container.querySelector<HTMLElement>(".tang-livebar__fill");
  return { clock, frames, view, bar, to: () => Number(fill?.style.getPropertyValue("--to")) };
}

describe("a level meter", () => {
  test("is a bar from -60 to 0 dB", () => {
    const { bar } = arrange(() => 0.5);
    expect(bar.getAttribute("aria-label")).toBe("Master");
    expect(bar.getAttribute("aria-valuemin")).toBe("-60");
    expect(bar.getAttribute("aria-valuemax")).toBe("0");
  });

  test("fills the bar by the peak's level in decibels, not by the peak", () => {
    let peak = 1;
    const { bar, to, clock } = arrange(() => peak);
    clock.tick(16);
    expect(to()).toBe(1);
    expect(bar.getAttribute("aria-valuetext")).toBe("0.0 dB");
    peak = 0.1; // -20 dB: two thirds of the way up from -60
    clock.tick(33);
    expect(to()).toBeCloseTo(2 / 3, 6);
    expect(bar.getAttribute("aria-valuetext")).toBe("-20.0 dB");
    peak = 0.001; // -60 dB
    clock.tick(50);
    expect(to()).toBeCloseTo(0, 6);
  });

  test("is empty for silence, full for a clip, and has nothing when there is no reading", () => {
    let peak: number | undefined = 0;
    const { bar, to, clock } = arrange(() => peak);
    clock.tick(16);
    expect(to()).toBe(0);
    expect(bar.getAttribute("aria-valuetext")).toBe("-60.0 dB");
    peak = 3;
    clock.tick(33);
    expect(to()).toBe(1);
    peak = undefined;
    clock.tick(50);
    expect(to()).toBe(0);
    expect(bar.hasAttribute("aria-valuenow")).toBe(false);
    peak = Number.NaN;
    clock.tick(66);
    expect(bar.hasAttribute("aria-valuenow")).toBe(false);
  });

  test("has a CLIP light when it is given the count, which comes on when the count goes up, and none when not", () => {
    let clips = 0;
    const withLight = arrange(
      () => 0.5,
      () => clips,
    );
    expect(screen.getByRole("img").textContent).toBe("CLIP");
    withLight.clock.tick(0);
    clips = 1;
    withLight.clock.tick(100);
    expect(screen.getByRole("img").hasAttribute("data-lit")).toBe(true);
    cleanup();
    arrange(() => 0.5);
    expect(screen.queryByRole("img")).toBeNull();
  });

  test("reads another source when it is given one", () => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const view = render(<Meter label="a" readPeak={() => 1} frames={frames} />);
    clock.tick(16);
    const fill = view.container.querySelector<HTMLElement>(".tang-livebar__fill");
    expect(fill?.style.getPropertyValue("--to")).toBe("1");
    view.rerender(<Meter label="a" readPeak={() => 0.1} frames={frames} />);
    clock.tick(33);
    expect(Number(fill?.style.getPropertyValue("--to"))).toBeCloseTo(2 / 3, 6);
  });
});
