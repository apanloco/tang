// @vitest-environment jsdom
import { cleanup, fireEvent, render } from "@testing-library/react";
import { Profiler, useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { FrameLoop } from "../client/frameloop";
import { Clock } from "../client/testing";
import { Knob, formatValue } from "./Knob";
import type { KnobProps } from "./Knob";
import { angleOf, arcPath, DRAG_PIXELS, pointAt } from "./knob-geometry";

afterEach(cleanup);

interface Harness {
  /** What the knob asked for, in order. */
  changes: number[];
  commits: number[];
  dial: HTMLElement;
  root: HTMLElement;
  container: HTMLElement;
  rerender: (props: Partial<KnobProps>) => void;
}

type Held = Partial<KnobProps> & { start?: number };

/** A knob whose value is held by whoever uses it, as in the app: it shows what it has asked for. */
function Controlled({ start = 5, log, ...props }: Held & { log: Pick<Harness, "changes" | "commits"> }) {
  const [value, setValue] = useState(start);
  return (
    <Knob
      label="Cutoff"
      min={0}
      max={10}
      {...props}
      value={value}
      onChange={(next) => {
        log.changes.push(next);
        setValue(next);
      }}
      onCommit={(next) => log.commits.push(next)}
    />
  );
}

function mount(props: Held = {}): Harness {
  const log = { changes: [] as number[], commits: [] as number[] };
  const view = render(<Controlled log={log} {...props} />);
  const dial = view.getByRole("slider");
  return {
    ...log,
    dial,
    root: rootOf(view.container),
    container: view.container,
    rerender: (more) => {
      view.rerender(<Controlled log={log} {...props} {...more} />);
    },
  };
}

/** A knob that only shows what it is given. */
function show(props: Partial<KnobProps> = {}) {
  const view = render(<Knob label="Cutoff" value={5} min={0} max={10} {...props} />);
  const dial = view.getByRole("slider");
  return { ...view, dial, root: rootOf(view.container) };
}

const rootOf = (container: HTMLElement): HTMLElement => {
  const root = container.querySelector<HTMLElement>(".tang-knob");
  if (!root) throw new Error("no knob");
  return root;
};

const dotOf = (container: HTMLElement): SVGCircleElement => {
  const dot = container.querySelector<SVGCircleElement>(".tang-knob__dot");
  if (!dot) throw new Error("no dot");
  return dot;
};

const dotAt = (container: HTMLElement): { x: string | null; y: string | null; visibility: string | null } => {
  const dot = dotOf(container);
  return { x: dot.getAttribute("cx"), y: dot.getAttribute("cy"), visibility: dot.getAttribute("visibility") };
};

describe("what a knob says", () => {
  test("is a slider with its label, range and value", () => {
    const { dial, getByText } = show({ label: "Resonance", value: 2, min: 0.5, max: 10 });
    expect(dial.getAttribute("aria-label")).toBe("Resonance");
    expect(dial.getAttribute("aria-valuemin")).toBe("0.5");
    expect(dial.getAttribute("aria-valuemax")).toBe("10");
    expect(dial.getAttribute("aria-valuenow")).toBe("2");
    expect(dial.getAttribute("aria-valuetext")).toBe("2");
    expect(getByText("Resonance")).toBeTruthy();
    expect(getByText("2", { selector: ".tang-knob__value" })).toBeTruthy();
  });

  test("writes the value to three figures, or as it is told to", () => {
    expect(formatValue(0.707)).toBe("0.707");
    expect(formatValue(0.123456)).toBe("0.123");
    expect(formatValue(1234.5)).toBe("1230");
    expect(formatValue(-24)).toBe("-24");
    expect(formatValue(0.1 + 0.2)).toBe("0.3");
    const { dial, getByText } = show({ value: 440, format: (value) => `${String(value)} Hz` });
    expect(dial.getAttribute("aria-valuetext")).toBe("440 Hz");
    expect(getByText("440 Hz")).toBeTruthy();
  });

  test("draws a pointer at the angle of the value, and an arc up to it", () => {
    const { container } = show({ value: 5 });
    const pointer = container.querySelector(".tang-knob__pointer");
    const from = pointAt(24, 5, angleOf(0.5));
    const to = pointAt(24, 18, angleOf(0.5));
    expect(Number(pointer?.getAttribute("x1"))).toBeCloseTo(from.x, 10);
    expect(Number(pointer?.getAttribute("y1"))).toBeCloseTo(from.y, 10);
    expect(Number(pointer?.getAttribute("x2"))).toBeCloseTo(to.x, 10);
    expect(Number(pointer?.getAttribute("y2"))).toBeCloseTo(to.y, 10);
    expect(container.querySelector(".tang-knob__arc")?.getAttribute("d")).toBe(arcPath(24, 18, 0, 0.5));
    expect(container.querySelector(".tang-knob__track")?.getAttribute("d")).toBe(arcPath(24, 18, 0, 1));
  });

  test("moves the pointer and the arc when the value changes", () => {
    const { container, rerender } = render(<Knob label="a" value={2.5} min={0} max={10} />);
    expect(container.querySelector(".tang-knob__arc")?.getAttribute("d")).toBe(arcPath(24, 18, 0, 0.25));
    rerender(<Knob label="a" value={7.5} min={0} max={10} />);
    expect(container.querySelector(".tang-knob__arc")?.getAttribute("d")).toBe(arcPath(24, 18, 0, 0.75));
  });

  test("draws a ring for what modulation does to the value, and none for nothing", () => {
    const withRing = show({ value: 5, modulation: { low: 2, high: 8 } });
    expect(withRing.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.2, 0.8));
    cleanup();
    expect(show({ value: 5 }).container.querySelector(".tang-knob__ring")).toBeNull();
    cleanup();
    expect(show({ value: 5, modulation: { low: 5, high: 5 } }).container.querySelector(".tang-knob__ring")).toBeNull();
    cleanup();
    // A swing past the ends is drawn up to them.
    const past = show({ value: 9, modulation: { low: 7, high: 13 } });
    expect(past.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.7, 1));
    cleanup();
    // And the ends are the knob's own, which need not start at zero.
    const bipolar = show({ value: 0, min: -24, max: 24, modulation: { low: -12, high: 12 } });
    expect(bipolar.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.25, 0.75));
  });
});

