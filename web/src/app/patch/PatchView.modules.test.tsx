// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import type { Intent } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { AddFreeEffectMenu } from "../AddFreeEffect";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// A chain of effects saved as a module (step 104): the button in the cell of a lane, beside its mute and its solo, and the field for the name that opens under it; and the bar's "+ Effect", which lists
// what was saved and adds it, in a lane of its own, with the effects it was saved with. What the page shows, what it asks and when.

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");

/** What the page says when it could not do what was asked, and what it says when it did. */
function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

async function rigged(snapshot = tracerSnapshot) {
  return rig(
    <>
      <PatchView />
      <AddFreeEffectMenu />
      <Board />
    </>,
    { snapshot },
  );
}

const saveButton = (name: string): Promise<HTMLElement> => screen.findByRole("button", { name: `Save the chain of ${name} as a module` });
const asked = (requests: () => { intent: unknown }[]): Intent[] => requests().map((request) => request.intent as Intent);
const noticeTexts = (): string[] => Array.from(document.querySelectorAll(".tang-notice__text")).map((text) => text.textContent);

/** The form that an element of the field for a name is in (the field is drawn in the page, in a form of its own). */
function formOf(element: HTMLElement): HTMLFormElement {
  const form = element.closest("form");
  if (!form) throw new Error("the field is in no form");
  return form;
}

/** The cell that an element is in. */
function cellOf(element: HTMLElement): HTMLElement {
  const cell = element.closest<HTMLElement>(".tang-cell");
  if (!cell) throw new Error("the button is in no cell");
  return cell;
}

