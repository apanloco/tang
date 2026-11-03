// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { RenderResult } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { MASTER, sourcesFixture } from "../../client/fixtures";
import type { Snapshot } from "../../protocol";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// The session of the sources fixture (`testutil::NOTES`): the keys (node 7) and a device (8, "Launchkey Mini MK4 37") are the sources; group 0 ("Pad") with Reverb 1 on its bus; lane 2 ("Oscillator 1") in it with
// Filter 3; lane 4 ("Oscillator 2"), which has a cable of notes (1) from the keys, and an empty lane (5); and an LFO (6) that hears lane 2. The lanes that no cable of notes ends at, 2 and 5, hear both sources by a link each.
const found = sourcesFixture.cases.find((entry) => entry.name === "wired");
if (!found) throw new Error("the fixture has the session of the sources");
const snapshot: Snapshot = found.snapshot;
// (Both ports of the device, and another input that no node stands for.)
const INPUTS = ["Launchkey Mini MK4 37 MIDI Out", "Launchkey Mini MK4 37 DAW Out", "Seaboard RISE"];

const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);
const cablesOf = (view: RenderResult, kind: "notes" | "hears"): SVGPathElement[] => Array.from(view.container.querySelectorAll<SVGPathElement>(`.tang-cable--${kind}`));
/** The nodes that have a handle of this name, by the id of the node, in order. */
const nodesWith = (view: RenderResult, handle: string): number[] =>
  Array.from(view.container.querySelectorAll(`.react-flow__handle[data-handleid="${handle}"]`), (port) => Number(port.getAttribute("data-nodeid"))).sort((a, b) => a - b);

