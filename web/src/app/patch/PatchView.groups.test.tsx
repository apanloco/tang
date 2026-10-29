// @vitest-environment jsdom
// Groups, from the page: every lane says which group it is in and puts itself in another (or in a new one), a group is given a name, and a lane
// or a group is taken out, after the page has asked a person whether to (a lane that has an instrument can have the instrument taken out alone).
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { MASTER, groupsFixture, tracerSnapshot } from "../../client/fixtures";
import type { Node } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// The session of the groups fixture: three groups (0, 1 and 2; the last is empty and called Lead; two modulators hear the first, 13 and 14, and one
// the second, 15), four lanes (3 and 7 in the first group, 6 in the second, 4 in none, which has a filter), an effect in two of them (5 and 8), and a modulator
// that hears each of the lanes 3 and 7 (9 and 12) and two that hear the lane 4 (10 and 11). The canvas numbers the nodes called alike across the session in the
// order of the graph: the lanes are Oscillator 1 to 4.
const session = { snapshot: groupsFixture.snapshot };
const LANES = [3, 4, 6, 7];

const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);
const laneName = (id: number): string => `Oscillator ${String(LANES.indexOf(id) + 1)}`;

/** The choice of group of the lane `id`. */
const choice = async (id: number): Promise<HTMLSelectElement> => within(await nodeOf(id)).findByRole<HTMLSelectElement>("combobox", { name: `Group of ${laneName(id)}` });

/** What a choice shows as chosen, in words. */
const chosen = (select: HTMLSelectElement): string | undefined => select.selectedOptions[0]?.textContent ?? undefined;

function Notices() {
  return <NoticeBoard notices={useNotices()} />;
}

/** Let what the page is going to do have its turn: a request leaves a moment after the click that makes it, so that "nothing was asked" is true of a moment that is too short. */
const settle = (): Promise<void> =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30));
  });

