import { describe, expect, test, vi } from "vitest";

import type { Intent } from "../protocol";
import { Client } from "./connection";
import { Drive } from "./drive";
import { FrameLoop } from "./frameloop";
import { MockServer } from "./mock";
import { Clock } from "./testing";

interface Call {
  intent: Intent;
  answer: () => void;
  refuse: (error: unknown) => void;
}

/** A drive whose server answers when the test says so. */
function setup() {
  const clock = new Clock();
  const frames = new FrameLoop(clock);
  const calls: Call[] = [];
  const errors: [unknown, Intent][] = [];
  const drive = new Drive({
    frames,
    send: (intent) =>
      new Promise<unknown>((resolve, reject) => {
        calls.push({
          intent,
          answer: () => {
            resolve(1);
          },
          refuse: reject,
        });
      }),
    onError: (error, intent) => {
      errors.push([error, intent]);
    },
  });
  return { clock, frames, calls, errors, drive };
}

/** Let the promises that have been answered pass on what they were answered with. */
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

const setParam = (value: number, node = 1, param = 0): Intent => ({ type: "setParam", node, param, value });

describe("a control that is turned", () => {
  test("shows what it was set to at once, and sends nothing before the frame", () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    expect(control.getPending()).toBeNull();
    control.set(0.25);
    expect(control.getPending()).toBe(0.25);
    expect(calls).toEqual([]);
    expect(clock.pending).toBe(1);
  });

  test("sends the latest value, once, on the frame, however often it was set in it", () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    for (const value of [0.1, 0.2, 0.3, 0.4]) control.set(value);
    clock.tick(16);
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.4)]);
    expect(control.getPending()).toBe(0.4);
  });

  test("sends one for each frame in which it changed, and nothing in a frame in which it did not", () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    control.set(0.1);
    clock.tick(16);
    control.set(0.2);
    control.set(0.3);
    clock.tick(33);
    clock.tick(50); // no change
    control.set(0.4);
    clock.tick(66);
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.1), setParam(0.3), setParam(0.4)]);
  });

  test("asks the frame loop for one frame, however often it is set before it", () => {
    const { drive, frames } = setup();
    const next = vi.spyOn(frames, "next");
    const control = drive.param(1, 0);
    for (const value of [0.1, 0.2, 0.3]) control.set(value);
    expect(next).toHaveBeenCalledTimes(1);
  });

  test("lets the frame loop rest between gestures", () => {
    const { drive, clock, frames } = setup();
    const control = drive.param(1, 0);
    control.set(0.5);
    clock.tick(16);
    expect(frames.running).toBe(false);
    expect(clock.pending).toBe(0);
  });

  test("shows what it was set to until the server has answered everything it was sent", async () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    control.set(0.1);
    clock.tick(16);
    control.set(0.2);
    clock.tick(33);
    expect(calls).toHaveLength(2);
    calls[0]?.answer();
    await settle();
    expect(control.getPending()).toBe(0.2); // the second is still out
    calls[1]?.answer();
    await settle();
    expect(control.getPending()).toBeNull();
  });

  test("goes on showing a value that was set after the last send, even when everything sent is answered", async () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    control.set(0.1);
    clock.tick(16);
    control.set(0.2); // not sent yet
    calls[0]?.answer();
    await settle();
    expect(control.getPending()).toBe(0.2);
    clock.tick(33);
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.1), setParam(0.2)]);
    calls[1]?.answer();
    await settle();
    expect(control.getPending()).toBeNull();
  });

  test("tells the views of each change of what it shows, and of nothing else", async () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    const heard: (number | null)[] = [];
    control.subscribe(() => heard.push(control.getPending()));
    control.set(0.1);
    control.set(0.1); // the same: nothing to tell
    control.set(0.2);
    expect(heard).toEqual([0.1, 0.2]);
    clock.tick(16);
    expect(heard).toEqual([0.1, 0.2]); // sending changes nothing to show
    calls[0]?.answer();
    await settle();
    expect(heard).toEqual([0.1, 0.2, null]);
  });

  test("sends a value that is the same as the last, if it was set again", () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    control.set(0.5);
    clock.tick(16);
    control.set(0.5);
    clock.tick(33);
    expect(calls).toHaveLength(2);
  });

  test("lets a view leave when it is told, and the others hear it all the same", () => {
    const { drive } = setup();
    const control = drive.param(1, 0);
    const heard: string[] = [];
    const stopFirst = control.subscribe(() => {
      heard.push("first");
      stopFirst();
    });
    control.subscribe(() => heard.push("second"));
    control.set(0.1);
    control.set(0.2);
    expect(heard).toEqual(["first", "second", "second"]);
  });

  test("a view that has gone is not told", () => {
    const { drive } = setup();
    const control = drive.param(1, 0);
    const heard: unknown[] = [];
    const stop = control.subscribe(() => heard.push(1));
    control.set(0.1);
    stop();
    control.set(0.2);
    expect(heard).toEqual([1]);
  });

  test("takes no notice of what cannot be sent", () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    for (const value of [Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) control.set(value);
    expect(control.getPending()).toBeNull();
    expect(clock.pending).toBe(0);
    clock.tick(16);
    expect(calls).toEqual([]);
  });
});

