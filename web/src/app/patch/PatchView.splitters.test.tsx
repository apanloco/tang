// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { RenderResult } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { splittersFixture } from "../../client/fixtures";
import type { Intent, NodeId, Snapshot } from "../../protocol";
import type { Rig } from "../testing";
import { rig } from "../testing";
import { PatchView } from "./PatchView";
import { SplitterBody } from "./Splitter";

afterEach(cleanup);

// The note splitters on the canvas (step 128 C): the node that a splitter is drawn as, its switch, its two outputs with the ports that a cable is dragged from and the menus that send their notes, what they ask of the server, and the cables
// that go to it and from it. The session is the fixture's of splitters (`app::splitters`, `testutil::SPLITTERS`): the lanes 0, 1 and 2 (all called Oscillator), the keys (3), a device (4, "Launchkey Mini MK4 37"), a box called Bass (5, with no
// cable of notes at it), a splitter called Bass line (6: it picks the lowest note, takes the keys, sends its pick to the first lane and its rest to the second) and one called Splitter (7: it picks the highest note, takes the box and the device,
// and sends its pick to the third lane). What the engine does with the keys is held in `splitterTakes.test.ts`; this is what the page asks and shows.
const found = splittersFixture.cases.find((entry) => entry.name === "splitters");
if (!found) throw new Error("the fixture has the session of the splitters");
const snapshot: Snapshot = found.snapshot;
const [FIRST_LANE, SECOND_LANE, THIRD_LANE, KEYS, DEVICE, BASS, SPLIT, OTHER] = [0, 1, 2, 3, 4, 5, 6, 7];

const nodeOf = (id: NodeId): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);
const cablesOf = (view: RenderResult, kind: "notes" | "hears"): SVGPathElement[] => Array.from(view.container.querySelectorAll<SVGPathElement>(`.tang-cable--${kind}`));
/** The nodes that have a handle of this name, by the id of the node, in order. */
const nodesWith = (view: RenderResult, handle: string): number[] =>
  Array.from(view.container.querySelectorAll(`.react-flow__handle[data-handleid="${handle}"]`), (port) => Number(port.getAttribute("data-nodeid"))).sort((a, b) => a - b);
/** The group of controls of the splitter that is called `name` (the name that the canvas gives it). */
const bodyOf = (name: string): HTMLElement => screen.getByRole("group", { name: `Note splitter ${name}` });
/** The intents about splitters that the server has been asked for, in order. */
const asked = (made: Rig): Intent[] =>
  made
    .requests()
    .flatMap(({ intent }) => (typeof intent === "object" && intent !== null && "type" in intent ? [intent as Intent] : []))
    .filter((intent) => ["addSplitter", "setSplitterField", "addCable", "removeNode"].includes(intent.type));

