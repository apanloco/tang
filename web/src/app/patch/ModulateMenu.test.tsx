// @vitest-environment jsdom
import { ReactFlow, ReactFlowProvider } from "@xyflow/react";
import type { NodeTypes } from "@xyflow/react";
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { fieldsFixture, nodeNamed, tracerSnapshot } from "../../client/fixtures";
import { displayNames } from "../../client/names";
import type { Edge, Port } from "../../protocol";
import { rig } from "../testing";
import { ModulatorSettingCell, ParamCell, ParamRow } from "./controls";
import { ModulateMenu } from "./ModulateMenu";

afterEach(cleanup);

const filter = nodeNamed(tracerSnapshot, "Filter");
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const lfo = nodeNamed(tracerSnapshot, "LFO");
const paramPort = (node: typeof filter, name: string): Port => {
  const found = (node.kind === "instrument" || node.kind === "effect") && node.plugin ? node.plugin.params.find((p) => p.name === name) : undefined;
  if (!found) throw new Error(`no ${name}`);
  return { type: "param", index: found.index };
};
const cutoff = paramPort(filter, "cutoff");
const resonance = paramPort(filter, "resonance");

const button = (name: string): HTMLElement => screen.getByRole("button", { name });
const menuItems = (): string[] => screen.queryAllByRole("menuitem").map((item) => item.textContent);
const open = (name: string): void => {
  fireEvent.click(button(name));
};
/** What the mock engine has been told since the test began (a microtask to send and to answer). */
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const cablesFrom = (edges: readonly Edge[], node: number): Edge[] => edges.filter((e) => e.kind === "modulation" && e.from.node === node);