describe("the end of a gesture", () => {
  test("sends what is set at once, and the frame that was waiting for it sends nothing more", () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    control.set(0.7);
    control.commit();
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.7)]);
    expect(clock.pending).toBe(0);
    clock.tick(16);
    expect(calls).toHaveLength(1);
  });

  test("is followed by another gesture, which is sent on its own frame", () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    control.set(0.7);
    control.commit();
    control.set(0.8);
    expect(clock.pending).toBe(1);
    clock.tick(16);
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.7), setParam(0.8)]);
  });

  test("waits for the next frame when an intent has gone in this one already: one intent a frame", () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    control.set(0.1);
    clock.tick(16); // sent, on the frame
    control.set(0.2);
    control.commit(); // the gesture is over, in the same frame
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.1)]);
    expect(clock.pending).toBe(1);
    clock.tick(33);
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.1), setParam(0.2)]);
    expect(clock.pending).toBe(0);
    // And the control is as it was: what is set next goes on the next frame.
    control.set(0.3);
    clock.tick(50);
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.1), setParam(0.2), setParam(0.3)]);
  });

  test("does not hold a commit back that comes in a frame in which this control sent nothing", () => {
    const { drive, calls, clock, frames } = setup();
    frames.subscribe(() => undefined); // something else draws on the frames, so that they go on
    const control = drive.param(1, 0);
    control.set(0.1);
    clock.tick(16);
    clock.tick(33); // another frame, in which this control sends nothing
    control.set(0.2);
    control.commit();
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.1), setParam(0.2)]);
  });

  test("when no frame has gone by since it sent, as when nothing draws on them, a commit waits for the next", () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    control.set(0.1);
    clock.tick(16); // sent; the loop has no one else and rests
    control.set(0.2);
    control.commit();
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.1)]);
    clock.tick(33);
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.1), setParam(0.2)]);
  });

  test("lets each control send its own intent in a frame", () => {
    const { drive, calls, clock } = setup();
    drive.param(1, 0).set(0.1);
    clock.tick(16);
    const other = drive.param(1, 1);
    other.set(0.3);
    other.commit(); // in the same frame, but it is not the control that sent
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.1, 1, 0), setParam(0.3, 1, 1)]);
  });

  test("sends nothing when everything has been sent", () => {
    const { drive, calls, clock } = setup();
    const control = drive.param(1, 0);
    control.commit();
    expect(calls).toEqual([]);
    control.set(0.7);
    clock.tick(16);
    control.commit();
    expect(calls).toHaveLength(1);
  });
});

describe("an intent that does not get through", () => {
  test("is reported with the intent, and the control goes back to the session's value", async () => {
    const { drive, calls, clock, errors } = setup();
    const control = drive.param(1, 0);
    control.set(0.9);
    clock.tick(16);
    const problem = new Error("no such parameter");
    calls[0]?.refuse(problem);
    await settle();
    expect(errors).toEqual([[problem, setParam(0.9)]]);
    expect(control.getPending()).toBeNull();
  });

  test("leaves what a later value shows alone until that one is answered", async () => {
    const { drive, calls, clock, errors } = setup();
    const control = drive.param(1, 0);
    control.set(0.1);
    clock.tick(16);
    control.set(0.2);
    clock.tick(33);
    calls[0]?.refuse(new Error("lost"));
    await settle();
    expect(errors).toHaveLength(1);
    expect(control.getPending()).toBe(0.2);
    calls[1]?.answer();
    await settle();
    expect(control.getPending()).toBeNull();
  });

  test("is not an unhandled rejection when nobody listens for errors", async () => {
    const clock = new Clock();
    const drive = new Drive({ frames: new FrameLoop(clock), send: () => Promise.reject(new Error("refused")) });
    const control = drive.param(1, 0);
    control.set(0.5);
    clock.tick(16);
    await settle();
    expect(control.getPending()).toBeNull();
  });

  test("a send that throws is the same as one that is refused", async () => {
    const clock = new Clock();
    const errors: unknown[] = [];
    const drive = new Drive({
      frames: new FrameLoop(clock),
      send: () => {
        throw new Error("no socket");
      },
      onError: (error) => {
        errors.push(error);
      },
    });
    const control = drive.param(1, 0);
    control.set(0.5);
    clock.tick(16);
    await settle();
    expect(String(errors[0])).toContain("no socket");
    expect(control.getPending()).toBeNull();
  });
});

