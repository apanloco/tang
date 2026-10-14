// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { Profiler } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { envelopeAreaPath, envelopePath, placeIn, shapeOf } from "../client/envelope";
import { FrameLoop } from "../client/frameloop";
import { Clock } from "../client/testing";
import { ENVELOPE_HEIGHT, ENVELOPE_PAD, ENVELOPE_WIDTH, LiveEnvelope } from "./LiveEnvelope";

afterEach(cleanup);

const NEW = { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 };
const BOX = { width: ENVELOPE_WIDTH, height: ENVELOPE_HEIGHT, pad: ENVELOPE_PAD };

/** An envelope on a frame clock that a test turns, reading `read`. */
function arrange(read: () => number | undefined, props: Partial<React.ComponentProps<typeof LiveEnvelope>> = {}) {
  const clock = new Clock();
  const frames = new FrameLoop(clock);
  const view = render(<LiveEnvelope label="ADSR shape" stages={NEW} read={read} frames={frames} {...props} />);
  const image = screen.getByRole("img");
  const dot = view.container.querySelector("circle");
  const line = view.container.querySelector(".tang-envelope__line");
  const fill = view.container.querySelector(".tang-envelope__fill");
  if (!dot || !line || !fill) throw new Error("the envelope has no dot, line or fill");
  return { clock, frames, view, image, dot, line, fill };
}

const at = (dot: SVGCircleElement): { x: number; y: number } => ({ x: Number(dot.getAttribute("cx")), y: Number(dot.getAttribute("cy")) });

