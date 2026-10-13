// @vitest-environment jsdom
import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { MASTER, nodeNamed, tracerSnapshot } from "../../client/fixtures";
import { decodeFrame } from "../../client/telemetry";
import type { Snapshot } from "../../protocol";
import { angleOf, arcPath, pointAt } from "../../ui/knob-geometry";
import { rig } from "../testing";
import { cableWidth } from "./describe";
import { PatchView, sameGraphStructure } from "./PatchView";

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");
if (filter.kind !== "effect") throw new Error("the tracer's filter is not an effect");
const filterPlugin = filter.plugin;

/** The node of the canvas that is the picture of this node of the session. */
const nodeOf = async (id: number): Promise<HTMLElement> => {
  const found = await screen.findByTestId(`rf__node-${String(id)}`);
  return found;
};

/** The names of the controls in a node, sliders and choices, in the order they are in. */
const controlsIn = (node: HTMLElement): string[] =>
  Array.from(node.querySelectorAll('[role="slider"], select')).map(
    (control) => control.getAttribute("aria-label") ?? control.closest("label")?.querySelector(".tang-choice__label")?.textContent ?? "?",
  );

describe("the patch", () => {
  test("has a node for each node of the session, of its kind, with its name", async () => {
    await rig(<PatchView />);
    for (const [node, kind] of [
      [oscillator, "instrument"],
      [filter, "effect"],
      [lfo, "modulator"],
    ] as const) {
      const placed = await nodeOf(node.id);
      expect(placed.classList.contains(`react-flow__node-${kind}`)).toBe(true);
      expect(placed.querySelector(".tang-node")?.classList.contains(`tang-node--${kind}`)).toBe(true);
      expect(within(placed).getByText(node.name, { selector: ".tang-node__name" })).toBeTruthy();
    }
    expect(within(await nodeOf(oscillator.id)).getByText("Built-in")).toBeTruthy();
    expect(within(await nodeOf(4294967295)).getByText("Master")).toBeTruthy();
  });

  test("shows each plugin node's parameters as controls, and its own gain after them", async () => {
    await rig(<PatchView />);
    expect(controlsIn(await nodeOf(oscillator.id))).toEqual(["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "Volume"]);
    expect(controlsIn(await nodeOf(filter.id))).toEqual(["cutoff", "resonance", "type", "drive", "Mix"]);
  });

  test("has a knob for what has a scale and a choice for what has names", async () => {
    await rig(<PatchView />);
    const osc = await nodeOf(oscillator.id);
    expect(within(osc).getByRole("combobox", { name: "waveform" })).toBeTruthy();
    expect(within(osc).getAllByRole("slider")).toHaveLength(8);
    const fil = await nodeOf(filter.id);
    expect(within(fil).getByRole("combobox", { name: "type" })).toBeTruthy();
    expect(within(fil).getAllByRole("slider")).toHaveLength(4);
  });

  test("draws the cables: the audio between the nodes, and the modulation, which is thicker the deeper it is", async () => {
    const { view } = await rig(<PatchView />);
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(3);
    });
    expect(view.container.querySelectorAll(".tang-cable--audio")).toHaveLength(2);
    expect(Array.from(view.container.querySelectorAll(".tang-cable")).map((path) => path.id).sort()).toEqual(["audio:0>1", `audio:1>${String(MASTER)}`, "mod:0"]);
    const modulation = view.container.querySelectorAll<SVGPathElement>(".tang-cable--modulation");
    expect(modulation).toHaveLength(1);
    expect(modulation[0]?.style.strokeWidth).toBe(String(cableWidth(0.4)));
  });

  test("draws a cable as thick as it is deep as it is turned, without laying the canvas out again", async () => {
    const { view, server } = await rig(<PatchView />);
    const width = () => view.container.querySelector<SVGPathElement>(".tang-cable--modulation")?.style.strokeWidth;
    await waitFor(() => {
      expect(width()).toBe(String(cableWidth(0.4)));
    });
    act(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: -1 });
    });
    await waitFor(() => {
      expect(width()).toBe(String(cableWidth(-1)));
    });
    expect(cableWidth(-1)).not.toBe(cableWidth(0.4));
  });

  test("marks the port of the control that a cable ends at, and no other", async () => {
    await rig(<PatchView />);
    const fil = await nodeOf(filter.id);
    const ports = Array.from(fil.querySelectorAll(".tang-port--param"));
    expect(ports).toHaveLength(4); // one for each of the filter's parameters
    expect(fil.querySelectorAll(".tang-port--driven")).toHaveLength(1);
    expect(fil.querySelector(".tang-port--driven")?.getAttribute("data-handleid")).toBe("param-0");
    expect((await nodeOf(oscillator.id)).querySelectorAll(".tang-port--driven")).toHaveLength(0);
  });

  test("shows the modulator for what it is, with a bar for what it puts out", async () => {
    await rig(<PatchView />);
    const mod = await nodeOf(lfo.id);
    // (An LFO is called what it is, so its kind is not said twice.)
    expect(within(mod).getByText("LFO", { selector: ".tang-node__name" })).toBeTruthy();
    expect(mod.querySelector(".tang-node__badge")).toBeNull();
    expect(within(mod).getByText("sine, 0.5 Hz")).toBeTruthy();
    const bar = within(mod).getByRole("meter", { name: `${lfo.name} output` });
    expect(bar.getAttribute("aria-valuemin")).toBe("-1");
    expect(bar.getAttribute("aria-valuemax")).toBe("1");
    expect(bar.hasAttribute("data-centred")).toBe(true);
  });

  test("says what kind of modulator an envelope and a MIDI control are, which their names do not", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.push(
      { id: 90, parent: 1, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } },
      { id: 91, parent: 1, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } },
    );
    await rig(<PatchView />, { snapshot });
    expect(within(await nodeOf(90)).getByText("Envelope", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(within(await nodeOf(90)).getByText("ADSR", { selector: ".tang-node__name" })).toBeTruthy();
    expect(within(await nodeOf(91)).getByText("MIDI", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(within(await nodeOf(91)).getByText("CC 74", { selector: ".tang-node__name" })).toBeTruthy();
  });

  test("gives each node the ports it has: an effect takes audio in and puts it out, an instrument only puts it out, a modulator puts out modulation, the master takes audio in", async () => {
    await rig(<PatchView />);
    const handles = async (id: number) =>
      Array.from((await nodeOf(id)).querySelectorAll(".react-flow__handle:not(.tang-port--param)")).map(
        (handle) => `${handle.getAttribute("data-handleid") ?? "?"}:${handle.classList.contains("source") ? "source" : "target"}:${handle.getAttribute("data-handlepos") ?? "?"}`,
      );
    // (Audio goes in on the left and out on the right; a modulator's output is at its bottom, over what it drives.)
    expect(await handles(oscillator.id)).toEqual(["audioOut:source:right"]);
    expect(await handles(filter.id)).toEqual(["audioIn:target:left", "audioOut:source:right"]);
    expect(await handles(lfo.id)).toEqual(["modOut:source:bottom"]);
    expect(await handles(MASTER)).toEqual(["audioIn:target:left"]);
    // And the ports of the controls are on their top edges.
    const ports = Array.from((await nodeOf(filter.id)).querySelectorAll(".tang-port--param")).map((handle) => handle.getAttribute("data-handlepos"));
    expect(new Set(ports)).toEqual(new Set(["top"]));
  });

  test("cannot be rearranged, selected or cabled by hand yet", async () => {
    const { view } = await rig(<PatchView />);
    await nodeOf(filter.id);
    for (const node of Array.from(view.container.querySelectorAll(".react-flow__node"))) {
      expect(node.classList.contains("draggable")).toBe(false);
      expect(node.classList.contains("selectable")).toBe(false);
    }
    expect(view.container.querySelectorAll(".react-flow__handle.connectable")).toHaveLength(0);
  });

  test("gives the master a level meter with a clip light", async () => {
    await rig(<PatchView />);
    const master = await nodeOf(4294967295);
    expect(within(master).getByRole("meter", { name: "Master level" }).getAttribute("aria-valuemin")).toBe("-60");
    expect(within(master).getByText("CLIP")).toBeTruthy();
  });
});