describe("the sources of notes on the canvas", () => {
  test("are a node each, of a kind of their own, with the name the session gives it and, in small type, what it is: the keys or a MIDI input", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    const keys = await nodeOf(7);
    const device = await nodeOf(8);
    expect(keys.querySelector(".tang-node--source")).not.toBeNull();
    expect(device.querySelector(".tang-node--source")).not.toBeNull();
    expect(keys.querySelector(".tang-node__name")?.textContent).toBe("Keys");
    expect(keys.querySelector(".tang-node__badge")?.textContent).toBe("Keyboard");
    expect(device.querySelector(".tang-node__name")?.textContent).toBe("Launchkey Mini MK4 37");
    expect(device.querySelector(".tang-node__badge")?.textContent).toBe("MIDI input");
  });

  test("have the badge under the name and the button that sends their notes on its line, as a lane has the button that sends its audio, and the button that takes it out after the title", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    for (const id of [7, 8]) {
      const header = (await nodeOf(id)).querySelector(".tang-node__header");
      const title = header?.querySelector(".tang-node__title");
      expect(Array.from(title?.children ?? [], (child) => child.className), String(id)).toEqual(["tang-node__name", "tang-node__badge", "tang-menu-button nodrag nopan tang-node__notes"]);
      expect(header?.firstElementChild, String(id)).toBe(title);
      expect(header?.querySelectorAll(".tang-node__remove"), String(id)).toHaveLength(1);
    }
  });

  test("say what the keys are, and, for a device, which of the inputs that are open it stands for, or that none is", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    expect(within(await nodeOf(7)).getByText("Computer keys and the piano")).toBeDefined();
    const plugged = within(await nodeOf(8)).getByText("Plugged in: Launchkey Mini MK4 37 MIDI Out, Launchkey Mini MK4 37 DAW Out");
    expect(plugged.getAttribute("title")).toBe("Launchkey Mini MK4 37 MIDI Out, Launchkey Mini MK4 37 DAW Out");
  });

  test("say which inputs a device stands for by the name it has in the session, and not by the number the canvas gives it when another node has the same name", async () => {
    // (A device that is called Keys and the keys: the canvas calls them Keys 1 and Keys 2, and the device stands for the inputs that have Keys in their names.)
    const named: Snapshot = structuredClone(snapshot);
    const device = named.graph.nodes.find((node) => node.id === 8);
    if (device?.kind !== "source") throw new Error("the fixture has a device as node 8");
    device.name = "Keys";
    device.source = { type: "device", name: "Keys" };
    await rig(<PatchView />, { snapshot: named, inputs: ["Keystation 61"] });
    const node = await nodeOf(8);
    expect(node.querySelector(".tang-node__name")?.textContent).toBe("Keys 2");
    expect(within(node).getByText("Plugged in: Keystation 61")).toBeDefined();
  });

  test("say that a device is not plugged in while no input it stands for is open", async () => {
    await rig(<PatchView />, { snapshot, inputs: ["Seaboard RISE"] });
    expect(within(await nodeOf(8)).getByText("Not plugged in")).toBeDefined();
    expect(within(await nodeOf(7)).queryByText(/plugged in/i)).toBeNull();
  });

  test("follow the inputs that open and close: a device is plugged in when the server says an input that its name stands for is open", async () => {
    const { server } = await rig(<PatchView />, { snapshot, inputs: [] });
    const device = await nodeOf(8);
    expect(within(device).getByText("Not plugged in")).toBeDefined();
    act(() => {
      server.announceInputs(["Launchkey Mini MK4 37 MIDI Out"]);
    });
    await waitFor(() => {
      expect(within(device).getByText("Plugged in: Launchkey Mini MK4 37 MIDI Out")).toBeDefined();
    });
    act(() => {
      server.announceInputs([]);
    });
    await waitFor(() => {
      expect(within(device).getByText("Not plugged in")).toBeDefined();
    });
  });

  test("have a button that takes each out, named for it: the server is asked, and the node goes with the cable of notes it had", async () => {
    const { requests, view } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    expect(within(await nodeOf(8)).getByRole("button", { name: "Remove Launchkey Mini MK4 37" })).toBeDefined();
    fireEvent.click(within(await nodeOf(7)).getByRole("button", { name: "Remove Keys" }));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "removeNode", node: 7 } }]);
    });
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-7")).toBeNull();
    });
    expect(cablesOf(view, "notes")).toHaveLength(0);
    expect(screen.queryByRole("button", { name: "Remove cable Keys → Oscillator 2" })).toBeNull();
  });
});

describe("the ports of notes of the canvas", () => {
  test("are an output on each source and an input on each lane (an empty one too), and on no other node", async () => {
    const { view } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    await nodeOf(MASTER);
    expect(nodesWith(view, "eventsOut")).toEqual([7, 8]);
    expect(nodesWith(view, "eventsIn")).toEqual([2, 4, 5]);
  });

  test("start a cable at an output and end it at an input, and not the other way round", async () => {
    const { view } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    await nodeOf(MASTER);
    const ports = Array.from(view.container.querySelectorAll(".react-flow__handle.tang-port--notes"));
    expect(ports).toHaveLength(5);
    for (const port of ports) {
      const output = port.getAttribute("data-handleid") === "eventsOut";
      const where = `${port.getAttribute("data-nodeid") ?? "?"}:${port.getAttribute("data-handleid") ?? "?"}`;
      expect([port.classList.contains("connectablestart"), port.classList.contains("connectableend")], where).toEqual(output ? [true, false] : [false, true]);
    }
  });
});

describe("the handles of the nodes that move notes", () => {
  test("have notes leave a source on its right and come into a lane on its left, where the lane's audio goes out on its right", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    const handles = async (id: number) =>
      Array.from((await nodeOf(id)).querySelectorAll(".react-flow__handle:not(.tang-port--param)"), (handle) => `${handle.getAttribute("data-handleid") ?? "?"}:${handle.classList.contains("source") ? "source" : "target"}:${handle.getAttribute("data-handlepos") ?? "?"}`);
    expect(await handles(7)).toEqual(["eventsOut:source:right"]);
    expect(await handles(8)).toEqual(["eventsOut:source:right"]);
    expect(await handles(2)).toEqual(["eventsIn:target:left", "audioOut:source:right"]);
    expect(await handles(5)).toEqual(["eventsIn:target:left", "audioOut:source:right"]);
  });
});

