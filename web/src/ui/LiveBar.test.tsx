// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { Profiler } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { FrameLoop } from "../client/frameloop";
import { Clock } from "../client/testing";
import { LiveBar } from "./LiveBar";

afterEach(cleanup);

function arrange(read: () => number | undefined, props: Partial<React.ComponentProps<typeof LiveBar>> = {}) {
  const clock = new Clock();
  const frames = new FrameLoop(clock);
  const view = render(<LiveBar label="Output" read={read} min={-1} max={1} frames={frames} {...props} />);
  const bar = screen.getByRole("meter");
  const fill = view.container.querySelector<HTMLElement>(".tang-livebar__fill");
  if (!fill) throw new Error("no fill");
  /** What the fill spans, as fractions of the bar. */
  const span = () => [Number(fill.style.getPropertyValue("--from")), Number(fill.style.getPropertyValue("--to"))];
  return { clock, frames, view, bar, fill, span };
}

describe("a bar that shows a live value", () => {
  test("is a meter with its label and its ends, which has nothing in it before a frame", () => {
    const { bar, span, clock } = arrange(() => 0.5);
    expect(bar.getAttribute("aria-label")).toBe("Output");
    expect(bar.getAttribute("aria-valuemin")).toBe("-1");
    expect(bar.getAttribute("aria-valuemax")).toBe("1");
    expect(bar.hasAttribute("aria-valuenow")).toBe(false);
    expect(span()).toEqual([0, 0]);
    expect(clock.pending).toBe(1);
  });

  test("fills from the left to the value, on the frame", () => {
    let value = 0;
    const { bar, span, clock } = arrange(() => value);
    clock.tick(16);
    expect(span()).toEqual([0, 0.5]); // 0 in -1…1 is half way
    expect(bar.getAttribute("aria-valuenow")).toBe("0");
    value = 1;
    clock.tick(33);
    expect(span()).toEqual([0, 1]);
    value = -1;
    clock.tick(50);
    expect(span()).toEqual([0, 0]);
    expect(bar.getAttribute("aria-valuenow")).toBe("-1");
  });

  test("stops at the ends, as a value beyond them is drawn at them", () => {
    let value = 5;
    const { span, clock } = arrange(() => value);
    clock.tick(16);
    expect(span()).toEqual([0, 1]);
    value = -5;
    clock.tick(33);
    expect(span()).toEqual([0, 0]);
  });

  test("fills from the middle for a value that goes both ways", () => {
    let value = 0.5;
    const { span, clock } = arrange(() => value, { centred: true });
    clock.tick(16);
    expect(span()).toEqual([0.5, 0.75]);
    value = -0.5;
    clock.tick(33);
    expect(span()).toEqual([0.25, 0.5]);
    value = 0;
    clock.tick(50);
    expect(span()).toEqual([0.5, 0.5]);
  });

  test("says the value in words for those who do not see the bar, the way it is told to", () => {
    const { bar, clock } = arrange(() => -12.34, { min: -60, max: 0, format: (value) => `${value.toFixed(1)} dB` });
    clock.tick(16);
    expect(bar.getAttribute("aria-valuetext")).toBe("-12.3 dB");
    expect(bar.getAttribute("aria-valuenow")).toBe("-12.34");
  });

  test("shows nothing when there is nothing to show, and comes back", () => {
    let value: number | undefined = 0.5;
    const { bar, span, clock } = arrange(() => value);
    clock.tick(16);
    expect(span()[1]).toBeGreaterThan(0);
    value = undefined;
    clock.tick(33);
    expect(span()).toEqual([0, 0]);
    expect(bar.hasAttribute("aria-valuenow")).toBe(false);
    expect(bar.hasAttribute("aria-valuetext")).toBe(false);
    value = Number.NaN;
    clock.tick(50);
    expect(span()).toEqual([0, 0]);
    value = Number.POSITIVE_INFINITY;
    clock.tick(66);
    expect(bar.hasAttribute("aria-valuenow")).toBe(false);
    value = 0.5;
    clock.tick(83);
    expect(span()[1]).toBeGreaterThan(0);
    expect(bar.hasAttribute("aria-valuenow")).toBe(true);
  });

  test("is written only when the value moved", () => {
    let value = 0.2;
    const { fill, clock } = arrange(() => value);
    const write = vi.spyOn(fill.style, "setProperty");
    clock.tick(16);
    expect(write).toHaveBeenCalledTimes(2);
    clock.tick(33);
    clock.tick(50);
    expect(write).toHaveBeenCalledTimes(2);
    value = 0.3;
    clock.tick(66);
    expect(write).toHaveBeenCalledTimes(4);
  });

  test("does not make anything render again, however the value moves", () => {
    let value = -1;
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    let renders = 0;
    const view = render(
      <Profiler id="bar" onRender={() => (renders += 1)}>
        <LiveBar label="a" read={() => value} min={-1} max={1} frames={frames} />
      </Profiler>,
    );
    const mounted = renders;
    for (let frame = 1; frame <= 100; frame += 1) {
      value = Math.sin(frame);
      clock.tick(frame * 16);
    }
    expect(renders).toBe(mounted);
    expect(view.container.querySelector<HTMLElement>(".tang-livebar__fill")?.style.getPropertyValue("--to")).not.toBe("0");
  });

  test("asks for no frame once it is gone, and for another when it is given another source", () => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const first = vi.fn(() => 0.1);
    const second = vi.fn(() => 0.9);
    const { rerender, unmount } = render(<LiveBar label="a" read={first} min={0} max={1} frames={frames} />);
    clock.tick(16);
    rerender(<LiveBar label="a" read={second} min={0} max={1} frames={frames} />);
    clock.tick(33);
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(1);
    unmount();
    expect(clock.pending).toBe(0);
  });

  test("is drawn again when its ends change under it, or it starts to fill from the middle", () => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const read = () => 0.5;
    const view = render(<LiveBar label="a" read={read} min={0} max={1} frames={frames} />);
    const fill = view.container.querySelector<HTMLElement>(".tang-livebar__fill");
    clock.tick(16);
    expect(fill?.style.getPropertyValue("--to")).toBe("0.5");
    view.rerender(<LiveBar label="a" read={read} min={0} max={2} frames={frames} />);
    clock.tick(33);
    expect(fill?.style.getPropertyValue("--to")).toBe("0.25");
    view.rerender(<LiveBar label="a" read={read} min={0} max={2} centred frames={frames} />);
    clock.tick(50);
    expect([fill?.style.getPropertyValue("--from"), fill?.style.getPropertyValue("--to")]).toEqual(["0.25", "0.5"]);
    expect(view.container.querySelector(".tang-livebar")?.hasAttribute("data-centred")).toBe(true);
  });
});