describe("a patch with a group", () => {
  /** The tracer's lane in a group, with the group between the filter and the master. */
  function grouped(): Snapshot {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.unshift({ id: 77, parent: null, name: "Pad", kind: "group", volume: 0.8, pan: -0.25 });
    for (const node of snapshot.graph.nodes) if (node.id === oscillator.id) node.parent = 77;
    snapshot.graph.edges = snapshot.graph.edges.flatMap((edge) => {
      if (edge.kind === "audio" && edge.to.node === MASTER) {
        return [
          { kind: "member" as const, from: edge.from, to: { node: 77, port: { type: "audioIn" as const } } },
          { kind: "audio" as const, from: { node: 77, port: { type: "audioOut" as const } }, to: edge.to },
        ];
      }
      return [edge];
    });
    return snapshot;
  }

  test("has the group's volume and pan on its node, with ports for the lanes that feed it and the master", async () => {
    const { view } = await rig(<PatchView />, { snapshot: grouped() });
    const group = await nodeOf(77);
    expect(group.classList.contains("react-flow__node-group")).toBe(true);
    expect(within(group).getByText("Pad")).toBeTruthy();
    expect(within(group).getByText("Group", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(within(group).getByRole("slider", { name: "Volume" }).getAttribute("aria-valuenow")).toBe("0.8");
    expect(within(group).getByRole("slider", { name: "Pan" }).getAttribute("aria-valuenow")).toBe("-0.25");
    expect(Array.from(group.querySelectorAll(".react-flow__handle")).map((handle) => handle.getAttribute("data-handleid"))).toEqual(["audioIn", "audioOut"]);
    // oscillator → filter, filter → group, group → master, and the modulation.
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(4);
    });
  });
});