describe("the cables of notes and the links by which the lanes hear the sources", () => {
  test("are drawn: a cable for each cable of notes, and a link for each source and each lane that no cable of notes ends at, each in a class of its own", async () => {
    const { view } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    await waitFor(() => {
      expect(cablesOf(view, "hears")).toHaveLength(4);
    });
    expect(cablesOf(view, "notes").map((path) => path.id)).toEqual(["notes:1"]);
    expect(cablesOf(view, "hears").map((path) => path.id)).toEqual(["hears:7>2", "hears:7>5", "hears:8>2", "hears:8>5"]);
    expect(view.container.querySelectorAll(".react-flow__edge-notes")).toHaveLength(1);
    expect(view.container.querySelectorAll(".react-flow__edge-hears")).toHaveLength(4);
    // (The links of the audio path, the audio cable and the cable of modulation are as they were: 6 + 1 + 1.)
    expect(view.container.querySelectorAll(".tang-cable--audio")).toHaveLength(6);
    expect(view.container.querySelectorAll(".tang-cable--send")).toHaveLength(1);
    expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(1);
    for (const path of [...cablesOf(view, "notes"), ...cablesOf(view, "hears")]) expect(path.classList.contains("tang-cable--audio"), path.id).toBe(false);
  });

  test("have a button on the cable of notes that takes it out, named for what it joins, and none on a link by which a lane hears a source, which is the lane's own hearing and is not drawn or taken out", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    expect(await screen.findByRole("button", { name: "Remove cable Keys → Oscillator 2" })).toBeDefined();
    // (The buttons on the cables there are: the links of the audio path, which are cables to a person and have one each, the audio cable, the cable of notes and the cable of modulation. The links by which the
    // lanes hear the sources have none.)
    expect(screen.getAllByRole("button", { name: /^Remove cable / }).map((button) => button.getAttribute("aria-label"))).toEqual([
      "Remove cable Pad → Reverb",
      "Remove cable Reverb → Master",
      "Remove cable Oscillator 1 → Filter",
      "Remove cable Filter → Pad",
      "Remove cable Oscillator 2 → Master",
      "Remove cable (empty) → Master",
      "Remove cable Oscillator 2 → Filter",
      "Remove cable Keys → Oscillator 2",
      "Remove cable LFO → Filter: cutoff",
    ]);
    // (None says what the sources are heard by: the links of hearing are faint and dotted and have nothing on them.)
    expect(screen.getAllByRole("button", { name: /^Remove cable (Keys|Device|Nothing)/ })).toHaveLength(1);
  });

  test("are taken out by it: the server is asked, the lane hears every source again by its links, and Undo draws the cable again", async () => {
    const { client, requests, server, view } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    fireEvent.click(await screen.findByRole("button", { name: "Remove cable Keys → Oscillator 2" }));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "removeEdge", edge: 1 } }]);
    });
    await waitFor(() => {
      expect(cablesOf(view, "notes")).toHaveLength(0);
    });
    // (Lane 4 hears both sources now, as the lanes 2 and 5 do.)
    expect(cablesOf(view, "hears").map((path) => path.id)).toEqual(["hears:7>2", "hears:7>4", "hears:7>5", "hears:8>2", "hears:8>4", "hears:8>5"]);
    expect(server.history).toEqual({ undo: "remove a cable of notes", redo: null });
    await client.request({ type: "undo" });
    await waitFor(() => {
      expect(cablesOf(view, "notes").map((path) => path.id)).toEqual(["notes:1"]);
    });
    expect(cablesOf(view, "hears")).toHaveLength(4);
  });

  test("give the focus, when a cable of notes is taken out, to the button that sends notes from the source it started at, which is where the next one is drawn from", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    const menu = within(await nodeOf(7)).getByRole("button", { name: "Send notes from Keys" });
    fireEvent.click(await screen.findByRole("button", { name: "Remove cable Keys → Oscillator 2" }));
    await waitFor(() => {
      expect(document.activeElement).toBe(menu);
    });
  });

  test("follow a cable that somebody else draws: the lane stops hearing the sources by links, and hears the one", async () => {
    const { server, view } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    act(() => {
      server.apply({ type: "addCable", from: { node: 8, port: { type: "eventsOut" } }, to: { node: 2, port: { type: "eventsIn" } }, gain: null });
    });
    await screen.findByRole("button", { name: "Remove cable Launchkey Mini MK4 37 → Oscillator 1" });
    expect(cablesOf(view, "notes").map((path) => path.id)).toEqual(["notes:1", "notes:3"]);
    expect(cablesOf(view, "hears").map((path) => path.id)).toEqual(["hears:7>5", "hears:8>5"]);
  });
});

