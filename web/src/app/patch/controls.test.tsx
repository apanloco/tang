// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { Profiler } from "react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { Modulation } from "../../client/modulation";
import type { Node, Param, Snapshot } from "../../protocol";
import { dragged } from "../../ui/knob-geometry";
import { angleOf, arcPath, pointAt } from "../../ui/knob-geometry";
import { rig } from "../testing";
import { GainControl, ModulatorFieldControl, ParamControl, ParamRow, sameModulations, sameParamFacts, WaveformControl } from "./controls";

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
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(["sine", "triangle", "square", "saw"]);
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
    withGroup.graph.nodes.push({ id: 77, parent: null, name: "Group 1", kind: "group", volume: 0.8, pan: -0.5 });
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

describe("the waveform of an LFO", () => {
  test("is a choice of the four, with the one it has selected", async () => {
    await rig(<WaveformControl node={lfo.id} />);
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Wave" });
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(["sine", "triangle", "saw", "square"]);
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
  const lfo = { type: "lfo", waveform: "sine", rate: 1 } as const;
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
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(["sine", "triangle", "square", "saw"]);
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
