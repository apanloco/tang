import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { PATTERN_HEIGHT } from "../src/app/patch/toFlow";
import { expectNodesApart, node, open as openMock } from "./canvas";
import { test } from "./own";

// The pattern of a lane (docs/web-ui.md §12, step 51): a block under the controls of every lane that records, clears and sets the pattern. Its size is tried on the page's own mock
// engine, and the whole of it on the real program, on the null audio backend (a program of its own: the file it saves is read, and a second page is told).

/** The pattern block of the lane called `name`. */
const block = (page: Page, name: string): Locator => page.getByRole("group", { name: `Pattern of ${name}` });
const record = (page: Page, name = "Oscillator"): Locator => block(page, name).getByRole("button", { name: new RegExp(`^(Record a pattern on|Stop recording) ${name}$`) });
const line = (page: Page, name = "Oscillator"): Locator => block(page, name).locator(".tang-pattern__line");
const length = (page: Page, name = "Oscillator"): Locator => block(page, name).getByRole("spinbutton", { name: `Length of the pattern of ${name}` });
const clear = (page: Page, name = "Oscillator"): Locator => block(page, name).getByRole("button", { name: `Clear the pattern of ${name}` });
const switches = { play: "Play the pattern of Oscillator", loop: "Loop the pattern of Oscillator", inKey: "Transpose the pattern of Oscillator in key" } as const;
const switchOf = (page: Page, which: keyof typeof switches): Locator => block(page, "Oscillator").getByRole("button", { name: switches[which] });

/** How much a pixel of the layout is on the screen: the canvas is zoomed to fit the window. */
async function scaleOf(target: Locator): Promise<number> {
  const box = await target.locator(".tang-node").boundingBox();
  if (!box) throw new Error("not on the page");
  return box.width / 304;
}

/** How tall the pattern block of a lane is, in the pixels of the layout. */
async function blockHeight(page: Page, name: string): Promise<number> {
  const box = await block(page, name).boundingBox();
  if (!box) throw new Error("the lane has no pattern block");
  return box.height / (await scaleOf(node(page, name)));
}

test("the pattern block of a lane is as tall as the layout assumes, whatever it shows, and no node lies over another", async ({ page }) => {
  await openMock(page, "lanes");
  const idle = await blockHeight(page, "Oscillator 1");
  expect(idle).toBeCloseTo(PATTERN_HEIGHT, 0);
  await expect(block(page, "Oscillator 2")).toBeVisible();
  await expectNodesApart(page);

  // A recording that goes on and a pattern that is there: the same height, which is what the layout counts on.
  await record(page, "Oscillator 1").click();
  await expect(record(page, "Oscillator 1")).toHaveText("■ Stop");
  expect(await blockHeight(page, "Oscillator 1")).toBeCloseTo(PATTERN_HEIGHT, 0);
  await expect(clear(page, "Oscillator 1")).toBeEnabled();
  await expectNodesApart(page);
  await record(page, "Oscillator 1").click();
  await expect(record(page, "Oscillator 1")).toHaveText("● Record");
  expect(await blockHeight(page, "Oscillator 1")).toBeCloseTo(PATTERN_HEIGHT, 0);
});

test("a pattern is recorded from the page, a second page and the saved file have it, and it is cleared, after asking, from either", async ({ page, browser, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(block(page, "Oscillator")).toBeVisible();
  await expect(line(page)).toHaveText("No pattern yet");

  // A short one: a beat at 300 beats a minute, which is counted in with four (a second all in all).
  await page.getByRole("spinbutton", { name: "Tempo" }).fill("300");
  await page.getByRole("spinbutton", { name: "Tempo" }).press("Enter");
  await length(page).fill("1");
  await length(page).press("Enter");
  await expect(line(page)).toHaveText("1 beat · no notes · 300 BPM");

  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect(line(other)).toHaveText("1 beat · no notes · 300 BPM");

  // It records: a key of the computer is played at once, in the count-in (which is recorded at the start of the pattern: the engine counts in for four beats, a second less than a fifth),
  // and the button says so, in both pages, and the line says what is going on.
  await record(page).click();
  await page.keyboard.down("z");
  await page.keyboard.up("z");
  await expect(record(page)).toHaveText("■ Stop");
  await expect(line(page)).toHaveText("Counting in, then 1 beat at 300 BPM: play now");
  await expect(record(other)).toHaveText("■ Stop");

  // When the engine is done both pages have the pattern: one note, the tempo it was recorded at, to be played by a held key (which loops, in semitones).
  await expect(line(page)).toHaveText("1 beat · 1 note · 300 BPM", { timeout: 20_000 });
  await expect(line(other)).toHaveText("1 beat · 1 note · 300 BPM");
  await expect(record(page)).toHaveText("● Record");
  await expect(switchOf(page, "play")).toHaveAttribute("aria-pressed", "true");
  await expect(switchOf(page, "loop")).toHaveAttribute("aria-pressed", "true");
  await expect(switchOf(page, "inKey")).toHaveAttribute("aria-pressed", "false");
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // A switch is the session's: the second page is told, and the file has it.
  await switchOf(page, "loop").click();
  await expect(switchOf(page, "loop")).toHaveAttribute("aria-pressed", "false");
  await expect(switchOf(other, "loop")).toHaveAttribute("aria-pressed", "false");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved).toContain("[instrument.pattern]");
  expect(saved).toContain("length_beats = 1.0");
  expect(saved).toContain("looping = false");
  expect(saved).toContain("base_note = ");

  // It asks before it clears, for there is no undo of it, and clears in both pages when it is said to; the focus goes back to the button that records.
  await clear(page).click();
  const menu = page.getByRole("menu", { name: "Clear the pattern of Oscillator" });
  await expect(menu.getByRole("menuitem")).toHaveText(["Keep the pattern", "Clear the pattern of Oscillator for good: 1 beat, 1 note"]);
  await menu.getByRole("menuitem", { name: "Keep the pattern" }).click();
  await expect(line(page)).toHaveText("1 beat · 1 note · 300 BPM");
  await clear(page).click();
  await menu.getByRole("menuitem", { name: /^Clear the pattern of Oscillator for good/ }).click();
  await expect(line(page)).toHaveText("No pattern yet");
  await expect(line(other)).toHaveText("No pattern yet");
  await expect(record(page)).toBeFocused();
  await expect(clear(page)).toBeDisabled();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(own.file, "utf8")).not.toContain("[instrument.pattern]");
  await other.close();
});

test("a recording that is stopped in its count-in leaves the lane's pattern as it was", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  // A pattern to begin with: one note, a beat long, at the session's 120 a minute (the count-in is two seconds).
  await length(page).fill("1");
  await length(page).press("Enter");
  await record(page).click();
  await page.keyboard.down("z");
  await page.keyboard.up("z");
  await expect(line(page)).toHaveText("1 beat · 1 note · 120 BPM", { timeout: 20_000 });

  // A second recording, with two notes, stopped while it is counted in: nothing was recorded.
  await record(page).click();
  await expect(record(page)).toHaveText("■ Stop");
  await page.keyboard.down("x");
  await page.keyboard.up("x");
  await page.keyboard.down("c");
  await page.keyboard.up("c");
  await record(page).click();
  await expect(record(page)).toHaveText("● Record");
  // (Well after the recording would have been done.)
  await page.waitForTimeout(3000);
  await expect(line(page)).toHaveText("1 beat · 1 note · 120 BPM");
});
