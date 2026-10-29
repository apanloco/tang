// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Profiler, useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { FrameLoop } from "../client/frameloop";
import { Clock } from "../client/testing";
import { StepBars } from "./StepBars";
import type { StepBarsProps } from "./StepBars";

afterEach(cleanup);

const RISING = Array.from({ length: 16 }, (_, step) => step / 8 - 1);
/** Where the bars are on the page, which jsdom has no layout to say: 160 wide, 64 tall, ten bars to the width (a bar is 10 pixels, a sixteenth of the width). */
const BOX = { left: 10, top: 20, width: 160, height: 64 };
/** The pointer at bar `column` (its middle) and a value as high up as `value` says (1 at the top, -1 at the bottom). */
const at = (column: number, value: number) => ({ clientX: BOX.left + column * 10 + 5, clientY: BOX.top + ((1 - value) / 2) * BOX.height, button: 0, pointerId: 1 });

interface Harness {
  changes: number[][];
  /** How many gestures have been committed. */
  commits: () => number;
  clock: Clock;
  group: HTMLElement;
  step: (number: number) => HTMLElement;
  /** What the bars show now. */
  shown: () => number[];
}

/** Bars whose values are held by whoever uses them, as in the app: they show what they have asked for. */
function Controlled({ start, changed, committed, ...props }: Partial<StepBarsProps> & { start: readonly number[]; changed: (values: number[]) => void; committed: () => void }) {
  const [values, setValues] = useState<readonly number[]>(start);
  return (
    <StepBars
      label="Steps of Steps"
      length={16}
      {...props}
      values={values}
      onChange={(next) => {
        changed(next);
        setValues(next);
      }}
      onCommit={committed}
    />
  );
}

function mount(props: Partial<StepBarsProps> & { start?: readonly number[] } = {}): Harness {
  const clock = new Clock();
  const changes: number[][] = [];
  let commits = 0;
  const { start = RISING, ...rest } = props;
  render(
    <Controlled
      start={start}
      changed={(values) => changes.push(values)}
      committed={() => {
        commits += 1;
      }}
      frames={new FrameLoop(clock)}
      {...rest}
    />,
  );
  const group = screen.getByRole("group", { name: "Steps of Steps" });
  group.getBoundingClientRect = () => ({ ...BOX, x: BOX.left, y: BOX.top, right: BOX.left + BOX.width, bottom: BOX.top + BOX.height, toJSON: () => ({}) });
  return {
    changes,
    commits: () => commits,
    clock,
    group,
    step: (number) => screen.getByRole("slider", { name: `Step ${String(number)}` }),
    shown: () => screen.getAllByRole("slider").map((bar) => Number(bar.getAttribute("aria-valuenow"))),
  };
}

describe("the bars of a step sequencer", () => {
  test("are a group of sixteen sliders, a step each, from -1 to 1, with the value that a screen reader says", () => {
    const { group, step } = mount();
    expect(group.getAttribute("aria-label")).toBe("Steps of Steps");
    expect(screen.getAllByRole("slider")).toHaveLength(16);
    const third = step(3);
    expect(third.getAttribute("aria-valuemin")).toBe("-1");
    expect(third.getAttribute("aria-valuemax")).toBe("1");
    expect(third.getAttribute("aria-valuenow")).toBe("-0.75");
    expect(third.getAttribute("aria-valuetext")).toBe("-0.75");
    expect(third.getAttribute("aria-orientation")).toBe("vertical");
    expect(step(16).getAttribute("aria-valuetext")).toBe("0.875");
  });

  test("draw each bar from the middle, up for a value above nothing and down for one below it, as far as the value says", () => {
    const { step } = mount({ start: [1, 0.5, 0, -0.25, -1, ...Array.from({ length: 11 }, () => 0)] });
    const fill = (number: number): HTMLElement => {
      const element = step(number).querySelector<HTMLElement>(".tang-stepbars__fill");
      if (!element) throw new Error("no fill");
      return element;
    };
    expect(fill(1).style.height).toBe("50%");
    expect(fill(2).style.height).toBe("25%");
    expect(fill(3).style.height).toBe("0%");
    expect(fill(4).style.height).toBe("12.5%");
    expect(fill(5).style.height).toBe("50%");
    expect(step(1).hasAttribute("data-negative")).toBe(false);
    expect(step(3).hasAttribute("data-negative")).toBe(false);
    expect(step(4).hasAttribute("data-negative")).toBe(true);
    expect(step(5).hasAttribute("data-negative")).toBe(true);
  });

  test("mark the steps that are past the length as not in use", () => {
    const { step } = mount({ length: 5 });
    for (let number = 1; number <= 16; number += 1) expect(step(number).hasAttribute("data-unused"), `step ${String(number)}`).toBe(number > 5);
  });

  test("have one bar in the tab order, the first until another is focused", () => {
    const { step } = mount();
    expect(screen.getAllByRole("slider").filter((bar) => bar.getAttribute("tabindex") === "0")).toEqual([step(1)]);
    fireEvent.focus(step(7));
    expect(screen.getAllByRole("slider").filter((bar) => bar.getAttribute("tabindex") === "0")).toEqual([step(7)]);
  });
});

