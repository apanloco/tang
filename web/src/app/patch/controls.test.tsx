// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { Profiler } from "react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { Modulation } from "../../client/modulation";
import type { Edge, Node, Param, Port, Snapshot } from "../../protocol";
import { dragged } from "../../ui/knob-geometry";
import { angleOf, arcPath, fractionOf, pointAt } from "../../ui/knob-geometry";
import { rig } from "../testing";
import { CableDepthControl, GainControl, MacroValueControl, ModulatorFieldControl, ModulatorValueControl, ParamControl, ParamRow, RetriggerControl, sameModulations, sameParamFacts, StepsControl, SyncControl, WaveformControl } from "./controls";

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

const slider = (name: string) => screen.getByRole("slider", { name });

/** Let what the mock engine has been told reach it: it takes a moment (a microtask) to send and to answer. */
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/** The dot of the knob that is in `root`, as it is drawn now. */
function dotOf(root: HTMLElement): { x: number; y: number; visible: boolean } {
  const dot = root.querySelector(".tang-knob__dot");
  return { x: Number(dot?.getAttribute("cx")), y: Number(dot?.getAttribute("cy")), visible: dot?.getAttribute("visibility") === "visible" };
}

/** A drag of a knob up by `pixels`, which is that many 200ths of its range. */
function drag(dial: HTMLElement, pixels: number, from = 300): void {
  fireEvent.pointerDown(dial, { clientY: from, button: 0, pointerId: 1 });
  fireEvent.pointerMove(dial, { clientY: from - pixels, pointerId: 1 });
}

describe("a parameter of a node", () => {
  test("is a knob with the parameter's name, range and value", async () => {
    await rig(<ParamControl node={filter.id} index={0} />);
    const cutoff = slider("cutoff");
    expect(cutoff.getAttribute("aria-valuemin")).toBe("0");
    expect(cutoff.getAttribute("aria-valuemax")).toBe("1");
    expect(cutoff.getAttribute("aria-valuenow")).toBe("0.5");
    expect(cutoff.getAttribute("aria-valuetext")).toBe("0.5"); // a parameter with no unit is written with none
  });

  test("says its value with its unit when it has one", async () => {
    await rig(<ParamControl node={oscillator.id} index={1} />);
    expect(slider("detune").getAttribute("aria-valuetext")).toBe("0 st");
    expect(slider("detune").getAttribute("aria-valuemin")).toBe("-24");
  });

  test("is a choice when its values have names, with the chosen one selected", async () => {
    await rig(<ParamControl node={oscillator.id} index={0} />);
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "waveform" });
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(["sine", "triangle", "square", "saw", "noise"]);
    expect(select.value).toBe("3");
    expect(screen.queryByRole("slider")).toBeNull();
  });

  test("shows nothing for a parameter that is not there, or a node that is not", async () => {
    const { view } = await rig(
      <div data-testid="where">
        <ParamControl node={filter.id} index={99} />
        <ParamControl node={4242} index={0} />
      </div>,
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });

  test("follows a change somebody else makes", async () => {
    const { server } = await rig(<ParamControl node={filter.id} index={0} />);
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.8 });
    });
    await waitFor(() => {
      expect(slider("cutoff").getAttribute("aria-valuenow")).toBe("0.8");
    });
  });

  test("shows what it is dragged to at once, and sends it on the frame, once", async () => {
    const { clock, requests } = await rig(<ParamControl node={filter.id} index={0} />);
    drag(slider("cutoff"), 50); // a quarter of the range
    expect(slider("cutoff").getAttribute("aria-valuenow")).toBe("0.75");
    await settle();
    expect(requests()).toEqual([]); // not before the frame
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 0, value: 0.75 } }]);
    });
  });

  test("goes on showing what it is dragged to while the engine answers, and then what the engine has", async () => {
    const { clock, client, requests } = await rig(<ParamControl node={filter.id} index={0} />);
    const dial = slider("cutoff");
    fireEvent.pointerDown(dial, { clientY: 300, button: 0, pointerId: 1 });
    for (let step = 1; step <= 4; step += 1) {
      fireEvent.pointerMove(dial, { clientY: 300 - step * 10, pointerId: 1 });
      act(() => {
        clock.tick(step * 16);
      });
    }
    expect(Number(dial.getAttribute("aria-valuenow"))).toBeCloseTo(0.7, 6);
    fireEvent.pointerUp(dial, { pointerId: 1 });
    await waitFor(() => {
      const node = client.state.snapshot?.graph.nodes.find((candidate) => candidate.id === filter.id);
      expect(node?.kind === "effect" && node.plugin.params[0]?.value).toBeCloseTo(0.7, 6);
    });
    expect(requests()).toHaveLength(4);
    await waitFor(() => {
      expect(Number(slider("cutoff").getAttribute("aria-valuenow"))).toBeCloseTo(0.7, 6);
    });
  });

  test("does not let somebody else's change pull it away from the pointer while it is dragged", async () => {
    const { server, clock, client } = await rig(<ParamControl node={filter.id} index={0} />);
    const dial = slider("cutoff");
    drag(dial, 20);
    act(() => {
      clock.tick(16);
    });
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.05 });
    });
    // The engine has the other value now (or will in a moment), but the knob is under the pointer.
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBeGreaterThanOrEqual(2);
    });
    expect(Number(dial.getAttribute("aria-valuenow"))).toBeCloseTo(0.6, 6);
    fireEvent.pointerUp(dial, { pointerId: 1 });
    await waitFor(() => {
      // Its own request was answered after the other change: that is the engine's last word.
      expect(Number(slider("cutoff").getAttribute("aria-valuenow"))).toBeCloseTo(0.6, 6);
    });
  });

  test("sends a key press at once", async () => {
    const { requests } = await rig(<ParamControl node={filter.id} index={0} />);
    fireEvent.keyDown(slider("cutoff"), { key: "ArrowUp" });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 0, value: 0.51 } }]);
    });
  });

  test("goes back to its default on a double click, at once", async () => {
    const { server, requests } = await rig(<ParamControl node={filter.id} index={1} />);
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 1, value: 5 });
    });
    await waitFor(() => {
      expect(slider("resonance").getAttribute("aria-valuenow")).toBe("5");
    });
    fireEvent.doubleClick(slider("resonance"));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 1, value: 0.707 } }]);
    });
  });

  test("sends the choice that is made at once, as the index", async () => {
    const { requests } = await rig(<ParamControl node={oscillator.id} index={0} />);
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "1" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: oscillator.id, param: 0, value: 1 } }]);
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLSelectElement>("combobox").value).toBe("1");
    });
  });
});

describe("the modulation of a parameter", () => {
  test("is a ring around the knob from where the cables that end at it can take it", async () => {
    const { view } = await rig(<ParamControl node={filter.id} index={0} />);
    // The LFO's depth is 0.4 of the range of 0 to 1, around the 0.5 the user set.
    expect(view.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.1, 0.9));
  });

  test("is drawn over the parameter's own range", async () => {
    // The LFO drives the resonance as well, which goes from 0.5 to 10 and is set to 2.
    const both: Snapshot = structuredClone(tracerSnapshot);
    both.graph.edges.push({
      kind: "modulation",
      id: 9,
      amount: 0.1,
      from: { node: nodeNamed(tracerSnapshot, "LFO").id, port: { type: "modOut" } },
      to: { node: filter.id, port: { type: "param", index: 1 } },
    });
    const { view } = await rig(<ParamControl node={filter.id} index={1} />, { snapshot: both });
    // 0.1 of the range of 9.5 on both sides of 2: from 1.05 to 2.95.
    expect(view.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.55 / 9.5, 2.45 / 9.5));
  });

  test("moves with the value the user sets", async () => {
    const { view } = await rig(<ParamControl node={filter.id} index={0} />);
    drag(slider("cutoff"), 50); // to 0.75: the ring is 0.35 up to the top
    expect(view.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.35, 1));
  });

  test("is not there for a parameter that nothing drives, or one that has named values", async () => {
    const { view } = await rig(
      <>
        <ParamControl node={filter.id} index={1} />
        <ParamControl node={oscillator.id} index={0} />
      </>,
    );
    expect(view.container.querySelector(".tang-knob__ring")).toBeNull();
  });

  test("is the dot where the engine says the parameter is, taken from the telemetry on the frame", async () => {
    const { view, client, clock, time } = await rig(<ParamControl node={filter.id} index={0} />);
    time.seconds = 0.5; // the LFO is at its peak: the cutoff is 0.5 + 0.4
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      expect(dotOf(view.container).visible).toBe(true);
      expect(dotOf(view.container).x).toBeCloseTo(pointAt(24, 18, angleOf(0.9)).x, 3);
    });
    const dot = dotOf(view.container);
    const peak = pointAt(24, 18, angleOf(0.9));
    expect([dot.x, dot.y]).toEqual([expect.closeTo(peak.x, 3), expect.closeTo(peak.y, 3)]);
    // And where it goes next.
    time.seconds = 1.5; // a quarter of a period on: sine(0.75 of a cycle) is the other end
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      expect(dotOf(view.container).x).toBeCloseTo(pointAt(24, 18, angleOf(0.1)).x, 3);
    });
  });

  test("has no dot for a parameter that nothing drives", async () => {
    const { view, client, clock } = await rig(<ParamControl node={filter.id} index={1} />);
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    act(() => {
      clock.tick(100);
    });
    expect(dotOf(view.container).visible).toBe(false);
  });

  test("does not render the control again for the telemetry, however many frames come", async () => {
    let renders = 0;
    const { client, clock, time } = await rig(
      <Profiler id="cutoff" onRender={() => (renders += 1)}>
        <ParamControl node={filter.id} index={0} />
      </Profiler>,
    );
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    const settled = renders;
    for (let frame = 1; frame <= 60; frame += 1) {
      time.seconds = frame / 40;
      await new Promise((resolve) => setTimeout(resolve, 6)); // the engine makes a frame every 5 ms
      act(() => {
        clock.tick(frame * 16);
      });
    }
    expect(client.telemetry.frames).toBeGreaterThan(20);
    expect(renders).toBe(settled);
  });

  test("does not render the control again when another parameter of the plugin changes", async () => {
    let renders = 0;
    const { server, client } = await rig(
      <Profiler id="cutoff" onRender={() => (renders += 1)}>
        <ParamControl node={filter.id} index={0} />
      </Profiler>,
    );
    const settled = renders;
    const rev = client.state.snapshot?.rev ?? 0;
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 1, value: 4 });
      server.apply({ type: "setParam", node: filter.id, param: 3, value: 2 });
    });
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBe(rev + 2);
    });
    expect(renders).toBe(settled);
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.2 });
    });
    await waitFor(() => {
      expect(renders).toBeGreaterThan(settled);
    });
  });
});