describe("an envelope drawn from its settings, with a dot where it is", () => {
  test("is an image with its label, a line and a fill made from the settings, and a dot that is not shown before a frame", () => {
    const { image, line, fill, dot, clock } = arrange(() => 0);
    expect(image.getAttribute("aria-label")).toBe("ADSR shape");
    expect(line.getAttribute("d")).toBe(envelopePath(shapeOf(NEW), BOX));
    expect(fill.getAttribute("d")).toBe(envelopeAreaPath(shapeOf(NEW), BOX));
    expect(dot.getAttribute("visibility")).toBe("hidden");
    expect(dot.getAttribute("aria-hidden")).toBe("true");
    expect(clock.pending).toBe(1);
  });

  test("puts the dot at the start of the line while the envelope puts out nothing", () => {
    const { dot, clock } = arrange(() => 0);
    clock.tick(0);
    expect(dot.getAttribute("visibility")).toBe("visible");
    expect(at(dot)).toEqual(placeIn(BOX, 0, 0));
  });

  test("moves the dot along the attack as the level rises, down the decay as it falls to the sustain, along the hold, and down the release", () => {
    let value: number | undefined = 0;
    const { dot, clock } = arrange(() => value);
    const shape = shapeOf(NEW);
    const frame = (level: number, time: number) => {
      value = level;
      clock.tick(time);
      return at(dot);
    };
    clock.tick(0);
    // The attack: rising, the dot on the line from the start to the top.
    const up = frame(0.5, 16);
    expect(up.x).toBeGreaterThan(placeIn(BOX, 0, 0).x);
    expect(up.x).toBeLessThan(placeIn(BOX, shape.attackEnd, 1).x);
    expect(up.y).toBeCloseTo(placeIn(BOX, 0, 0.5).y, 9);
    // The decay: falling from the top towards the sustain, further along and lower.
    frame(1, 33);
    const down = frame(0.85, 50);
    expect(down.x).toBeGreaterThan(placeIn(BOX, shape.attackEnd, 1).x);
    expect(down.x).toBeLessThan(placeIn(BOX, shape.decayEnd, 0.7).x);
    // The hold: the level stays at the sustain, in the middle of its stretch.
    frame(0.7, 66);
    const held = frame(0.7, 83);
    expect(held.x).toBeCloseTo(placeIn(BOX, (shape.decayEnd + shape.holdEnd) / 2, 0.7).x, 9);
    expect(held.y).toBeCloseTo(placeIn(BOX, 0, 0.7).y, 9);
    // The release: falling from the sustain, past the end of the hold.
    const released = frame(0.3, 100);
    expect(released.x).toBeGreaterThan(placeIn(BOX, shape.holdEnd, 0.7).x);
    expect(released.y).toBeCloseTo(placeIn(BOX, 0, 0.3).y, 9);
    // And at rest it is at the start again.
    expect(frame(0, 116)).toEqual(placeIn(BOX, 0, 0));
  });

  test("keeps a rise on the attack through the frames that find the level where it was: the telemetry comes more slowly than the page's frames", () => {
    let value: number | undefined = 0;
    const { dot, clock } = arrange(() => value);
    const shape = shapeOf(NEW);
    clock.tick(0);
    // Each level is read on two frames (30 readings a second against 60 frames), on the way from nothing to the top. The level the sustain holds is
    // among them: the attack goes through it on its way up, and is not the hold.
    let time = 16;
    for (const level of [0.1, 0.3, 0.5, 0.7, 0.9, 1]) {
      value = level;
      for (let frame = 1; frame <= 2; frame += 1) {
        clock.tick(time);
        time += 16;
        const place = placeIn(BOX, level * shape.attackEnd, level);
        expect(at(dot).x, `x at ${String(level)}, frame ${String(frame)}`).toBeCloseTo(place.x, 9);
        expect(at(dot).y, `y at ${String(level)}, frame ${String(frame)}`).toBeCloseTo(place.y, 9);
      }
    }
  });

  test("takes the dot from the attack to the hold when a rise has stood where it is for as long as the telemetry takes to be sure: the attack and the decay were over between two readings", () => {
    let value: number | undefined = 0;
    const { dot, clock } = arrange(() => value);
    const shape = shapeOf(NEW);
    clock.tick(0);
    // The first reading of the note is the sustain already (a pluck whose attack and decay took less than a reading, or the mock's envelope): a rise.
    value = 0.7;
    clock.tick(16);
    const onTheAttack = placeIn(BOX, 0.7 * shape.attackEnd, 0.7).x;
    expect(at(dot).x).toBeCloseTo(onTheAttack, 9);
    // The frames that find it where it was are no news for a while (the next reading may be on its way) ...
    for (const time of [33, 50, 66, 83, 100, 115]) {
      clock.tick(time);
      expect(at(dot).x, `at ${String(time)} ms`).toBeCloseTo(onTheAttack, 9);
    }
    // ... and then it is steady: the hold, in the middle of its stretch, and there it stays.
    clock.tick(116);
    expect(at(dot).x).toBeCloseTo(placeIn(BOX, (shape.decayEnd + shape.holdEnd) / 2, 0.7).x, 9);
    clock.tick(300);
    expect(at(dot).x).toBeCloseTo(placeIn(BOX, (shape.decayEnd + shape.holdEnd) / 2, 0.7).x, 9);
  });

  test("keeps a fall on the decay and then on the release through the frames that find the level where it was", () => {
    let value: number | undefined = 1;
    const { dot, clock } = arrange(() => value);
    const shape = shapeOf(NEW);
    clock.tick(0);
    let time = 16;
    for (const level of [0.95, 0.85, 0.75]) {
      value = level;
      for (let frame = 1; frame <= 2; frame += 1) {
        clock.tick(time);
        time += 16;
        const x = at(dot).x;
        expect(x, `${String(level)}, frame ${String(frame)}`).toBeGreaterThan(placeIn(BOX, shape.attackEnd, 1).x);
        expect(x).toBeLessThan(placeIn(BOX, shape.decayEnd, 0.7).x);
      }
    }
    for (const level of [0.6, 0.4, 0.2]) {
      value = level;
      for (let frame = 1; frame <= 2; frame += 1) {
        clock.tick(time);
        time += 16;
        expect(at(dot).x, `${String(level)}, frame ${String(frame)}`).toBeGreaterThan(placeIn(BOX, shape.holdEnd, 0.7).x);
      }
    }
  });

  test("is in the middle of the hold when the level gets to the sustain, though the telemetry's number is a hair under the session's (it holds 32 bits)", () => {
    let value: number | undefined = 0.8;
    const { dot, clock } = arrange(() => value);
    const shape = shapeOf(NEW);
    clock.tick(0);
    value = Math.fround(0.7);
    expect(value, "the premise: the 32-bit number is under the session's").toBeLessThan(0.7);
    clock.tick(16);
    // Not the start of the release, which is where a level a hair under the sustain and falling is by the lines alone.
    expect(at(dot).x).toBeCloseTo(placeIn(BOX, (shape.decayEnd + shape.holdEnd) / 2, 0.7).x, 9);
    clock.tick(33);
    expect(at(dot).x).toBeCloseTo(placeIn(BOX, (shape.decayEnd + shape.holdEnd) / 2, 0.7).x, 9);
  });

  test("hides the dot while there is no value to follow, and shows it again when there is one", () => {
    let value: number | undefined = 0.5;
    const { dot, clock } = arrange(() => value);
    clock.tick(0);
    expect(dot.getAttribute("visibility")).toBe("visible");
    value = undefined;
    clock.tick(16);
    expect(dot.getAttribute("visibility")).toBe("hidden");
    value = Number.NaN;
    clock.tick(33);
    expect(dot.getAttribute("visibility")).toBe("hidden");
    value = 0.2;
    clock.tick(50);
    expect(dot.getAttribute("visibility")).toBe("visible");
  });

  test("draws the new line when the settings change, the dot on the new shape, and does not forget which way the level was going", () => {
    let value: number | undefined = 0.9;
    const { view, clock, line, dot, frames } = arrange(() => value);
    clock.tick(0);
    value = 0.8;
    clock.tick(16); // falling from 0.9: on the decay
    const slow = { attack: 2, decay: 2, sustain: 0.7, release: 2 };
    view.rerender(<LiveEnvelope label="ADSR shape" stages={slow} read={() => value} frames={frames} />);
    expect(line.getAttribute("d")).toBe(envelopePath(shapeOf(slow), BOX));
    // (The same level, still falling: the decay of the new shape, not an attack that begins again.)
    value = 0.75;
    clock.tick(33);
    const shape = shapeOf(slow);
    expect(at(dot).x).toBeGreaterThan(placeIn(BOX, shape.attackEnd, 1).x);
    expect(at(dot).x).toBeLessThan(placeIn(BOX, shape.decayEnd, 0.7).x);
  });

  test("writes to the DOM only when the dot moves: a frame that finds the level where it was, and the dot placed for it, writes nothing", () => {
    let value: number | undefined = 0;
    const { dot, clock, view, frames } = arrange(() => value);
    const written = vi.spyOn(dot, "setAttribute");
    clock.tick(0);
    expect(written).toHaveBeenCalled();
    // At rest, frame after frame: nothing more.
    written.mockClear();
    for (let i = 1; i <= 5; i += 1) clock.tick(i * 16);
    expect(written).not.toHaveBeenCalled();
    // A level that moves is a move; the frames that find it where it was are not (the dot stays where the move put it): nothing is written after.
    value = 0.7;
    clock.tick(100);
    const placed = at(dot);
    written.mockClear();
    for (let i = 0; i < 5; i += 1) clock.tick(116 + i * 16);
    expect(written).not.toHaveBeenCalled();
    expect(at(dot)).toEqual(placed);
    // Once it has stood there for long enough it is the hold, which is one more move, and nothing after it.
    clock.tick(200);
    expect(written).toHaveBeenCalled();
    expect(at(dot).x).toBeGreaterThan(placed.x);
    written.mockClear();
    for (let i = 0; i < 5; i += 1) clock.tick(216 + i * 16);
    expect(written).not.toHaveBeenCalled();
    // No value hides the dot once, and not again for the frames that have none either.
    value = undefined;
    clock.tick(300);
    expect(dot.getAttribute("visibility")).toBe("hidden");
    written.mockClear();
    clock.tick(316);
    clock.tick(333);
    expect(written).not.toHaveBeenCalled();
    // The value comes back: the dot is shown and placed again.
    value = 0.7;
    clock.tick(350);
    expect(dot.getAttribute("visibility")).toBe("visible");
    clock.tick(366);
    // A change of the settings places the dot on the new shape at the next frame, though the level is where it was.
    const slow = { attack: 2, decay: 2, sustain: 0.7, release: 2 };
    const before = at(dot);
    view.rerender(<LiveEnvelope label="ADSR shape" stages={slow} read={() => value} frames={frames} />);
    written.mockClear();
    clock.tick(383);
    expect(written).toHaveBeenCalled();
    expect(at(dot).x).not.toBe(before.x);
  });

  test("still hides the dot when the value goes away after the settings have changed, and shows it again", () => {
    let value: number | undefined = 0.7;
    const { view, clock, dot, frames } = arrange(() => value);
    clock.tick(0);
    const slow = { attack: 2, decay: 2, sustain: 0.7, release: 2 };
    view.rerender(<LiveEnvelope label="ADSR shape" stages={slow} read={() => value} frames={frames} />);
    // (The dot is shown, and the very next frame has no value: it is the new subscription that has to know the dot is there.)
    expect(dot.getAttribute("visibility")).toBe("visible");
    value = undefined;
    clock.tick(16);
    expect(dot.getAttribute("visibility")).toBe("hidden");
    value = 0.7;
    clock.tick(33);
    expect(dot.getAttribute("visibility")).toBe("visible");
  });

  test("forgets which way the level was going when there was no value for a while: what comes after is told by where it is, not as a rise from what it was", () => {
    let value: number | undefined = 0.2;
    const { dot, clock } = arrange(() => value);
    const shape = shapeOf(NEW);
    clock.tick(0);
    value = undefined;
    clock.tick(16);
    // The level comes back at 0.9, above the sustain: still, so it is on the decay, where a rise from 0.2 would have put it on the attack.
    value = 0.9;
    clock.tick(33);
    expect(at(dot).x).toBeGreaterThan(placeIn(BOX, shape.attackEnd, 1).x);
    expect(at(dot).x).toBeLessThan(placeIn(BOX, shape.decayEnd, 0.7).x);
  });

  test("says the new label when it is given one", () => {
    const { view, image, frames } = arrange(() => 0);
    view.rerender(<LiveEnvelope label="Other shape" stages={NEW} read={() => 0} frames={frames} />);
    expect(image.getAttribute("aria-label")).toBe("Other shape");
  });

  test("renders once, and not again for the frames: the dot is written to the DOM", () => {
    let commits = 0;
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    let value = 0;
    const read = () => value;
    render(
      <Profiler
        id="envelope"
        onRender={() => {
          commits += 1;
        }}
      >
        <LiveEnvelope label="ADSR shape" stages={NEW} read={read} frames={frames} />
      </Profiler>,
    );
    const before = commits;
    for (let i = 0; i < 10; i += 1) {
      value = i / 10;
      clock.tick(i * 16);
    }
    expect(commits).toBe(before);
  });

  test("is subscribed to the frames only while it is on the page", () => {
    const { view, clock, frames } = arrange(() => 0);
    expect(frames.running).toBe(true);
    view.unmount();
    expect(frames.running).toBe(false);
    expect(clock.pending).toBe(0);
  });
});