describe("the controls of a session", () => {
  test("are one for each parameter, and one for each gain", () => {
    const { drive } = setup();
    expect(drive.param(1, 0)).toBe(drive.param(1, 0));
    expect(drive.param(1, 0)).not.toBe(drive.param(1, 1));
    expect(drive.param(1, 0)).not.toBe(drive.param(2, 0));
    expect(drive.gain(1, "volume")).toBe(drive.gain(1, "volume"));
    expect(drive.gain(1, "volume")).not.toBe(drive.gain(1, "mix"));
    expect(drive.gain(1, "mix")).not.toBe(drive.gain(2, "mix"));
    expect(drive.param(1, 0)).not.toBe(drive.gain(1, "volume"));
  });

  test("send a parameter as setParam and a gain as setGain", () => {
    const { drive, calls, clock } = setup();
    drive.param(3, 2).set(0.5);
    drive.gain(4, "groupPan").set(-0.5);
    clock.tick(16);
    expect(calls.map((call) => call.intent)).toEqual([
      { type: "setParam", node: 3, param: 2, value: 0.5 },
      { type: "setGain", node: 4, gain: "groupPan", value: -0.5 },
    ]);
  });

  test("are one for each setting of a modulator, and one for each cable's depth", () => {
    const { drive } = setup();
    expect(drive.modulatorField(2, "rate")).toBe(drive.modulatorField(2, "rate"));
    expect(drive.modulatorField(2, "rate")).not.toBe(drive.modulatorField(2, "attack"));
    expect(drive.modulatorField(2, "rate")).not.toBe(drive.modulatorField(3, "rate"));
    expect(drive.cableDepth(7)).toBe(drive.cableDepth(7));
    expect(drive.cableDepth(7)).not.toBe(drive.cableDepth(8));
    // (Not the control of a parameter or a gain that has the same numbers.)
    expect(drive.modulatorField(2, "rate")).not.toBe(drive.param(2, 0));
    expect(drive.cableDepth(2)).not.toBe(drive.gain(2, "volume"));
    expect(drive.cableDepth(2)).not.toBe(drive.param(2, 0));
  });

  test("send a setting of a modulator as setModulatorField and a cable's depth as setEdgeDepth", () => {
    const { drive, calls, clock } = setup();
    drive.modulatorField(5, "sustain").set(0.25);
    drive.cableDepth(9).set(-0.75);
    clock.tick(16);
    expect(calls.map((call) => call.intent)).toEqual([
      { type: "setModulatorField", node: 5, field: "sustain", value: 0.25 },
      { type: "setEdgeDepth", edge: 9, depth: -0.75 },
    ]);
  });

  test("do not share a frame's worth: each sends its own", () => {
    const { drive, calls, clock } = setup();
    drive.param(1, 0).set(0.1);
    drive.param(1, 1).set(0.2);
    drive.param(1, 0).set(0.3);
    clock.tick(16);
    expect(calls.map((call) => call.intent)).toEqual([setParam(0.3, 1, 0), setParam(0.2, 1, 1)]);
  });
});

describe("a control against the engine that answers", () => {
  test("shows the user's value while a drag is answered, and the session's after it", async () => {
    const server = new MockServer({ telemetryPeriod: 1000 });
    const client = new Client({ transport: server.transport });
    client.start();
    await vi.waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    const clock = new Clock();
    const drive = new Drive({ frames: new FrameLoop(clock), send: (intent) => client.request(intent) });
    const control = drive.param(1, 0); // the filter's cutoff
    const sessionValue = (): number | undefined => {
      const node = client.state.snapshot?.graph.nodes.find((n) => n.id === 1);
      return node?.kind === "effect" ? node.plugin.params.find((p) => p.index === 0)?.value : undefined;
    };

    const shown: (number | null)[] = [];
    control.subscribe(() => shown.push(control.getPending()));
    // A drag: a value every frame, answered by the engine as they come.
    for (let frame = 1; frame <= 5; frame += 1) {
      control.set(frame / 10);
      clock.tick(frame * 16);
    }
    control.commit();
    await vi.waitFor(() => {
      expect(control.getPending()).toBeNull();
    });
    expect(sessionValue()).toBeCloseTo(0.5, 6);
    // Only what the user did was ever shown, and then the session's own value.
    expect(shown.filter((value) => value !== null)).toEqual([0.1, 0.2, 0.3, 0.4, 0.5]);
    expect(shown.at(-1)).toBeNull();
    // One intent a frame: five, though the engine made five events and the client got them all.
    const sent = server.received.filter((message) => message.type === "request");
    expect(sent).toHaveLength(5);
    client.stop();
  });
});