describe("the master's meter", () => {
  test("lights the clip light when the count of clipped buffers goes up, from the telemetry", async () => {
    // The mock makes no telemetry here (its period is long), so that the frames are the test's own.
    const { client, clock, view } = await rig(<PatchView />, { telemetryPeriod: 3_600_000 });
    const { layout } = { layout: client.state.snapshot?.telemetry };
    if (!layout) throw new Error("no layout");
    const frame = (clips: number): ArrayBuffer => {
      const values = layout.meters.map((entry) => (entry.kind === "clips" ? clips : 0.25));
      const buffer = new ArrayBuffer(8 + 4 * values.length);
      const data = new DataView(buffer);
      data.setUint16(2, values.length, true);
      data.setUint32(4, layout.rev, true);
      values.forEach((value, i) => {
        data.setFloat32(8 + 4 * i, value, true);
      });
      return buffer;
    };
    const accept = (clips: number) => {
      const decoded = decodeFrame(frame(clips));
      if (decoded) client.telemetry.accept(decoded);
    };
    const light = view.container.querySelector(".tang-clip");
    accept(0);
    act(() => {
      clock.tick(0);
    });
    expect(light?.hasAttribute("data-lit")).toBe(false);
    accept(1);
    act(() => {
      clock.tick(100);
    });
    expect(light?.hasAttribute("data-lit")).toBe(true);
    act(() => {
      clock.tick(2200);
    });
    expect(light?.hasAttribute("data-lit")).toBe(false);
    // And the level meter shows the 0.25 of full scale the frames carried: about -12 dB.
    expect(Number(within(await nodeOf(MASTER)).getByRole("meter").getAttribute("aria-valuenow"))).toBeCloseTo(-12.04, 1);
  });
});