describe("the button that saves the chain of a lane as a module", () => {
  test("is in the cell of every lane that has an effect, beside its mute and its solo, and in no other: not an empty lane, not a lane that has no effect, not a group", async () => {
    await rigged();
    const button = await saveButton("Oscillator");
    expect(cellOf(button).textContent).toContain("Mute");
    expect(within(cellOf(button)).getAllByRole("button").map((b) => b.getAttribute("aria-label"))).toEqual(["Mute Oscillator", "Solo Oscillator", "Save the chain of Oscillator as a module"]);
    cleanup();

    await rigged(reachFixture.snapshot);
    await screen.findAllByRole("button", { name: /^Mute / });
    const nodes = reachFixture.snapshot.graph.nodes;
    const withChains = nodes.filter((node) => node.kind === "instrument" && nodes.some((n) => n.kind === "effect" && n.parent === node.id));
    expect(withChains.length).toBeGreaterThan(0);
    expect(nodes.filter((node) => node.kind === "instrument").length).toBeGreaterThan(withChains.length);
    expect(screen.getAllByRole("button", { name: /^Save the chain of / })).toHaveLength(withChains.length);
    // (The mute and the solo are on every lane and every group, as they were.)
    expect(screen.getAllByRole("button", { name: /^Mute / }).length).toBeGreaterThan(withChains.length);
  });

  test("is on the effect that holds a lane's cell when a lane has only effects, the first of them, and the name is that effect's", async () => {
    const { server } = await rigged();
    server.apply({ type: "addFreeEffect", plugin: "builtin:reverb" });
    const button = await saveButton("Reverb");
    expect(button.closest(".tang-node")?.querySelector(".tang-node__name")?.textContent).toBe("Reverb");
    expect(screen.getAllByRole("button", { name: /^Save the chain of / })).toHaveLength(2);
  });

  test("opens a field for the name under it, which has the focus, with Save that waits for a name; and it asks nothing yet", async () => {
    const { requests } = await rigged();
    const button = await saveButton("Oscillator");
    expect(button.getAttribute("aria-expanded")).toBe("false");
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(button);
    const dialog = await screen.findByRole("dialog", { name: "Save the chain of Oscillator as a module" });
    expect(button.getAttribute("aria-expanded")).toBe("true");
    const field = within(dialog).getByRole("textbox", { name: "Module name" });
    expect(document.activeElement).toBe(field);
    const save = within(dialog).getByRole("button", { name: "Save" });
    expect((save as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(field, { target: { value: "   " } });
    expect((save as HTMLButtonElement).disabled).toBe(true);
    fireEvent.change(field, { target: { value: "Room" } });
    expect((save as HTMLButtonElement).disabled).toBe(false);
    expect(asked(requests)).toEqual([]);
    // (The button that opens it closes it again.)
    fireEvent.click(button);
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  test("asks the server to save the chain under the name as it was typed, says so when it is saved, closes the field and gives the focus back to the button; the module is then in the bar's + Effect", async () => {
    const { requests } = await rigged();
    const button = await saveButton("Oscillator");
    fireEvent.click(button);
    const field = await screen.findByRole("textbox", { name: "Module name" });
    fireEvent.change(field, { target: { value: "  Tracer chain " } });
    fireEvent.submit(formOf(field));
    await waitFor(() => {
      expect(noticeTexts()).toEqual(['Saved the module "Tracer chain": + Effect adds it']);
    });
    expect(asked(requests)).toEqual([{ type: "saveModule", node: oscillator.id, name: "  Tracer chain " }]);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(document.activeElement).toBe(button);
    // The catalog lists it from now on, and the bar's menu has it first, under a heading of its own.
    fireEvent.click(screen.getByRole("button", { name: "Add an effect" }));
    const menu = await screen.findByRole("menu", { name: "Add an effect" });
    const items = within(menu).getAllByRole("menuitem").map((item) => item.textContent);
    expect(items[0]).toBe("Tracer chain");
    expect(within(menu).getByRole("group", { name: "Module" })).toBeTruthy();
    expect(items).toContain("Reverb");
  });

  test("is left with Escape or Cancel, asking nothing, and the focus goes back to the button; a press anywhere else closes it and the focus stays where the person put it", async () => {
    const { requests } = await rigged();
    const button = await saveButton("Oscillator");
    fireEvent.click(button);
    fireEvent.keyDown(await screen.findByRole("textbox", { name: "Module name" }), { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(button);

    fireEvent.click(button);
    fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(button);

    fireEvent.click(button);
    await screen.findByRole("dialog");
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("dialog")).toBeNull();
    // (A press in the field itself is not away from it.)
    fireEvent.click(button);
    fireEvent.pointerDown(await screen.findByRole("textbox", { name: "Module name" }));
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(asked(requests)).toEqual([]);
  });

  test("is refused as the server refuses a name: the page says why, and the field stays, with what was typed, to be tried again", async () => {
    const { requests } = await rigged();
    fireEvent.click(await saveButton("Oscillator"));
    const field = await screen.findByRole("textbox", { name: "Module name" });
    fireEvent.change(field, { target: { value: "a/b" } });
    fireEvent.submit(formOf(field));
    await waitFor(() => {
      expect(noticeTexts()).toEqual(['Could not save the chain of Oscillator as the module "a/b": a file name has no folder in it: `/` and `\\` are not allowed']);
    });
    expect(screen.getByRole<HTMLInputElement>("textbox", { name: "Module name" }).value).toBe("a/b");
    expect(document.activeElement).toBe(screen.getByRole("textbox", { name: "Module name" }));
    // (Tried again with a name that is one.)
    fireEvent.change(screen.getByRole("textbox", { name: "Module name" }), { target: { value: "Fixed" } });
    fireEvent.submit(formOf(screen.getByRole("textbox", { name: "Module name" })));
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(asked(requests).map((intent) => (intent.type === "saveModule" ? intent.name : intent.type))).toEqual(["a/b", "Fixed"]);
  });

  test("asks once for a name that is asked twice at once, and nothing for a name of nothing", async () => {
    const { requests } = await rigged();
    fireEvent.click(await saveButton("Oscillator"));
    const field = await screen.findByRole("textbox", { name: "Module name" });
    const form = formOf(field);
    fireEvent.submit(form);
    expect(asked(requests)).toEqual([]);
    fireEvent.change(field, { target: { value: "Twice" } });
    fireEvent.submit(form);
    fireEvent.submit(form);
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).toBeNull();
    });
    expect(asked(requests)).toEqual([{ type: "saveModule", node: oscillator.id, name: "Twice" }]);
  });
});

describe("the modules that were saved, in the bar's + Effect", () => {
  const space = { effects: [{ plugin: "builtin:filter", preset: null, params: { cutoff: 0.25 }, mix: 0.8, pinned: ["cutoff"], fed: true }, { plugin: "builtin:reverb", preset: null, params: {}, mix: 0.4, pinned: [], fed: true }], cables: [] };

  test("are listed first, each under the heading Module, and add the chain in a lane of its own: the effects it was saved with, in order, as modules on the canvas", async () => {
    const { server, requests } = await rig(
      <>
        <PatchView />
        <AddFreeEffectMenu />
        <Board />
      </>,
      { modules: { Space: space } },
    );
    fireEvent.click(await screen.findByRole("button", { name: "Add an effect" }));
    const menu = await screen.findByRole("menu", { name: "Add an effect" });
    const [first] = within(menu).getAllByRole("menuitem");
    if (!first) throw new Error("the menu has an item");
    expect(first.textContent).toBe("Space");
    fireEvent.click(first);
    await waitFor(() => {
      expect(Array.from(document.querySelectorAll(".tang-node__name")).map((name) => name.textContent)).toEqual(["Oscillator", "Filter 1", "Filter 2", "Reverb", "LFO", "Master"]);
    });
    expect(asked(requests)).toEqual([{ type: "addFreeEffect", plugin: "module:Space" }]);
    expect(server.history.undo).toBe("add Space");
    // (The first effect of the holder has the lane's cell, and its button saves the chain that is there now: the module that was added can be saved again as it is, whole.)
    expect(screen.getByRole("button", { name: "Save the chain of Filter 2 as a module" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Save the chain of Reverb as a module" })).toBeNull();
    expect(noticeTexts()).toEqual([]);
  });

  test("are not in the menu of a lane that adds an effect to its chain: a chain does not go in a chain", async () => {
    await rig(
      <>
        <PatchView />
        <Board />
      </>,
      { modules: { Space: space } },
    );
    fireEvent.click(await screen.findByRole("button", { name: "Add an effect to Oscillator" }));
    const menu = await screen.findByRole("menu", { name: "Add an effect to Oscillator" });
    const items = within(menu).getAllByRole("menuitem").map((item) => item.textContent);
    expect(items).toContain("Reverb");
    expect(items).not.toContain("Space");
    expect(within(menu).queryByRole("group", { name: "Module" })).toBeNull();
  });
});