describe("the modulation of a host-side gain", () => {
  const lfo = nodeNamed(tracerSnapshot, "LFO");
  const mix = filter.kind === "effect" ? filter.mix : Number.NaN;
  /** A cable from the tracer's LFO to the host-side value `port` of `node`, drawn the way a page asks the engine for one. */
  const draw = (server: { apply: (intent: never) => unknown }, node: number, port: Port): void => {
    act(() => {
      server.apply({ type: "addModEdge", modulator: lfo.id, to: { node, port } } as never);
    });
  };

  test("is a ring around the knob from where the cables that end at it can take it, as a parameter's is, and none before there is a cable", async () => {
    const { server, view } = await rig(<GainControl node={filter.id} gain="mix" />);
    expect(view.container.querySelector(".tang-knob__ring")).toBeNull();
    draw(server, filter.id, { type: "mix" });
    // The cable is at half depth, the LFO swings the mix half the range of 0 to 1 each way from where the user set it, and the ring is held to the knob.
    await waitFor(() => {
      expect(view.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, Math.max(mix - 0.5, 0), Math.min(mix + 0.5, 1)));
    });
  });

  test("is drawn over the range of the value: a volume goes from 0 to 4, a group's pan from -1 to 1", async () => {
    const grouped: Snapshot = structuredClone(tracerSnapshot);
    const { server, view } = await rig(
      <>
        <GainControl node={oscillator.id} gain="volume" />
      </>,
      { snapshot: grouped },
    );
    const volume = oscillator.kind === "instrument" ? oscillator.volume : Number.NaN;
    draw(server, oscillator.id, { type: "volume" });
    // Half of the range of 4, each way from the volume that the user set, held to 0 … 4.
    await waitFor(() => {
      expect(view.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, Math.max(volume - 2, 0) / 4, Math.min(volume + 2, 4) / 4));
    });
  });

  test("moves with the value the user sets", async () => {
    const { server, view } = await rig(<GainControl node={filter.id} gain="mix" />);
    draw(server, filter.id, { type: "mix" });
    await waitFor(() => {
      expect(view.container.querySelector(".tang-knob__ring")).not.toBeNull();
    });
    drag(slider("Mix"), -100); // down by half of the range
    await waitFor(() => {
      expect(view.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0, Math.min(Math.max(mix - 0.5, 0) + 0.5, 1)));
    });
  });

  test("is the dot where the engine says the value is, taken from the telemetry on the frame", async () => {
    const { server, view, client, clock, time } = await rig(<GainControl node={filter.id} gain="mix" />);
    draw(server, filter.id, { type: "mix" });
    await waitFor(() => {
      expect(view.container.querySelector(".tang-knob__ring")).not.toBeNull();
    });
    time.seconds = 0.5; // the LFO is at its peak: the mix is what the user set and half the range more, held to 1
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      expect(client.telemetry.has("modulation")).toBe(true);
      expect(dotOf(view.container).visible).toBe(true);
    });
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      expect(dotOf(view.container).x).toBeCloseTo(pointAt(24, 18, angleOf(Math.min(mix + 0.5, 1))).x, 3);
    });
    // And where it goes next: the other end.
    time.seconds = 1.5;
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      expect(dotOf(view.container).x).toBeCloseTo(pointAt(24, 18, angleOf(Math.max(mix - 0.5, 0))).x, 3);
    });
  });

  /** Turn frames by hand, with real time between them for the mock's telemetry to come (not inside `waitFor`: a failing assertion there is run again by every frame's change of the DOM). */
  const until = async (clock: { tick: (time: number) => void }, condition: () => boolean, what: string): Promise<void> => {
    const deadline = Date.now() + 2000;
    while (!condition()) {
      if (Date.now() > deadline) throw new Error(`gave up waiting for ${what}`);
      await new Promise((resolve) => setTimeout(resolve, 10));
      act(() => {
        clock.tick(performance.now());
      });
    }
  };

  test("is the dot of a lane's volume where the engine says that volume is: the telemetry of the value that the knob is for, and not that of another value of the node", async () => {
    const { server, view, clock, time } = await rig(<GainControl node={oscillator.id} gain="volume" />);
    const volume = oscillator.kind === "instrument" ? oscillator.volume : Number.NaN;
    draw(server, oscillator.id, { type: "volume" });
    const at = () => dotOf(view.container).x;
    const near = (fraction: number): boolean => Math.abs(at() - pointAt(24, 18, angleOf(fraction)).x) < 1e-3;
    time.seconds = 0.5; // the LFO is at its peak: the volume is what the user set and half of the range of 4 more, held to 4
    await until(clock, () => dotOf(view.container).visible && near(Math.min(volume + 2, 4) / 4), "the dot at the top of the volume");
    time.seconds = 1.5; // and the other end: half of the range of 4 less, held to 0
    await until(clock, () => near(Math.max(volume - 2, 0) / 4), "the dot at the bottom of the volume");
  });

  test("has no ring and no dot for a value that nothing drives: the other gains of the session, and a modulator's cable to a parameter of the same node", async () => {
    const { view, client, clock } = await rig(
      <>
        <GainControl node={oscillator.id} gain="volume" />
        <ParamControl node={filter.id} index={0} />
      </>,
    );
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    act(() => {
      clock.tick(100);
    });
    // (The one ring is the cutoff's.)
    expect(view.container.querySelectorAll(".tang-knob__ring")).toHaveLength(1);
    const knobs = Array.from(view.container.querySelectorAll(".tang-knob"));
    expect(knobs.map((knob) => knob.querySelector(".tang-knob__dot")?.getAttribute("visibility") === "visible")).toEqual([false, true]);
  });
});

describe("a parameter that a modulator drives", () => {
  const lfo = nodeNamed(tracerSnapshot, "LFO");

  test("does not render the control again when the modulator's rate or waveform is turned, which the ring does not depend on", async () => {
    let renders = 0;
    const { server, client } = await rig(
      <Profiler id="cutoff" onRender={() => (renders += 1)}>
        <ParamControl node={filter.id} index={0} />
      </Profiler>,
    );
    const settled = renders;
    const rev = client.state.snapshot?.rev ?? 0;
    act(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "rate", value: 5 });
      server.apply({ type: "setModulatorField", node: lfo.id, field: "rate", value: 6 });
      server.apply({ type: "setModulatorField", node: lfo.id, field: "waveform", value: 3 });
    });
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBe(rev + 3);
    });
    expect(renders).toBe(settled);
    // What the ring does depend on: how deep the cable goes.
    act(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: 0.1 });
    });
    await waitFor(() => {
      expect(renders).toBeGreaterThan(settled);
    });
  });
});

describe("a host-side gain of a node", () => {
  test("is a knob called Volume for an instrument, over 0 to 4, at the value it has", async () => {
    await rig(<GainControl node={oscillator.id} gain="volume" />);
    const volume = slider("Volume");
    expect(volume.getAttribute("aria-valuemin")).toBe("0");
    expect(volume.getAttribute("aria-valuemax")).toBe("4");
    expect(Number(volume.getAttribute("aria-valuenow"))).toBeCloseTo(0.1, 6);
  });

  test("is a knob called Mix for an effect, over 0 to 1", async () => {
    await rig(<GainControl node={filter.id} gain="mix" />);
    expect(slider("Mix").getAttribute("aria-valuemax")).toBe("1");
    expect(slider("Mix").getAttribute("aria-valuenow")).toBe("1");
  });

  test("is sent as a gain, on the frame, and shown meanwhile", async () => {
    const { clock, requests } = await rig(<GainControl node={oscillator.id} gain="volume" />);
    drag(slider("Volume"), 50); // a quarter of 0 to 4, from 0.1
    expect(Number(slider("Volume").getAttribute("aria-valuenow"))).toBeCloseTo(1.1, 6);
    await settle();
    expect(requests()).toEqual([]);
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toMatchObject({ type: "setGain", node: oscillator.id, gain: "volume" });
  });

  test("goes back to unity on a double click (volume) and to wet (mix)", async () => {
    const { requests } = await rig(
      <>
        <GainControl node={oscillator.id} gain="volume" />
        <GainControl node={filter.id} gain="mix" />
      </>,
    );
    // The mix is at its default already, so a double click changes nothing; the volume is not.
    fireEvent.doubleClick(slider("Mix"));
    fireEvent.doubleClick(slider("Volume"));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setGain", node: oscillator.id, gain: "volume", value: 1 } }]);
    });
  });

  test("shows a group's volume and pan, over their ranges, and pan goes back to the middle", async () => {
    const withGroup: Snapshot = structuredClone(tracerSnapshot);
    withGroup.graph.nodes.push({ id: 77, parent: null, name: "Group 1", kind: "group", volume: 0.8, pan: -0.5, muted: false, solo: false, route: "out" });
    const { requests } = await rig(
      <div data-testid="group">
        <GainControl node={77} gain="groupVolume" />
        <GainControl node={77} gain="groupPan" />
      </div>,
      { snapshot: withGroup },
    );
    const group = screen.getByTestId("group");
    const volume = within(group).getByRole("slider", { name: "Volume" });
    const pan = within(group).getByRole("slider", { name: "Pan" });
    expect([volume.getAttribute("aria-valuemin"), volume.getAttribute("aria-valuemax"), volume.getAttribute("aria-valuenow")]).toEqual(["0", "4", "0.8"]);
    expect([pan.getAttribute("aria-valuemin"), pan.getAttribute("aria-valuemax"), pan.getAttribute("aria-valuenow")]).toEqual(["-1", "1", "-0.5"]);
    fireEvent.doubleClick(pan);
    fireEvent.doubleClick(volume);
    await waitFor(() => {
      expect(requests()).toEqual([
        { intent: { type: "setGain", node: 77, gain: "groupPan", value: 0 } },
        { intent: { type: "setGain", node: 77, gain: "groupVolume", value: 1 } },
      ]);
    });
  });

  test("shows nothing for a gain the node does not have", async () => {
    const { view } = await rig(
      <div data-testid="where">
        <GainControl node={oscillator.id} gain="mix" />
        <GainControl node={filter.id} gain="volume" />
        <GainControl node={4242} gain="volume" />
      </div>,
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });
});