describe("dragging", () => {
  test("up raises the value by the share of the range that the pointer has gone", () => {
    const knob = mount();
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 300 - DRAG_PIXELS / 4, pointerId: 1 });
    expect(knob.changes).toEqual([7.5]);
    fireEvent.pointerMove(knob.dial, { clientY: 300 - DRAG_PIXELS / 2, pointerId: 1 });
    expect(knob.changes).toEqual([7.5, 10]);
    expect(knob.dial.getAttribute("aria-valuenow")).toBe("10");
  });

  test("down lowers it", () => {
    const knob = mount({ start: 5 });
    fireEvent.pointerDown(knob.dial, { clientY: 100, button: 0, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 100 + DRAG_PIXELS / 4, pointerId: 1 });
    expect(knob.changes).toEqual([2.5]);
  });

  test("stops at the ends, says nothing more there, and comes back as soon as the pointer does", () => {
    const knob = mount({ start: 9 });
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 250, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 100, pointerId: 1 });
    expect(knob.changes).toEqual([10]); // the second move changed nothing, so said nothing
    fireEvent.pointerMove(knob.dial, { clientY: 150, pointerId: 1 }); // 50 px back down
    expect(knob.changes).toEqual([10, 7.5]);
  });

  test("is a tenth as fast with shift held, and does not jump when shift is pressed or let go", () => {
    const knob = mount();
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 280, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 260, pointerId: 1, shiftKey: true });
    fireEvent.pointerMove(knob.dial, { clientY: 240, pointerId: 1 });
    expect(knob.changes).toHaveLength(3);
    expect(knob.changes[0]).toBeCloseTo(6, 10); // 20 px is a tenth of the range of 10
    expect(knob.changes[1]).toBeCloseTo(6.1, 10); // and with shift a tenth of that, from where it was
    expect(knob.changes[2]).toBeCloseTo(7.1, 10);
  });

  test("does not listen to the value it is given while the pointer is down: it is the holder's to say", () => {
    // The holder shows what it likes (here: a late echo of an old value), and the next
    // change builds on what the gesture has, not on that.
    const changes: number[] = [];
    const view = render(<Knob label="a" value={5} min={0} max={10} onChange={(next) => changes.push(next)} />);
    const dial = view.getByRole("slider");
    fireEvent.pointerDown(dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(dial, { clientY: 250, pointerId: 1 });
    view.rerender(<Knob label="a" value={1} min={0} max={10} onChange={(next) => changes.push(next)} />);
    fireEvent.pointerMove(dial, { clientY: 200, pointerId: 1 });
    expect(changes).toEqual([7.5, 10]);
  });

  test("a gesture that changed the value ends with a commit of the value it ended on", () => {
    const knob = mount();
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 250, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 240, pointerId: 1 });
    expect(knob.commits).toEqual([]);
    fireEvent.pointerUp(knob.dial, { clientY: 240, pointerId: 1 });
    expect(knob.commits).toEqual([knob.changes[1]]);
    fireEvent.pointerMove(knob.dial, { clientY: 100, pointerId: 1 });
    expect(knob.changes).toHaveLength(2); // the pointer is not held any more
    fireEvent.pointerUp(knob.dial, { pointerId: 1 });
    expect(knob.commits).toHaveLength(1);
  });

  test("a gesture ends with a commit even when its last move changed nothing", () => {
    const knob = mount({ start: 9 });
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 250, pointerId: 1 }); // to the top
    fireEvent.pointerMove(knob.dial, { clientY: 100, pointerId: 1 }); // and on, which is nothing
    fireEvent.pointerUp(knob.dial, { clientY: 100, pointerId: 1 });
    expect(knob.changes).toEqual([10]);
    expect(knob.commits).toEqual([10]);
  });

  test("commits what the gesture reached, whatever the holder shows by then", () => {
    // A holder that does not follow (it is waiting for the server, or it refuses).
    const changes: number[] = [];
    const commits: number[] = [];
    const view = render(<Knob label="a" value={5} min={0} max={10} onChange={(next) => changes.push(next)} onCommit={(next) => commits.push(next)} />);
    const dial = view.getByRole("slider");
    fireEvent.pointerDown(dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(dial, { clientY: 250, pointerId: 1 });
    fireEvent.pointerUp(dial, { pointerId: 1 });
    expect(changes).toEqual([7.5]);
    expect(commits).toEqual([7.5]);
  });

  test("a cancelled gesture ends like a released one", () => {
    const knob = mount();
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 250, pointerId: 1 });
    fireEvent.pointerCancel(knob.dial, { pointerId: 1 });
    expect(knob.commits).toEqual([7.5]);
    expect(knob.root.hasAttribute("data-dragging")).toBe(false);
  });

  test("a click that changes nothing commits nothing, nor does a drag that went nowhere", () => {
    const knob = mount({ start: 10 });
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerUp(knob.dial, { clientY: 300, pointerId: 1 });
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 100, pointerId: 1 }); // up, at the top already
    fireEvent.pointerUp(knob.dial, { clientY: 100, pointerId: 1 });
    expect(knob.changes).toEqual([]);
    expect(knob.commits).toEqual([]);
  });

  test("moving the pointer over the knob without pressing does nothing", () => {
    const knob = mount();
    fireEvent.pointerMove(knob.dial, { clientY: 100, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 10, pointerId: 1 });
    expect(knob.changes).toEqual([]);
  });

  test("only the main button turns it", () => {
    const knob = mount();
    for (const button of [1, 2]) {
      fireEvent.pointerDown(knob.dial, { clientY: 300, button, pointerId: 1 });
      fireEvent.pointerMove(knob.dial, { clientY: 200, pointerId: 1 });
      fireEvent.pointerUp(knob.dial, { pointerId: 1 });
    }
    expect(knob.changes).toEqual([]);
    expect(knob.root.hasAttribute("data-dragging")).toBe(false);
  });

  test("is marked while it is turned, and takes the focus", () => {
    const knob = mount();
    expect(knob.root.hasAttribute("data-dragging")).toBe(false);
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
    expect(knob.root.hasAttribute("data-dragging")).toBe(true);
    expect(document.activeElement).toBe(knob.dial);
    fireEvent.pointerUp(knob.dial, { pointerId: 1 });
    expect(knob.root.hasAttribute("data-dragging")).toBe(false);
  });

  test("asks to keep the pointer when it is over the edge of the knob, and goes on without it where that cannot be", () => {
    const knob = mount();
    const capture = vi.fn();
    knob.dial.setPointerCapture = capture;
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 7 });
    expect(capture).toHaveBeenCalledWith(7);
    fireEvent.pointerUp(knob.dial, { pointerId: 7 });

    knob.dial.setPointerCapture = () => {
      throw new DOMException("no such pointer", "NotFoundError");
    };
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 8 });
    fireEvent.pointerMove(knob.dial, { clientY: 250, pointerId: 8 });
    expect(knob.changes).toEqual([7.5]);
  });
});

