import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { SPLITTER_HEIGHT } from "../src/app/patch/toFlow";
import { drag, expectNodesApart, node, notesCables, notesIn, notesOut, notesOutRest, open } from "./canvas";

// The note splitters of the page (docs/patch-graph.md §11.6, P4i-C, step 128): a node that takes notes in and sends the lowest (or the highest) of the keys that are held to its first output and the others to its second. Its size, its
// two outputs and what a person does with it are tried on the page's own mock engine (what the session says of a splitter is what the controller says: `splitters.test.ts` holds the mock to it), and the whole of it on the real
// program in `splitters.real.spec.ts`. The canvas adds one from the MIDI menu of its right-click menu, with the keys and a pattern box.

/** The controls of the splitter called `name`. */
const splitter = (page: Page, name: string): Locator => page.getByRole("group", { name: `Note splitter ${name}` });
const pick = (page: Page, name: string, which: "lowest" | "highest"): Locator => splitter(page, name).getByRole("button", { name: `Pick the ${which} note with ${name}` });

/** Adds a splitter from the MIDI menu of the canvas, where the pointer is. */
async function addSplitter(page: Page, at = { x: 700, y: 120 }): Promise<void> {
  await page.locator(".react-flow__pane").click({ button: "right", position: at });
  await page.getByRole("menuitem", { name: "MIDI" }).click();
  await page.getByRole("menuitem", { name: "Note splitter" }).click();
}

/** How much a pixel of the layout is on the screen: the canvas is zoomed to fit the window. */
async function scaleOf(target: Locator): Promise<number> {
  const found = await target.locator(".tang-node").boundingBox();
  if (!found) throw new Error("not on the page");
  return found.width / 304;
}

/** How tall the node of a splitter is, in the pixels of the layout. */
async function heightOf(page: Page, name: string): Promise<number> {
  const target = node(page, name);
  const found = await target.locator(".tang-node").boundingBox();
  if (!found) throw new Error("the splitter is not on the page");
  return found.height / (await scaleOf(target));
}

test("the canvas has a MIDI menu that holds what is MIDI: the keys, a pattern box and a note splitter, and the splitter is added where the pointer is", async ({ page }) => {
  await open(page, "lanes");
  await page.locator(".react-flow__pane").click({ button: "right", position: { x: 700, y: 120 } });
  // (Nothing MIDI is loose in the menu of the canvas: it is all under one item.)
  const items = await page.getByRole("menuitem").allTextContents();
  expect(items.slice(0, 4)).toEqual(["Add an instrument", "Add an effect", "Add a modulator", "MIDI"]);
  expect(items.filter((label) => /source of notes|pattern box|splitter/i.test(label))).toEqual([]);
  await page.getByRole("menuitem", { name: "MIDI" }).click();
  await expect(page.getByRole("menuitem", { name: "Note splitter" })).toBeVisible();
  // (The page's mock has two MIDI inputs open: they are in the menu too, under their own heading, between the keys and the nodes.)
  expect(await page.getByRole("menu", { name: "MIDI" }).getByRole("menuitem").allTextContents()).toEqual(["Keys", "Mock Keyboard MIDI In", "Mock Pad Controller", "Pattern box", "Note splitter"]);
  await expect(page.getByRole("menu", { name: "MIDI" }).getByRole("group", { name: "MIDI inputs" })).toBeVisible();
  await page.getByRole("menuitem", { name: "Note splitter" }).click();
  await expect(splitter(page, "Splitter 1")).toBeVisible();
  await expect(page.getByRole("menu")).toHaveCount(0);
});