/** The tracer with an envelope in it, for the settings that an LFO does not have. */
function withEnvelope(): { snapshot: Snapshot; envelope: Node } {
  const envelope: Node = { id: 90, parent: 0, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 2 } };
  const snapshot: Snapshot = structuredClone(tracerSnapshot);
  snapshot.graph.nodes.push(envelope);
  return { snapshot, envelope };
}

const lfo = nodeNamed(tracerSnapshot, "LFO");

/** The tracer with a macro at 0.25 in it. */
function withMacro(): { snapshot: Snapshot; knob: Node } {
  const knob: Node = { id: 91, parent: 0, name: "Macro", kind: "modulator", source: { type: "macro", value: 0.25 } };
  const snapshot: Snapshot = structuredClone(tracerSnapshot);
  snapshot.graph.nodes.push(knob);
  return { snapshot, knob };
}

describe("the value of a macro", () => {
  test("is a knob from nothing to one, at the value the macro has, written as a number", async () => {
    const { snapshot, knob } = withMacro();
    await rig(<MacroValueControl node={knob.id} />, { snapshot });
    const value = slider("Value");
    expect([value.getAttribute("aria-valuemin"), value.getAttribute("aria-valuemax"), value.getAttribute("aria-valuenow")]).toEqual(["0", "1", "0.25"]);
    expect(value.getAttribute("aria-valuetext")).toBe("0.25");
  });

  test("shows what it is dragged to at once and sends it as the macro's value on the frame, once", async () => {
    const { snapshot, knob } = withMacro();
    const { requests, clock } = await rig(<MacroValueControl node={knob.id} />, { snapshot });
    // (A drag of 50 pixels is a quarter of the range: 0.25 and a quarter is half.)
    drag(slider("Value"), 50);
    expect(Number(slider("Value").getAttribute("aria-valuenow"))).toBeCloseTo(0.5, 9);
    await settle();
    expect(requests()).toEqual([]); // not before the frame
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toEqual({ type: "setModulatorField", node: knob.id, field: "value", value: expect.closeTo(0.5, 9) as unknown });
  });

  test("goes back to nothing, which is what a new macro has, on a double click", async () => {
    const { snapshot, knob } = withMacro();
    const { requests } = await rig(<MacroValueControl node={knob.id} />, { snapshot });
    fireEvent.doubleClick(slider("Value"));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setModulatorField", node: knob.id, field: "value", value: 0 } }]);
    });
  });

  test("sends a key press at once, a hundredth of the range on", async () => {
    const { snapshot, knob } = withMacro();
    const { requests } = await rig(<MacroValueControl node={knob.id} />, { snapshot });
    fireEvent.keyDown(slider("Value"), { key: "ArrowUp" });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toEqual({ type: "setModulatorField", node: knob.id, field: "value", value: expect.closeTo(0.26, 9) as unknown });
  });

  test("follows a change somebody else makes (the TUI, another browser)", async () => {
    const { snapshot, knob } = withMacro();
    const { server } = await rig(<MacroValueControl node={knob.id} />, { snapshot });
    act(() => {
      server.apply({ type: "setModulatorField", node: knob.id, field: "value", value: 0.9 });
    });
    await waitFor(() => {
      expect(slider("Value").getAttribute("aria-valuenow")).toBe("0.9");
    });
  });

  test("shows nothing for a node that has no value: an LFO, a node that is not a modulator, or one that is not there", async () => {
    const { view } = await rig(
      <div data-testid="where">
        <MacroValueControl node={lfo.id} />
        <MacroValueControl node={filter.id} />
        <MacroValueControl node={4242} />
      </div>,
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });
});

/** The tracer with a step sequencer in it: four steps, at 6 steps a second, a glide of a quarter, and a note does not start it again. */
function withSequencer(): { snapshot: Snapshot; sequencer: Node } {
  const values = [0.5, -0.25, 1, -1, ...Array<number>(12).fill(0)];
  const sequencer: Node = { id: 92, parent: 0, name: "Steps", kind: "modulator", source: { type: "steps", values, length: 4, rate: 6, glide: 0.25, retrigger: false, sync: 0 } };
  const snapshot: Snapshot = structuredClone(tracerSnapshot);
  snapshot.graph.nodes.push(sequencer);
  return { snapshot, sequencer };
}

describe("the glide and the length of a step sequencer", () => {
  test("are knobs over the range the controller clamps to, at the values the step sequencer has, a length written as a whole number", async () => {
    const { snapshot, sequencer } = withSequencer();
    await rig(
      <>
        <ModulatorValueControl node={sequencer.id} field="glide" />
        <ModulatorValueControl node={sequencer.id} field="length" />
      </>,
      { snapshot },
    );
    const glide = slider("Glide");
    expect([glide.getAttribute("aria-valuemin"), glide.getAttribute("aria-valuemax"), glide.getAttribute("aria-valuenow"), glide.getAttribute("aria-valuetext")]).toEqual(["0", "1", "0.25", "0.25"]);
    const length = slider("Length");
    expect([length.getAttribute("aria-valuemin"), length.getAttribute("aria-valuemax"), length.getAttribute("aria-valuenow"), length.getAttribute("aria-valuetext")]).toEqual(["1", "16", "4", "4"]);
  });

  test("send what they are dragged to as the setting of the step sequencer, on the frame, once", async () => {
    const { snapshot, sequencer } = withSequencer();
    const { requests, clock } = await rig(<ModulatorValueControl node={sequencer.id} field="glide" />, { snapshot });
    // (A drag of 100 pixels is half the range: a quarter and a half is three quarters.)
    drag(slider("Glide"), 100);
    expect(Number(slider("Glide").getAttribute("aria-valuenow"))).toBeCloseTo(0.75, 9);
    await settle();
    expect(requests()).toEqual([]);
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toEqual({ type: "setModulatorField", node: sequencer.id, field: "glide", value: expect.closeTo(0.75, 9) as unknown });
  });

  test("go back to what a new step sequencer has on a double click: no glide, and all sixteen steps", async () => {
    const { snapshot, sequencer } = withSequencer();
    const { requests } = await rig(
      <>
        <ModulatorValueControl node={sequencer.id} field="glide" />
        <ModulatorValueControl node={sequencer.id} field="length" />
      </>,
      { snapshot },
    );
    fireEvent.doubleClick(slider("Glide"));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setModulatorField", node: sequencer.id, field: "glide", value: 0 } }]);
    });
    fireEvent.doubleClick(slider("Length"));
    await waitFor(() => {
      expect(requests()).toHaveLength(2);
    });
    expect(requests()[1]).toEqual({ intent: { type: "setModulatorField", node: sequencer.id, field: "length", value: 16 } });
  });

  test("follow a change somebody else makes, and show nothing for a node that has none: an LFO, a node that is not a modulator, or one that is not there", async () => {
    const { snapshot, sequencer } = withSequencer();
    const { server, view } = await rig(
      <>
        <ModulatorValueControl node={sequencer.id} field="length" />
        <div data-testid="where">
          <ModulatorValueControl node={lfo.id} field="glide" />
          <ModulatorValueControl node={filter.id} field="length" />
          <ModulatorValueControl node={4242} field="glide" />
        </div>
      </>,
      { snapshot },
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
    act(() => {
      server.apply({ type: "setModulatorField", node: sequencer.id, field: "length", value: 9 });
    });
    await waitFor(() => {
      expect(slider("Length").getAttribute("aria-valuenow")).toBe("9");
    });
  });
});

