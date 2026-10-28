// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { MASTER, freeReachFixture, nodeNamed, tracerSnapshot } from "../../client/fixtures";
import { displayNames } from "../../client/names";
import type { Intent } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { AddFreeModulatorMenu } from "../AddFreeModulator";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { UndoRedo } from "../UndoRedo";
import { ModulateMenu } from "./ModulateMenu";
import { ADD_FREE_MODULATOR_ID } from "./nodes";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// A modulator of the session's own (step 92, P5b-3; the controller's side is step 91; every modulator is one since step 96): the bar's button that adds one, the node it is on the canvas, what it can be drawn to, and what a knob's "~" menu
// offers of it. (The tracer has the nodes 0 to 2: a lane, its filter and its LFO, which hears the lane; the first modulator that is added is the node 3.)

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);

/** What the page says when it could not do what was asked. */
function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

/** The page, the bar's button that adds a modulator of its own, Undo and Redo and the notices, over the tracer. */
const page = () =>
  rig(
    <>
      <PatchView />
      <AddFreeModulatorMenu />
      <UndoRedo />
      <Board />
    </>,
  );

/** The intents that the page asked of the server. */
const asked = (requests: () => { intent: unknown }[]): Intent[] => requests().map((request) => request.intent as Intent);

const addButton = (): HTMLElement => screen.getByRole("button", { name: "Add a modulator" });

/** Choose the kind `label` from the bar's menu, and wait until the node `id` is on the canvas. */
async function add(label: string, id: number): Promise<HTMLElement> {
  fireEvent.click(addButton());
  fireEvent.click(within(screen.getByRole("menu", { name: "Add a modulator" })).getByRole("menuitem", { name: label }));
  return nodeOf(id);
}

describe("the menu that chooses whose notes a modulator hears (step 93)", () => {
  const hearsButton = (node: HTMLElement): HTMLElement => within(node).getByRole("button", { name: /^Whose notes / });

  test("is on the node of every modulator, lit for the one that hears a lane: the tracer's own LFO has one too", async () => {
    await page();
    const node = await add("LFO", 3);
    expect(hearsButton(node).textContent).toBe("\u266b");
    expect(hearsButton(node).getAttribute("aria-label")).toBe("Whose notes LFO 2 hears: every note");
    expect(hearsButton(node).id).toBe("hears-3");
    expect(hearsButton(node).className).not.toContain("tang-node__notes--set");
    // (The tracer's own LFO hears the lane.)
    const first = hearsButton(await nodeOf(2));
    expect(first.getAttribute("aria-label")).toBe("Whose notes LFO 1 hears: Oscillator");
    expect(first.className).toContain("tang-node__notes--set");
  });

  test("lists every note, as the choice now, and the lanes and the groups of the session by the names that the canvas gives them", async () => {
    await page();
    const node = await add("Envelope", 3);
    fireEvent.click(hearsButton(node));
    const menu = screen.getByRole("menu");
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Every note", "Oscillator"]);
    expect(within(menu).getAllByRole("group").map((group) => group.getAttribute("aria-label"))).toEqual(["Lanes"]);
    // (It is the choice now, and marked: the menu opens on it.)
    expect(within(menu).getAllByRole("menuitem").map((item) => item.getAttribute("aria-current"))).toEqual(["true", null]);
  });

  test("asks for the lane that is chosen, and the button is lit and says whom the modulator hears, until every note is chosen", async () => {
    const { server, requests } = await page();
    const node = await add("Envelope", 3);
    expect(hearsButton(node).className).not.toContain("tang-node__notes--set");
    fireEvent.click(hearsButton(node));
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Oscillator" }));
    await waitFor(() => {
      expect(server.snapshot.graph.nodes.find((n) => n.id === 3)).toMatchObject({ hears: oscillator.id });
    });
    expect(asked(requests).slice(1)).toEqual([{ type: "setModulatorEar", node: 3, ear: oscillator.id }]);
    await waitFor(() => {
      expect(hearsButton(within(document.body).getByTestId("rf__node-3")).getAttribute("aria-label")).toBe("Whose notes ADSR hears: Oscillator");
    });
    expect(hearsButton(await nodeOf(3)).className).toContain("tang-node__notes--set");
    // The menu opens on what it hears now, and every note is a choice that asks for none.
    fireEvent.click(hearsButton(await nodeOf(3)));
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Every note" }));
    await waitFor(() => {
      expect(server.snapshot.graph.nodes.find((n) => n.id === 3)).not.toHaveProperty("hears");
    });
    expect(asked(requests).slice(2)).toEqual([{ type: "setModulatorEar", node: 3, ear: null }]);
    await waitFor(() => {
      expect(hearsButton(within(document.body).getByTestId("rf__node-3")).className).not.toContain("tang-node__notes--set");
    });
  });

  test("goes back with Undo, which says whom the modulator hears in its words, and comes again with Redo", async () => {
    const { server } = await page();
    const node = await add("LFO", 3);
    fireEvent.click(hearsButton(node));
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Oscillator" }));
    fireEvent.click(await screen.findByRole("button", { name: "Undo make an LFO hear the lane of Oscillator" }));
    await waitFor(() => {
      expect(server.snapshot.graph.nodes.find((n) => n.id === 3)).not.toHaveProperty("hears");
    });
    fireEvent.click(await screen.findByRole("button", { name: "Redo make an LFO hear the lane of Oscillator" }));
    await waitFor(() => {
      expect(server.snapshot.graph.nodes.find((n) => n.id === 3)).toMatchObject({ hears: oscillator.id });
    });
  });
});

