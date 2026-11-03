import { afterEach, describe, expect, test, vi } from "vitest";

import { FrameLoop } from "./frameloop";
import { Clock } from "./testing";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("the frame loop", () => {
  test("asks for no frame until somebody listens", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    expect(clock.requested).toBe(0);
    expect(loop.running).toBe(false);
    loop.subscribe(() => undefined);
    expect(clock.pending).toBe(1);
    expect(loop.running).toBe(true);
  });

  test("calls everybody once a frame, in order, with the time of the frame, from one request", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    const heard: string[] = [];
    loop.subscribe((time) => heard.push(`a${time}`));
    loop.subscribe((time) => heard.push(`b${time}`));
    loop.subscribe((time) => heard.push(`c${time}`));
    expect(clock.pending).toBe(1);
    clock.tick(16);
    expect(heard).toEqual(["a16", "b16", "c16"]);
    expect(clock.pending).toBe(1); // the next frame is asked for once
    clock.tick(33);
    expect(heard).toHaveLength(6);
    expect(loop.frames).toBe(2);
  });

  test("stops asking for frames when the last listener goes, and takes back the one it asked for", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    const first = loop.subscribe(() => undefined);
    const second = loop.subscribe(() => undefined);
    first();
    expect(clock.pending).toBe(1);
    second();
    expect(clock.pending).toBe(0);
    expect(loop.running).toBe(false);
    clock.tick(16); // nothing happens
    expect(loop.frames).toBe(0);
  });

  test("starts again when somebody listens again", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    loop.subscribe(() => undefined)();
    const calls: number[] = [];
    loop.subscribe((time) => calls.push(time));
    clock.tick(5);
    expect(calls).toEqual([5]);
    expect(clock.pending).toBe(1);
  });

  test("lets a listener leave in the middle of a frame, and the others go on", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    const heard: string[] = [];
    const leave = loop.subscribe(() => {
      heard.push("a");
      leave();
    });
    loop.subscribe(() => heard.push("b"));
    clock.tick(1);
    clock.tick(2);
    expect(heard).toEqual(["a", "b", "b"]);
  });

  test("does not call a listener that was taken away by one before it in the same frame", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    const heard: string[] = [];
    let leaveB = (): void => undefined;
    loop.subscribe(() => {
      heard.push("a");
      leaveB();
    });
    leaveB = loop.subscribe(() => heard.push("b"));
    clock.tick(1);
    expect(heard).toEqual(["a"]);
  });

  test("first calls a listener that comes in the middle of a frame on the next", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    const heard: string[] = [];
    let added = false;
    loop.subscribe(() => {
      heard.push("a");
      if (!added) {
        added = true;
        loop.subscribe(() => heard.push("late"));
      }
    });
    clock.tick(1);
    expect(heard).toEqual(["a"]);
    clock.tick(2);
    expect(heard).toEqual(["a", "a", "late"]);
  });

  test("calls a listener that asked for the next frame once, on it, and then lets the loop rest", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    const heard: number[] = [];
    loop.next((time) => heard.push(time));
    expect(clock.pending).toBe(1);
    clock.tick(16);
    expect(heard).toEqual([16]);
    expect(clock.pending).toBe(0);
    expect(loop.running).toBe(false);
    clock.tick(33); // nothing is waiting
    expect(heard).toEqual([16]);
  });

  test("takes back a request for the next frame, and the frame that was asked for it", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    const heard: number[] = [];
    const cancel = loop.next((time) => heard.push(time));
    cancel();
    expect(clock.pending).toBe(0);
    expect(heard).toEqual([]);
    clock.tick(16);
    expect(heard).toEqual([]);
    cancel(); // again, and after the fact: nothing happens
  });

  test("lets the others go on after a request for the next frame is met", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    const heard: string[] = [];
    loop.subscribe(() => heard.push("always"));
    loop.next(() => heard.push("once"));
    clock.tick(1);
    clock.tick(2);
    expect(heard).toEqual(["always", "once", "always"]);
    expect(clock.pending).toBe(1);
  });

  test("lets a request for the next frame ask for another, which is for the frame after", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    const heard: number[] = [];
    const again = (time: number): void => {
      heard.push(time);
      if (heard.length < 3) loop.next(again);
    };
    loop.next(again);
    clock.tick(1);
    expect(heard).toEqual([1]);
    clock.tick(2);
    clock.tick(3);
    expect(heard).toEqual([1, 2, 3]);
    expect(clock.pending).toBe(0);
  });

  test("counts a function that listens twice twice, and one that leaves once leaves one", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    let calls = 0;
    const draw = (): void => {
      calls += 1;
    };
    const first = loop.subscribe(draw);
    loop.subscribe(draw);
    clock.tick(1);
    expect(calls).toBe(2);
    first();
    clock.tick(2);
    expect(calls).toBe(3);
  });

  test("keeps going when one drawing fails, and says so", () => {
    const clock = new Clock();
    const loop = new FrameLoop(clock);
    const problem = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const heard: string[] = [];
    loop.subscribe(() => {
      throw new Error("a bad drawing");
    });
    loop.subscribe(() => heard.push("b"));
    clock.tick(1);
    clock.tick(2);
    expect(heard).toEqual(["b", "b"]);
    expect(problem).toHaveBeenCalledTimes(2);
    expect(String(problem.mock.calls[0]?.[0])).toContain("a bad drawing");
  });
});