describe("whether a note starts a step sequencer again", () => {
  test("is a choice of off and on, with the one that the step sequencer has chosen", async () => {
    const { snapshot, sequencer } = withSequencer();
    await rig(<RetriggerControl node={sequencer.id} />, { snapshot });
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Retrigger" });
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(["off", "on"]);
    expect(select.value).toBe("0");
  });

  test("asks for the switch to be turned on or off, as 1 or 0, and follows a change somebody else makes", async () => {
    const { snapshot, sequencer } = withSequencer();
    const { requests, server } = await rig(<RetriggerControl node={sequencer.id} />, { snapshot });
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Retrigger" });
    fireEvent.change(select, { target: { value: "1" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setModulatorField", node: sequencer.id, field: "retrigger", value: 1 } }]);
    });
    act(() => {
      server.apply({ type: "setModulatorField", node: sequencer.id, field: "retrigger", value: 0 });
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Retrigger" }).value).toBe("0");
    });
  });

  test("is the switch of an LFO too, which is its key's: off for one that runs free, and turned on by a request", async () => {
    const { requests } = await rig(<RetriggerControl node={lfo.id} />);
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Retrigger" });
    expect(select.value).toBe("0");
    fireEvent.change(select, { target: { value: "1" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setModulatorField", node: lfo.id, field: "retrigger", value: 1 } }]);
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Retrigger" }).value).toBe("1");
    });
  });

  test("shows nothing for a node that has no switch", async () => {
    const { view } = await rig(
      <div data-testid="where">
        <RetriggerControl node={filter.id} />
        <RetriggerControl node={4242} />
      </div>,
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });
});

describe("the note length that an LFO or a step sequencer follows the tempo by", () => {
  const names = ["free", "4 bars", "2 bars", "1 bar", "1/2 dotted", "1/2", "1/2 triplet", "1/4 dotted", "1/4", "1/4 triplet", "1/8 dotted", "1/8", "1/8 triplet", "1/16 dotted", "1/16", "1/16 triplet", "1/32"];

  test("is a choice of free and each of the note lengths, in the order of the list, with the one that the LFO has chosen", async () => {
    await rig(<SyncControl node={lfo.id} />);
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Sync" });
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(names);
    expect(select.value, "an LFO that is new runs at its own rate").toBe("0");
  });

  test("asks for a place in the list, as a number from 0 for none, and follows a change somebody else makes", async () => {
    const { requests, server } = await rig(<SyncControl node={lfo.id} />);
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Sync" });
    fireEvent.change(select, { target: { value: "11" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setModulatorField", node: lfo.id, field: "sync", value: 11 } }]);
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Sync" }).selectedOptions[0]?.textContent).toBe("1/8");
    });
    act(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "sync", value: 0 });
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Sync" }).value).toBe("0");
    });
  });

  test("is the choice of a step sequencer too, which starts at the one that it has", async () => {
    const { snapshot, sequencer } = withSequencer();
    const synced: Node = { id: sequencer.id, parent: sequencer.parent, name: sequencer.name, kind: "modulator", source: { type: "steps", values: [0.5, -0.25, 1, -1, ...Array<number>(12).fill(0)], length: 4, rate: 6, glide: 0.25, retrigger: false, sync: 14 } };
    snapshot.graph.nodes = snapshot.graph.nodes.map((node) => (node.id === sequencer.id ? synced : node));
    const { requests } = await rig(<SyncControl node={sequencer.id} />, { snapshot });
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Sync" });
    expect(select.selectedOptions[0]?.textContent).toBe("1/16");
    fireEvent.change(select, { target: { value: "0" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setModulatorField", node: sequencer.id, field: "sync", value: 0 } }]);
    });
    // (And the server took it: the choice stays at free when what it says has come.)
    await waitFor(() => {
      expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Sync" }).selectedOptions[0]?.textContent).toBe("free");
    });
  });

  test("shows nothing for a node that has no rate to follow the tempo by", async () => {
    const { view } = await rig(
      <div data-testid="where">
        <SyncControl node={filter.id} />
        <SyncControl node={4242} />
      </div>,
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });
});

describe("the rate of a modulator that follows the tempo", () => {
  /** The tracer, with its LFO following the tempo by the place `sync` in the list of note lengths (an eighth note is 11), at `bpm` beats a minute. Its own rate is 0.5. */
  const following = (sync: number, bpm = 120): Snapshot => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes = snapshot.graph.nodes.map((node) => (node.id === lfo.id && node.kind === "modulator" && node.source.type === "lfo" ? { ...node, source: { ...node.source, sync } } : node));
    snapshot.meta.bpm = bpm;
    return snapshot;
  };
  const rate = () => slider("Rate");

  test("is the knob of the rate that the tempo makes of the note length, and cannot be turned: an eighth note at 120 beats a minute is 4 Hz, and a knob that is dragged sends nothing", async () => {
    const { requests, clock } = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />, { snapshot: following(11) });
    expect(rate().getAttribute("aria-valuenow")).toBe("4");
    expect(rate().getAttribute("aria-valuetext")).toBe("4 Hz");
    expect(rate().getAttribute("aria-disabled")).toBe("true");
    drag(rate(), 50);
    act(() => {
      clock.tick(16);
    });
    await settle();
    expect(requests()).toEqual([]);
    expect(rate().getAttribute("aria-valuenow")).toBe("4");
  });

  test("goes by the tempo as it changes: half the tempo, half the rate", async () => {
    const { server } = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />, { snapshot: following(11) });
    act(() => {
      server.apply({ type: "setBpm", bpm: 60 });
    });
    await waitFor(() => {
      expect(rate().getAttribute("aria-valuenow")).toBe("2");
    });
  });

  test("is the knob of its own rate, which it kept, as soon as it is free again, and can be turned", async () => {
    const { server, requests, clock } = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />, { snapshot: following(11) });
    act(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "sync", value: 0 });
    });
    await waitFor(() => {
      expect(rate().getAttribute("aria-valuenow")).toBe("0.5");
    });
    expect(rate().getAttribute("aria-disabled")).toBeNull();
    drag(rate(), 50);
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toMatchObject({ type: "setModulatorField", node: lfo.id, field: "rate" });
  });

  test("is an ordinary knob for a modulator that is free, at its own rate", async () => {
    await rig(<ModulatorFieldControl node={lfo.id} field="rate" />);
    expect(rate().getAttribute("aria-valuenow")).toBe("0.5");
    expect(rate().getAttribute("aria-disabled")).toBeNull();
  });
});

