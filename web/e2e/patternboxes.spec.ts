import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { PATTERN_BOX_HEIGHT } from "../src/app/patch/toFlow";
import { drag, expectNodesApart, node, notesCables, notesIn, notesOut, open } from "./canvas";

// The pattern boxes of the page (docs/patch-graph.md §11.6, P4h-C, step 124): a node that notes are recorded into and sent from. Its size and what a person does with it are tried on the page's own mock engine (the engine's counts
// and passes are the real player's, which `patternBoxTakes.test.ts` holds the mock to), and the whole of it on the real program in `patternboxes.real.spec.ts`.

/** The controls of the box called `name`. */
const box = (page: Page, name: string): Locator => page.getByRole("group", { name: `Pattern box ${name}` });
const record = (page: Page, name: string): Locator => box(page, name).getByRole("button", { name: new RegExp(`^(Record into|Finish recording) ${name}$`) });
const play = (page: Page, name: string): Locator => box(page, name).getByRole("button", { name: new RegExp(`^(Play|Stop) ${name}$`) });
const line = (page: Page, name: string): Locator => box(page, name).locator(".tang-pattern__line");
const marks = (page: Page, name: string): Locator => box(page, name).locator(".tang-roll__note");
const add = (page: Page): Locator => page.getByRole("button", { name: "Add a pattern box" });

/** How much a pixel of the layout is on the screen: the canvas is zoomed to fit the window. */
async function scaleOf(target: Locator): Promise<number> {
  const found = await target.locator(".tang-node").boundingBox();
  if (!found) throw new Error("not on the page");
  return found.width / 304;
}

/** How tall the node of a box is, in the pixels of the layout. */
async function heightOf(page: Page, name: string): Promise<number> {
  const target = node(page, name);
  const found = await target.locator(".tang-node").boundingBox();
  if (!found) throw new Error("the box is not on the page");
  return found.height / (await scaleOf(target));
}

/** The tempo set to 200 beats a minute, so that a count-in and a pass of the box's four beats take a second and a fifth each (a pass shorter than that is too short for a test to play a key in, which it sees only when it polls). */
async function quick(page: Page, name: string): Promise<void> {
  await page.getByRole("spinbutton", { name: "Tempo" }).fill("200");
  await page.getByRole("spinbutton", { name: "Tempo" }).press("Enter");
  await expect(line(page, name)).toHaveText("4 beats · no notes");
}

test("a pattern box is as tall as the layout assumes, whatever it is doing, and no node lies over another", async ({ page }) => {
  await open(page, "lanes");
  await add(page).click();
  await expect(box(page, "Pattern 1")).toBeVisible();
  expect(await heightOf(page, "Pattern 1")).toBeCloseTo(PATTERN_BOX_HEIGHT, 0);
  await expectNodesApart(page);
  // Counting in, and a box that has notes: the same height, which the layout counts on.
  await quick(page, "Pattern 1");
  await record(page, "Pattern 1").click();
  await expect(record(page, "Pattern 1")).toHaveText("■ Finish");
  expect(await heightOf(page, "Pattern 1")).toBeCloseTo(PATTERN_BOX_HEIGHT, 0);
  await expect(line(page, "Pattern 1")).toHaveText("Recording 4 beats: play now", { timeout: 10_000 });
  await page.keyboard.down("z");
  await page.keyboard.up("z");
  await expect(line(page, "Pattern 1")).toHaveText("Playing · 4 beats · 1 note", { timeout: 15_000 });
  expect(await heightOf(page, "Pattern 1")).toBeCloseTo(PATTERN_BOX_HEIGHT, 0);
  await expectNodesApart(page);
  // Another box goes below the first, and neither is on a lane or a source.
  await add(page).click();
  await expect(box(page, "Pattern 2")).toBeVisible();
  await expectNodesApart(page);
});

test("a box is made, recorded into with the keys of the computer, drawn to a lane, and Undo takes each back", async ({ page }) => {
  await open(page, "lanes");
  await expect(notesCables(page)).toHaveCount(0);
  await add(page).click();
  await expect(line(page, "Pattern 1")).toHaveText("4 beats · no notes");
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await quick(page, "Pattern 1");

  // It counts in and takes a pass, the button and the line saying what it is doing; a key that is played in the pass is a note of the box (the engine of the page counts the beats by its clock).
  await record(page, "Pattern 1").click();
  await expect(record(page, "Pattern 1")).toHaveText("■ Finish");
  await expect(line(page, "Pattern 1")).toHaveText("Counting in, then 4 beats: play now");
  await expect(line(page, "Pattern 1")).toHaveText("Recording 4 beats: play now", { timeout: 10_000 });
  await page.keyboard.down("z");
  await page.keyboard.up("z");
  await expect(line(page, "Pattern 1")).toHaveText("Playing · 4 beats · 1 note", { timeout: 10_000 });
  await expect(record(page, "Pattern 1")).toHaveText("● Record");
  await expect(play(page, "Pattern 1")).toHaveText("■ Stop");
  await expect(marks(page, "Pattern 1")).toHaveCount(1);

  // The notes are sent to a lane by a cable from the box's menu, and the lane has the cable.
  await page.getByRole("button", { name: "Send notes from Pattern 1" }).click();
  await page.getByRole("menuitem", { name: "Oscillator 1" }).click();
  await expect(notesCables(page)).toHaveCount(1);

  // Undo takes back the cable, and then the recording, which is a step of its own.
  await page.getByRole("button", { name: /^Undo/ }).click();
  await expect(notesCables(page)).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Undo record into Pattern 1" })).toBeVisible();
  await page.getByRole("button", { name: "Undo record into Pattern 1" }).click();
  // (What a box is doing is the engine's and not a step of the history: the box goes on playing, and it has the notes that it had, none.)
  await expect(line(page, "Pattern 1")).toHaveText("Playing · 4 beats · no notes");
  await expect(marks(page, "Pattern 1")).toHaveCount(0);
  // Redo puts the notes back.
  await page.getByRole("button", { name: "Redo record into Pattern 1" }).click();
  await expect(line(page, "Pattern 1")).toHaveText("Playing · 4 beats · 1 note");
  await expect(marks(page, "Pattern 1")).toHaveCount(1);
});

