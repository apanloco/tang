// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { FrameLoop } from "../client/frameloop";
import { Clock } from "../client/testing";
import { LiveTrace } from "./LiveTrace";
import { callsOf, recordingContext } from "./testing";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  Object.defineProperty(window, "devicePixelRatio", { value: 1, configurable: true });
});

/**
 * A trace of `read` over a second (so that 5 ms is a pixel of the 200 the canvas is drawn at), on a frame clock that a test
 * turns, and a context that remembers what it was asked to draw (or none, as jsdom has).
 */
function arrange(read: () => number | undefined, props: Partial<React.ComponentProps<typeof LiveTrace>> = {}, options: { context?: boolean; ratio?: number } = {}) {
  const clock = new Clock();
  const frames = new FrameLoop(clock);
  const context = recordingContext();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation((() => (options.context === false ? null : context)) as never);
  Object.defineProperty(window, "devicePixelRatio", { value: options.ratio ?? 1, configurable: true });
  const view = render(<LiveTrace label="Output" read={read} min={-1} max={1} seconds={1} frames={frames} {...props} />);
  const meter = screen.getByRole("meter");
  const canvas = view.container.querySelector("canvas");
  if (!canvas) throw new Error("no canvas");
  // (jsdom has no layout: the test says how big the canvas is.)
  canvas.style.width = "200px";
  canvas.style.height = "40px";
  /** What was drawn from now on. */
  const since = () => {
    context.calls.length = 0;
  };
  return { clock, frames, view, meter, canvas, context, since };
}

/** Where the line of the last frame went: the points it moved to and drew through. */
const lineOf = (context: ReturnType<typeof recordingContext>): number[][] => [...callsOf(context, "moveTo").slice(-1), ...callsOf(context, "lineTo")].map((args) => args.map(Number));