describe("the bars of a step sequencer", () => {
  /** The bars of a node (they have no layout in jsdom: a box 160 wide and 64 tall is what they are told to be in), and what they show. */
  const barsOf = (name = "Steps of Steps") => {
    const group = screen.getByRole("group", { name });
    group.getBoundingClientRect = () => ({ left: 0, top: 0, x: 0, y: 0, width: 160, height: 64, right: 160, bottom: 64, toJSON: () => ({}) });
    return group;
  };
  const shown = (): number[] => within(barsOf()).getAllByRole("slider").map((bar) => Number(bar.getAttribute("aria-valuenow")));
  const SEQUENCER_STEPS = [0.5, -0.25, 1, -1, ...Array<number>(12).fill(0)];

  test("show the steps that the step sequencer has, from the first, and dim those past its length", async () => {
    const { snapshot, sequencer } = withSequencer();
    await rig(<StepsControl node={sequencer.id} name="Steps" />, { snapshot });
    expect(shown()).toEqual(SEQUENCER_STEPS);
    const unused = within(barsOf()).getAllByRole("slider").flatMap((bar, index) => (bar.hasAttribute("data-unused") ? [index + 1] : []));
    expect(unused).toEqual([5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
  });

  test("send a stroke across them as the steps of the step sequencer, on the frame, once, and show what the stroke has painted", async () => {
    const { snapshot, sequencer } = withSequencer();
    const { requests, clock } = await rig(<StepsControl node={sequencer.id} name="Steps" />, { snapshot });
    const group = barsOf();
    // Press on the first bar at the top and go across to the fourth at the bottom: ten pixels to a bar, a bar of 64 pixels.
    fireEvent.pointerDown(group, { clientX: 5, clientY: 0, button: 0, pointerId: 1 });
    fireEvent.pointerMove(group, { clientX: 35, clientY: 64, pointerId: 1 });
    expect(shown().slice(0, 5)).toEqual([1, 0.33, -0.33, -1, 0]);
    await settle();
    expect(requests()).toEqual([]);
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toEqual({ type: "setSteps", node: sequencer.id, values: [1, 0.33, -0.33, -1, ...Array<number>(12).fill(0)] });
    fireEvent.pointerUp(group, { pointerId: 1 });
    await waitFor(() => {
      expect(shown().slice(0, 4)).toEqual([1, 0.33, -0.33, -1]);
    });
  });

  test("send a key press as the steps, at once: it is a gesture of its own", async () => {
    const { snapshot, sequencer } = withSequencer();
    const { requests } = await rig(<StepsControl node={sequencer.id} name="Steps" />, { snapshot });
    fireEvent.keyDown(screen.getByRole("slider", { name: "Step 2" }), { key: "ArrowUp" });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toEqual({ type: "setSteps", node: sequencer.id, values: [0.5, expect.closeTo(-0.23, 9), 1, -1, ...Array<number>(12).fill(0)] });
  });

  test("follow a change somebody else makes, and of the length", async () => {
    const { snapshot, sequencer } = withSequencer();
    const { server } = await rig(<StepsControl node={sequencer.id} name="Steps" />, { snapshot });
    act(() => {
      server.apply({ type: "setSteps", node: sequencer.id, values: Array.from({ length: 16 }, (_, step) => (step % 2 === 0 ? 0.5 : -0.5)) });
      server.apply({ type: "setModulatorField", node: sequencer.id, field: "length", value: 2 });
    });
    await waitFor(() => {
      expect(shown().slice(0, 3)).toEqual([0.5, -0.5, 0.5]);
    });
    const unused = within(barsOf()).getAllByRole("slider").filter((bar) => bar.hasAttribute("data-unused"));
    expect(unused).toHaveLength(14);
  });

  test("show nothing for a node that has no steps: an LFO, a node that is not a modulator, or one that is not there", async () => {
    const { view } = await rig(
      <div data-testid="where">
        <StepsControl node={lfo.id} name="LFO" />
        <StepsControl node={filter.id} name="Filter" />
        <StepsControl node={4242} name="Nothing" />
      </div>,
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });

  test("have a playhead on the step that the engine says the step sequencer is at, which follows it", async () => {
    const { snapshot, sequencer } = withSequencer();
    // (The session says where its telemetry is: the output of the step sequencer and its position, in slots after the others.)
    const slot = Math.max(...snapshot.telemetry.modulation.map((entry) => entry.slot)) + 1;
    snapshot.telemetry.modulation.unshift(
      { slot, node: sequencer.id, port: { type: "modOut" }, kind: "modOutput" },
      { slot: slot + 1, node: sequencer.id, port: { type: "modOut" }, kind: "modPosition" },
    );
    const { clock, time } = await rig(<StepsControl node={sequencer.id} name="Steps" />, { snapshot });
    const playing = (): number[] => within(barsOf()).getAllByRole("slider").flatMap((bar, index) => (bar.hasAttribute("data-playing") ? [index + 1] : []));
    // Six steps a second, of four in use: 0.45 s is 2.7 steps, in the third.
    time.seconds = 0.45;
    await waitFor(() => {
      act(() => {
        clock.tick(16);
      });
      expect(playing()).toEqual([3]);
    });
    // 0.7 s is 4.2 steps, which is round again and 0.2 into the first.
    time.seconds = 0.7;
    await waitFor(() => {
      act(() => {
        clock.tick(33);
      });
      expect(playing()).toEqual([1]);
    });
  });
});

describe("a number of a modulator", () => {
  test("is a knob over the range the controller clamps to, at the value the modulator has, in its unit", async () => {
    await rig(<ModulatorFieldControl node={lfo.id} field="rate" />);
    const rate = slider("Rate");
    expect([rate.getAttribute("aria-valuemin"), rate.getAttribute("aria-valuemax"), rate.getAttribute("aria-valuenow")]).toEqual(["0.01", "50", "0.5"]);
    expect(rate.getAttribute("aria-valuetext")).toBe("0.5 Hz");
  });

  test("is one of the four stages of an envelope, each called what it is, a time written in milliseconds or seconds", async () => {
    const { snapshot, envelope } = withEnvelope();
    await rig(
      <div data-testid="stages">
        {(["attack", "decay", "sustain", "release"] as const).map((field) => (
          <ModulatorFieldControl key={field} node={envelope.id} field={field} />
        ))}
      </div>,
      { snapshot },
    );
    const stages = ["Attack", "Decay", "Sustain", "Release"].map((name) => slider(name));
    expect(stages.map((stage) => stage.getAttribute("aria-valuetext"))).toEqual(["10 ms", "300 ms", "0.7", "2 s"]);
    expect(stages.map((stage) => [stage.getAttribute("aria-valuemin"), stage.getAttribute("aria-valuemax")])).toEqual([
      ["0.001", "10"],
      ["0.001", "10"],
      ["0", "1"],
      ["0.001", "10"],
    ]);
  });

  test("turns through the decades for a rate and a time, and evenly for a level", async () => {
    const { snapshot, envelope } = withEnvelope();
    const { requests, clock } = await rig(
      <>
        <ModulatorFieldControl node={lfo.id} field="rate" />
        <ModulatorFieldControl node={envelope.id} field="sustain" />
      </>,
      { snapshot },
    );
    drag(slider("Rate"), 50);
    expect(Number(slider("Rate").getAttribute("aria-valuenow"))).toBeCloseTo(dragged(0.5, -50, 0.01, 50, 1, "log"), 9);
    // A quarter of the sweep on from 0.5, which is not far from the middle of the decades: about 4 Hz, not the 13 of a quarter of the range.
    expect(Number(slider("Rate").getAttribute("aria-valuenow"))).toBeGreaterThan(4);
    expect(Number(slider("Rate").getAttribute("aria-valuenow"))).toBeLessThan(5);
    drag(slider("Sustain"), 50);
    expect(Number(slider("Sustain").getAttribute("aria-valuenow"))).toBe(0.95);
    // (The times of an envelope run over decades as a rate does: 10 ms is nearly a third of the way up from a millisecond to ten seconds.)
    const times = withEnvelope();
    await rig(
      <>
        {(["attack", "decay", "release"] as const).map((field) => (
          <ModulatorFieldControl key={field} node={times.envelope.id} field={field} />
        ))}
      </>,
      { snapshot: times.snapshot },
    );
    for (const [name, from] of [["Attack", 0.01], ["Decay", 0.3], ["Release", 2]] as const) {
      const [knob] = screen.getAllByRole("slider", { name });
      if (!knob) throw new Error(`no ${name}`);
      drag(knob, 20);
      expect(Number(knob.getAttribute("aria-valuenow")), name).toBeCloseTo(dragged(from, -20, 0.001, 10, 1, "log"), 9);
      fireEvent.pointerUp(knob, { pointerId: 1 });
    }
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(2);
    });
  });

  test("is sent as a setting of the modulator, on the frame, once, and shown meanwhile", async () => {
    const { requests, clock } = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />);
    drag(slider("Rate"), 50);
    await settle();
    expect(requests()).toEqual([]); // not before the frame
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toEqual({ type: "setModulatorField", node: lfo.id, field: "rate", value: expect.closeTo(dragged(0.5, -50, 0.01, 50, 1, "log"), 9) as unknown });
  });

  test("goes back to what a new modulator has on a double click", async () => {
    const { snapshot, envelope } = withEnvelope();
    const { requests } = await rig(
      <>
        <ModulatorFieldControl node={lfo.id} field="rate" />
        <ModulatorFieldControl node={envelope.id} field="release" />
      </>,
      { snapshot },
    );
    fireEvent.doubleClick(slider("Rate"));
    fireEvent.doubleClick(slider("Release"));
    await waitFor(() => {
      expect(requests()).toEqual([
        { intent: { type: "setModulatorField", node: lfo.id, field: "rate", value: 1 } },
        { intent: { type: "setModulatorField", node: envelope.id, field: "release", value: 0.5 } },
      ]);
    });
  });

  test("sends a key press at once, a hundredth of the sweep on", async () => {
    const { requests } = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />);
    fireEvent.keyDown(slider("Rate"), { key: "ArrowUp" });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toEqual({ type: "setModulatorField", node: lfo.id, field: "rate", value: expect.closeTo(0.5 * 5000 ** 0.01, 9) as unknown });
  });

  test("follows a change somebody else makes (the TUI, another browser)", async () => {
    const { server } = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />);
    act(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "rate", value: 7 });
    });
    await waitFor(() => {
      expect(slider("Rate").getAttribute("aria-valuenow")).toBe("7");
    });
  });

  test("shows nothing for a setting the modulator has not, or a node that is not one", async () => {
    const { view } = await rig(
      <div data-testid="where">
        <ModulatorFieldControl node={lfo.id} field="attack" />
        <ModulatorFieldControl node={filter.id} field="rate" />
        <ModulatorFieldControl node={4242} field="rate" />
      </div>,
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });

  test("is not rendered again for a change of another setting, or of another node", async () => {
    let renders = 0;
    const { snapshot, envelope } = withEnvelope();
    const { server, client } = await rig(
      <Profiler id="rate" onRender={() => (renders += 1)}>
        <ModulatorFieldControl node={lfo.id} field="rate" />
      </Profiler>,
      { snapshot },
    );
    const settled = renders;
    const rev = client.state.snapshot?.rev ?? 0;
    act(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "waveform", value: 2 });
      server.apply({ type: "setModulatorField", node: envelope.id, field: "attack", value: 0.5 });
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.3 });
    });
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBe(rev + 3);
    });
    expect(renders).toBe(settled);
    act(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "rate", value: 3 });
    });
    await waitFor(() => {
      expect(renders).toBeGreaterThan(settled);
    });
  });
});