describe("a stroke across the bars", () => {
  test("sets the bar under the pointer to the value at its height when it is pressed, and is not over until the pointer is up", () => {
    const { group, changes, step, shown, commits } = mount();
    fireEvent.pointerDown(group, at(2, 0.5));
    expect(changes).toHaveLength(1);
    expect(changes[0]).toEqual(RISING.map((value, index) => (index === 2 ? 0.5 : value)));
    expect(shown()[2]).toBe(0.5);
    expect(step(3).getAttribute("aria-valuenow")).toBe("0.5");
    expect(commits()).toBe(0);
  });

  test("is committed once, when the pointer is up, or when it is taken away", () => {
    const first = mount();
    fireEvent.pointerDown(first.group, at(0, 1));
    fireEvent.pointerMove(first.group, at(1, 1));
    expect(first.commits()).toBe(0);
    fireEvent.pointerUp(first.group, { pointerId: 1 });
    expect(first.commits()).toBe(1);
    fireEvent.pointerMove(first.group, at(5, 1));
    fireEvent.pointerUp(first.group, { pointerId: 1 });
    expect(first.commits(), "a pointer that moves with nothing pressed is no stroke").toBe(1);
    cleanup();
    const second = mount();
    fireEvent.pointerDown(second.group, at(0, 1));
    fireEvent.pointerCancel(second.group, { pointerId: 1 });
    expect(second.commits()).toBe(1);
  });

  test("sets each bar that the pointer passes over, and the bars between two places that it jumped from one to the other, from the list it has made so far", () => {
    const { group, changes, shown } = mount({ start: Array.from({ length: 16 }, () => 0) });
    fireEvent.pointerDown(group, at(1, 0));
    fireEvent.pointerMove(group, at(5, 1));
    expect(shown().slice(0, 7)).toEqual([0, 0, 0.25, 0.5, 0.75, 1, 0]);
    fireEvent.pointerMove(group, at(3, -1));
    // Back over bars that it painted: the stroke is from its last place, bar 5, to bar 3.
    expect(shown().slice(0, 7)).toEqual([0, 0, 0.25, -1, 0, 1, 0]);
    expect(changes).toHaveLength(3);
  });

  test("goes on from the list that the stroke has made so far, and not from the values it is given, which come back a render later", () => {
    const onChange = vi.fn();
    render(<StepBars label="Steps of Steps" values={RISING} length={16} onChange={onChange} frames={new FrameLoop(new Clock())} />);
    const group = screen.getByRole("group", { name: "Steps of Steps" });
    group.getBoundingClientRect = () => ({ ...BOX, x: BOX.left, y: BOX.top, right: BOX.left + BOX.width, bottom: BOX.top + BOX.height, toJSON: () => ({}) });
    fireEvent.pointerDown(group, at(1, 1));
    fireEvent.pointerMove(group, at(4, -1));
    // (The values that it is given never change here, so they are RISING still when the pointer has moved: the second list is the first one's bar, and the line from it.)
    expect(onChange).toHaveBeenCalledTimes(2);
    expect((onChange.mock.calls[1] as [number[]])[0].slice(0, 6)).toEqual([RISING[0], 1, 0.33, -0.33, -1, RISING[5]]);
  });

  test("leaves a bar where it is when the pointer moves within it to the same value, and asks for nothing", () => {
    const { group, changes } = mount();
    fireEvent.pointerDown(group, at(4, 0.5));
    fireEvent.pointerMove(group, at(4, 0.5));
    expect(changes).toHaveLength(1);
    fireEvent.pointerMove(group, at(4, 0.25));
    expect(changes).toHaveLength(2);
  });

  test("is held to the ends of the box by a pointer that has gone past them", () => {
    const { group, shown } = mount({ start: Array.from({ length: 16 }, () => 0) });
    fireEvent.pointerDown(group, { clientX: BOX.left - 40, clientY: BOX.top - 40, button: 0, pointerId: 1 });
    expect(shown()[0]).toBe(1);
    fireEvent.pointerMove(group, { clientX: BOX.left + BOX.width + 40, clientY: BOX.top + BOX.height + 40, pointerId: 1 });
    expect(shown().slice(0, 16).at(-1)).toBe(-1);
  });

  test("is one that only the main button makes", () => {
    const { group, changes } = mount();
    fireEvent.pointerDown(group, { ...at(2, 0.5), button: 2 });
    expect(changes).toEqual([]);
  });

  test("is no stroke for bars that cannot be changed, which only show", () => {
    cleanup();
    render(<StepBars label="Steps of Steps" values={RISING} length={16} />);
    const group = screen.getByRole("group", { name: "Steps of Steps" });
    group.getBoundingClientRect = () => ({ ...BOX, x: 0, y: 0, right: 160, bottom: 64, toJSON: () => ({}) });
    fireEvent.pointerDown(group, at(2, 0.5));
    expect(screen.getAllByRole("slider").map((bar) => Number(bar.getAttribute("aria-valuenow")))).toEqual(RISING);
    expect(screen.getByRole("slider", { name: "Step 1" }).getAttribute("aria-readonly")).toBe("true");
  });
});