describe("the menu that sends the notes of a source", () => {
  const buttonIn = async (id: number, name: string): Promise<HTMLButtonElement> => within(await nodeOf(id)).getByRole<HTMLButtonElement>("button", { name: `Send notes from ${name}` });
  const listed = (): string[] => within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent);

  test("is a button on each source, and on no other node", async () => {
    const { view } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    await nodeOf(MASTER);
    const buttons = Array.from(view.container.querySelectorAll('button[aria-label^="Send notes from "]'), (button) => button.getAttribute("aria-label"));
    expect(buttons.sort()).toEqual(["Send notes from Keys", "Send notes from Launchkey Mini MK4 37"]);
  });

  test("has an id of its own among the ids of the page: the number of the node after `notes-to-`, which what takes a cable out gives the focus to", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    expect((await buttonIn(7, "Keys")).id).toBe("notes-to-7");
    expect((await buttonIn(8, "Launchkey Mini MK4 37")).id).toBe("notes-to-8");
  });

  test("lists, when it is opened, the lanes that take notes from the source, by the names the canvas gives them, in the order of the session", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    fireEvent.click(await buttonIn(7, "Keys"));
    // (Lane 4 has a cable from the keys already.)
    expect(listed()).toEqual(["Oscillator 1", "(empty)"]);
  });

  test("lists every lane for a source that has no cable", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    fireEvent.click(await buttonIn(8, "Launchkey Mini MK4 37"));
    expect(listed()).toEqual(["Oscillator 1", "Oscillator 2", "(empty)"]);
  });

  test("draws the cable that is chosen, and the lane is not listed again: it hears the source by the cable, and no longer by the link", async () => {
    const { requests, view } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    const menu = await buttonIn(7, "Keys");
    fireEvent.click(menu);
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Oscillator 1" }));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "addCable", from: { node: 7, port: { type: "eventsOut" } }, to: { node: 2, port: { type: "eventsIn" } }, gain: null } }]);
    });
    await screen.findByRole("button", { name: "Remove cable Keys → Oscillator 1" });
    expect(cablesOf(view, "notes").map((path) => path.id)).toEqual(["notes:1", "notes:3"]);
    expect(cablesOf(view, "hears").map((path) => path.id)).toEqual(["hears:7>5", "hears:8>5"]);
    // (The focus is on the button that opened the menu, so that the next one is a key away.)
    expect(document.activeElement).toBe(menu);
    fireEvent.click(menu);
    expect(listed()).toEqual(["(empty)"]);
  });

  test("follows the session: a cable that is drawn by somebody else takes its lane off the list", async () => {
    const { server } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    const menu = await buttonIn(7, "Keys");
    act(() => {
      server.apply({ type: "addCable", from: { node: 7, port: { type: "eventsOut" } }, to: { node: 5, port: { type: "eventsIn" } }, gain: null });
    });
    await screen.findByRole("button", { name: "Remove cable Keys → (empty)" });
    fireEvent.click(menu);
    expect(listed()).toEqual(["Oscillator 1"]);
  });

  test("is shut when there is nowhere to send: a source that has a cable to every lane", async () => {
    const { server } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    const menu = await buttonIn(7, "Keys");
    expect(menu.disabled).toBe(false);
    act(() => {
      server.apply({ type: "addCable", from: { node: 7, port: { type: "eventsOut" } }, to: { node: 2, port: { type: "eventsIn" } }, gain: null });
      server.apply({ type: "addCable", from: { node: 7, port: { type: "eventsOut" } }, to: { node: 5, port: { type: "eventsIn" } }, gain: null });
    });
    await waitFor(() => {
      expect(menu.disabled).toBe(true);
    });
  });
});

