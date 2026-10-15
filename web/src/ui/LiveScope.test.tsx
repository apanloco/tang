// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { FrameLoop } from "../client/frameloop";
import { FLOOR_DB } from "../client/fft";
import { frequencyColumns } from "../client/scope";
import { Clock } from "../client/testing";
import { SCOPE_FRAME_LENGTH } from "../protocol";
import { LiveScope } from "./LiveScope";
import { callsOf, recordingContext } from "./testing";
import type { RecordingContext } from "./testing";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  Object.defineProperty(window, "devicePixelRatio", { value: 1, configurable: true });
});

/** A frame of the scope: the left side, and the right side, which is the left's if it is not given, each of the length that the server sends. */
function scopeOf(left: readonly number[], right: readonly number[] = left): Float32Array {
  const values = new Float32Array(2 * SCOPE_FRAME_LENGTH);
  values.set(left, 0);
  values.set(right, SCOPE_FRAME_LENGTH);
  return values;
}

/** The left side of a wave that goes up through the middle at the 10th sample and then goes through five values. */
const WAVE = [...new Array<number>(10).fill(-0.5), 0.5, 0, -0.5, 1, 0.25];

/**
 * A scope of what `source` says, 200 px by 100 (the padding of its line is 4), on a frame clock that a test turns and a context that remembers what it was asked to
 * draw. `source.values` and `source.frames` are what the scope reads: a test changes them.
 */
function arrange(props: Partial<React.ComponentProps<typeof LiveScope>> = {}, first: Float32Array | null = scopeOf(WAVE)) {
  const clock = new Clock();
  const frames = new FrameLoop(clock);
  const context = recordingContext();
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation((() => context) as never);
  const source: { values: Float32Array | undefined; frames: number } = { values: first ?? undefined, frames: 1 };
  const read = () => source.values;
  const readFrames = () => source.frames;
  const view = render(<LiveScope label="Output" read={read} readFrames={readFrames} view="wave" samples={5} sampleRate={48_000} frames={frames} {...props} />);
  const meter = screen.getByRole("meter");
  const canvas = view.container.querySelector("canvas");
  if (!canvas) throw new Error("no canvas");
  // (jsdom has no layout: the test says how big the canvas is.)
  canvas.style.width = "200px";
  canvas.style.height = "100px";
  const since = () => {
    context.calls.length = 0;
  };
  const again = (more: Partial<React.ComponentProps<typeof LiveScope>>) => {
    view.rerender(<LiveScope label="Output" read={read} readFrames={readFrames} view="wave" samples={5} sampleRate={48_000} frames={frames} {...props} {...more} />);
  };
  return { clock, frames, view, meter, canvas, context, source, since, again };
}

/** The last `count` points that were drawn through, in order: the picture's line, which is the last that is drawn. */
const lineOf = (context: RecordingContext, count: number): number[][] =>
  [...callsOf(context, "moveTo").slice(-1), ...callsOf(context, "lineTo").slice(-(count - 1))].map((args) => args.map(Number));

/** The strokes that were made, by the colour they were set to. */
function colours(context: RecordingContext): unknown[] {
  const found: unknown[] = [];
  let colour: unknown;
  for (const [name, value] of context.calls) {
    if (name === "strokeStyle") colour = value;
    if (name === "stroke") found.push(colour);
  }
  return found;
}