describe("the bar's button that adds a modulator that hears every note", () => {
  test("is a menu of the kinds of modulator, as the menu of a lane's \"+\" is, which is not there until it is opened", async () => {
    await page();
    expect(addButton().textContent).toBe("+ Modulator");
    expect(addButton().id).toBe(ADD_FREE_MODULATOR_ID);
    expect(screen.queryByRole("menu")).toBeNull();
    fireEvent.click(addButton());
    expect(screen.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["LFO", "Envelope", "Macro", "MIDI control", "Velocity", "Key", "Step sequencer"]);
  });

  test("asks for the kind that is chosen, one request, and the session has a modulator with no parent, a node on the canvas that is told what it is", async () => {
    const { server, requests } = await page();
    const node = await add("Envelope", 3);
    expect(asked(requests)).toEqual([{ type: "addFreeModulator", kind: "envelope", hears: null }]);
    expect(server.snapshot.graph.nodes.find((n) => n.id === 3)).toMatchObject({ id: 3, parent: null, kind: "modulator", name: "ADSR" });
    expect(node.querySelector(".tang-node__name")?.textContent).toBe("ADSR");
    // (No lane has it: the tracer's own lane has the modulators it had, and a request names no lane.)
    expect(screen.queryByRole("menu")).toBeNull();
  });

  test("has a node with the controls of its kind and a list of what it can be drawn to that is not empty: every plugin of the session, and the settings of the other modulators", async () => {
    await page();
    const node = await add("LFO", 3);
    const picker = within(node).getByRole("button", { name: /^Add a cable from / });
    expect(picker.hasAttribute("disabled")).toBe(false);
    expect(picker.textContent).toBe("Add a cable…");
    fireEvent.click(picker);
    // (The oscillator's and the filter's parameters, plugin by plugin in the order of the graph, and the settings of the tracer's LFO, which every modulator reaches.)
    expect(within(screen.getByRole("menu")).getAllByRole("group").map((group) => group.getAttribute("aria-label"))).toEqual(["Oscillator", "Filter", "LFO 1"]);
    expect(screen.getByRole("menuitem", { name: "detune" })).toBeTruthy();
    expect(screen.getByRole("menuitem", { name: "cutoff" })).toBeTruthy();
  });

  test("draws a cable from it to a parameter of a plugin: the cable is the controller's, from a node that hears no lane", async () => {
    const { server, requests } = await page();
    const node = await add("LFO", 3);
    fireEvent.click(within(node).getByRole("button", { name: /^Add a cable from / }));
    fireEvent.click(screen.getByRole("menuitem", { name: "detune" }));
    await waitFor(() => {
      expect(server.snapshot.graph.edges.some((e) => e.kind === "modulation" && e.from.node === 3)).toBe(true);
    });
    const detune = oscillator.kind === "instrument" ? oscillator.plugin?.params.find((p) => p.name === "detune") : undefined;
    expect(asked(requests).slice(1)).toEqual([{ type: "addModEdge", modulator: 3, to: { node: oscillator.id, port: { type: "param", index: detune?.index ?? -1 } } }]);
    // (It has a row for the cable, and the list does not offer the parameter again.)
    expect(within(await nodeOf(3)).getByText("Oscillator: detune")).toBeTruthy();
    fireEvent.click(within(await nodeOf(3)).getByRole("button", { name: /^Add a cable from / }));
    expect(screen.queryByRole("menuitem", { name: "detune" })).toBeNull();
    expect(screen.getByRole("menuitem", { name: "volume" })).toBeTruthy();
  });

  test("is taken out by the \"×\" of its node with its cables, the focus goes to the bar's button that adds one, and Undo puts it back whole under its id, with the cable", async () => {
    const { server } = await page();
    const node = await add("LFO", 3);
    fireEvent.click(within(node).getByRole("button", { name: /^Add a cable from / }));
    fireEvent.click(screen.getByRole("menuitem", { name: "cutoff" }));
    await waitFor(() => {
      expect(server.snapshot.graph.edges.filter((e) => e.kind === "modulation")).toHaveLength(2);
    });
    fireEvent.click(within(await nodeOf(3)).getByRole("button", { name: /^Remove LFO/ }));
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-3")).toBeNull();
    });
    expect(server.snapshot.graph.edges.filter((e) => e.kind === "modulation")).toHaveLength(1);
    expect(document.activeElement).toBe(addButton());
    // Undo: the modulator and its cable, as they were.
    fireEvent.click(await screen.findByRole("button", { name: "Undo remove an LFO" }));
    expect(await nodeOf(3)).toBeTruthy();
    expect(server.snapshot.graph.edges.filter((e) => e.kind === "modulation")).toHaveLength(2);
    expect(server.snapshot.graph.nodes.find((n) => n.id === 3)?.parent).toBeNull();
  });

  test("gives the focus back, when a modulator that hears a lane is taken out, to the \"+\" of that lane, which makes one, and not to the bar's button", async () => {
    // (The tracer as a program has it once loaded: the LFO is the session's own and hears the lane.)
    await rig(
      <>
        <PatchView />
        <AddFreeModulatorMenu />
      </>,
      { snapshot: tracerSnapshot },
    );
    fireEvent.click(within(await nodeOf(2)).getByRole("button", { name: /^Remove LFO/ }));
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-2")).toBeNull();
    });
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add a modulator to Oscillator" }));
    });
    expect(document.activeElement).not.toBe(addButton());
  });

  test("is a step of the history in the words of the button that adds a modulator to a lane: Undo takes the modulator out, and Redo makes it again under the id it had", async () => {
    const { server } = await page();
    await add("Envelope", 3);
    fireEvent.click(await screen.findByRole("button", { name: "Undo add an envelope" }));
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-3")).toBeNull();
    });
    expect(server.snapshot.graph.nodes.some((n) => n.id === 3)).toBe(false);
    fireEvent.click(await screen.findByRole("button", { name: "Redo add an envelope" }));
    expect(await nodeOf(3)).toBeTruthy();
    expect(server.snapshot.graph.nodes.find((n) => n.id === 3)).toMatchObject({ parent: null, name: "ADSR" });
  });

  test("makes each kind there is, and a second is the next node: two of them can be drawn to each other's settings and to the tracer's LFO's", async () => {
    const { server } = await page();
    await add("LFO", 3);
    await add("Envelope", 4);
    fireEvent.click(within(await nodeOf(3)).getByRole("button", { name: /^Add a cable from / }));
    // The tracer's LFO's rate and the envelope's four stages are the settings it can be drawn to.
    expect(
      within(screen.getByRole("menu"))
        .getAllByRole("group")
        .map((group) => group.getAttribute("aria-label")),
    ).toEqual(["Oscillator", "Filter", "LFO 1", "ADSR"]);
    expect(within(within(screen.getByRole("menu")).getByRole("group", { name: "ADSR" })).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["attack", "decay", "sustain", "release"]);
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "modulator").map((n) => n.id)).toEqual([2, 3, 4]);
  });

  test("is refused for what the server cannot do, as every add is, and says so among the notices with no node made", async () => {
    const { client, server } = await page();
    server.drop();
    await waitFor(() => {
      expect(client.state.status).not.toBe("open");
    });
    fireEvent.click(addButton());
    fireEvent.click(within(screen.getByRole("menu", { name: "Add a modulator" })).getByRole("menuitem", { name: "LFO" }));
    expect(await screen.findByText("Could not add an LFO of its own: Not connected to the server.")).toBeTruthy();
    expect(screen.queryByTestId("rf__node-3")).toBeNull();
  });
});