describe("a note splitter on the canvas", () => {
  test("is a node of a kind of its own for each splitter, with the name the session gives it and, in small type, that it is a note splitter", async () => {
    await rig(<PatchView />, { snapshot });
    for (const [id, name] of [
      [SPLIT, "Bass line"],
      [OTHER, "Splitter"],
    ] as const) {
      const node = await nodeOf(id);
      expect(node.querySelector(".tang-node--splitter"), name).not.toBeNull();
      expect(node.querySelector(".tang-node__name")?.textContent, name).toBe(name);
      expect(node.querySelector(".tang-node__badge")?.textContent, name).toBe("Note splitter");
    }
  });

  test("calls two splitters of one name by a number each, as the canvas does for any nodes, which the controls say too", async () => {
    const twins = structuredClone(snapshot);
    const first = twins.graph.nodes.find((n) => n.id === SPLIT);
    if (first) first.name = "Splitter";
    await rig(<PatchView />, { snapshot: twins });
    expect((await nodeOf(SPLIT)).querySelector(".tang-node__name")?.textContent).toBe("Splitter 1");
    expect((await nodeOf(OTHER)).querySelector(".tang-node__name")?.textContent).toBe("Splitter 2");
    expect(bodyOf("Splitter 1")).toBeDefined();
    expect(bodyOf("Splitter 2")).toBeDefined();
  });

  test("has the menu that takes notes in on the line of its badge, and the buttons that rename it and take it out after the title", async () => {
    await rig(<PatchView />, { snapshot });
    for (const id of [SPLIT, OTHER]) {
      const header = (await nodeOf(id)).querySelector(".tang-node__header");
      const title = header?.querySelector(".tang-node__title");
      expect(Array.from(title?.children ?? [], (child) => child.className), String(id)).toEqual(["tang-node__name", "tang-node__badge", "tang-menu-button nodrag nopan tang-node__notes"]);
      expect(Array.from(title?.querySelectorAll("button") ?? [], (each) => each.textContent), String(id)).toEqual(["▸♫"]);
      expect(Array.from(header?.querySelectorAll(".tang-node__actions button") ?? [], (each) => each.getAttribute("aria-label")), String(id)).toEqual([
        id === SPLIT ? "Rename Bass line" : "Rename Splitter",
        id === SPLIT ? "Remove Bass line" : "Remove Splitter",
      ]);
    }
  });

  test("has a port that notes come in at and two that they leave at, and the canvas draws the cables that end at them and start at them (a node that has no entry among the kinds is drawn empty, and its cables are not drawn)", async () => {
    const view = (await rig(<PatchView />, { snapshot })).view;
    await nodeOf(OTHER);
    // (Lanes take notes in, sources send them out, a box does both, and a splitter takes them in and has a second output.)
    expect(nodesWith(view, "eventsIn")).toEqual([FIRST_LANE, SECOND_LANE, THIRD_LANE, BASS, SPLIT, OTHER]);
    expect(nodesWith(view, "eventsOut")).toEqual([KEYS, DEVICE, BASS, SPLIT, OTHER]);
    expect(nodesWith(view, "eventsOutRest")).toEqual([SPLIT, OTHER]);
    // (Six cables of notes, one of them from a second output; and the two links by which the sources reach the box that no cable of notes ends at.)
    expect(cablesOf(view, "notes")).toHaveLength(6);
    expect(cablesOf(view, "hears")).toHaveLength(2);
    for (const edge of snapshot.graph.edges.filter((each) => each.kind === "events")) {
      expect(screen.getByTestId(`rf__edge-notes:${String(edge.id)}`), String(edge.id)).toBeDefined();
    }
  });

  test("has its second output's port inside the row of that output, and its first inside the row of its own, so that each is where the name beside it says", async () => {
    await rig(<PatchView />, { snapshot });
    const node = await nodeOf(SPLIT);
    const rows = Array.from(node.querySelectorAll<HTMLElement>(".tang-splitter__out"));
    expect(rows.map((row) => row.getAttribute("data-output"))).toEqual(["pick", "rest"]);
    expect(rows.map((row) => row.querySelector(".react-flow__handle")?.getAttribute("data-handleid"))).toEqual(["eventsOut", "eventsOutRest"]);
    expect(rows.map((row) => row.querySelector(".tang-splitter__label")?.textContent)).toEqual(["Lowest", "Rest"]);
    // (The input is at the node and not in a row.)
    expect(node.querySelector(".tang-splitter .react-flow__handle[data-handleid='eventsIn']")).toBeNull();
  });

  test("is the node of every splitter that the session has, the sixteen of a full one included", async () => {
    const full = splittersFixture.cases.find((entry) => entry.name === "full")?.snapshot;
    if (!full) throw new Error("the fixture has a session with sixteen splitters");
    await rig(<PatchView />, { snapshot: full });
    await waitFor(() => {
      expect(screen.getAllByRole("group", { name: /^Note splitter / })).toHaveLength(16);
    });
  });
});

describe("the body of a note splitter", () => {
  test("draws nothing for a node that is no splitter or is not there: a lane, the keys, a box, one that has gone", async () => {
    for (const node of [FIRST_LANE, KEYS, BASS, 9_999]) {
      const made = await rig(<SplitterBody node={node} name="Nothing" />, { snapshot });
      expect(made.view.container.querySelector(".tang-splitter"), String(node)).toBeNull();
      expect(screen.queryByRole("group", { name: "Note splitter Nothing" }), String(node)).toBeNull();
      cleanup();
    }
  });
});