describe("a trace that shows a live value", () => {
  test("is a meter with its label and its ends, with a canvas that is hidden from what reads the page, and nothing before a frame", () => {
    const { meter, canvas, clock, context } = arrange(() => 0.5);
    expect(meter.getAttribute("aria-label")).toBe("Output");
    expect(meter.getAttribute("aria-valuemin")).toBe("-1");
    expect(meter.getAttribute("aria-valuemax")).toBe("1");
    expect(meter.hasAttribute("aria-valuenow")).toBe(false);
    expect(meter.hasAttribute("data-centred")).toBe(false);
    expect(canvas.getAttribute("aria-hidden")).toBe("true");
    expect(context.calls).toEqual([]);
    expect(clock.pending).toBe(1);
  });

  test("says that it goes both ways when it does", () => {
    const { meter } = arrange(() => 0, { centred: true });
    expect(meter.hasAttribute("data-centred")).toBe(true);
  });

  test("says the value as it is on each frame, and nothing when there is none", () => {
    let value: number | undefined = 0.25;
    const { meter, clock } = arrange(() => value, {}, { context: false });
    clock.tick(0);
    expect(meter.getAttribute("aria-valuenow")).toBe("0.25");
    expect(meter.getAttribute("aria-valuetext")).toBe("0.25");
    value = -1;
    clock.tick(16);
    expect(meter.getAttribute("aria-valuenow")).toBe("-1");
    value = undefined;
    clock.tick(33);
    expect(meter.hasAttribute("aria-valuenow")).toBe(false);
    expect(meter.hasAttribute("aria-valuetext")).toBe(false);
  });

  test("says the value in the words it is given", () => {
    const { meter, clock } = arrange(() => 0.5, { format: (value) => `${String(value * 100)} %` });
    clock.tick(0);
    expect(meter.getAttribute("aria-valuetext")).toBe("50 %");
  });

  test("draws a line through the samples it has had, the newest at the right edge and the rest as far left as they are old", () => {
    let value = 0;
    const { clock, context, since } = arrange(() => value);
    clock.tick(0);
    value = 1;
    clock.tick(250);
    value = -1;
    since();
    clock.tick(500);
    // A second is 200 px, the values -1 to 1 are the height of 40 less 3 above and below: 0 is the middle, 1 the top, -1 the bottom.
    expect(lineOf(context)).toEqual([
      [100, 20],
      [150, 3],
      [200, 37],
    ]);
  });

  test("scrolls: the same value a frame later is drawn further left, and held to the right edge", () => {
    const { clock, context, since } = arrange(() => 0.5);
    clock.tick(0);
    clock.tick(100);
    since();
    clock.tick(200);
    // (0.5 in -1…1 is three quarters of the way up the 34 px between the paddings.)
    expect(lineOf(context)).toEqual([
      [160, 11.5],
      [180, 11.5],
      [200, 11.5],
    ]);
  });

  test("shows as much of the past as it is told to", () => {
    const { clock, context, since } = arrange(() => 0, { seconds: 2 });
    clock.tick(0);
    since();
    clock.tick(500);
    expect(lineOf(context).map((point) => point[0])).toEqual([150, 200]);
  });

  test("clears the canvas on every draw, before it draws, and draws a dot at the newest value", () => {
    const { clock, context, since } = arrange(() => 0);
    clock.tick(0);
    since();
    clock.tick(16);
    expect(context.calls[0]?.[0]).toBe("setTransform");
    expect(context.calls[1]).toEqual(["clearRect", 0, 0, 200, 40]);
    expect(callsOf(context, "arc")).toEqual([[197, 20, 2.5, 0, 2 * Math.PI]]);
  });

  test("draws a mark across the middle when the value goes both ways, and none when it does not", () => {
    const centred = arrange(() => 0, { centred: true });
    centred.clock.tick(0);
    expect(callsOf(centred.context, "moveTo")[0]).toEqual([0, 20.5]);
    cleanup();
    const plain = arrange(() => 0);
    plain.clock.tick(0);
    expect(callsOf(plain.context, "moveTo")).toEqual([]);
  });

  test("makes its canvas as big as it is on the page, in the pixels of the device, and draws at that scale", () => {
    const { clock, canvas, context } = arrange(() => 0, {}, { ratio: 2 });
    clock.tick(0);
    expect(canvas.width).toBe(400);
    expect(canvas.height).toBe(80);
    expect(context.calls[0]).toEqual(["setTransform", 2, 0, 0, 2, 0, 0]);
    expect(context.calls[1]).toEqual(["clearRect", 0, 0, 200, 40]);
  });

  test("follows the size it is made, on the frame that sees it, and draws again at the new size", () => {
    const { clock, canvas, context, since } = arrange(() => 0);
    clock.tick(0);
    expect([canvas.width, canvas.height]).toEqual([200, 40]);
    canvas.style.width = "100px";
    since();
    clock.tick(16);
    expect([canvas.width, canvas.height]).toEqual([100, 40]);
    expect(context.calls[1]).toEqual(["clearRect", 0, 0, 100, 40]);
    // (The line is across the new width: a second is 100 px now.)
    expect(callsOf(context, "arc")).toEqual([[97, 20, 2.5, 0, 2 * Math.PI]]);
  });

  test("draws nothing on a canvas that has no size (a node that is not shown)", () => {
    const { clock, canvas, context } = arrange(() => 0);
    Object.defineProperty(canvas, "offsetWidth", { value: 0 });
    clock.tick(0);
    expect(context.calls).toEqual([]);
    cleanup();
    const flat = arrange(() => 0);
    Object.defineProperty(flat.canvas, "offsetHeight", { value: 0 });
    flat.clock.tick(0);
    expect(flat.context.calls).toEqual([]);
  });

  test("does not draw again once the value has not moved for as long as it shows: what is drawn is the same whenever", () => {
    let value = 0.5;
    const { clock, context, since } = arrange(() => value);
    // Every 100 ms for a second and a half: the line is the whole width from the frame at a second.
    for (let time = 0; time <= 1500; time += 100) clock.tick(time);
    expect(callsOf(context, "clearRect")).toHaveLength(11);
    since();
    clock.tick(1600);
    clock.tick(1700);
    expect(context.calls).toEqual([]);
    // A value that moves is drawn, and then every frame while the line is not the whole width of the same.
    value = 0.75;
    clock.tick(1800);
    expect(callsOf(context, "clearRect")).toHaveLength(1);
    since();
    clock.tick(1900);
    expect(callsOf(context, "clearRect")).toHaveLength(1);
  });

  test("draws again when the size changes after it has settled", () => {
    const { clock, canvas, context, since } = arrange(() => 0.5);
    for (let time = 0; time <= 1500; time += 100) clock.tick(time);
    since();
    canvas.style.width = "150px";
    clock.tick(1600);
    expect(callsOf(context, "clearRect")).toEqual([[0, 0, 150, 40]]);
  });

  test("forgets what it was showing when the value goes, and draws nothing of it", () => {
    let value: number | undefined = 0.5;
    const { clock, context, since } = arrange(() => value);
    clock.tick(0);
    clock.tick(100);
    value = undefined;
    since();
    clock.tick(200);
    expect(callsOf(context, "clearRect")).toHaveLength(1);
    expect(callsOf(context, "lineTo")).toEqual([]);
    expect(callsOf(context, "arc")).toEqual([]);
    // And there is nothing to do on the frames after, as there is nothing to scroll.
    since();
    clock.tick(300);
    expect(context.calls).toEqual([]);
    // It starts again with the value.
    value = 0.25;
    clock.tick(400);
    expect(callsOf(context, "arc")).toHaveLength(1);
  });

  test("takes no sample of a value that is not a number", () => {
    let value = 0.5;
    const { clock, context, since } = arrange(() => value);
    clock.tick(0);
    value = Number.NaN;
    since();
    clock.tick(100);
    expect(callsOf(context, "arc")).toEqual([]);
  });

  test("does what it can without a canvas to draw on, which jsdom has not: it says the value", () => {
    const { clock, meter } = arrange(() => 0.5, {}, { context: false });
    expect(() => {
      clock.tick(0);
      clock.tick(16);
    }).not.toThrow();
    expect(meter.getAttribute("aria-valuenow")).toBe("0.5");
  });

  test("subscribes once to the frames, and stops when it is taken away", () => {
    const { clock, frames } = arrange(() => 0);
    expect(clock.pending).toBe(1);
    clock.tick(0);
    expect(clock.pending).toBe(1);
    cleanup();
    expect(clock.pending).toBe(0);
    expect(frames.running).toBe(false);
  });
});
