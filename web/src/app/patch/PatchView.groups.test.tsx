// @vitest-environment jsdom
// Groups, from the page: every lane says which group it is in and puts itself in another (or in a new one), a group is given a name, and a lane
// or a group is taken out, after the page has asked a person whether to, since there is no undo.
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { MASTER, groupsFixture } from "../../client/fixtures";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// The session of the groups fixture: three groups (0, 1 and 2; the last is empty and called Lead; the first has two modulators, 13 and 14, the
// second one, 15), four lanes (3 and 10 in the first group, 9 in the second, 5 in none, which has a filter and two modulators of its own), an effect
// in two of them. The canvas numbers the nodes called alike across the session in the order of the graph: the lanes are Oscillator 1 to 4.
const session = { snapshot: groupsFixture.snapshot };
const LANES = [3, 5, 9, 10];

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
    for (const id of [0, 1, 2, 6, 4, 13, 14, 15, MASTER]) {
      expect(within(await nodeOf(id)).queryByRole("combobox", { name: /^Group of / }), `node ${String(id)}`).toBeNull();
    }
    expect(screen.getAllByRole("combobox", { name: /^Group of / })).toHaveLength(4);
  });

  test("offers none, each group by the name it has, and a new group, in that order", async () => {
    await rig(<PatchView />, session);
    const options = Array.from((await choice(5)).options).map((option) => [option.value, option.textContent]);
    expect(options).toEqual([
      ["", "none"],
      ["0", "Group 1"],
      ["1", "Group 2"],
      ["2", "Lead"],
      ["new", "new group"],
    ]);
  });

  test("asks for the lane to be put in the group that is chosen, and the canvas draws its audio into that group when the server has done it", async () => {
    const { requests } = await rig(<PatchView />, session);
    const select = await choice(5);
    fireEvent.change(select, { target: { value: "2" } });
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "setLaneGroup", node: 5, group: 2 }]);
    });
    // The last link of the lane's chain (its filter, 6) goes into the group (2) and not to the master.
    await waitFor(() => {
      expect(document.querySelector(`[id="audio:6>2"]`)).toBeTruthy();
    });
    expect(document.querySelector(`[id="audio:6>${String(MASTER)}"]`)).toBeNull();
    expect(chosen(await choice(5))).toBe("Lead");
  });

  test("takes the lane out of its group for none, and the cables of the group to it go", async () => {
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
    await waitFor(() => {
      expect(cables()).toBe(4);
    });
    expect(chosen(await choice(3))).toBe("none");
  });

  test("asks for a new group with the lane in it for a new group, and the lane is in it when the server has made it", async () => {
    const { requests } = await rig(<PatchView />, session);
    const select = await choice(5);
    fireEvent.change(select, { target: { value: "new" } });
    // Until the server has made it the choice says so, and so does the tooltip.
    expect([select.value, select.title]).toEqual(["new", "Oscillator 2 is in a new group"]);
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "addGroup", members: [5] }]);
    });
    await waitFor(async () => {
      expect(chosen(await choice(5))).toBe("Group 4");
    });
    expect((await choice(5)).value).toBe("16");
    // And every lane's choice has the new group among its options.
    expect(Array.from((await choice(9)).options).map((option) => option.textContent)).toEqual(["none", "Group 1", "Group 2", "Lead", "Group 4", "new group"]);
  });

  test("follows a group that somebody else names, and a lane that somebody else puts in another group", async () => {
    const { server } = await rig(<PatchView />, session);
    expect(chosen(await choice(9))).toBe("Group 2");
    server.apply({ type: "renameGroup", node: 1, name: "Strings" });
    await waitFor(async () => {
      expect(chosen(await choice(9))).toBe("Strings");
    });
    server.apply({ type: "setLaneGroup", node: 9, group: 2 });
    await waitFor(async () => {
      expect(chosen(await choice(9))).toBe("Lead");
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
      expect((await choice(9)).title).toBe("Oscillator 3 is in Strings and the brass of the second row");
    });
    server.apply({ type: "setLaneGroup", node: 9, group: null });
    await waitFor(async () => {
      expect((await choice(9)).title).toBe("Oscillator 3 is in no group");
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
    const select = await choice(5);
    server.drop();
    await waitFor(() => {
      expect(client.state.status).not.toBe("open");
    });
    fireEvent.change(select, { target: { value: "1" } });
    expect(await screen.findByText("Could not put Oscillator 2 in Group 2: Not connected to the server.")).toBeTruthy();
    await waitFor(async () => {
      expect(chosen(await choice(5))).toBe("none");
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
    expect(Array.from((await choice(5)).options).map((option) => option.textContent)).toContain("Strings");
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
    for (const [id, name] of [[0, "Remove Group 1"], [1, "Remove Group 2"], [2, "Remove Lead"], [3, "Remove the lane of Oscillator 1"], [5, "Remove the lane of Oscillator 2"], [9, "Remove the lane of Oscillator 3"], [10, "Remove the lane of Oscillator 4"]] as const) {
      expect(await removeButton(id, name), name).toBeTruthy();
    }
    expect(within(await nodeOf(MASTER)).queryByRole("button", { name: /^Remove / })).toBeNull();
    cleanup();
    // (The tracer, which has one lane: a session keeps a lane.)
    await rig(<PatchView />);
    expect(screen.queryByRole("button", { name: /^Remove the lane of / })).toBeNull();
  });

  test("asks whether to, in words that say what goes and what stays, and asks the server nothing until it is said", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(await removeButton(0, "Remove Group 1"));
    const menu = screen.getByRole("menu", { name: "Remove Group 1" });
    expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Keep Group 1",
      "Remove Group 1 for good: its 2 modulators go with it, and its 2 lanes stay, in no group",
    ]);
    await settle();
    expect(requests()).toEqual([]);
    fireEvent.keyDown(menu, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    // A lane says what goes with it; a group with nothing in it, nothing.
    fireEvent.click(await removeButton(5, "Remove the lane of Oscillator 2"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Keep Oscillator 2",
      "Remove the lane of Oscillator 2 for good: its 1 effect and 2 modulators go with it",
    ]);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    // One thing goes, and one lane stays.
    fireEvent.click(await removeButton(1, "Remove Group 2"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
      "Keep Group 2",
      "Remove Group 2 for good: its 1 modulator goes with it, and its 1 lane stays, in no group",
    ]);
    fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
    fireEvent.click(await removeButton(2, "Remove Lead"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keep Lead", "Remove Lead for good"]);
    await settle();
    expect(requests()).toEqual([]);
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
    // Its modulators went with it; its lanes (3 and 10) stay, in no group, and the group after it is the first now (the lane in it says so).
    expect(screen.queryByTestId("rf__node-13")).toBeNull();
    expect(screen.queryByTestId("rf__node-14")).toBeNull();
    expect(await nodeOf(3)).toBeTruthy();
    expect([chosen(await choice(3)), chosen(await choice(10)), chosen(await choice(9))]).toEqual(["none", "none", "Group 1"]);
  });

  test("takes a lane out when it is said to, with what is in it", async () => {
    const { requests } = await rig(<PatchView />, session);
    fireEvent.click(await removeButton(5, "Remove the lane of Oscillator 2"));
    fireEvent.click(screen.getByRole("menuitem", { name: /^Remove the lane of Oscillator 2 for good/ }));
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "removeNode", node: 5 }]);
    });
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-5")).toBeNull();
    });
    for (const gone of [6, 7, 8]) expect(screen.queryByTestId(`rf__node-${String(gone)}`), `node ${String(gone)}`).toBeNull();
    expect(screen.getAllByRole("combobox", { name: /^Group of / })).toHaveLength(3);
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