describe("the group of a lane", () => {
  test("is chosen on every lane, by a choice that names the lane and shows its group, or none, and on no other node", async () => {
    await rig(<PatchView />, session);
    expect(await Promise.all(LANES.map(async (id) => chosen(await choice(id))))).toEqual(["Group 1", "none", "Group 2", "Group 1"]);
    expect(await Promise.all(LANES.map(async (id) => (await choice(id)).value))).toEqual(["0", "", "1", "0"]);
    // A group, an effect, a modulator and the master are not in a group.
    for (const id of [0, 1, 2, 5, 8, 9, 13, 14, 15, MASTER]) {
      expect(within(await nodeOf(id)).queryByRole("combobox", { name: /^Group of / }), `node ${String(id)}`).toBeNull();
    }
    expect(screen.getAllByRole("combobox", { name: /^Group of / })).toHaveLength(4);
  });

  test("offers none and each group by the name it has, in that order, and no new group: the page offers no way to make one", async () => {
    await rig(<PatchView />, session);
    const options = Array.from((await choice(4)).options).map((option) => [option.value, option.textContent]);
    expect(options).toEqual([
      ["", "none"],
      ["0", "Group 1"],
      ["1", "Group 2"],
      ["2", "Lead"],
    ]);
  });

  test("is not there on any lane while the session has no group, is there when the session gets one (the terminal makes one), and goes when the last is taken out", async () => {
    const { server } = await rig(<PatchView />);
    await screen.findAllByRole("combobox", { name: /^Lowest key of / });
    expect(screen.queryAllByRole("combobox", { name: /^Group of / })).toHaveLength(0);
    server.apply({ type: "addGroup", members: [] });
    const made = await screen.findByRole<HTMLSelectElement>("combobox", { name: "Group of Oscillator" });
    expect(Array.from(made.options).map((option) => option.textContent)).toEqual(["none", "Group 1"]);
    const group = server.snapshot.graph.nodes.find((node) => node.kind === "group");
    expect(group).toBeDefined();
    server.apply({ type: "removeNode", node: group?.id ?? -1 });
    await waitFor(() => {
      expect(screen.queryAllByRole("combobox", { name: /^Group of / })).toHaveLength(0);
    });
  });

  test("asks for the lane to be put in the group that is chosen, and the canvas draws its audio into that group when the server has done it", async () => {
    const { requests } = await rig(<PatchView />, session);
    const select = await choice(4);
    fireEvent.change(select, { target: { value: "2" } });
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "setLaneGroup", node: 4, group: 2 }]);
    });
    // The last link of the lane's chain (its filter, 5) goes into the group (2) and not to the master.
    await waitFor(() => {
      expect(document.querySelector(`[id="audio:5>2"]`)).toBeTruthy();
    });
    expect(document.querySelector(`[id="audio:5>${String(MASTER)}"]`)).toBeNull();
    expect(chosen(await choice(4))).toBe("Lead");
  });

  test("takes the lane out of its group for none, and the cables stay: a cable names the plugin it drives, and the modulators that hear the group go on hearing it", async () => {
    const { requests, view } = await rig(<PatchView />, session);
    // (All five cables are drawn, the one that drives the depth of another's too.)
    const cables = (): number => view.container.querySelectorAll(".tang-cable--modulation").length;
    await waitFor(() => {
      expect(cables()).toBe(5);
    });
    fireEvent.change(await choice(3), { target: { value: "" } });
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "setLaneGroup", node: 3, group: null }]);
    });
    await waitFor(async () => {
      expect(chosen(await choice(3))).toBe("none");
    });
    expect(cables()).toBe(5);
  });

  test("has a group that somebody else makes among every lane's options, and the lane that is put in it when it is made is in it", async () => {
    const { server } = await rig(<PatchView />, session);
    expect(chosen(await choice(4))).toBe("none");
    server.apply({ type: "addGroup", members: [4] });
    await waitFor(async () => {
      expect(chosen(await choice(4))).toBe("Group 4");
    });
    expect((await choice(4)).value).toBe("16");
    expect(Array.from((await choice(6)).options).map((option) => option.textContent)).toEqual(["none", "Group 1", "Group 2", "Lead", "Group 4"]);
  });

  test("follows a group that somebody else names, and a lane that somebody else puts in another group", async () => {
    const { server } = await rig(<PatchView />, session);
    expect(chosen(await choice(6))).toBe("Group 2");
    server.apply({ type: "renameGroup", node: 1, name: "Strings" });
    await waitFor(async () => {
      expect(chosen(await choice(6))).toBe("Strings");
    });
    server.apply({ type: "setLaneGroup", node: 6, group: 2 });
    await waitFor(async () => {
      expect(chosen(await choice(6))).toBe("Lead");
    });
  });

  test("says in its tooltip which group the lane is in, by the whole name that the narrow choice may cut, and follows the name", async () => {
    const { server } = await rig(<PatchView />, session);
    expect(await Promise.all(LANES.map(async (id) => (await choice(id)).title))).toEqual([
      "Oscillator 1 is in Group 1",
      "Oscillator 2 is in no group",
      "Oscillator 3 is in Group 2",
      "Oscillator 4 is in Group 1",
    ]);
    server.apply({ type: "renameGroup", node: 1, name: "Strings and the brass of the second row" });
    await waitFor(async () => {
      expect((await choice(6)).title).toBe("Oscillator 3 is in Strings and the brass of the second row");
    });
    server.apply({ type: "setLaneGroup", node: 6, group: null });
    await waitFor(async () => {
      expect((await choice(6)).title).toBe("Oscillator 3 is in no group");
    });
  });

  test("says why when the lane could not be put in the group, and goes back to the group the session says", async () => {
    const { server, client } = await rig(
      <>
        <PatchView />
        <Notices />
      </>,
      session,
    );
    const select = await choice(4);
    server.drop();
    await waitFor(() => {
      expect(client.state.status).not.toBe("open");
    });
    fireEvent.change(select, { target: { value: "1" } });
    expect(await screen.findByText("Could not put Oscillator 2 in Group 2: Not connected to the server.")).toBeTruthy();
    await waitFor(async () => {
      expect(chosen(await choice(4))).toBe("none");
    });
  });
});