describe("a session whose own rack has modulators in it", () => {
  const { snapshot, reach, fields } = freeReachFixture;
  const names = displayNames(snapshot.graph);
  const [freeLfo, freeEnvelope] = [18, 19];
  // (The fixture is the session of the reach tests with an LFO and an envelope that hear every note: the LFO drives the detune of the first lane's oscillator (4), the envelope the resonance of its filter (5).)

  const groupsOf = (): { label: string | null; items: string[] }[] =>
    within(screen.getByRole("menu"))
      .getAllByRole("group")
      .map((group) => ({ label: group.getAttribute("aria-label"), items: within(group).getAllByRole("menuitem").map((item) => item.textContent) }));

  test("lists what each of them can be drawn to: every plugin there is that has a parameter left, node by node, and the settings and cable depths of the other", async () => {
    await rig(<PatchView />, { snapshot });
    const picker = within(await nodeOf(freeLfo)).getByRole("button", { name: /^Add a cable from / });
    fireEvent.click(picker);
    const groups = groupsOf();
    // The nine plugins that the controller let a cable be drawn to (`reach`), in the order of the graph, and then the settings and the cable depths of the other modulators (`fields`), each under the modulator that has them:
    // the last is the envelope's: its four stages and the depth of its cable.
    const plugins = reach[String(freeLfo)] ?? [];
    const others = [...new Set((fields[String(freeLfo)] ?? []).map((field) => field.node))];
    expect(plugins).toHaveLength(9);
    expect(others.at(-1)).toBe(freeEnvelope);
    expect(groups.map((group) => group.label)).toEqual([...snapshot.graph.nodes.filter((node) => plugins.includes(node.id)).map((node) => names.get(node.id)), ...others.map((node) => names.get(node))]);
    expect(groups.at(-1)?.items.slice(0, 4)).toEqual(["attack", "decay", "sustain", "release"]);
    expect(groups.at(-1)?.items[4]).toMatch(/^depth → /);
  });

  test("does not offer a parameter that it drives already, and does offer it to the other one", async () => {
    await rig(<PatchView />, { snapshot });
    fireEvent.click(within(await nodeOf(freeLfo)).getByRole("button", { name: /^Add a cable from / }));
    // The LFO drives the first oscillator's detune: not among its parameters again. (The same parameter of the others are there.)
    const first = groupsOf().find((group) => group.label === names.get(4));
    expect(first?.items).not.toContain("detune");
    expect(first?.items).toContain("volume");
    cleanup();
    await rig(<PatchView />, { snapshot });
    fireEvent.click(within(await nodeOf(freeEnvelope)).getByRole("button", { name: /^Add a cable from / }));
    expect(groupsOf().find((group) => group.label === names.get(4))?.items).toContain("detune");
  });

  test("has a modulator that hears a lane reach what one that hears every note reaches: every plugin, and the settings of the other modulators", async () => {
    await rig(<PatchView />, { snapshot });
    // The LFO that hears the first lane (12): the lists are the same as for the session's own LFO (18).
    fireEvent.click(within(await nodeOf(12)).getByRole("button", { name: /^Add a cable from / }));
    const plugins = reach["12"] ?? [];
    const others = [...new Set((fields["12"] ?? []).map((field) => field.node))];
    expect(plugins).toHaveLength(9);
    expect(groupsOf().map((group) => group.label)).toEqual([...snapshot.graph.nodes.filter((node) => plugins.includes(node.id)).map((node) => names.get(node.id)), ...others.map((node) => names.get(node))]);
  });

  test("is among the modulators that a knob's \"~\" menu offers to attach, for a plugin of any lane, beside the lane's own", async () => {
    const { requests, server } = await rig(<ModulateMenu node={9} port={{ type: "param", index: 1 }} name="detune" />, { snapshot });
    fireEvent.click(screen.getByRole("button", { name: "Modulate detune" }));
    // The oscillator of the lane in no group (9): every modulator of the session reaches it, whatever it hears, and none drives it yet.
    const attach = screen.getAllByRole("menuitem").map((item) => item.textContent).slice(7);
    expect(attach).toEqual([12, 13, 14, 15, 16, 17, freeLfo, freeEnvelope].map((id) => names.get(id)));
    fireEvent.click(screen.getByRole("menuitem", { name: names.get(freeEnvelope) ?? "" }));
    await waitFor(() => {
      expect(server.snapshot.graph.edges.some((e) => e.kind === "modulation" && e.from.node === freeEnvelope && e.to.node === 9)).toBe(true);
    });
    expect(asked(requests)).toEqual([{ type: "addModEdge", modulator: freeEnvelope, to: { node: 9, port: { type: "param", index: 1 } } }]);
  });

  test("is not offered again where it drives the knob already, and the others still are", async () => {
    await rig(<ModulateMenu node={4} port={{ type: "param", index: 1 }} name="detune" />, { snapshot });
    fireEvent.click(screen.getByRole("button", { name: "Modulate detune" }));
    // The first lane's oscillator: every modulator but the LFO (18), which drives its detune already.
    const attach = screen.getAllByRole("menuitem").map((item) => item.textContent).slice(7);
    expect(attach).toEqual([12, 13, 14, 15, 16, 17, freeEnvelope].map((id) => names.get(id)));
  });

  test("makes the modulator for a setting of one of them another that hears what it hears: a new LFO for the rate of the LFO that hears every note hears every note", async () => {
    const { server, requests } = await rig(<ModulateMenu node={freeLfo} port={{ type: "rate" }} name="rate" />, { snapshot });
    fireEvent.click(screen.getByRole("button", { name: "Modulate rate" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "New LFO" }));
    const hearing = (): number => server.snapshot.graph.nodes.filter((n) => n.kind === "modulator" && n.hears === undefined).length;
    await waitFor(() => {
      // (The LFO that hears nothing in particular, the one that drives it and the two that the fixture has besides.)
      expect(hearing()).toBe(4);
    });
    expect(asked(requests)).toEqual([{ type: "modulate", to: { node: freeLfo, port: { type: "rate" } }, kind: "lfo", depth: null }]);
    const made = server.snapshot.graph.nodes.filter((n) => n.kind === "modulator" && n.hears === undefined).at(-1);
    expect(server.snapshot.graph.edges.some((e) => e.kind === "modulation" && e.from.node === made?.id && e.to.node === freeLfo)).toBe(true);
  });
});

describe("the names of the modulators of one name", () => {
  test("go in the order of the graph, which lists the session's own after the lanes': the tracer's LFO is the first and the one that is added the second", async () => {
    const { server } = await page();
    await add("LFO", 3);
    expect(Array.from(document.querySelectorAll(".tang-node__name")).map((name) => name.textContent)).toEqual(["Oscillator", "Filter", "LFO 1", "LFO 2", "Master"]);
    expect(server.snapshot.graph.nodes.map((n) => n.id)).toEqual([oscillator.id, filter.id, 2, 3, MASTER]);
  });
});