describe("a scope of the output", () => {
  test("is a meter with its label and a canvas that is hidden from what reads the page, and draws nothing before a frame", () => {
    const { meter, canvas, clock, context } = arrange();
    expect(meter.getAttribute("aria-label")).toBe("Output");
    expect(meter.getAttribute("data-view")).toBe("wave");
    expect(canvas.getAttribute("aria-hidden")).toBe("true");
    expect(context.calls).toEqual([]);
    expect(clock.pending).toBe(1);
  });

  test("makes its bitmap in the pixels of the device and draws in the pixels of the page, whatever the ratio of the one to the other", () => {
    Object.defineProperty(window, "devicePixelRatio", { value: 2, configurable: true });
    const { clock, context, canvas } = arrange();
    clock.tick(0);
    expect([canvas.width, canvas.height]).toEqual([400, 200]);
    expect(context.calls[0]).toEqual(["setTransform", 2, 0, 0, 2, 0, 0]);
    expect(context.calls[1]).toEqual(["clearRect", 0, 0, 200, 100]);
  });

  test("draws the wave from where the left side goes up through the middle, a sample to a point across the width", () => {
    const { clock, context } = arrange();
    clock.tick(0);
    // 5 samples over 200 px, from 0.5 to 1.0 (the room is 4 to 96): 0.5 is a quarter of the way down the room, 1 the top, -0.5 three quarters.
    expect(lineOf(context, 5)).toEqual([
      [0, 27],
      [50, 50],
      [100, 73],
      [150, 4],
      [200, 38.5],
    ]);
  });

  test("draws the latest picture that fits when the wave does not go up through the middle", () => {
    const { clock, context } = arrange({}, scopeOf(new Array<number>(SCOPE_FRAME_LENGTH).fill(0.5).map((_, i) => (i >= SCOPE_FRAME_LENGTH - 5 ? i - SCOPE_FRAME_LENGTH + 5 : 0.5))));
    clock.tick(0);
    // (The last five samples are 0, 1, 2, 3 and 4: beyond full scale, which is held at the top.)
    expect(lineOf(context, 5).map(([, y]) => y)).toEqual([50, 4, 4, 4, 4]);
  });

  test("draws the right side under the left when it is not the same, in a colour of its own, and does not when it is", () => {
    const same = arrange();
    same.clock.tick(0);
    expect(colours(same.context)).toHaveLength(4);
    cleanup();
    const apart = arrange({}, scopeOf(WAVE, WAVE.map((v) => -v)));
    apart.clock.tick(0);
    expect(colours(apart.context)).toHaveLength(5);
    expect(callsOf(apart.context, "moveTo").slice(-2).map(([, y]) => Number(y))).toEqual([73, 27]);
  });

  test("draws a wave again only when there is a new frame, or its size is not the same", () => {
    const { clock, context, source, since, canvas } = arrange();
    clock.tick(0);
    since();
    clock.tick(16);
    expect(context.calls).toEqual([]);
    source.frames = 2;
    clock.tick(33);
    expect(callsOf(context, "clearRect")).toHaveLength(1);
    since();
    canvas.style.width = "300px";
    clock.tick(50);
    expect(callsOf(context, "clearRect")).toEqual([[0, 0, 300, 100]]);
  });

  test("draws the picture that it is told to, a longer time base spreading the same width over more samples", () => {
    const { clock, context } = arrange({ samples: 15 });
    clock.tick(0);
    expect(callsOf(context, "lineTo").slice(-14)).toHaveLength(14);
    expect(callsOf(context, "lineTo").at(-1)?.[0]).toBe(200);
  });

  test("says the loudest sample of what it has, on both sides, as its value, and nothing when it has nothing", () => {
    const { meter, clock, source } = arrange({}, scopeOf([0.1, -0.6, 0.2], [0.3, 0.4, -0.8]));
    clock.tick(0);
    expect(meter.getAttribute("aria-valuenow")).toBe("0.8");
    expect(meter.getAttribute("aria-valuemin")).toBe("0");
    expect(meter.getAttribute("aria-valuemax")).toBe("1");
    source.values = undefined;
    source.frames = 2;
    clock.tick(33);
    expect(meter.hasAttribute("aria-valuenow")).toBe(false);
  });

  test("draws the grid alone when it has nothing, and takes a frame of another length for nothing", () => {
    const { clock, context, source, since } = arrange({}, null);
    clock.tick(0);
    expect(callsOf(context, "clearRect")).toHaveLength(1);
    expect(colours(context)).toHaveLength(3);
    source.values = new Float32Array(10);
    source.frames = 2;
    since();
    clock.tick(33);
    expect(colours(context)).toHaveLength(3);
  });

  test("goes on when there is no context, as jsdom has none, and does what it can: its value", () => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation((() => null) as never);
    render(<LiveScope label="Output" read={() => scopeOf([0.5])} readFrames={() => 1} view="wave" samples={5} sampleRate={48_000} frames={frames} />);
    clock.tick(0);
    expect(screen.getByRole("meter").getAttribute("aria-valuenow")).toBe("0.5");
  });

  test("stops drawing when it is taken off the page", () => {
    const { clock, view } = arrange();
    expect(clock.pending).toBe(1);
    view.unmount();
    expect(clock.pending).toBe(0);
  });
});