describe("naming a group", () => {
  const rename = (name: string): string => `Rename ${name}`;

  test("is offered on every group by a button that names it, and on no other node", async () => {
    await rig(<PatchView />, session);
    for (const [id, name] of [[0, "Group 1"], [1, "Group 2"], [2, "Lead"]] as const) {
      expect(within(await nodeOf(id)).getByRole("button", { name: rename(name) })).toBeTruthy();
    }
    expect(screen.getAllByRole("button", { name: /^Rename / })).toHaveLength(3);
  });

  test("turns the name into a field with the name in it, that takes as many characters as a name can have, and the name of nothing is not asked for yet", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(within(await nodeOf(2)).getByRole("button", { name: rename("Lead") }));
    const field = await within(await nodeOf(2)).findByRole<HTMLInputElement>("textbox", { name: "Name of Lead" });
    expect(field.value).toBe("Lead");
    expect(field.maxLength).toBe(64);
    expect(document.activeElement).toBe(field);
    // The name is selected, so that typing replaces it.
    expect([field.selectionStart, field.selectionEnd]).toEqual([0, 4]);
    await settle();
    expect(requests()).toEqual([]);
  });

  test("asks for the name that was typed when Enter is pressed, and the group is called so, in the choices of the lanes as well", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(within(await nodeOf(2)).getByRole("button", { name: rename("Lead") }));
    const field = await within(await nodeOf(2)).findByRole<HTMLInputElement>("textbox", { name: "Name of Lead" });
    fireEvent.change(field, { target: { value: "Strings" } });
    fireEvent.keyDown(field, { key: "Enter" });
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "renameGroup", node: 2, name: "Strings" }]);
    });
    // The field is a name again, with the buttons of the group named by it.
    expect(await within(await nodeOf(2)).findByRole("button", { name: rename("Strings") })).toBeTruthy();
    expect(within(await nodeOf(2)).getByRole("button", { name: "Add an effect to Strings" })).toBeTruthy();
    expect(within(await nodeOf(2)).queryByRole("textbox")).toBeNull();
    expect(Array.from((await choice(4)).options).map((option) => option.textContent)).toContain("Strings");
    // The button that was pressed has the focus back.
    await waitFor(() => {
      expect(document.activeElement).toBe(within(screen.getByTestId("rf__node-2")).getByRole("button", { name: rename("Strings") }));
    });
  });

  test("takes the Enter that ends the name: it is not left to the button that has the focus then, which a browser would press, and the field would open again", async () => {
    await rig(<PatchView />, session);
    fireEvent.click(within(await nodeOf(2)).getByRole("button", { name: rename("Lead") }));
    const field = await within(await nodeOf(2)).findByRole<HTMLInputElement>("textbox", { name: "Name of Lead" });
    // (`fireEvent` says whether the key was left alone: false is a key that was taken.)
    expect(fireEvent.keyDown(field, { key: "Enter" })).toBe(false);
    fireEvent.click(within(await nodeOf(2)).getByRole("button", { name: rename("Lead") }));
    const again = await within(await nodeOf(2)).findByRole("textbox", { name: "Name of Lead" });
    expect(fireEvent.keyDown(again, { key: "Escape" })).toBe(false);
    // Any other key is the field's own, and the field stays.
    fireEvent.click(within(await nodeOf(2)).getByRole("button", { name: rename("Lead") }));
    const typing = await within(await nodeOf(2)).findByRole("textbox", { name: "Name of Lead" });
    for (const key of ["a", "z", "Backspace", "Tab", " ", "ArrowLeft"]) expect(fireEvent.keyDown(typing, { key }), key).toBe(true);
    expect(within(await nodeOf(2)).getByRole("textbox", { name: "Name of Lead" })).toBe(typing);
  });

  test("asks for the name when the field is left, and for nothing when Escape is pressed, or when the name is the one it was", async () => {
    const { requests } = await rig(<PatchView />, session);
    const open = async (name: string): Promise<HTMLInputElement> => {
      fireEvent.click(within(await nodeOf(2)).getByRole("button", { name: rename(name) }));
      return within(await nodeOf(2)).findByRole<HTMLInputElement>("textbox", { name: `Name of ${name}` });
    };
    // Escape: no change, and the name as it was.
    let field = await open("Lead");
    fireEvent.change(field, { target: { value: "Strings" } });
    fireEvent.keyDown(field, { key: "Escape" });
    expect(await within(await nodeOf(2)).findByRole("button", { name: rename("Lead") })).toBeTruthy();
    // The same name: nothing to ask.
    field = await open("Lead");
    fireEvent.keyDown(field, { key: "Enter" });
    expect(await within(await nodeOf(2)).findByRole("button", { name: rename("Lead") })).toBeTruthy();
    await settle();
    expect(requests()).toEqual([]);
    // Left with another name in it: that is the name, and the focus is not taken to the button: the person has gone somewhere else.
    field = await open("Lead");
    fireEvent.change(field, { target: { value: "Pad" } });
    fireEvent.blur(field);
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "renameGroup", node: 2, name: "Pad" }]);
    });
    const renamed = await within(await nodeOf(2)).findByRole("button", { name: rename("Pad") });
    await settle();
    expect(document.activeElement).not.toBe(renamed);
  });

  test("asks for a name of nothing as it is typed, which gives the group the name it has by default", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(within(await nodeOf(2)).getByRole("button", { name: rename("Lead") }));
    const field = await within(await nodeOf(2)).findByRole<HTMLInputElement>("textbox", { name: "Name of Lead" });
    fireEvent.change(field, { target: { value: "   " } });
    fireEvent.keyDown(field, { key: "Enter" });
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "renameGroup", node: 2, name: "   " }]);
    });
    expect(await within(await nodeOf(2)).findByRole("button", { name: rename("Group 3") })).toBeTruthy();
  });

  test("does not take a key typed in the field for a note or for the canvas: the field is for text", async () => {
    const { server } = await rig(<PatchView />, session);
    fireEvent.click(within(await nodeOf(2)).getByRole("button", { name: rename("Lead") }));
    const field = await within(await nodeOf(2)).findByRole("textbox", { name: "Name of Lead" });
    fireEvent.keyDown(field, { key: "z" });
    fireEvent.keyDown(field, { key: "Backspace" });
    expect(server.received.filter((message) => message.type === "noteOn" || message.type === "noteOff")).toEqual([]);
  });

  test("says why a name could not be given, and the field goes back to the name", async () => {
    const { server, client } = await rig(
      <>
        <PatchView />
        <Notices />
      </>,
      session,
    );
    fireEvent.click(within(await nodeOf(2)).getByRole("button", { name: rename("Lead") }));
    const field = await within(await nodeOf(2)).findByRole<HTMLInputElement>("textbox", { name: "Name of Lead" });
    server.drop();
    await waitFor(() => {
      expect(client.state.status).not.toBe("open");
    });
    fireEvent.change(field, { target: { value: "Strings" } });
    fireEvent.keyDown(field, { key: "Enter" });
    expect(await screen.findByText('Could not give Lead the name "Strings": Not connected to the server.')).toBeTruthy();
    expect(await within(await nodeOf(2)).findByRole("button", { name: rename("Lead") })).toBeTruthy();
  });
});