/** The page is hidden, as a browser does when its tab is left (jsdom has no tabs: `document.hidden` says it, and the event follows). */
function hidePage(): void {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
  fireEvent(document, new Event("visibilitychange"));
  Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
}

describe("a stroke whose release was never seen", () => {
  // (The same as the knob's: the button goes up over another window, a context menu takes the release, the window loses the focus: the bars went on painting under a mouse that was not pressed.)
  const mouse = { pointerType: "mouse" };

  test("is over when a mouse moves over the bars with no button down: the bars keep what the stroke painted, with a commit, and the mouse paints nothing more", () => {
    const { group, changes, commits } = mount();
    fireEvent.pointerDown(group, { ...at(0, 1), buttons: 1, ...mouse });
    fireEvent.pointerMove(group, { ...at(1, 1), buttons: 1, ...mouse });
    expect(changes).toHaveLength(2);
    fireEvent.pointerMove(group, { ...at(2, 1), buttons: 0, ...mouse });
    expect(commits()).toBe(1);
    // (Not a bar painted by that move, nor by the ones that come after, pressed or not: no stroke is on until a press.)
    fireEvent.pointerMove(group, { ...at(3, 1), buttons: 0, ...mouse });
    fireEvent.pointerMove(group, { ...at(4, 1), buttons: 1, ...mouse });
    expect(changes).toHaveLength(2);
  });

  test("is not over for a touch or a pen that moves with no button", () => {
    const { group, changes, commits } = mount();
    fireEvent.pointerDown(group, { ...at(0, 1), buttons: 1, pointerType: "pen" });
    fireEvent.pointerMove(group, { ...at(1, 1), buttons: 0, pointerType: "pen" });
    fireEvent.pointerMove(group, { ...at(2, 1), buttons: 0, pointerType: "touch" });
    expect(changes).toHaveLength(3);
    expect(commits()).toBe(0);
  });

  test("is over when the window loses the focus, when a pointer goes up or is cancelled anywhere, when a context menu opens and when the capture is lost: each commits once", () => {
    const endings: [string, (group: HTMLElement) => void][] = [
      ["the window loses the focus", () => fireEvent.blur(window)],
      ["the page is hidden", () => hidePage()],
      ["a pointer goes up somewhere else", () => fireEvent.pointerUp(document.body, { pointerId: 1 })],
      ["a pointer is cancelled somewhere else", () => fireEvent.pointerCancel(document.body, { pointerId: 1 })],
      ["a context menu opens", () => fireEvent.contextMenu(document.body)],
      ["the capture is lost", (group) => fireEvent.lostPointerCapture(group, { pointerId: 1 })],
    ];
    for (const [what, end] of endings) {
      cleanup();
      const { group, commits } = mount();
      fireEvent.pointerDown(group, { ...at(0, 1), buttons: 1, ...mouse });
      end(group);
      expect(commits(), what).toBe(1);
      fireEvent.pointerUp(group, { pointerId: 1 });
      expect(commits(), `${what}, and then a pointer up`).toBe(1);
    }
  });

  test("takes its listeners off the window when the stroke is over, and has none before one", () => {
    const off = vi.spyOn(window, "removeEventListener");
    const { group } = mount();
    fireEvent.pointerDown(group, { ...at(0, 1), buttons: 1, ...mouse });
    off.mockClear();
    fireEvent.pointerUp(group, { pointerId: 1 });
    expect(off.mock.calls.map(([type]) => type)).toContain("blur");
    off.mockRestore();
  });

  test("is not begun by a press with Control held (the secondary button of a Mac, which opens a menu that takes the release)", () => {
    const { group, changes } = mount();
    fireEvent.pointerDown(group, { ...at(0, 1), buttons: 1, ctrlKey: true, ...mouse });
    fireEvent.pointerMove(group, { ...at(1, 1), buttons: 1, ctrlKey: true, ...mouse });
    expect(changes).toEqual([]);
  });

  test("leaves nothing listening when the bars go while a stroke is on", () => {
    const { group, commits } = mount();
    fireEvent.pointerDown(group, { ...at(0, 1), buttons: 1, ...mouse });
    cleanup();
    fireEvent.blur(window);
    expect(commits()).toBe(0);
  });
});