describe("the menu by which a lane takes notes", () => {
  const buttonIn = async (id: number, name: string): Promise<HTMLButtonElement> => within(await nodeOf(id)).getByRole<HTMLButtonElement>("button", { name: `Take notes into ${name}` });
  const listed = (): string[] => within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent);

  test("is a button on each lane, an empty one too, and on no other node: not an effect, a group, a modulator, a source or the master", async () => {
    const { view } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    await nodeOf(MASTER);
    const buttons = Array.from(view.container.querySelectorAll('button[aria-label^="Take notes into "]'), (button) => button.getAttribute("aria-label"));
    expect(buttons.sort()).toEqual(["Take notes into (empty)", "Take notes into Oscillator 1", "Take notes into Oscillator 2"]);
  });

  test("has an id of its own among the ids of the page: the number of the node after `notes-from-`", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    expect((await buttonIn(2, "Oscillator 1")).id).toBe("notes-from-2");
    expect((await buttonIn(5, "(empty)")).id).toBe("notes-from-5");
  });

  test("lists, when it is opened, the sources that have no cable to the lane, by the names the canvas gives them", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    fireEvent.click(await buttonIn(2, "Oscillator 1"));
    expect(listed()).toEqual(["Keys", "Launchkey Mini MK4 37"]);
  });

  test("lists the sources that the lane has no cable from, and not the one it has: lane 4 has a cable from the keys", async () => {
    await rig(<PatchView />, { snapshot, inputs: INPUTS });
    fireEvent.click(await buttonIn(4, "Oscillator 2"));
    expect(listed()).toEqual(["Launchkey Mini MK4 37"]);
  });

  test("draws the cable that is chosen, from the source to the lane, and the source is not listed again", async () => {
    const { requests } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    const menu = await buttonIn(5, "(empty)");
    fireEvent.click(menu);
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Launchkey Mini MK4 37" }));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "addCable", from: { node: 8, port: { type: "eventsOut" } }, to: { node: 5, port: { type: "eventsIn" } }, gain: null } }]);
    });
    await screen.findByRole("button", { name: "Remove cable Launchkey Mini MK4 37 → (empty)" });
    expect(document.activeElement).toBe(menu);
    fireEvent.click(menu);
    expect(listed()).toEqual(["Keys"]);
  });

  test("is shut when no source is left to take notes from: a lane that has a cable from each", async () => {
    const { server } = await rig(<PatchView />, { snapshot, inputs: INPUTS });
    const menu = await buttonIn(4, "Oscillator 2");
    expect(menu.disabled).toBe(false);
    act(() => {
      server.apply({ type: "addCable", from: { node: 8, port: { type: "eventsOut" } }, to: { node: 4, port: { type: "eventsIn" } }, gain: null });
    });
    await waitFor(() => {
      expect(menu.disabled).toBe(true);
    });
  });
});