test("a note splitter is as tall as the layout assumes, whatever it picks, and no node lies over another", async ({ page }) => {
  await open(page, "lanes");
  await addSplitter(page);
  await expect(splitter(page, "Splitter 1")).toBeVisible();
  expect(await heightOf(page, "Splitter 1")).toBeCloseTo(SPLITTER_HEIGHT, 0);
  await expectNodesApart(page);
  await pick(page, "Splitter 1", "highest").click();
  await expect(pick(page, "Splitter 1", "highest")).toHaveAttribute("aria-pressed", "true");
  expect(await heightOf(page, "Splitter 1")).toBeCloseTo(SPLITTER_HEIGHT, 0);
  // Another goes below the first, and neither is on a lane.
  await addSplitter(page, { x: 700, y: 520 });
  await expect(splitter(page, "Splitter 2")).toBeVisible();
  await expectNodesApart(page);
});

test("the switch picks the note, the first output is called by it, and Undo and Redo put it back and set it again", async ({ page }) => {
  await open(page, "lanes");
  await addSplitter(page);
  const outputs = splitter(page, "Splitter 1").locator(".tang-splitter__out .tang-splitter__label");
  await expect(outputs).toHaveText(["Lowest", "Rest"]);
  await expect(pick(page, "Splitter 1", "lowest")).toHaveAttribute("aria-pressed", "true");
  await pick(page, "Splitter 1", "highest").click();
  await expect(outputs).toHaveText(["Highest", "Rest"]);
  await expect(splitter(page, "Splitter 1").getByRole("button", { name: "Send the highest note from Splitter 1" })).toBeVisible();
  await page.getByRole("button", { name: "Undo let Splitter 1 pick the highest note" }).click();
  await expect(outputs).toHaveText(["Lowest", "Rest"]);
  await page.getByRole("button", { name: "Redo let Splitter 1 pick the highest note" }).click();
  await expect(outputs).toHaveText(["Highest", "Rest"]);
});

test("a cable of notes is dragged from each output of a splitter to a lane, from a source to the splitter, and is taken out with its splitter", async ({ page }) => {
  await open(page, "lanes");
  await addSplitter(page);
  await page.getByRole("button", { name: "Add a source of notes" }).click();
  await page.getByRole("menuitem", { name: "Keys" }).click();
  await expect(node(page, "Keys")).toBeVisible();
  const split = node(page, "Splitter 1");
  const first = node(page, "Oscillator 1");
  const second = node(page, "Oscillator 2");
  // The first output goes to one lane, the second to the other: two cables, from two outputs of one node.
  await drag(page, notesOut(split), notesIn(first));
  await expect(notesCables(page)).toHaveCount(1);
  await drag(page, notesOutRest(split), notesIn(second));
  await expect(notesCables(page)).toHaveCount(2);
  await drag(page, notesOut(node(page, "Keys")), notesIn(split));
  await expect(notesCables(page)).toHaveCount(3);
  await expectNodesApart(page);
  // (The cable from the second output is told from the first in its button, which a lane that has one from each needs.)
  await expect(page.getByRole("button", { name: "Remove cable Splitter 1 → Oscillator 1" })).toBeAttached();
  await expect(page.getByRole("button", { name: "Remove cable Splitter 1 (rest) → Oscillator 2" })).toBeAttached();
  // Both outputs may go to one lane, and a second cable from one output to it is not drawn.
  await drag(page, notesOutRest(split), notesIn(first));
  await expect(notesCables(page)).toHaveCount(4);
  await drag(page, notesOutRest(split), notesIn(first));
  await expect(notesCables(page)).toHaveCount(4);
  // The splitter goes with the cables at it.
  await split.getByRole("button", { name: "Remove Splitter 1" }).click();
  await expect(node(page, "Splitter 1")).toHaveCount(0);
  await expect(notesCables(page)).toHaveCount(0);
});