describe("the modulation of a number of a modulator", () => {
  const cableTo = (from: number, to: number, port: Port, amount: number, id: number): Edge => ({
    kind: "modulation",
    id,
    amount,
    from: { node: from, port: { type: "modOut" } },
    to: { node: to, port },
  });
  const ringOf = (root: HTMLElement) => root.querySelector(".tang-knob__ring")?.getAttribute("d");
  /** The rate of the tracer's LFO is 0.5 Hz, over 0.01 to 50 and a knob of decades. */
  const rateFraction = (hertz: number) => fractionOf(hertz, 0.01, 50, "log");
  /** The same, of a second LFO and of a control that is not bound, which can drive it as well. */
  const second: Node = { id: 91, parent: 0, name: "LFO 2", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 } };
  const unbound: Node = { id: 92, parent: 0, name: "Control", kind: "modulator", source: { type: "midiLearn", control: null, learning: true } };
  /** The tracer with an envelope (90), an LFO (91) and a control (92) that can be given cables. */
  const crowded = (cables: Edge[]): Snapshot => {
    const { snapshot } = withEnvelope();
    snapshot.graph.nodes.push(second, unbound);
    snapshot.graph.edges.push(...cables);
    return snapshot;
  };
  /** Turn frames by hand, with real time between them for the mock's telemetry to come (not inside `waitFor`: see the envelope's test of its dot). */
  const until = async (clock: { tick: (time: number) => void }, condition: () => boolean, what: string): Promise<void> => {
    const deadline = Date.now() + 2000;
    while (!condition()) {
      if (Date.now() > deadline) throw new Error(`gave up waiting for ${what}`);
      await new Promise((resolve) => setTimeout(resolve, 10));
      act(() => {
        clock.tick(performance.now());
      });
    }
  };
  const near = (a: number, b: number): boolean => Math.abs(a - b) < 1e-3;

  test("is a ring around the knob from where the cable that ends at it can take it, by the travel of the knob, which is on decades", async () => {
    // The envelope is at its peak at the 0.5 Hz the user set, and the cable at half depth pulls it down half the way down the knob to the lowest, 0.01 Hz, when the envelope is at rest.
    const { view } = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />, { snapshot: crowded([cableTo(90, lfo.id, { type: "rate" }, 0.5, 9)]) });
    expect(ringOf(view.container)).toBe(arcPath(24, 21.5, 0.5 * rateFraction(0.5), rateFraction(0.5)));
  });

  test("is that of the cable that is last, as the engine writes the cables one after another and the last stands, whichever the others", async () => {
    const lfoLast = crowded([cableTo(90, lfo.id, { type: "rate" }, 0.5, 9), cableTo(91, lfo.id, { type: "rate" }, 0.1, 10)]);
    const first = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />, { snapshot: lfoLast });
    // The LFO's: ±0.1 of the knob's travel around the place of the 0.5 Hz, as far up as down.
    expect(ringOf(first.view.container)).toBe(arcPath(24, 21.5, rateFraction(0.5) - 0.1, rateFraction(0.5) + 0.1));
    cleanup();
    const envelopeLast = crowded([cableTo(91, lfo.id, { type: "rate" }, 0.1, 9), cableTo(90, lfo.id, { type: "rate" }, 0.5, 10)]);
    const second = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />, { snapshot: envelopeLast });
    expect(ringOf(second.view.container)).toBe(arcPath(24, 21.5, 0.5 * rateFraction(0.5), rateFraction(0.5)));
  });

  test("is not there for a setting that nothing drives, or one whose last cable is a control that is not bound, which writes the set value over the others", async () => {
    const free = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />, { snapshot: crowded([cableTo(90, lfo.id, { type: "attack" }, 0.5, 9)]) });
    expect(ringOf(free.view.container)).toBeUndefined();
    cleanup();
    const bound = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />, { snapshot: crowded([cableTo(90, lfo.id, { type: "rate" }, 0.5, 9), cableTo(92, lfo.id, { type: "rate" }, 0.5, 10)]) });
    expect(ringOf(bound.view.container)).toBeUndefined();
  });

  test("is the ring of a stage of an envelope by that stage's own law: a level moves a fraction of its range, a time a fraction of the knob's travel on decades", async () => {
    // The LFO drives the sustain (0.7, over 0 to 1) at 0.2 and the release (2 s, over 0.001 to 10) at 0.1.
    const snapshot = crowded([cableTo(91, 90, { type: "sustain" }, 0.2, 9), cableTo(91, 90, { type: "release" }, 0.1, 10)]);
    const { view } = await rig(
      <>
        <ModulatorFieldControl node={90} field="sustain" />
        <ModulatorFieldControl node={90} field="release" />
      </>,
      { snapshot },
    );
    const rings = [...view.container.querySelectorAll(".tang-knob__ring")].map((ring) => ring.getAttribute("d"));
    const release = fractionOf(2, 0.001, 10, "log");
    expect(rings).toEqual([arcPath(24, 21.5, 0.5, 0.9), arcPath(24, 21.5, release - 0.1, release + 0.1)]);
  });

  test("moves with the value the user sets", async () => {
    const { view } = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />, { snapshot: crowded([cableTo(90, lfo.id, { type: "rate" }, 0.5, 9)]) });
    drag(slider("Rate"), 50);
    const set = Number(slider("Rate").getAttribute("aria-valuenow"));
    expect(set).toBeGreaterThan(0.5);
    expect(ringOf(view.container)).toBe(arcPath(24, 21.5, 0.5 * rateFraction(set), rateFraction(set)));
  });

  test("is the dot where the engine says the setting is, taken from the telemetry on the frame, and follows what the user sets", async () => {
    const { view, client, clock, server } = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />, { snapshot: withEnvelope().snapshot });
    // An envelope at rest, drawn to the rate: half the way down the knob from the place of the 0.5 Hz the user set to the lowest, 0.01 Hz.
    await client.request({ type: "addModEdge", modulator: 90, to: { node: lfo.id, port: { type: "rate" } } });
    const at = () => ({ x: dotOf(view.container).x, y: dotOf(view.container).y });
    const rest = pointAt(24, 18, angleOf(0.5 * rateFraction(0.5)));
    await until(clock, () => dotOf(view.container).visible && near(at().x, rest.x) && near(at().y, rest.y), "the dot half way down the knob from 0.5 Hz");
    // And where it goes when the user sets another rate: 4 Hz is pulled half the way down the knob from its own place.
    act(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "rate", value: 4 });
    });
    const moved = pointAt(24, 18, angleOf(0.5 * rateFraction(4)));
    await until(clock, () => near(at().x, moved.x) && near(at().y, moved.y), "the dot half way down the knob from 4 Hz");
  });

  test("is the dot of a stage of an envelope, where the LFO that drives it puts it: the user's value and what its peak adds of the knob's travel", async () => {
    const { view, client, clock, time } = await rig(<ModulatorFieldControl node={90} field="attack" />, { snapshot: withEnvelope().snapshot });
    await client.request({ type: "addModEdge", modulator: lfo.id, to: { node: 90, port: { type: "attack" } } });
    time.seconds = 0.5; // the LFO (0.5 Hz) is at its peak: the place of 0.01 s on the knob and 0.5 of the travel more
    const peak = pointAt(24, 18, angleOf(fractionOf(0.01, 0.001, 10, "log") + 0.5));
    await until(clock, () => dotOf(view.container).visible && near(dotOf(view.container).x, peak.x) && near(dotOf(view.container).y, peak.y), "the dot at the peak");
  });

  test("has no dot for a setting that nothing drives", async () => {
    const { view, client, clock } = await rig(<ModulatorFieldControl node={lfo.id} field="rate" />);
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    act(() => {
      clock.tick(100);
    });
    expect(dotOf(view.container).visible).toBe(false);
  });

  test("does not render the control again for the telemetry, however many frames come, but does for a depth that changes the ring", async () => {
    let renders = 0;
    const { server, client, clock, time } = await rig(
      <Profiler id="rate" onRender={() => (renders += 1)}>
        <ModulatorFieldControl node={lfo.id} field="rate" />
      </Profiler>,
      { snapshot: crowded([cableTo(91, lfo.id, { type: "rate" }, 0.1, 9)]) },
    );
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    const settled = renders;
    for (let frame = 1; frame <= 40; frame += 1) {
      time.seconds = frame / 40;
      await new Promise((resolve) => setTimeout(resolve, 6)); // the engine makes a frame every 5 ms
      act(() => {
        clock.tick(frame * 16);
      });
    }
    expect(client.telemetry.frames).toBeGreaterThan(10);
    expect(renders).toBe(settled);
    act(() => {
      server.apply({ type: "setEdgeDepth", edge: 9, depth: 0.3 });
    });
    await waitFor(() => {
      expect(renders).toBeGreaterThan(settled);
    });
  });
});

describe("the depth of a cable that another cable drives", () => {
  const name = "LFO to cutoff";
  const meter = () => screen.queryByRole("meter", { name: `Depth of ${name}, as modulated` });
  /** The cable 0 of the tracer, which the LFO draws to the filter's cutoff at 0.4, and an envelope (90) that can drive how deep it goes. */
  const rigged = () => rig(<CableDepthControl edge={0} label={name} />, { snapshot: withEnvelope().snapshot });
  const until = async (clock: { tick: (time: number) => void }, condition: () => boolean, what: string): Promise<void> => {
    const deadline = Date.now() + 2000;
    while (!condition()) {
      if (Date.now() > deadline) throw new Error(`gave up waiting for ${what}`);
      await new Promise((resolve) => setTimeout(resolve, 10));
      act(() => {
        clock.tick(performance.now());
      });
    }
  };

  test("is a slider at the depth the user set, and has nothing that says what the engine makes of it while nothing drives it", async () => {
    await rigged();
    expect(slider(`Depth of ${name}`).getAttribute("aria-valuetext")).toBe("40%");
    expect(meter()).toBeNull();
  });

  test("has a bar under the slider that says the depth the engine has made of it, from the middle, once a cable drives it (f118)", async () => {
    const { client, clock } = await rigged();
    // The envelope at rest pulls it down half the way from the 0.4 the user set to the lowest depth, -1: to -0.3.
    await client.request({ type: "addModEdge", modulator: 90, to: { node: lfo.id, port: { type: "depth", edge: 0 } } });
    const bar = await screen.findByRole("meter", { name: `Depth of ${name}, as modulated` });
    expect([bar.getAttribute("aria-valuemin"), bar.getAttribute("aria-valuemax"), bar.hasAttribute("data-centred")]).toEqual(["-1", "1", true]);
    await until(clock, () => Math.abs(Number(bar.getAttribute("aria-valuenow")) + 0.3) < 1e-3, "the depth the engine has made of it");
    expect(bar.getAttribute("aria-valuetext")).toBe("-30%");
    // The slider is still the user's: 40%.
    expect(slider(`Depth of ${name}`).getAttribute("aria-valuetext")).toBe("40%");
  });

  test("is the bar of the cable whose depth is driven and of no other: each cable's depth is its own setting, named by the cable", async () => {
    // A second cable of the LFO, to the resonance (9), and the envelope drives how deep that one goes.
    const { snapshot } = withEnvelope();
    snapshot.graph.edges.push({ kind: "modulation", id: 9, amount: 0.2, from: { node: lfo.id, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 1 } } });
    const { client, clock } = await rig(
      <>
        <CableDepthControl edge={0} label="first" />
        <CableDepthControl edge={9} label="second" />
      </>,
      { snapshot },
    );
    await client.request({ type: "addModEdge", modulator: 90, to: { node: lfo.id, port: { type: "depth", edge: 9 } } });
    const bar = await screen.findByRole("meter", { name: "Depth of second, as modulated" });
    expect(screen.queryByRole("meter", { name: "Depth of first, as modulated" })).toBeNull();
    // The envelope at rest pulls the 0.2 the user set half the way down to -1: -0.4.
    await until(clock, () => Math.abs(Number(bar.getAttribute("aria-valuenow")) + 0.4) < 1e-3, "the depth of the second cable");
  });

  test("follows the depth the user sets", async () => {
    const { client, clock, server } = await rigged();
    await client.request({ type: "addModEdge", modulator: 90, to: { node: lfo.id, port: { type: "depth", edge: 0 } } });
    const bar = await screen.findByRole("meter", { name: `Depth of ${name}, as modulated` });
    act(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: 0.8 });
    });
    // 0.8 less half the way down to -1: -0.1.
    await until(clock, () => Math.abs(Number(bar.getAttribute("aria-valuenow")) + 0.1) < 1e-3, "the depth for the new value");
  });

  test("goes when the cable that drove it is taken out", async () => {
    const { client } = await rigged();
    await client.request({ type: "addModEdge", modulator: 90, to: { node: lfo.id, port: { type: "depth", edge: 0 } } });
    await screen.findByRole("meter", { name: `Depth of ${name}, as modulated` });
    const driving = client.state.snapshot?.graph.edges.findLast((e) => e.kind === "modulation" && e.to.port.type === "depth");
    if (driving?.kind !== "modulation") throw new Error("the cable is not there");
    await client.request({ type: "removeEdge", edge: driving.id });
    await waitFor(() => {
      expect(meter()).toBeNull();
    });
  });

  test("does not render the control again for the telemetry, however many frames come", async () => {
    let renders = 0;
    const { client, clock, time } = await rig(
      <Profiler id="depth" onRender={() => (renders += 1)}>
        <CableDepthControl edge={0} label={name} />
      </Profiler>,
      { snapshot: withEnvelope().snapshot },
    );
    await client.request({ type: "addModEdge", modulator: 90, to: { node: lfo.id, port: { type: "depth", edge: 0 } } });
    await screen.findByRole("meter", { name: `Depth of ${name}, as modulated` });
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    const settled = renders;
    for (let frame = 1; frame <= 40; frame += 1) {
      time.seconds = frame / 40;
      await new Promise((resolve) => setTimeout(resolve, 6)); // the engine makes a frame every 5 ms
      act(() => {
        clock.tick(frame * 16);
      });
    }
    expect(client.telemetry.frames).toBeGreaterThan(10);
    expect(renders).toBe(settled);
  });
});