describe("what a splitter shows", () => {
  test("is a switch of the note it picks, with the one it picks pressed, and a name for each output: the note that it picks and the rest", async () => {
    await rig(<PatchView />, { snapshot });
    await nodeOf(SPLIT);
    const pressed = (name: string, label: string): string | null => within(bodyOf(name)).getByRole("button", { name: label }).getAttribute("aria-pressed");
    expect([pressed("Bass line", "Pick the lowest note with Bass line"), pressed("Bass line", "Pick the highest note with Bass line")]).toEqual(["true", "false"]);
    expect([pressed("Splitter", "Pick the lowest note with Splitter"), pressed("Splitter", "Pick the highest note with Splitter")]).toEqual(["false", "true"]);
    const outputs = (name: string): (string | null | undefined)[] => Array.from(bodyOf(name).querySelectorAll(".tang-splitter__out .tang-splitter__label"), (label) => label.textContent);
    expect(outputs("Bass line")).toEqual(["Lowest", "Rest"]);
    expect(outputs("Splitter")).toEqual(["Highest", "Rest"]);
  });

  test("says in the hint of each choice what it does, and in the label what splitter it is of", async () => {
    await rig(<PatchView />, { snapshot });
    await nodeOf(SPLIT);
    const choice = (label: string): HTMLElement => within(bodyOf("Bass line")).getByRole("button", { name: label });
    expect(choice("Pick the lowest note with Bass line").textContent).toBe("Lowest");
    expect(choice("Pick the highest note with Bass line").textContent).toBe("Highest");
    expect(choice("Pick the lowest note with Bass line").getAttribute("title")).toBe("The lowest key that is held goes to the first output (the bass of a chord), and every other key to the second");
    expect(choice("Pick the highest note with Bass line").getAttribute("title")).toBe("The highest key that is held goes to the first output (the top of a chord), and every other key to the second");
  });

  test("is a group that a person who does not see it is told of by name, and its switch is a switch that takes no key of the piano: a node for the canvas to leave the keys of the computer to the page", async () => {
    await rig(<PatchView />, { snapshot });
    const node = await nodeOf(SPLIT);
    expect(bodyOf("Bass line").className).toContain("nodrag");
    expect(node.querySelector(".tang-node--splitter")?.className).toContain("nokey");
  });
});

describe("the switch of a splitter", () => {
  test("asks the server for the note that it says, and the node then says which it picks, both its first output and its menu following it", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(SPLIT);
    fireEvent.click(within(bodyOf("Bass line")).getByRole("button", { name: "Pick the highest note with Bass line" }));
    await waitFor(() => {
      expect(within(bodyOf("Bass line")).getByRole("button", { name: "Pick the highest note with Bass line" }).getAttribute("aria-pressed")).toBe("true");
    });
    expect(within(bodyOf("Bass line")).getByRole("button", { name: "Pick the lowest note with Bass line" }).getAttribute("aria-pressed")).toBe("false");
    expect(asked(made)).toEqual([{ type: "setSplitterField", node: SPLIT, field: { type: "pick", pick: "highest" } }]);
    // (The first output is called by the note that it sends now, and so is the label of its menu.)
    expect(Array.from(bodyOf("Bass line").querySelectorAll(".tang-splitter__out .tang-splitter__label"), (label) => label.textContent)).toEqual(["Highest", "Rest"]);
    expect(within(bodyOf("Bass line")).getByRole("button", { name: "Send the highest note from Bass line" })).toBeDefined();
  });

  test("asks again for the note that it picks already, and the controller says it has it: the node is as it was", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(SPLIT);
    fireEvent.click(within(bodyOf("Bass line")).getByRole("button", { name: "Pick the lowest note with Bass line" }));
    await waitFor(() => {
      expect(asked(made)).toHaveLength(1);
    });
    expect(within(bodyOf("Bass line")).getByRole("button", { name: "Pick the lowest note with Bass line" }).getAttribute("aria-pressed")).toBe("true");
  });
});