describe("taking a lane or a group out", () => {
  const removeButton = async (id: number, name: string): Promise<HTMLElement> => within(await nodeOf(id)).findByRole("button", { name });

  test("is offered on every group and every lane, but not on the only lane of a session, and not on any other node", async () => {
    await rig(<PatchView />, session);
    for (const [id, name] of [[0, "Remove Group 1"], [1, "Remove Group 2"], [2, "Remove Lead"], [3, "Remove the lane of Oscillator 1"], [4, "Remove the lane of Oscillator 2"], [6, "Remove the lane of Oscillator 3"], [7, "Remove the lane of Oscillator 4"]] as const) {
      expect(await removeButton(id, name), name).toBeTruthy();
    }
    expect(within(await nodeOf(MASTER)).queryByRole("button", { name: /^Remove / })).toBeNull();
    cleanup();
    // (The tracer, which has one lane: a session keeps a lane, so that it cannot be taken out, but its instrument can: the menu offers that and not the lane's removal.)
    await rig(<PatchView />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove the lane of Oscillator" }));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keep Oscillator", "Remove Oscillator only: its 1 effect stays in the lane"]);
    cleanup();
    // A lane that has no instrument, and is the only one, has nothing to be taken out of it and cannot be taken out: no button. (It has a key range, which is what has it a card: a lane that has nothing but
    // effects has none, and its effects stand by themselves.)
    const nodes = tracerSnapshot.graph.nodes.map((node) => (node.kind === "instrument" ? { ...node, name: "(empty)", plugin: null, range: { low: 36, high: 60 } } : node));
    await rig(<PatchView />, { snapshot: { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } } });
    await screen.findByRole("button", { name: "Choose an instrument for (empty)" });
    expect(screen.queryByRole("button", { name: /^Remove the lane of / })).toBeNull();
  });

  test("is offered on a lane that has no instrument when there is another lane, and takes the lane out and nothing less", async () => {
    const nodes = groupsFixture.snapshot.graph.nodes.map((node) => (node.id === 6 && node.kind === "instrument" ? { ...node, name: "(empty)", plugin: null } : node));
    const { requests } = await rig(<PatchView />, { snapshot: { ...groupsFixture.snapshot, graph: { ...groupsFixture.snapshot.graph, nodes } } });
    // (The canvas numbers the three oscillators that are left, 1 to 3: the empty lane is called what it is.)
    fireEvent.click(await removeButton(6, "Remove the lane of (empty)"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keep (empty)", "Remove the lane of (empty) for good"]);
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove the lane of \(empty\) for good/ }));
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "removeNode", node: 6 }]);
    });
  });

  test("asks whether to, in words that say what goes and what stays, and asks the server nothing until it is said", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(await removeButton(0, "Remove Group 1"));
    const menu = screen.getByRole("menu", { name: "Remove Group 1" });
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Keep Group 1",
      "Remove Group 1 for good: its 2 lanes stay, in no group, and 2 modulators that hear it stay, hearing every note",
    ]);
    await settle();
    expect(requests()).toEqual([]);
    fireEvent.keyDown(menu, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    // A lane says what stays in it if its instrument alone is taken out, and what goes with it if it is taken out, and which modulators hear it; a group with nothing in it, nothing.
    fireEvent.click(await removeButton(4, "Remove the lane of Oscillator 2"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Keep Oscillator 2",
      "Remove Oscillator 2 only: its 1 effect stays in the lane",
      "Remove the lane of Oscillator 2 for good: its 1 effect goes with it, and 2 modulators that hear it stay, hearing every note",
    ]);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    // One modulator hears it, or one effect goes and one modulator hears it, or nothing is said.
    for (const [id, name, only, whole] of [
      [3, "Oscillator 1", "the lane stays, empty", "1 modulator that hears it stays, hearing every note"],
      [7, "Oscillator 4", "its 1 effect stays in the lane", "its 1 effect goes with it, and 1 modulator that hears it stays, hearing every note"],
      [6, "Oscillator 3", "the lane stays, empty", null],
    ] as const) {
      fireEvent.click(await removeButton(id, `Remove the lane of ${name}`));
      expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent), name).toEqual([
        `Keep ${name}`,
        `Remove ${name} only: ${only}`,
        `Remove the lane of ${name} for good${whole === null ? "" : `: ${whole}`}`,
      ]);
      fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    }
    // One lane stays, and one modulator hears the group.
    fireEvent.click(await removeButton(1, "Remove Group 2"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Keep Group 2",
      "Remove Group 2 for good: its 1 lane stays, in no group, and 1 modulator that hears it stays, hearing every note",
    ]);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    fireEvent.click(await removeButton(2, "Remove Lead"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keep Lead", "Remove Lead for good"]);
    await settle();
    expect(requests()).toEqual([]);
  });

  test("counts the modulators that hear a lane or a group that the session has besides those of the fixture, and says what goes with it", async () => {
    const own = (id: number, hears: number): Node => ({ id, parent: null, name: `LFO ${String(id)}`, kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1, retrigger: false, fade: 0, sync: 0 }, hears });
    const base = groupsFixture.snapshot;
    const master = base.graph.nodes.findIndex((node) => node.kind === "master");
    const nodes = [...base.graph.nodes.slice(0, master), own(100, 4), own(101, 4), own(102, 6), own(103, 0), ...base.graph.nodes.slice(master)];
    await rig(<PatchView />, { snapshot: { ...base, graph: { ...base.graph, nodes } } });
    // A lane: its 1 effect goes with it, and the 4 that hear it stay (a lane that is taken out alone leaves its instrument's hearers as they were: they hear the lane still).
    fireEvent.click(await removeButton(4, "Remove the lane of Oscillator 2"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Keep Oscillator 2",
      "Remove Oscillator 2 only: its 1 effect stays in the lane",
      "Remove the lane of Oscillator 2 for good: its 1 effect goes with it, and 4 modulators that hear it stay, hearing every note",
    ]);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    // One of them: "hears it stays"; and a lane that has nothing else of its own says only that.
    fireEvent.click(await removeButton(6, "Remove the lane of Oscillator 3"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Keep Oscillator 3",
      "Remove Oscillator 3 only: the lane stays, empty",
      "Remove the lane of Oscillator 3 for good: 1 modulator that hears it stays, hearing every note",
    ]);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    // A group: its lanes that stay, and the three that hear it.
    fireEvent.click(await removeButton(0, "Remove Group 1"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Keep Group 1",
      "Remove Group 1 for good: its 2 lanes stay, in no group, and 3 modulators that hear it stay, hearing every note",
    ]);
  });

  test("keeps what it is asked to keep, and asks the server nothing", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(await removeButton(1, "Remove Group 2"));
    fireEvent.click(screen.getByRole("menuitem", { name: "Keep Group 2" }));
    expect(screen.queryByRole("menu")).toBeNull();
    await settle();
    expect(requests()).toEqual([]);
    expect(await nodeOf(1)).toBeTruthy();
  });

  test("takes a group out when it is said to, and its lanes are on the canvas in no group", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(await removeButton(0, "Remove Group 1"));
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove Group 1 for good/ }));
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "removeNode", node: 0 }]);
    });
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-0")).toBeNull();
    });
    // The modulators that heard it stay, hearing every note; its lanes (3 and 7) stay, in no group, and the group after it is the first now (the lane in it says so).
    expect(await nodeOf(13)).toBeTruthy();
    expect(await nodeOf(14)).toBeTruthy();
    expect(await nodeOf(3)).toBeTruthy();
    expect([chosen(await choice(3)), chosen(await choice(7)), chosen(await choice(6))]).toEqual(["none", "none", "Group 1"]);
  });

  test("takes a lane out when it is said to, with its effects: the modulators that heard it stay", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(await removeButton(4, "Remove the lane of Oscillator 2"));
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove the lane of Oscillator 2 for good/ }));
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "removeNode", node: 4 }]);
    });
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-4")).toBeNull();
    });
    expect(screen.queryByTestId("rf__node-5"), "its filter").toBeNull();
    // The two LFOs that heard it are still there, hearing every note.
    for (const stays of [10, 11]) expect(await nodeOf(stays), `node ${String(stays)}`).toBeTruthy();
    expect(screen.getAllByRole("combobox", { name: /^Group of / })).toHaveLength(3);
  });

  test("takes the instrument out of a lane alone when it is said to: the lane stays, with its effect, which stands by itself, and nothing else on the canvas changes", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(await removeButton(4, "Remove the lane of Oscillator 2"));
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove Oscillator 2 only/ }));
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "clearInstrument", node: 4 }]);
    });
    // The lane has nothing but its filter now, and the canvas draws no card for it: the filter is on it, as every other node is, and has what the lane's card had of the lane as a whole (it is the
    // first of its effects); the two LFOs that hear the lane are where they were.
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-4")).toBeNull();
    });
    expect(screen.queryByRole("button", { name: "Choose an instrument for (empty)" })).toBeNull();
    for (const stays of [0, 1, 2, 3, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, MASTER]) expect(screen.getByTestId(`rf__node-${String(stays)}`), `node ${String(stays)}`).toBeTruthy();
    const filter = await nodeOf(5);
    expect(within(filter).getByRole("button", { name: "Mute Filter" })).toBeTruthy();
    expect(within(filter).getByRole("button", { name: "Add a modulator to Filter" })).toBeTruthy();
    expect(screen.getAllByRole("combobox", { name: /^Group of / })).toHaveLength(3);
    // The filter is all that the lane has: its "×" takes the lane with it, and the LFOs that heard the lane stay, hearing every note.
    fireEvent.click(within(filter).getByRole("button", { name: "Remove Filter" }));
    await waitFor(() => {
      expect(requests().map((request) => request.intent).at(-1)).toEqual({ type: "removeNode", node: 4 });
    });
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-5")).toBeNull();
    });
    for (const stays of [10, 11]) expect(await nodeOf(stays), `node ${String(stays)}`).toBeTruthy();
  });

  test("takes the instrument out of the only lane of a session and leaves the lane: its effect and its LFO stand by themselves, and the lane has no card", async () => {
    const { requests } = await rig(<PatchView />);
    fireEvent.click(await screen.findByRole("button", { name: "Remove the lane of Oscillator" }));
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove Oscillator only/ }));
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "clearInstrument", node: 0 }]);
    });
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-0")).toBeNull();
    });
    // The lane is the only one and has no instrument: nothing is left to take out of it, and it is not taken out. Its filter is the one that is on the canvas, with the lane's mute and solo.
    expect(screen.queryByRole("button", { name: /^Remove the lane of / })).toBeNull();
    expect(within(await nodeOf(1)).getByRole("button", { name: "Solo Filter" })).toBeTruthy();
    expect(await nodeOf(2)).toBeTruthy();
  });

  test("says why when the instrument could not be taken out of a lane, and the lane is as it was", async () => {
    const { server, client } = await rig(
      <>
        <PatchView />
        <Notices />
      </>,
      session,
    );
    fireEvent.click(await removeButton(4, "Remove the lane of Oscillator 2"));
    server.drop();
    await waitFor(() => {
      expect(client.state.status).not.toBe("open");
    });
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove Oscillator 2 only/ }));
    expect(await screen.findByText("Could not take Oscillator 2 out of its lane: Not connected to the server.")).toBeTruthy();
    expect(within(await nodeOf(4)).getByRole("button", { name: "Change the instrument of Oscillator 2" })).toBeTruthy();
  });

  test("says why when it could not be taken out", async () => {
    const { server, client } = await rig(
      <>
        <PatchView />
        <Notices />
      </>,
      session,
    );
    fireEvent.click(await removeButton(2, "Remove Lead"));
    server.drop();
    await waitFor(() => {
      expect(client.state.status).not.toBe("open");
    });
    fireEvent.click(screen.getByRole("menuitem", { name: "Remove Lead for good" }));
    expect(await screen.findByText("Could not remove Lead: Not connected to the server.")).toBeTruthy();
    expect(await nodeOf(2)).toBeTruthy();
  });
});