describe("the keys of a bar", () => {
  test("change its value by a fiftieth with the keys that go up and down, by a tenth with the page keys and to the ends with Home and End, each press a gesture of its own", () => {
    const { step, changes, commits } = mount({ start: Array.from({ length: 16 }, () => 0) });
    fireEvent.keyDown(step(4), { key: "ArrowUp" });
    expect(changes.at(-1)?.[3]).toBeCloseTo(0.02, 9);
    fireEvent.keyDown(step(4), { key: "ArrowDown" });
    fireEvent.keyDown(step(4), { key: "ArrowDown" });
    expect(changes.at(-1)?.[3]).toBeCloseTo(-0.02, 9);
    fireEvent.keyDown(step(4), { key: "PageUp" });
    expect(changes.at(-1)?.[3]).toBeCloseTo(0.18, 9);
    fireEvent.keyDown(step(4), { key: "End" });
    expect(changes.at(-1)?.[3]).toBe(1);
    fireEvent.keyDown(step(4), { key: "Home" });
    expect(changes.at(-1)?.[3]).toBe(-1);
    expect(changes).toHaveLength(6);
    expect(commits()).toBe(6);
  });

  test("change the bar that has the focus and no other", () => {
    const { step, changes } = mount({ start: Array.from({ length: 16 }, () => 0) });
    fireEvent.keyDown(step(9), { key: "ArrowUp" });
    expect(changes).toHaveLength(1);
    expect(changes[0]?.[8]).toBeCloseTo(0.02, 9);
    expect(changes[0]?.filter((value) => value !== 0)).toHaveLength(1);
  });

  test("take the focus to the next bar and the one before with the arrows that go across, and change nothing", () => {
    const { step, changes, commits } = mount();
    step(5).focus();
    fireEvent.keyDown(step(5), { key: "ArrowRight" });
    expect(document.activeElement).toBe(step(6));
    fireEvent.keyDown(step(6), { key: "ArrowLeft" });
    fireEvent.keyDown(step(5), { key: "ArrowLeft" });
    expect(document.activeElement).toBe(step(4));
    expect(changes).toEqual([]);
    expect(commits()).toBe(0);
    step(1).focus();
    fireEvent.keyDown(step(1), { key: "ArrowLeft" });
    expect(document.activeElement, "there is no bar before the first").toBe(step(1));
    step(16).focus();
    fireEvent.keyDown(step(16), { key: "ArrowRight" });
    expect(document.activeElement, "and none after the last").toBe(step(16));
  });

  test("do nothing for a key that is not theirs, or when the bars cannot be changed", () => {
    const { step, changes } = mount();
    fireEvent.keyDown(step(2), { key: "a" });
    fireEvent.keyDown(step(2), { key: "Enter" });
    expect(changes).toEqual([]);
    cleanup();
    render(<StepBars label="Steps of Steps" values={RISING} length={16} />);
    fireEvent.keyDown(screen.getByRole("slider", { name: "Step 2" }), { key: "ArrowUp" });
    expect(screen.getByRole("slider", { name: "Step 2" }).getAttribute("aria-valuenow")).toBe("-0.875");
  });

  test("do nothing to a bar that is at the end it is sent to", () => {
    const { step, changes } = mount({ start: Array.from({ length: 16 }, () => 1) });
    fireEvent.keyDown(step(2), { key: "ArrowUp" });
    fireEvent.keyDown(step(2), { key: "End" });
    expect(changes).toEqual([]);
  });
});