describe("the waveform of an LFO", () => {
  test("is a choice of every waveform, the random ones among them, with the one it has selected", async () => {
    await rig(<WaveformControl node={lfo.id} />);
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Wave" });
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(["sine", "triangle", "saw", "square", "random", "smooth"]);
    expect(select.value).toBe("0");
  });

  test("is sent, as the index of the one chosen, at once", async () => {
    const { requests } = await rig(<WaveformControl node={lfo.id} />);
    fireEvent.change(screen.getByRole("combobox", { name: "Wave" }), { target: { value: "3" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setModulatorField", node: lfo.id, field: "waveform", value: 3 } }]);
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Wave" }).value).toBe("3");
    });
  });

  test("follows a change somebody else makes", async () => {
    const { server } = await rig(<WaveformControl node={lfo.id} />);
    act(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "waveform", value: 2 });
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Wave" }).value).toBe("2");
    });
  });

  test("shows nothing for a modulator that has no waveform, or a node that is not one", async () => {
    const { snapshot, envelope } = withEnvelope();
    const { view } = await rig(
      <div data-testid="where">
        <WaveformControl node={envelope.id} />
        <WaveformControl node={filter.id} />
        <WaveformControl node={4242} />
      </div>,
      { snapshot },
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });
});

describe("whether two parameters are drawn the same", () => {
  const cutoff = (): Param => {
    const found = tracerSnapshot.graph.nodes.flatMap((node) => (node.kind === "effect" ? node.plugin.params : [])).find((param) => param.index === 0);
    if (!found) throw new Error("no cutoff");
    return found;
  };

  test("they are when only the value differs, or they are the same", () => {
    const base = cutoff();
    expect(sameParamFacts(base, base)).toBe(true);
    expect(sameParamFacts(base, { ...base, value: 0.9 })).toBe(true);
    expect(sameParamFacts(undefined, undefined)).toBe(true);
  });

  test("they are not when one is there and the other is not", () => {
    expect(sameParamFacts(cutoff(), undefined)).toBe(false);
    expect(sameParamFacts(undefined, cutoff())).toBe(false);
  });

  test("they are not when what is drawn differs: the index, name, range, default, unit or the names of the values", () => {
    const base = cutoff();
    const differences: Partial<Param>[] = [
      { index: 9 },
      { name: "other" },
      { min: -1 },
      { max: 2 },
      { default: 0.25 },
      { unit: "Hz" },
      { labels: ["a", "b"] },
    ];
    for (const difference of differences) {
      expect(sameParamFacts(base, { ...base, ...difference }), JSON.stringify(difference)).toBe(false);
      expect(sameParamFacts({ ...base, ...difference }, base), JSON.stringify(difference)).toBe(false);
    }
  });

  test("the names of the values are the same when they are the very same list", () => {
    const labels = ["a", "b"];
    const base = { ...cutoff(), labels };
    expect(sameParamFacts(base, { ...base, value: 1 })).toBe(true);
    expect(sameParamFacts(base, { ...base, labels: ["a", "b"] })).toBe(false); // another list, though equal: the plugin was loaded again
  });
});

describe("whether two sets of modulators are the same", () => {
  const lfo = { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 } as const;
  const envelope = { type: "envelope", attack: 0.1, decay: 0.1, sustain: 0.5, release: 0.1 } as const;
  const one: Modulation = { source: lfo, amount: 0.4 };

  test("they are when they are the same modulators at the same depths, in the same order", () => {
    expect(sameModulations([], [])).toBe(true);
    expect(sameModulations([one], [one])).toBe(true);
    expect(sameModulations([one], [{ source: lfo, amount: 0.4 }])).toBe(true);
    const two: Modulation = { source: envelope, amount: -0.2 };
    expect(sameModulations([one, two], [one, two])).toBe(true);
  });

  test("they are not when there are more or fewer", () => {
    expect(sameModulations([one], [])).toBe(false);
    expect(sameModulations([], [one])).toBe(false);
    expect(sameModulations([one], [one, one])).toBe(false);
  });

  test("they are when a modulator's settings differ, which the ring does not depend on, and not when a depth or a kind does", () => {
    expect(sameModulations([one], [{ source: { ...lfo, rate: 9, waveform: "saw" }, amount: 0.4 }])).toBe(true);
    expect(sameModulations([{ source: envelope, amount: 0.4 }], [{ source: { ...envelope, attack: 5, sustain: 1 }, amount: 0.4 }])).toBe(true);
    expect(sameModulations([one], [{ source: lfo, amount: 0.5 }])).toBe(false);
    expect(sameModulations([one], [{ source: envelope, amount: 0.4 }])).toBe(false);
  });

  test("they are not when a control is bound to another, or is bound that was not, and are when it only stops learning", () => {
    const control = (c: { type: "cc"; number: number } | { type: "pitchBend" } | null, learning = false) => ({ source: { type: "midiLearn", control: c, learning } as const, amount: 0.4 });
    expect(sameModulations([control({ type: "cc", number: 74 })], [control({ type: "cc", number: 74 })])).toBe(true);
    expect(sameModulations([control({ type: "cc", number: 74 })], [control({ type: "cc", number: 1 })])).toBe(false);
    expect(sameModulations([control({ type: "cc", number: 74 })], [control({ type: "pitchBend" })])).toBe(false);
    expect(sameModulations([control(null, true)], [control({ type: "cc", number: 74 })])).toBe(false);
    expect(sameModulations([control(null, true)], [control(null, false)])).toBe(true);
    expect(sameModulations([control(null, true)], [one])).toBe(false);
  });

  test("they are not when the order differs", () => {
    const two: Modulation = { source: envelope, amount: -0.2 };
    expect(sameModulations([one, two], [two, one])).toBe(false);
  });
});