test("a cable of notes is dragged from the output of a box to the input of a lane, and from a key source to a box, and is taken out with its box", async ({ page }) => {
  await open(page, "lanes");
  await add(page).click();
  await expect(box(page, "Pattern 1")).toBeVisible();
  await page.getByRole("button", { name: "Add a source of notes" }).click();
  await page.getByRole("menuitem", { name: "Keys" }).click();
  await expect(node(page, "Keys")).toBeVisible();
  // (A box that no cable of notes ends at records every source and is reached by a faint link from each.)
  await expect(page.locator(".tang-cable--hears").first()).toBeAttached();
  const pattern = node(page, "Pattern 1");
  const first = node(page, "Oscillator 1");
  await drag(page, notesOut(pattern), notesIn(first));
  await expect(notesCables(page)).toHaveCount(1);
  await drag(page, notesOut(node(page, "Keys")), notesIn(pattern));
  await expect(notesCables(page)).toHaveCount(2);
  await expectNodesApart(page);
  // The box goes with the cables at it, and the lane that heard it hears every source again.
  await pattern.getByRole("button", { name: "Remove Pattern 1" }).click();
  await expect(node(page, "Pattern 1")).toHaveCount(0);
  await expect(notesCables(page)).toHaveCount(0);
  await expect(add(page)).toBeFocused();
});

test("a box is renamed in place, its notes are cleared after asking, and its settings are the session's", async ({ page }) => {
  await open(page, "lanes");
  await add(page).click();
  await quick(page, "Pattern 1");
  await record(page, "Pattern 1").click();
  await expect(line(page, "Pattern 1")).toHaveText("Recording 4 beats: play now", { timeout: 10_000 });
  await page.keyboard.down("x");
  await page.keyboard.up("x");
  await expect(line(page, "Pattern 1")).toHaveText("Playing · 4 beats · 1 note", { timeout: 10_000 });
  await play(page, "Pattern 1").click();
  await expect(line(page, "Pattern 1")).toHaveText("4 beats · 1 note");

  // The name is a field in the header: Enter asks for it, and the box is called it everywhere (the notes it names in the buttons too).
  await node(page, "Pattern 1").getByRole("button", { name: "Rename Pattern 1" }).click();
  await page.getByRole("textbox", { name: "Name of Pattern 1" }).fill("Lead");
  await page.getByRole("textbox", { name: "Name of Pattern 1" }).press("Enter");
  await expect(box(page, "Lead")).toBeVisible();
  await expect(record(page, "Lead")).toHaveText("● Record");

  // Switches: overdub is on, and the hint of the record button says what a recording does then.
  await box(page, "Lead").getByRole("button", { name: "Overdub into Lead" }).click();
  await expect(box(page, "Lead").getByRole("button", { name: "Overdub into Lead" })).toHaveAttribute("aria-pressed", "true");
  await expect(record(page, "Lead")).toHaveAttribute("title", /added to the ones the box has/);

  // Clear asks first, and the notes go when it is said to.
  await box(page, "Lead").getByRole("button", { name: "Clear the notes of Lead" }).click();
  const menu = page.getByRole("menu", { name: "Clear the notes of Lead" });
  await expect(menu.getByRole("menuitem")).toHaveText(["Keep the notes", "Clear the notes of Lead: 1 note"]);
  await menu.getByRole("menuitem", { name: "Keep the notes" }).click();
  await expect(line(page, "Lead")).toHaveText("4 beats · 1 note");
  await box(page, "Lead").getByRole("button", { name: "Clear the notes of Lead" }).click();
  await page.getByRole("menuitem", { name: /^Clear the notes of Lead/ }).click();
  await expect(line(page, "Lead")).toHaveText("4 beats · no notes");
  await expect(marks(page, "Lead")).toHaveCount(0);
  await expect(record(page, "Lead")).toBeFocused();
});

test("a box has a menu of its own on the canvas, which records, plays, clears and takes it out, and the canvas has one that adds a box where the pointer is", async ({ page }) => {
  await open(page, "lanes");
  await page.locator(".react-flow__pane").click({ button: "right", position: { x: 700, y: 120 } });
  await page.getByRole("menuitem", { name: "Add a pattern box" }).click();
  await expect(box(page, "Pattern 1")).toBeVisible();
  await node(page, "Pattern 1").locator(".tang-node__header").click({ button: "right" });
  const labels = await page.getByRole("menuitem").allTextContents();
  expect(labels.slice(0, 3)).toEqual(["Record into Pattern 1", "Play Pattern 1", "Clear the notes of Pattern 1"]);
  expect(labels).toContain("Remove Pattern 1");
  await page.getByRole("menuitem", { name: "Play Pattern 1" }).click();
  await expect(play(page, "Pattern 1")).toHaveText("■ Stop");
  await node(page, "Pattern 1").locator(".tang-node__header").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Remove Pattern 1" }).click();
  await expect(node(page, "Pattern 1")).toHaveCount(0);
});