/** A sine of full scale at bin `bin` of a block of the length that the server sends. */
const sineAt = (bin: number, amplitude = 1): number[] => Array.from({ length: SCOPE_FRAME_LENGTH }, (_, n) => amplitude * Math.sin((2 * Math.PI * bin * n) / SCOPE_FRAME_LENGTH));

/** The highest point (the smallest y) of the line that the last frame drew, and where across it is. */
function peakOf(context: RecordingContext, width = 200): { x: number; y: number } {
  const points = [...callsOf(context, "moveTo").slice(-1), ...callsOf(context, "lineTo").slice(-width)].map((args) => args.map(Number));
  let best = { x: 0, y: Number.POSITIVE_INFINITY };
  for (const [x, y] of points) if ((y ?? 0) < best.y) best = { x: x ?? 0, y: y ?? 0 };
  return best;
}

describe("a scope of the spectrum", () => {
  test("is a spectrum, with the lines of its grid down at the decades and across at the levels, the area under the line and the line", () => {
    const { clock, context, meter } = arrange({ view: "spectrum" }, scopeOf(sineAt(100, 0.5)));
    clock.tick(0);
    expect(meter.getAttribute("data-view")).toBe("spectrum");
    const range = { minHz: 20, maxHz: 20_000, floorDb: -90, topDb: 0 };
    const columns = frequencyColumns([100, 1000, 10_000], range, 200).map((x) => Math.round(x) + 0.5);
    expect(callsOf(context, "moveTo").slice(0, 3).map(([x]) => x)).toEqual(columns);
    expect(callsOf(context, "moveTo").slice(3, 7).map(([x]) => x)).toEqual([0, 0, 0, 0]);
    expect(callsOf(context, "closePath")).toHaveLength(1);
    expect(callsOf(context, "fill")).toHaveLength(1);
  });

  test("puts the peak of a sound where its frequency is, as high as its level: a sine of half full scale is 6 dB down", () => {
    const { clock, context } = arrange({ view: "spectrum" }, scopeOf(sineAt(100, 0.5)));
    clock.tick(0);
    // Bin 100 of 2048 at 48 kHz is 2343.75 Hz: the column is 200 * ln(2343.75 / 20) / ln(1000), and -6 dB is 84 of the 90 up the room of 92 from 4.
    const { x, y } = peakOf(context);
    expect(x).toBeCloseTo((200 * Math.log(2343.75 / 20)) / Math.log(1000), 0);
    expect(y).toBeCloseTo(4 + (6 / 90) * 92, 0);
  });

  test("takes the two sides together: a sound that is on one side only is half as loud", () => {
    const { clock, context } = arrange({ view: "spectrum" }, scopeOf(sineAt(100, 1), new Array<number>(SCOPE_FRAME_LENGTH).fill(0)));
    clock.tick(0);
    expect(peakOf(context).y).toBeCloseTo(4 + (6.02 / 90) * 92, 0);
  });

  test("falls slowly when the sound stops: 40 dB a second, and not at once", () => {
    const { clock, context, source, since } = arrange({ view: "spectrum" }, scopeOf(sineAt(100)));
    clock.tick(0);
    const loud = peakOf(context).y;
    expect(loud).toBeCloseTo(4, 0);
    source.values = scopeOf(new Array<number>(SCOPE_FRAME_LENGTH).fill(0));
    source.frames = 2;
    since();
    clock.tick(100);
    // A tenth of a second is 4 dB: 4 / 90 of the room.
    expect(peakOf(context).y).toBeCloseTo(4 + (4 / 90) * 92, 0);
    // And the next frame, two tenths later, is 8 dB on that: from the time of the frame before and not of the first.
    source.frames = 3;
    since();
    clock.tick(300);
    expect(peakOf(context).y).toBeCloseTo(4 + (12 / 90) * 92, 0);
    for (let frame = 4; frame <= 40; frame += 1) {
      source.frames = frame;
      since();
      clock.tick(100 * frame);
    }
    // (Four seconds later it is down 90 dB and more: at the bottom of the room.)
    expect(peakOf(context).y).toBeCloseTo(96, 0);
  });

  test("is not held up by a long wait between frames: the fall is of a quarter of a second at the most", () => {
    const { clock, context, source, since } = arrange({ view: "spectrum" }, scopeOf(sineAt(100)));
    clock.tick(0);
    source.values = scopeOf(new Array<number>(SCOPE_FRAME_LENGTH).fill(0));
    source.frames = 2;
    since();
    clock.tick(5000);
    // 10 dB, and not the 200 that five seconds is.
    expect(peakOf(context).y).toBeCloseTo(4 + (10 / 90) * 92, 0);
  });

  test("ends at half the sample rate where that is under 20 kHz: there is nothing above it to draw", () => {
    const { clock, context } = arrange({ view: "spectrum", sampleRate: 22_050 }, scopeOf(sineAt(100)));
    clock.tick(0);
    const range = { minHz: 20, maxHz: 11_025, floorDb: -90, topDb: 0 };
    const columns = frequencyColumns([100, 1000, 10_000], range, 200).map((x) => Math.round(x) + 0.5);
    expect(callsOf(context, "moveTo").slice(0, 3).map(([x]) => x)).toEqual(columns);
  });

  test("starts from the floor, and the floor is the lowest there is", () => {
    const { clock, context } = arrange({ view: "spectrum" }, scopeOf(new Array<number>(SCOPE_FRAME_LENGTH).fill(0)));
    clock.tick(0);
    expect(FLOOR_DB).toBeLessThan(-90);
    expect(peakOf(context).y).toBeCloseTo(96, 5);
  });

  test("has the labels of its grid under the decades, which the wave does not", () => {
    const { view, again, clock, canvas } = arrange({ view: "spectrum" });
    clock.tick(0);
    const labels = () => Array.from(view.container.querySelectorAll(".tang-scope__label"), (label) => label.textContent);
    expect(labels()).toEqual(["100", "1k", "10k"]);
    // Each is where its line is: a decade as far as the next, from 20 Hz to 20 kHz across the width.
    const places = Array.from(view.container.querySelectorAll<HTMLElement>(".tang-scope__label"), (label) => Number.parseFloat(label.style.left));
    expect(places[0]).toBeCloseTo((100 * Math.log(5)) / Math.log(1000), 3);
    expect(places[1]).toBeCloseTo((100 * Math.log(50)) / Math.log(1000), 3);
    expect(places[2]).toBeCloseTo((100 * Math.log(500)) / Math.log(1000), 3);
    again({ view: "wave" });
    expect(labels()).toEqual([]);
    expect(canvas.isConnected).toBe(true);
  });

  test("changes from one picture to the other when told to, drawing the new one on the next frame whether or not there is a frame to it", () => {
    const { clock, context, again, since } = arrange({ view: "wave" }, scopeOf(sineAt(100)));
    clock.tick(0);
    expect(callsOf(context, "closePath")).toHaveLength(0);
    since();
    again({ view: "spectrum" });
    clock.tick(16);
    expect(callsOf(context, "closePath")).toHaveLength(1);
  });
});