describe("the button that modulates a control", () => {
  test("is a button named for what it modulates, with a menu that is not there until it is opened", async () => {
    await rig(<ModulateMenu node={filter.id} port={resonance} name="resonance" />);
    const trigger = button("Modulate resonance");
    expect(trigger.textContent).toBe("~");
    expect(trigger.getAttribute("aria-haspopup")).toBe("menu");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("offers a new LFO, a new envelope and a new MIDI control under a heading, and a modulator that is there under another", async () => {
    await rig(<ModulateMenu node={filter.id} port={resonance} name="resonance" />);
    open("Modulate resonance");
    const menu = screen.getByRole("menu", { name: "Modulate resonance" });
    expect(within(menu).getAllByRole("group").map((group) => group.getAttribute("aria-label"))).toEqual(["New", "Attach"]);
    // (The tracer's LFO drives the cutoff and not the resonance.)
    expect(menuItems()).toEqual(["New LFO", "New envelope", "New MIDI control", "LFO"]);
  });

  test("offers only the new ones when no modulator that is there can drive it: the tracer's LFO drives the cutoff already", async () => {
    await rig(<ModulateMenu node={filter.id} port={cutoff} name="cutoff" />);
    open("Modulate cutoff");
    expect(menuItems()).toEqual(["New LFO", "New envelope", "New MIDI control"]);
    expect(within(screen.getByRole("menu")).getAllByRole("group").map((group) => group.getAttribute("aria-label"))).toEqual(["New"]);
  });

  test("makes a modulator and a cable to the control with one request, when a new one is chosen, which the session has then", async () => {
    const { server, requests } = await rig(<ModulateMenu node={filter.id} port={resonance} name="resonance" />);
    open("Modulate resonance");
    fireEvent.click(screen.getByRole("menuitem", { name: "New envelope" }));
    await settle();
    expect(requests().map((r) => r.intent)).toEqual([{ type: "modulate", to: { node: filter.id, port: resonance }, kind: "envelope", depth: null }]);
    const added = server.snapshot.graph.nodes.filter((n) => n.kind === "modulator" && n.id !== lfo.id);
    expect(added.map((n) => n.name)).toEqual(["ADSR"]);
    const [cable] = cablesFrom(server.snapshot.graph.edges, added[0]?.id ?? -1);
    expect(cable).toMatchObject({ kind: "modulation", to: { node: filter.id, port: resonance }, amount: 0.5 });
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("makes a MIDI control that is armed to learn, with a cable at the whole range, when that is chosen", async () => {
    const { server, requests } = await rig(<ModulateMenu node={filter.id} port={resonance} name="resonance" />);
    open("Modulate resonance");
    fireEvent.click(screen.getByRole("menuitem", { name: "New MIDI control" }));
    await settle();
    expect(requests().map((r) => r.intent)).toEqual([{ type: "modulate", to: { node: filter.id, port: resonance }, kind: "midiLearn", depth: 1 }]);
    const control = server.snapshot.graph.nodes.find((n) => n.kind === "modulator" && n.source.type === "midiLearn");
    expect(control?.kind === "modulator" ? control.source : null).toEqual({ type: "midiLearn", control: null, learning: true });
    expect(cablesFrom(server.snapshot.graph.edges, control?.id ?? -1)).toMatchObject([{ amount: 1 }]);
  });

  test("draws a cable from the modulator that is chosen, with the request that draws one, and makes no modulator", async () => {
    const { server, requests } = await rig(<ModulateMenu node={filter.id} port={resonance} name="resonance" />);
    const before = server.snapshot.graph.nodes.length;
    open("Modulate resonance");
    fireEvent.click(screen.getByRole("menuitem", { name: "LFO" }));
    await settle();
    expect(requests().map((r) => r.intent)).toEqual([{ type: "addModEdge", modulator: lfo.id, to: { node: filter.id, port: resonance } }]);
    expect(server.snapshot.graph.nodes).toHaveLength(before);
    expect(cablesFrom(server.snapshot.graph.edges, lfo.id).some((e) => e.kind === "modulation" && e.to.node === filter.id && JSON.stringify(e.to.port) === JSON.stringify(resonance))).toBe(true);
  });

  test("looks for the modulators that could drive it when the menu is opened, and not before: one that is made in between is in the list", async () => {
    const { server } = await rig(<ModulateMenu node={filter.id} port={resonance} name="resonance" />);
    await act(async () => {
      server.apply({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
      await settle();
    });
    open("Modulate resonance");
    expect(menuItems()).toEqual(["New LFO", "New envelope", "New MIDI control", "LFO", "ADSR"]);
    // And opened again, after another is made, the list is as it is then.
    fireEvent.click(button("Modulate resonance"));
    await act(async () => {
      server.apply({ type: "addModulator", parent: oscillator.id, kind: "midiLearn" });
      await settle();
    });
    open("Modulate resonance");
    expect(menuItems()).toEqual(["New LFO", "New envelope", "New MIDI control", "LFO", "ADSR", "MIDI (learning…)"]);
  });

  test("names the modulators that are there as the canvas names them: two LFOs are numbered", async () => {
    const { server } = await rig(<ModulateMenu node={filter.id} port={resonance} name="resonance" />);
    await act(async () => {
      server.apply({ type: "addModulator", parent: oscillator.id, kind: "lfo" });
      await settle();
    });
    open("Modulate resonance");
    expect(menuItems().slice(3)).toEqual(["LFO 1", "LFO 2"]);
  });

  test("modulates a setting of a modulator with a sibling: the rate of the LFO has the others of its lane to attach, and the new ones", async () => {
    const { server, requests } = await rig(<ModulateMenu node={lfo.id} port={{ type: "rate" }} name="rate" />);
    await act(async () => {
      server.apply({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
      await settle();
    });
    open("Modulate rate");
    expect(menuItems()).toEqual(["New LFO", "New envelope", "New MIDI control", "ADSR"]);
    fireEvent.click(screen.getByRole("menuitem", { name: "New LFO" }));
    await settle();
    expect(requests().at(-1)?.intent).toEqual({ type: "modulate", to: { node: lfo.id, port: { type: "rate" } }, kind: "lfo", depth: null });
    await waitFor(() => {
      expect(server.snapshot.graph.nodes.filter((n) => n.kind === "modulator" && n.parent === oscillator.id)).toHaveLength(3);
    });
  });

  test("lists the group's modulators for the parameter of a lane that is in the group, besides the lane's own, and not the modulators of another lane", async () => {
    const { snapshot } = fieldsFixture;
    // (The fixture's first member: its modulators are 4, 5 and 6, its group's 13, 14 and 15, and the lone LFO of the other member is 8.)
    const lane = snapshot.graph.nodes.find((n) => n.id === 3);
    if (lane?.kind !== "instrument" || !lane.plugin) throw new Error("the fixture has a lane in a group");
    const first = lane.plugin.params[0];
    if (!first) throw new Error("the lane has a parameter");
    const names = displayNames(snapshot.graph);
    await rig(<ModulateMenu node={lane.id} port={{ type: "param", index: first.index }} name={first.name} />, { snapshot });
    open(`Modulate ${first.name}`);
    const attach = menuItems().slice(3);
    const named = (...ids: number[]): (string | undefined)[] => ids.map((id) => names.get(id));
    expect(attach).toEqual(expect.arrayContaining(named(4, 5, 6, 13, 14, 15)));
    expect(attach).toHaveLength(6);
    expect(attach).not.toContain(names.get(8));
  });
});

// The controls that have the button: the cells of a node (a knob of a plugin's parameter, a number of a modulator), and the rows of the panel of a plugin's
// parameters. Not the controls of what is chosen from a list (a waveform, a parameter whose values have names): a cable can be dragged to those, and a
// modulator that steps through names is not what the button is for.
describe("the controls that have the button that modulates them", () => {
  const { snapshot } = fieldsFixture;
  // The fixture's first member lane (3: waveform, detune, volume...), its LFO (4), envelope (5).
  const NODE_TYPES: NodeTypes = {
    cells: () => (
      <div>
        <ParamCell node={3} index={0} driven={false} />
        <ParamCell node={3} index={1} driven={false} />
        <ModulatorSettingCell node={4} field="rate" driven={false} />
        <ModulatorSettingCell node={4} field="waveform" driven={false} />
        <ModulatorSettingCell node={5} field="attack" driven={false} />
      </div>
    ),
  };

  async function cells() {
    const made = await rig(
      <ReactFlowProvider>
        <div style={{ width: 800, height: 600 }}>
          <ReactFlow nodes={[{ id: "1", type: "cells", position: { x: 0, y: 0 }, data: {} }]} edges={[]} nodeTypes={NODE_TYPES} />
        </div>
      </ReactFlowProvider>,
      { snapshot },
    );
    await screen.findByTestId("rf__node-1");
    return made;
  }

  const modulateButtons = (): string[] => screen.queryAllByRole("button", { name: /^Modulate / }).map((b) => b.getAttribute("aria-label") ?? "");

  test("is on the knob of a parameter of a node and on a number of a modulator, and on no choice", async () => {
    await cells();
    // (The waveform of the lane's instrument and of the LFO are chosen from a list.)
    expect(modulateButtons()).toEqual(["Modulate detune", "Modulate rate", "Modulate attack"]);
  });

  test("modulates what its cell sets: the parameter of the node, the setting of the modulator", async () => {
    const { requests } = await cells();
    open("Modulate detune");
    expect(screen.getByRole("menu", { name: "Modulate detune" })).toBeTruthy();
    fireEvent.click(screen.getByRole("menuitem", { name: "New LFO" }));
    open("Modulate rate");
    fireEvent.click(screen.getByRole("menuitem", { name: "New envelope" }));
    open("Modulate attack");
    fireEvent.click(screen.getByRole("menuitem", { name: "New MIDI control" }));
    await settle();
    expect(requests().map((r) => r.intent)).toEqual([
      { type: "modulate", to: { node: 3, port: { type: "param", index: 1 } }, kind: "lfo", depth: null },
      { type: "modulate", to: { node: 4, port: { type: "rate" } }, kind: "envelope", depth: null },
      { type: "modulate", to: { node: 5, port: { type: "attack" } }, kind: "midiLearn", depth: 1 },
    ]);
  });

  test("is on a row of the panel of a plugin's parameters, beside its pin, and on no row that is chosen from a list", async () => {
    await rig(
      <div>
        <ParamRow node={3} index={0} />
        <ParamRow node={3} index={1} />
      </div>,
      { snapshot },
    );
    expect(modulateButtons()).toEqual(["Modulate detune"]);
    // It is the pin's neighbour, and does not take the pin's place.
    expect(screen.getByRole("button", { name: "Show detune on the node" })).toBeTruthy();
  });

  test("modulates the parameter of its row, by the plugin's own index", async () => {
    const { requests } = await rig(
      <div>
        <ParamRow node={3} index={1} />
        <ParamRow node={3} index={2} />
      </div>,
      { snapshot },
    );
    open("Modulate volume");
    fireEvent.click(screen.getByRole("menuitem", { name: "New LFO" }));
    await settle();
    expect(requests().map((r) => r.intent)).toEqual([{ type: "modulate", to: { node: 3, port: { type: "param", index: 2 } }, kind: "lfo", depth: null }]);
  });
});