describe("the name of a splitter", () => {
  test("is changed in place: the pencil turns it into a field, Enter asks the server for what was typed, and the focus goes back to the pencil", async () => {
    const made = await rig(<PatchView />, { snapshot });
    const node = await nodeOf(OTHER);
    fireEvent.click(within(node).getByRole("button", { name: "Rename Splitter" }));
    const field = within(node).getByRole<HTMLInputElement>("textbox", { name: "Name of Splitter" });
    expect(field.value).toBe("Splitter");
    fireEvent.change(field, { target: { value: "  Lead  " } });
    fireEvent.keyDown(field, { key: "Enter" });
    await waitFor(() => {
      expect(within(node).getByRole("button", { name: "Rename Lead" })).toBeDefined();
    });
    expect(node.querySelector(".tang-node__name")?.textContent).toBe("Lead");
    expect(bodyOf("Lead")).toBeDefined();
    expect(asked(made)).toEqual([{ type: "setSplitterField", node: OTHER, field: { type: "name", name: "  Lead  " } }]);
    await waitFor(() => {
      expect(document.activeElement?.id).toBe(`rename-splitter-${String(OTHER)}`);
    });
  });

  test("is left as it was by Escape, and by a name that is the one it has: nothing is asked", async () => {
    const made = await rig(<PatchView />, { snapshot });
    const node = await nodeOf(OTHER);
    fireEvent.click(within(node).getByRole("button", { name: "Rename Splitter" }));
    fireEvent.change(within(node).getByRole("textbox", { name: "Name of Splitter" }), { target: { value: "Lead" } });
    fireEvent.keyDown(within(node).getByRole("textbox", { name: "Name of Splitter" }), { key: "Escape" });
    expect(within(node).queryByRole("textbox")).toBeNull();
    fireEvent.click(within(node).getByRole("button", { name: "Rename Splitter" }));
    fireEvent.keyDown(within(node).getByRole("textbox", { name: "Name of Splitter" }), { key: "Enter" });
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(asked(made)).toEqual([]);
    expect(node.querySelector(".tang-node__name")?.textContent).toBe("Splitter");
  });
});

describe("the menus of notes of a splitter", () => {
  test("show the glyphs of what they do: an arrow towards the note on the menu that takes notes in, and one away from it on each of the two that send them", async () => {
    await rig(<PatchView />, { snapshot });
    const node = await nodeOf(SPLIT);
    expect(within(node).getByRole("button", { name: "Take notes into Bass line" }).textContent).toBe("▸♫");
    expect(within(node).getByRole("button", { name: "Send the lowest note from Bass line" }).textContent).toBe("♫▸");
    expect(within(node).getByRole("button", { name: "Send the other notes from Bass line" }).textContent).toBe("♫▸");
    // (The second splitter picks the highest.)
    expect(within(await nodeOf(OTHER)).getByRole("button", { name: "Send the highest note from Splitter" })).toBeDefined();
  });

  test("send the notes of the output that they are of: the first has the lanes that have no cable from the pick, the second those that have none from the rest", async () => {
    const made = await rig(<PatchView />, { snapshot });
    const node = await nodeOf(SPLIT);
    // (The pick of the first splitter has a cable to the first lane and its rest one to the second: the three lanes are called alike and the canvas numbers them.)
    fireEvent.click(within(node).getByRole("button", { name: "Send the lowest note from Bass line" }));
    expect(screen.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Oscillator 2", "Oscillator 3"]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator 3" }));
    await waitFor(() => {
      expect(asked(made)).toEqual([{ type: "addCable", from: { node: SPLIT, port: { type: "eventsOut" } }, to: { node: THIRD_LANE, port: { type: "eventsIn" } }, gain: null }]);
    });
    fireEvent.click(within(node).getByRole("button", { name: "Send the other notes from Bass line" }));
    expect(screen.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Oscillator 1", "Oscillator 3"]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator 1" }));
    await waitFor(() => {
      expect(asked(made)).toHaveLength(2);
    });
    expect(asked(made)[1]).toEqual({ type: "addCable", from: { node: SPLIT, port: { type: "eventsOutRest" } }, to: { node: FIRST_LANE, port: { type: "eventsIn" } }, gain: null });
  });

  test("take notes in from a source or a box that has no cable to the splitter, and no splitter's, and the cable ends at the splitter", async () => {
    const made = await rig(<PatchView />, { snapshot });
    const node = await nodeOf(OTHER);
    fireEvent.click(within(node).getByRole("button", { name: "Take notes into Splitter" }));
    // (The second splitter has the box and the device already: the keys are left.)
    expect(screen.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keys"]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Keys" }));
    await waitFor(() => {
      expect(asked(made)).toEqual([{ type: "addCable", from: { node: KEYS, port: { type: "eventsOut" } }, to: { node: OTHER, port: { type: "eventsIn" } }, gain: null }]);
    });
  });

  test("are what a lane takes a splitter's notes by, one item for each output, by what it sends: the lane that has a cable from the pick of one is offered its rest and the pick of the other", async () => {
    const made = await rig(<PatchView />, { snapshot });
    const lane = await nodeOf(THIRD_LANE);
    fireEvent.click(within(lane).getByRole("button", { name: /^Take notes into / }));
    expect(screen.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keys", "Launchkey Mini MK4 37", "Bass", "Bass line · lowest", "Bass line · rest", "Splitter · rest"]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Bass line · rest" }));
    await waitFor(() => {
      expect(asked(made)).toEqual([{ type: "addCable", from: { node: SPLIT, port: { type: "eventsOutRest" } }, to: { node: THIRD_LANE, port: { type: "eventsIn" } }, gain: null }]);
    });
  });
});