describe("a double click on a bar", () => {
  test("puts it back to nothing, as a gesture of its own", () => {
    const { step, changes, commits } = mount();
    fireEvent.doubleClick(step(4));
    expect(changes).toHaveLength(1);
    expect(changes[0]?.[3]).toBe(0);
    expect(changes[0]?.[4]).toBe(RISING[4]);
    expect(commits()).toBe(1);
  });

  test("changes nothing for a bar that is nothing already", () => {
    const { step, changes, commits } = mount({ start: Array.from({ length: 16 }, () => 0) });
    fireEvent.doubleClick(step(4));
    expect(changes).toEqual([]);
    expect(commits()).toBe(0);
  });
});

describe("the playhead on the bars", () => {
  const playing = (): number[] => screen.getAllByRole("slider").flatMap((bar, index) => (bar.hasAttribute("data-playing") ? [index + 1] : []));

  test("is on the step that the position is in, and moves with it", () => {
    let position: number | undefined = 2.5;
    const { clock } = mount({ read: () => position });
    expect(playing(), "none before a frame").toEqual([]);
    clock.tick(16);
    expect(playing()).toEqual([3]);
    position = 4.9;
    clock.tick(33);
    expect(playing()).toEqual([5]);
    position = 0;
    clock.tick(50);
    expect(playing()).toEqual([1]);
  });

  test("is nowhere when there is no position, and goes when it goes", () => {
    let position: number | undefined;
    const { clock } = mount({ read: () => position });
    clock.tick(16);
    expect(playing()).toEqual([]);
    position = 7.2;
    clock.tick(33);
    expect(playing()).toEqual([8]);
    position = undefined;
    clock.tick(50);
    expect(playing()).toEqual([]);
  });

  test("goes round at the length, which is where a position that is past it is", () => {
    const { clock } = mount({ read: () => 4.5, length: 3 });
    clock.tick(16);
    expect(playing()).toEqual([2]);
  });

  test("follows a length that changes", () => {
    const clock = new Clock();
    const read = () => 4.5;
    const frames = new FrameLoop(clock);
    const { rerender } = render(<StepBars label="Steps of Steps" values={RISING} length={16} read={read} frames={frames} />);
    clock.tick(16);
    expect(playing()).toEqual([5]);
    rerender(<StepBars label="Steps of Steps" values={RISING} length={3} read={read} frames={frames} />);
    clock.tick(33);
    expect(playing()).toEqual([2]);
  });

  test("writes to the page only when it moves: a frame that finds it where it was writes nothing, and takes no render", () => {
    let position = 1.5;
    let renders = 0;
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    render(
      <Profiler
        id="bars"
        onRender={() => {
          renders += 1;
        }}
      >
        <StepBars label="Steps of Steps" values={RISING} length={16} read={() => position} frames={frames} />
      </Profiler>,
    );
    clock.tick(16);
    const rendered = renders;
    const bar = screen.getByRole("slider", { name: "Step 2" });
    const setAttribute = vi.spyOn(bar, "setAttribute");
    const removeAttribute = vi.spyOn(bar, "removeAttribute");
    clock.tick(33);
    clock.tick(50);
    expect(setAttribute).not.toHaveBeenCalled();
    expect(removeAttribute).not.toHaveBeenCalled();
    position = 2.5;
    clock.tick(66);
    expect(removeAttribute).toHaveBeenCalledWith("data-playing");
    expect(renders, "the playhead does not render the bars").toBe(rendered);
  });

  test("leaves with the bars: the loop is let go of", () => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const { unmount } = render(<StepBars label="Steps of Steps" values={RISING} length={16} read={() => 1} frames={frames} />);
    expect(frames.running).toBe(true);
    unmount();
    expect(frames.running).toBe(false);
  });
});