describe("a parameter of a plugin as a row", () => {
  const row = (name: string) => screen.getByRole<HTMLInputElement>("slider", { name });

  test("is a slider named for the parameter, over its range, with its value written beside it, and its unit if it has one", async () => {
    await rig(
      <>
        <ParamRow node={filter.id} index={0} />
        <ParamRow node={oscillator.id} index={1} />
      </>,
    );
    const cutoff = row("cutoff");
    expect([cutoff.min, cutoff.max, cutoff.value, cutoff.step]).toEqual(["0", "1", "0.5", "any"]);
    expect(cutoff.getAttribute("aria-valuetext")).toBe("0.5");
    expect(screen.getByText("cutoff")).toBeTruthy();
    expect(screen.getByText("0.5")).toBeTruthy();
    const detune = row("detune");
    expect([detune.min, detune.max, detune.value]).toEqual(["-24", "24", "0"]);
    expect(detune.getAttribute("aria-valuetext")).toBe("0 st");
    expect(screen.getByText("0 st")).toBeTruthy();
  });

  test("is a choice when its values have names, with the chosen one selected, and no slider", async () => {
    await rig(<ParamRow node={oscillator.id} index={0} />);
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "waveform" });
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(["sine", "triangle", "square", "saw", "noise"]);
    expect(select.value).toBe("3");
    expect(screen.queryByRole("slider")).toBeNull();
  });

  test("shows nothing for a parameter that is not there, or a node that is not", async () => {
    const { view } = await rig(
      <div data-testid="where">
        <ParamRow node={filter.id} index={99} />
        <ParamRow node={4242} index={0} />
      </div>,
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });

  test("follows a change somebody else makes", async () => {
    const { server } = await rig(<ParamRow node={filter.id} index={0} />);
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.8 });
    });
    await waitFor(() => {
      expect(row("cutoff").value).toBe("0.8");
    });
    expect(screen.getByText("0.8")).toBeTruthy();
  });

  test("shows what it is set to at once, and sends it on the frame, once", async () => {
    const { clock, requests } = await rig(<ParamRow node={filter.id} index={0} />);
    fireEvent.change(row("cutoff"), { target: { value: "0.75" } });
    expect(row("cutoff").value).toBe("0.75");
    expect(row("cutoff").getAttribute("aria-valuetext")).toBe("0.75");
    await settle();
    expect(requests()).toEqual([]); // not before the frame
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 0, value: 0.75 } }]);
    });
  });

  test("sends where it ended at once when the gesture is over, and goes on showing it while the engine answers", async () => {
    const { requests, client } = await rig(<ParamRow node={filter.id} index={0} />);
    fireEvent.change(row("cutoff"), { target: { value: "0.3" } });
    fireEvent.pointerUp(row("cutoff"));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 0, value: 0.3 } }]);
    });
    expect(row("cutoff").value).toBe("0.3");
    await waitFor(() => {
      const node = client.state.snapshot?.graph.nodes.find((candidate) => candidate.id === filter.id);
      expect(node?.kind === "effect" && node.plugin.params[0]?.value).toBeCloseTo(0.3, 6);
    });
    expect(row("cutoff").value).toBe("0.3");
  });

  test("sends what it was set to at once when it loses the focus, which a gesture that ended off the slider leaves", async () => {
    const { requests } = await rig(<ParamRow node={filter.id} index={0} />);
    fireEvent.change(row("cutoff"), { target: { value: "0.2" } });
    fireEvent.blur(row("cutoff"));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 0, value: 0.2 } }]);
    });
  });

  test("does not let somebody else's change pull it away while it is being set", async () => {
    const { server, clock, client } = await rig(<ParamRow node={filter.id} index={0} />);
    fireEvent.change(row("cutoff"), { target: { value: "0.6" } });
    act(() => {
      clock.tick(16);
    });
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.05 });
    });
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBeGreaterThanOrEqual(2);
    });
    expect(Number(row("cutoff").value)).toBeCloseTo(0.6, 6);
  });

  test("steps by a hundredth of its range on the arrows, a thousandth with shift, a tenth on the page keys, to the ends on Home and End: each sent at once, and the browser's own step kept off", async () => {
    const { requests, clock } = await rig(<ParamRow node={filter.id} index={0} />);
    let sent = 0;
    // (One intent goes in a frame: the next press has to wait for the frame after the one before it.)
    const press = async (key: string, shiftKey = false): Promise<boolean> => {
      const left = fireEvent.keyDown(row("cutoff"), { key, shiftKey });
      act(() => {
        clock.tick(16);
      });
      sent += 1;
      await waitFor(() => {
        expect(requests()).toHaveLength(sent);
      });
      return left;
    };
    expect(await press("ArrowUp")).toBe(false); // (false: the key was kept from the browser)
    expect(row("cutoff").value).toBe("0.51");
    await press("ArrowDown", true);
    await press("PageUp");
    await press("End");
    await press("Home");
    expect(requests().map((request) => (request.intent as { value: number }).value)).toEqual([0.51, 0.509, 0.609, 1, 0]);
    expect(requests().every((request) => (request.intent as { param: number }).param === 0)).toBe(true);
  });

  test("sends a key press at once, before the frame", async () => {
    const { requests } = await rig(<ParamRow node={filter.id} index={0} />);
    fireEvent.keyDown(row("cutoff"), { key: "ArrowUp" });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 0, value: 0.51 } }]);
    });
  });

  test("has the whole name where it is too long for the row: as the tooltip of the name", async () => {
    await rig(<ParamRow node={filter.id} index={0} />);
    expect(screen.getByText("cutoff").getAttribute("title")).toBe("cutoff");
  });

  test("leaves a key that is not its own to the browser: Tab, a letter", async () => {
    const { requests } = await rig(<ParamRow node={filter.id} index={0} />);
    expect(fireEvent.keyDown(row("cutoff"), { key: "Tab" })).toBe(true);
    expect(fireEvent.keyDown(row("cutoff"), { key: "a" })).toBe(true);
    await settle();
    expect(requests()).toEqual([]);
  });

  test("goes back to its default on a double click, at once", async () => {
    const { server, requests } = await rig(<ParamRow node={filter.id} index={1} />);
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 1, value: 5 });
    });
    await waitFor(() => {
      expect(row("resonance").value).toBe("5");
    });
    fireEvent.doubleClick(row("resonance"));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 1, value: 0.707 } }]);
    });
  });

  test("does nothing on a double click at its default", async () => {
    const { requests } = await rig(<ParamRow node={filter.id} index={3} />);
    fireEvent.doubleClick(row("drive"));
    await settle();
    expect(requests()).toEqual([]);
  });

  test("sends the choice that is made at once, as the index", async () => {
    const { requests } = await rig(<ParamRow node={oscillator.id} index={0} />);
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "1" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: oscillator.id, param: 0, value: 1 } }]);
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLSelectElement>("combobox").value).toBe("1");
    });
  });

  test("has a bar of what the engine makes of it, taken from the telemetry on the frame, when a cable drives it", async () => {
    const { client, clock, time } = await rig(<ParamRow node={filter.id} index={0} />);
    const bar = screen.getByRole("meter", { name: "cutoff, as modulated" });
    time.seconds = 0.5; // the LFO is at its peak: the cutoff is 0.5 + 0.4
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      expect(Number(bar.getAttribute("aria-valuenow"))).toBeCloseTo(0.9, 3);
    });
    time.seconds = 1.5;
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      expect(Number(bar.getAttribute("aria-valuenow"))).toBeCloseTo(0.1, 3);
    });
    expect([bar.getAttribute("aria-valuemin"), bar.getAttribute("aria-valuemax")]).toEqual(["0", "1"]);
  });

  test("has no bar for a parameter that nothing drives, or one that has named values", async () => {
    await rig(
      <>
        <ParamRow node={filter.id} index={1} />
        <ParamRow node={oscillator.id} index={0} />
      </>,
    );
    expect(screen.queryByRole("meter")).toBeNull();
  });

  test("has the bar from the moment a cable comes to it, and not after the cable goes", async () => {
    const { server } = await rig(<ParamRow node={filter.id} index={1} />);
    expect(screen.queryByRole("meter")).toBeNull();
    act(() => {
      server.apply({ type: "addModEdge", modulator: lfo.id, to: { node: filter.id, port: { type: "param", index: 1 } } });
    });
    expect(await screen.findByRole("meter", { name: "resonance, as modulated" })).toBeTruthy();
    act(() => {
      server.apply({ type: "removeEdge", edge: 1 });
    });
    await waitFor(() => {
      expect(screen.queryByRole("meter")).toBeNull();
    });
  });

  test("does not render the row again for the telemetry, however many frames come", async () => {
    let renders = 0;
    const { client, clock, time } = await rig(
      <Profiler id="cutoff" onRender={() => (renders += 1)}>
        <ParamRow node={filter.id} index={0} />
      </Profiler>,
    );
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    const settled = renders;
    for (let frame = 1; frame <= 60; frame += 1) {
      time.seconds = frame / 40;
      await new Promise((resolve) => setTimeout(resolve, 6)); // the engine makes a frame every 5 ms
      act(() => {
        clock.tick(frame * 16);
      });
    }
    expect(client.telemetry.frames).toBeGreaterThan(20);
    expect(renders).toBe(settled);
  });

  test("does not render the row again when another parameter of the plugin changes, nor when the depth of its cable does", async () => {
    let renders = 0;
    const { server, client } = await rig(
      <Profiler id="cutoff" onRender={() => (renders += 1)}>
        <ParamRow node={filter.id} index={0} />
      </Profiler>,
    );
    const settled = renders;
    const rev = client.state.snapshot?.rev ?? 0;
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 1, value: 4 });
      server.apply({ type: "setParam", node: filter.id, param: 3, value: 2 });
      server.apply({ type: "setEdgeDepth", edge: 0, depth: 0.1 });
    });
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBe(rev + 3);
    });
    expect(renders).toBe(settled);
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.2 });
    });
    await waitFor(() => {
      expect(renders).toBeGreaterThan(settled);
    });
  });
});

/** The tracer with eight more parameters (indices 100 to 107, "extra 0" to "extra 7") on the oscillator, which has eight: its node shows the first eight. */
function wide(): Snapshot {
  const snapshot = structuredClone(tracerSnapshot);
  const node = snapshot.graph.nodes.find((n) => n.id === oscillator.id);
  if (node?.kind !== "instrument" || !node.plugin) throw new Error("no oscillator");
  const template = node.plugin.params[1];
  if (!template) throw new Error("no parameter");
  node.plugin.params.push(...Array.from({ length: 8 }, (_, i): Param => ({ ...template, index: 100 + i, name: `extra ${String(i)}` })));
  return snapshot;
}

describe("the pin of a row of the parameters of a plugin", () => {
  const pin = (name: string) => screen.getByRole<HTMLButtonElement>("button", { name: `Show ${name} on the node` });
  const FIRST_EIGHT = [0, 1, 2, 3, 4, 5, 6, 7];

  test("is a button named for the parameter, pressed when its node shows it: the first eight when nothing is pinned, and not the others", async () => {
    await rig(
      <>
        <ParamRow node={oscillator.id} index={1} />
        <ParamRow node={oscillator.id} index={7} />
        <ParamRow node={oscillator.id} index={100} />
      </>,
      { snapshot: wide() },
    );
    expect(pin("detune").getAttribute("aria-pressed")).toBe("true");
    expect(pin("pan").getAttribute("aria-pressed")).toBe("true");
    expect(pin("extra 0").getAttribute("aria-pressed")).toBe("false");
    expect(pin("extra 0").getAttribute("title")).toBe("Show as a knob on the node");
    // (What a person sees of it: filled when the node shows the parameter, open when it does not.)
    expect([pin("detune").textContent, pin("extra 0").textContent]).toEqual(["●", "○"]);
  });

  test("is on a choice as it is on a slider", async () => {
    await rig(<ParamRow node={oscillator.id} index={0} />, { snapshot: wide() });
    expect(pin("waveform").getAttribute("aria-pressed")).toBe("true");
    expect(screen.queryByRole("slider")).toBeNull();
  });

  test("pins the parameter when it is pressed and the node does not show it: the first eight stay and it is the ninth", async () => {
    const { requests } = await rig(<ParamRow node={oscillator.id} index={100} />, { snapshot: wide() });
    fireEvent.click(pin("extra 0"));
    await waitFor(() => {
      expect(pin("extra 0").getAttribute("aria-pressed")).toBe("true");
    });
    expect(requests()).toEqual([{ intent: { type: "setPinned", node: oscillator.id, params: [...FIRST_EIGHT, 100] } }]);
  });

  test("takes it off when it is pressed and the node shows it: the other seven stay", async () => {
    const { requests } = await rig(<ParamRow node={oscillator.id} index={3} />, { snapshot: wide() });
    fireEvent.click(pin("attack"));
    await waitFor(() => {
      expect(pin("attack").getAttribute("aria-pressed")).toBe("false");
    });
    expect(requests()).toEqual([{ intent: { type: "setPinned", node: oscillator.id, params: FIRST_EIGHT.filter((i) => i !== 3) } }]);
  });

  test("follows a pin that somebody else makes, in either direction", async () => {
    const { server } = await rig(
      <>
        <ParamRow node={oscillator.id} index={1} />
        <ParamRow node={oscillator.id} index={101} />
      </>,
      { snapshot: wide() },
    );
    act(() => {
      server.apply({ type: "setPinned", node: oscillator.id, params: [101, 5] });
    });
    await waitFor(() => {
      expect(pin("extra 1").getAttribute("aria-pressed")).toBe("true");
    });
    expect(pin("detune").getAttribute("aria-pressed")).toBe("false");
  });

  test("says why when the node cannot show another, and the press changes nothing", async () => {
    const full = wide();
    const node = full.graph.nodes.find((n) => n.id === oscillator.id);
    if (node?.kind !== "instrument" || !node.plugin) throw new Error("no oscillator");
    const template = node.plugin.params[1];
    if (!template) throw new Error("no parameter");
    node.plugin.params = Array.from({ length: 70 }, (_, i): Param => ({ ...template, index: i, name: `p${String(i)}` }));
    node.plugin.pinned = Array.from({ length: 64 }, (_, i) => i);
    const { requests, client } = await rig(<ParamRow node={oscillator.id} index={65} />, { snapshot: full });
    fireEvent.click(pin("p65"));
    await settle();
    expect(requests()).toEqual([]);
    expect(pin("p65").getAttribute("aria-pressed")).toBe("false");
    expect(client.state.snapshot?.rev).toBe(full.rev);
  });
});