describe("the cables of a splitter", () => {
  test("are what take the splitter out with it: the cables of notes at it go, and the lanes it sent to hear what their other cables bring, or every source", async () => {
    const made = await rig(<PatchView />, { snapshot });
    const view = made.view;
    await nodeOf(SPLIT);
    expect(cablesOf(view, "notes")).toHaveLength(6);
    fireEvent.click(within(await nodeOf(SPLIT)).getByRole("button", { name: "Remove Bass line" }));
    await waitFor(() => {
      expect(screen.queryByTestId(`rf__node-${String(SPLIT)}`)).toBeNull();
    });
    expect(asked(made)).toEqual([{ type: "removeNode", node: SPLIT }]);
    // (The cable from the keys to it and the two from it to the first and the second lane went with it; the first two lanes have none now and hear both sources.)
    await waitFor(() => {
      expect(cablesOf(view, "notes")).toHaveLength(3);
    });
  });

  test("give the focus back, when one is taken out by its button, to the menu of the output that it started at: the second output's menu for the cable from the rest, the first's for the cable from the pick", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(SPLIT);
    fireEvent.click(screen.getByRole("button", { name: "Remove cable Bass line (rest) → Oscillator 2" }));
    await waitFor(() => {
      expect(document.activeElement?.id).toBe(`notes-to-rest-${String(SPLIT)}`);
    });
    fireEvent.click(screen.getByRole("button", { name: "Remove cable Bass line → Oscillator 1" }));
    await waitFor(() => {
      expect(document.activeElement?.id).toBe(`notes-to-${String(SPLIT)}`);
    });
    // (A cable is taken out by `removeEdge`, once for each.)
    expect(made.requests().map(({ intent }) => (intent as Intent).type)).toEqual(["removeEdge", "removeEdge"]);
  });

  test("tell the cable that starts at the second output from the one at the first, and the remove button of each says so: a lane can have a cable from each output of one splitter", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(SPLIT);
    // (The cable from the rest of the first splitter to the second lane is cable 4: its button is named after the output.)
    const buttons = screen.getAllByRole("button", { name: /^Remove cable / }).map((each) => each.getAttribute("aria-label"));
    expect(buttons).toContain("Remove cable Bass line → Oscillator 1");
    expect(buttons).toContain("Remove cable Bass line (rest) → Oscillator 2");
    expect(asked(made)).toEqual([]);
  });
});