describe("whether the canvas is the same", () => {
  test("it is when both have no session, or when what differs is only values", () => {
    expect(sameGraphStructure(null, null)).toBe(true);
    const changed = structuredClone(tracerSnapshot.graph);
    for (const node of changed.nodes) if (node.kind === "instrument") node.volume = 0.5;
    expect(sameGraphStructure(tracerSnapshot.graph, changed)).toBe(true);
  });

  test("it is not when one has a session and the other has not, or they differ in structure", () => {
    expect(sameGraphStructure(null, tracerSnapshot.graph)).toBe(false);
    expect(sameGraphStructure(tracerSnapshot.graph, null)).toBe(false);
    expect(sameGraphStructure(tracerSnapshot.graph, { nodes: tracerSnapshot.graph.nodes.slice(1), edges: tracerSnapshot.graph.edges })).toBe(false);
  });
});

describe("the patch while it plays", () => {
  test("has the cutoff's dot and ring where the LFO takes it, the LFO's bar where it is, and the master's level", async () => {
    const { client, clock, time } = await rig(<PatchView />);
    const cutoff = within(await nodeOf(filter.id)).getByRole("slider", { name: "cutoff" }).closest(".tang-knob");
    const bar = within(await nodeOf(lfo.id)).getByRole("meter");
    const level = within(await nodeOf(4294967295)).getByRole("meter");
    expect(cutoff?.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.1, 0.9));
    time.seconds = 0.5; // the LFO is at its peak
    client.noteOn(60); // and a note is held
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      const dot = cutoff?.querySelector(".tang-knob__dot");
      expect(dot?.getAttribute("visibility")).toBe("visible");
      expect(Number(dot?.getAttribute("cx"))).toBeCloseTo(pointAt(24, 18, angleOf(0.9)).x, 3);
      expect(bar.getAttribute("aria-valuenow")).toBe("1");
      expect(level.getAttribute("aria-valuenow")).not.toBeNull();
    });
    const fill = (element: Element) => element.querySelector<HTMLElement>(".tang-livebar__fill")?.style;
    expect([fill(bar)?.getPropertyValue("--from"), fill(bar)?.getPropertyValue("--to")]).toEqual(["0.5", "1"]);
    expect(Number(fill(level)?.getPropertyValue("--to"))).toBeGreaterThan(0.7); // 0.35 of full scale is about -9 dB
  });

  test("follows a parameter somebody else changes without making the canvas again", async () => {
    const { server } = await rig(<PatchView />);
    const fil = await nodeOf(filter.id);
    const nodes = Array.from(document.querySelectorAll(".react-flow__node"));
    const cutoff = within(fil).getByRole("slider", { name: "cutoff" });
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.8 });
    });
    await waitFor(() => {
      expect(cutoff.getAttribute("aria-valuenow")).toBe("0.8");
    });
    // The same elements, not new ones: nothing was laid out again.
    expect(Array.from(document.querySelectorAll(".react-flow__node"))).toEqual(nodes);
  });

  test("is laid out again when the session changes in structure: a node comes", async () => {
    const { client, view } = await rig(<PatchView />);
    await screen.findByTestId(`rf__node-${String(filter.id)}`);
    expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(4);
    const bigger: Snapshot = structuredClone(tracerSnapshot);
    bigger.rev = 1;
    bigger.graph.nodes.splice(2, 0, { id: 88, parent: 0, name: "Second filter", kind: "effect", plugin: structuredClone(filterPlugin), mix: 1 });
    act(() => {
      client.store.setState({ snapshot: bigger });
    });
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(5);
    });
    expect(within(await nodeOf(88)).getByText("Second filter")).toBeTruthy();
  });

  test("has nothing on it before there is a session", async () => {
    const { view, client } = await rig(<PatchView />);
    act(() => {
      client.store.setState({ snapshot: null });
    });
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(0);
    });
  });
});