test("the cable that is dragged from a splitter's output ends at a lane and at no splitter and no box, and one dragged from its second output ends at no other node's", async ({ page }) => {
  await open(page, "lanes");
  await addSplitter(page, { x: 700, y: 120 });
  await addSplitter(page, { x: 700, y: 520 });
  const [one, two] = [node(page, "Splitter 1"), node(page, "Splitter 2")];
  // A splitter takes the notes of a source or a box and no splitter's: the cable that is let go on the other splitter's input is not drawn.
  await drag(page, notesOut(one), notesIn(two));
  await expect(notesCables(page)).toHaveCount(0);
  await drag(page, notesOutRest(two), notesIn(one));
  await expect(notesCables(page)).toHaveCount(0);
  // (And the lane takes it.)
  await drag(page, notesOutRest(two), notesIn(node(page, "Oscillator 1")));
  await expect(notesCables(page)).toHaveCount(1);
});

test("the menus of a splitter send and take notes where the keyboard goes, the second output's menu names the output, and the splitter has a menu of its own on the canvas", async ({ page }) => {
  await open(page, "lanes");
  await addSplitter(page);
  const split = node(page, "Splitter 1");
  await split.getByRole("button", { name: "Send the other notes from Splitter 1" }).click();
  await page.getByRole("menuitem", { name: "Oscillator 2" }).click();
  await expect(notesCables(page)).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Remove cable Splitter 1 (rest) → Oscillator 2" })).toBeAttached();
  await split.getByRole("button", { name: "Send the lowest note from Splitter 1" }).click();
  await page.getByRole("menuitem", { name: "Oscillator 1" }).click();
  await expect(notesCables(page)).toHaveCount(2);
  // A lane takes a splitter's notes from either output.
  await node(page, "Oscillator 1").getByRole("button", { name: /^Take notes into / }).click();
  expect(await page.getByRole("menuitem").allTextContents()).toEqual(["Splitter 1 · rest"]);
  await page.keyboard.press("Escape");
  // The menu of the node on the canvas: the switch, the notes it takes and sends, and the removal.
  await split.locator(".tang-node__header").click({ button: "right" });
  // (The two choices of the note that it picks are switches, which a menu says by a check: roles of their own.)
  const labels = await page.locator('[role="menuitem"], [role="menuitemcheckbox"]').allTextContents();
  expect(labels).toEqual(["Pick the lowest note", "Pick the highest note", "Take notes from", "Send the lowest note to", "Send the other notes to", "Remove Splitter 1"]);
  await expect(page.getByRole("menuitemcheckbox", { name: "Pick the lowest note" })).toHaveAttribute("aria-checked", "true");
  await page.getByRole("menuitemcheckbox", { name: "Pick the highest note" }).click();
  await expect(pick(page, "Splitter 1", "highest")).toHaveAttribute("aria-pressed", "true");
  await split.locator(".tang-node__header").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Remove Splitter 1" }).click();
  await expect(node(page, "Splitter 1")).toHaveCount(0);
});

test("a splitter is renamed in place, and the keys, a pattern box and a splitter are all added from the MIDI menu", async ({ page }) => {
  await open(page, "lanes");
  await addSplitter(page);
  await node(page, "Splitter 1").getByRole("button", { name: "Rename Splitter 1" }).click();
  await page.getByRole("textbox", { name: "Name of Splitter 1" }).fill("Bass line");
  await page.getByRole("textbox", { name: "Name of Splitter 1" }).press("Enter");
  await expect(splitter(page, "Bass line")).toBeVisible();
  await expect(node(page, "Bass line").getByRole("button", { name: "Rename Bass line" })).toBeFocused();
  for (const [at, item, shown] of [
    [{ x: 700, y: 400 }, "Keys", node(page, "Keys")],
    [{ x: 700, y: 600 }, "Pattern box", page.getByRole("group", { name: "Pattern box Pattern 1" })],
  ] as const) {
    await page.locator(".react-flow__pane").click({ button: "right", position: at });
    await page.getByRole("menuitem", { name: "MIDI" }).click();
    await page.getByRole("menuitem", { name: item }).click();
    await expect(shown).toBeVisible();
  }
  await expectNodesApart(page);
});