describe("the keyboard", () => {
  test("arrows step the value by a hundredth of the range, and say so as a gesture of their own", () => {
    const knob = mount();
    fireEvent.keyDown(knob.dial, { key: "ArrowUp" });
    fireEvent.keyDown(knob.dial, { key: "ArrowRight" });
    fireEvent.keyDown(knob.dial, { key: "ArrowDown" });
    fireEvent.keyDown(knob.dial, { key: "ArrowLeft" });
    expect(knob.changes.map((value) => Number(value.toFixed(6)))).toEqual([5.1, 5.2, 5.1, 5]);
    expect(knob.commits).toEqual(knob.changes);
  });

  test("shift makes the step a tenth of that; the page keys make it ten times", () => {
    const knob = mount();
    fireEvent.keyDown(knob.dial, { key: "ArrowUp", shiftKey: true });
    fireEvent.keyDown(knob.dial, { key: "PageUp" });
    fireEvent.keyDown(knob.dial, { key: "PageDown" });
    expect(knob.changes.map((value) => Number(value.toFixed(6)))).toEqual([5.01, 6.01, 5.01]);
  });

  test("Home and End go to the ends", () => {
    const knob = mount({ min: -24, max: 24, start: 3 });
    fireEvent.keyDown(knob.dial, { key: "Home" });
    fireEvent.keyDown(knob.dial, { key: "End" });
    expect(knob.changes).toEqual([-24, 24]);
  });

  test("keys are the knob's: it keeps them from the page, and leaves the others to it", () => {
    const knob = mount();
    expect(fireEvent.keyDown(knob.dial, { key: "ArrowUp" })).toBe(false); // the page does not scroll
    expect(fireEvent.keyDown(knob.dial, { key: "Tab" })).toBe(true);
    expect(fireEvent.keyDown(knob.dial, { key: "a" })).toBe(true);
    expect(knob.changes).toHaveLength(1);
  });

  test("a key that changes nothing says nothing", () => {
    const knob = mount({ start: 10 });
    fireEvent.keyDown(knob.dial, { key: "ArrowUp" });
    fireEvent.keyDown(knob.dial, { key: "End" });
    expect(knob.changes).toEqual([]);
    expect(knob.commits).toEqual([]);
  });
});