describe("the card of a lane whose instrument takes no notes (an audio input, step 99c)", () => {
  // The session of the fixture with the instrument of lane 4 saying that it takes no notes, as the controller's graph has it: a graph that has such a lane has no cable of notes to it and no link by which it hears a source (the keys' cable and
  // the device's link, which the fixture has for lane 4, are taken out with that).
  const microphone: Snapshot = structuredClone(snapshot);
  const lane = microphone.graph.nodes.find((node) => node.id === 4);
  if (lane?.kind !== "instrument" || lane.plugin === null) throw new Error("the fixture has an instrument in lane 4");
  lane.plugin.takesNotes = false;
  microphone.graph.edges = microphone.graph.edges.filter((edge) => !((edge.kind === "events" || edge.kind === "hears") && edge.to.node === 4));

  /** The cells of a lane's card that have a small label of their own, by it, in order: its keys, its group, its mute and solo. */
  const cellsOf = (card: HTMLElement): (string | null)[] => Array.from(card.querySelectorAll(".tang-keys__label"), (label) => label.textContent);

  test("has no keys, no pattern, no input of events and no menu that takes notes in, which the other lanes have", async () => {
    const { view } = await rig(<PatchView />, { snapshot: microphone, inputs: INPUTS });
    const card = await nodeOf(4);
    await nodeOf(MASTER);
    expect(nodesWith(view, "eventsIn")).toEqual([2, 5]);
    // (The cell of the group has the style of the keys', which is why the cells are told by their labels: the lane has its group, its mute and solo, and no keys.)
    expect(cellsOf(card)).toEqual(["Group", "Mute / Solo"]);
    expect(card.querySelector(".tang-pattern")).toBeNull();
    expect(within(card).queryByLabelText(/^(Lowest|Highest) key of /)).toBeNull();
    expect(within(card).queryByRole("button", { name: "Take notes into Oscillator 2" })).toBeNull();
    for (const id of [2, 5]) {
      const other = await nodeOf(id);
      expect(cellsOf(other), `lane ${String(id)} has its keys`).toEqual(["Keys", "Group", "Mute / Solo"]);
      expect(other.querySelector(".tang-pattern"), `lane ${String(id)} has its pattern`).not.toBeNull();
    }
    expect(within(await nodeOf(2)).getByRole("button", { name: "Take notes into Oscillator 1" })).toBeDefined();
  });

  test("keeps what a lane has that is not notes: its audio out and the menu that sends it, its volume, its group, its mute and its solo", async () => {
    await rig(<PatchView />, { snapshot: microphone, inputs: INPUTS });
    const card = await nodeOf(4);
    expect(card.querySelector('.react-flow__handle[data-handleid="audioOut"]')).not.toBeNull();
    expect(within(card).getByRole("button", { name: "Send audio from Oscillator 2" })).toBeDefined();
    expect(within(card).getByRole("slider", { name: "Volume" })).toBeDefined();
    expect(within(card).getByRole("combobox", { name: "Group of Oscillator 2" })).toBeDefined();
    expect(within(card).getByRole("button", { name: "Mute Oscillator 2" })).toBeDefined();
    expect(within(card).getByRole("button", { name: "Solo Oscillator 2" })).toBeDefined();
  });

  test("is not offered by the menu that sends the notes of a source: the device lists the lanes that take notes and have no cable from it", async () => {
    await rig(<PatchView />, { snapshot: microphone, inputs: INPUTS });
    fireEvent.click(within(await nodeOf(8)).getByRole("button", { name: "Send notes from Launchkey Mini MK4 37" }));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Oscillator 1", "(empty)"]);
  });

  test("hears no source by a link that is drawn: the links go to the lanes that take notes", async () => {
    const { view } = await rig(<PatchView />, { snapshot: microphone, inputs: INPUTS });
    await nodeOf(MASTER);
    expect(cablesOf(view, "hears").map((path) => path.id).sort()).toEqual(["hears:7>2", "hears:7>5", "hears:8>2", "hears:8>5"]);
    expect(cablesOf(view, "notes")).toHaveLength(0);
  });
});