describe("a double click", () => {
  test("puts the value back to its default", () => {
    const knob = mount({ start: 7, defaultValue: 2 });
    fireEvent.doubleClick(knob.dial);
    expect(knob.changes).toEqual([2]);
    expect(knob.commits).toEqual([2]);
    fireEvent.doubleClick(knob.dial); // there already
    expect(knob.changes).toEqual([2]);
  });

  test("does nothing for a knob that has no default", () => {
    const knob = mount({ start: 7 });
    fireEvent.doubleClick(knob.dial);
    expect(knob.changes).toEqual([]);
  });

  test("is made of two clicks, which are not drags", () => {
    const knob = mount({ start: 7, defaultValue: 2 });
    for (let click = 0; click < 2; click += 1) {
      fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
      fireEvent.pointerUp(knob.dial, { clientY: 300, pointerId: 1 });
    }
    fireEvent.doubleClick(knob.dial);
    expect(knob.changes).toEqual([2]);
    expect(knob.commits).toEqual([2]);
  });
});

describe("a knob that cannot be turned", () => {
  test("one without a way to say what it wants only shows, and says so", () => {
    const { dial, root } = show();
    expect(dial.getAttribute("aria-readonly")).toBe("true");
    expect(dial.hasAttribute("aria-disabled")).toBe(false);
    expect(dial.getAttribute("tabindex")).toBe("0");
    expect(root.hasAttribute("data-readonly")).toBe(true);
    expect(root.hasAttribute("data-disabled")).toBe(false);
    // Nothing happens when it is handled (and nothing breaks).
    fireEvent.pointerDown(dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(dial, { clientY: 100, pointerId: 1 });
    fireEvent.keyDown(dial, { key: "ArrowUp" });
    fireEvent.doubleClick(dial);
    expect(root.hasAttribute("data-dragging")).toBe(false);
  });

  test("a disabled one is out of the tab order and does not answer to anything", () => {
    const knob = mount({ disabled: true, defaultValue: 1 });
    expect(knob.dial.getAttribute("aria-disabled")).toBe("true");
    expect(knob.dial.hasAttribute("aria-readonly")).toBe(false);
    expect(knob.dial.getAttribute("tabindex")).toBe("-1");
    expect(knob.root.hasAttribute("data-disabled")).toBe(true);
    fireEvent.pointerDown(knob.dial, { clientY: 300, button: 0, pointerId: 1 });
    fireEvent.pointerMove(knob.dial, { clientY: 100, pointerId: 1 });
    fireEvent.pointerUp(knob.dial, { pointerId: 1 });
    fireEvent.keyDown(knob.dial, { key: "ArrowUp" });
    fireEvent.doubleClick(knob.dial);
    expect(knob.changes).toEqual([]);
    expect(knob.commits).toEqual([]);
  });

  test("one that can be turned is in the tab order, and is neither", () => {
    const knob = mount();
    expect(knob.dial.getAttribute("tabindex")).toBe("0");
    expect(knob.dial.hasAttribute("aria-readonly")).toBe(false);
    expect(knob.dial.hasAttribute("aria-disabled")).toBe(false);
    expect(knob.root.hasAttribute("data-readonly")).toBe(false);
  });
});

describe("the dot that shows what the parameter is now", () => {
  const arrange = (read: () => number | undefined, props: Partial<KnobProps> = {}) => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const view = render(<Knob label="Cutoff" value={5} min={0} max={10} readEffective={read} frames={frames} {...props} />);
    return { clock, frames, ...view };
  };

  test("is hidden until there is a frame to read it on", () => {
    const { container, clock } = arrange(() => 5);
    expect(dotAt(container).visibility).toBe("hidden");
    expect(clock.pending).toBe(1);
  });

  test("is put where the value is on the frame, and moves with it", () => {
    let effective = 5;
    const { container, clock } = arrange(() => effective);
    clock.tick(16);
    const middle = pointAt(24, 18, angleOf(0.5));
    expect(dotAt(container)).toEqual({ x: String(middle.x), y: String(middle.y), visibility: "visible" });
    effective = 10;
    clock.tick(33);
    const top = pointAt(24, 18, angleOf(1));
    expect(dotAt(container)).toEqual({ x: String(top.x), y: String(top.y), visibility: "visible" });
    effective = -3; // beyond the range: at its end
    clock.tick(50);
    const bottom = pointAt(24, 18, angleOf(0));
    expect(dotAt(container)).toEqual({ x: String(bottom.x), y: String(bottom.y), visibility: "visible" });
  });

  test("goes when there is nothing to show, and comes back", () => {
    let effective: number | undefined = 5;
    const { container, clock } = arrange(() => effective);
    clock.tick(16);
    expect(dotAt(container).visibility).toBe("visible");
    effective = undefined;
    clock.tick(33);
    expect(dotAt(container).visibility).toBe("hidden");
    effective = Number.NaN;
    clock.tick(50);
    expect(dotAt(container).visibility).toBe("hidden");
    effective = Number.POSITIVE_INFINITY;
    clock.tick(66);
    expect(dotAt(container).visibility).toBe("hidden");
    effective = 2;
    clock.tick(83);
    expect(dotAt(container).visibility).toBe("visible");
  });

  test("is written only when the value moved", () => {
    let effective = 5;
    const { container, clock } = arrange(() => effective);
    const write = vi.spyOn(dotOf(container), "setAttribute");
    clock.tick(16);
    expect(write).toHaveBeenCalledTimes(3);
    clock.tick(33);
    clock.tick(50);
    expect(write).toHaveBeenCalledTimes(3);
    effective = 6;
    clock.tick(66);
    expect(write).toHaveBeenCalledTimes(6);
  });

  test("does not make the knob render again, however it moves", () => {
    let effective = 0;
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    let renders = 0;
    const view = render(
      <Profiler id="knob" onRender={() => (renders += 1)}>
        <Knob label="Cutoff" value={5} min={0} max={10} readEffective={() => effective} frames={frames} />
      </Profiler>,
    );
    const mounted = renders;
    for (let frame = 1; frame <= 100; frame += 1) {
      effective = (frame % 11) - 0.5;
      clock.tick(frame * 16);
    }
    expect(renders).toBe(mounted);
    expect(dotAt(view.container).visibility).toBe("visible");
  });

  test("asks for no frames when there is nothing to read, and none once the knob is gone", () => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const idle = render(<Knob label="a" value={5} min={0} max={10} frames={frames} />);
    expect(clock.requested).toBe(0);
    idle.unmount();
    const live = render(<Knob label="a" value={5} min={0} max={10} readEffective={() => 1} frames={frames} />);
    expect(clock.pending).toBe(1);
    live.unmount();
    expect(clock.pending).toBe(0);
    expect(frames.running).toBe(false);
  });

  test("follows another source when it is given one, and forgets the old", () => {
    const first = vi.fn(() => 2);
    const second = vi.fn(() => 8);
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const { container, rerender } = render(<Knob label="a" value={5} min={0} max={10} readEffective={first} frames={frames} />);
    clock.tick(16);
    expect(first).toHaveBeenCalledTimes(1);
    rerender(<Knob label="a" value={5} min={0} max={10} readEffective={second} frames={frames} />);
    clock.tick(33);
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).toHaveBeenCalledTimes(1);
    const at = pointAt(24, 18, angleOf(0.8));
    expect(dotAt(container).x).toBe(String(at.x));
  });

  test("is read on the frames of another loop when it is given one", () => {
    const here = new Clock();
    const there = new Clock();
    const read = () => 5;
    const { container, rerender } = render(<Knob label="a" value={5} min={0} max={10} readEffective={read} frames={new FrameLoop(here)} />);
    expect(here.pending).toBe(1);
    rerender(<Knob label="a" value={5} min={0} max={10} readEffective={read} frames={new FrameLoop(there)} />);
    expect(here.pending).toBe(0);
    expect(there.pending).toBe(1);
    there.tick(16);
    expect(dotAt(container).visibility).toBe("visible");
  });

  test("is put again when the range changes under it", () => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const read = () => 5;
    const { container, rerender } = render(<Knob label="a" value={5} min={0} max={10} readEffective={read} frames={frames} />);
    clock.tick(16);
    const before = dotAt(container);
    rerender(<Knob label="a" value={5} min={0} max={20} readEffective={read} frames={frames} />);
    clock.tick(33);
    const at = pointAt(24, 18, angleOf(0.25));
    expect(dotAt(container)).toEqual({ x: String(at.x), y: String(at.y), visibility: "visible" });
    expect(dotAt(container).x).not.toBe(before.x);
  });

  test("a failing source does not stop the frame loop, nor the other knobs", () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const broken = render(
      <Knob
        label="broken"
        value={5}
        min={0}
        max={10}
        readEffective={() => {
          throw new Error("no value");
        }}
        frames={frames}
      />,
    );
    const fine = render(<Knob label="fine" value={5} min={0} max={10} readEffective={() => 5} frames={frames} />);
    clock.tick(16);
    expect(error).toHaveBeenCalledTimes(1);
    expect(dotAt(fine.container).visibility).toBe("visible");
    expect(dotAt(broken.container).visibility).toBe("hidden");
    expect(clock.pending).toBe(1);
    error.mockRestore();
  });
});
