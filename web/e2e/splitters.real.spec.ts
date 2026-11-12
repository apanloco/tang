import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { emptySpot, node, notesCables } from "./canvas";
import { test as plain, testWithSplitters } from "./own";

// The note splitters on the real program, on the null audio backend (a program of its own: the file it saves is read, and a second page is told): a file that has a splitter, the keys and cables of notes from the keys to the splitter and from
// its pick to a lane, a switch and a cable from its second output that are the session's (the second page has them and the file keeps them), a splitter that is made from the MIDI menu of the canvas, and what each page asks of it. What the
// engine does with the keys that are played is held in `tests/serve.rs` (a real program, a browser's keys and the lanes that hear them) and in the oracle; this is the page, the wire and the file.
const test = testWithSplitters();

const splitter = (page: Page, name: string): Locator => page.getByRole("group", { name: `Note splitter ${name}` });
const pick = (page: Page, name: string, which: "lowest" | "highest"): Locator => splitter(page, name).getByRole("button", { name: `Pick the ${which} note with ${name}` });
const outputs = (page: Page, name: string): Locator => splitter(page, name).locator(".tang-splitter__out .tang-splitter__label");

async function connect(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
}

test("a splitter that the file has is on the page with its two outputs, the switch and a cable from its second output are the session's: the other page has them, the file keeps them, and Undo takes them back", async ({ page, browser, own }) => {
  await connect(page, own.url);
  const other = await browser.newPage();
  await connect(other, own.url);

  // The splitter of the file: it picks the lowest note, takes the keys and sends its pick to the first lane; two cables of notes, and no cable from its second output.
  await expect(splitter(page, "Bass line")).toBeVisible();
  await expect(pick(page, "Bass line", "lowest")).toHaveAttribute("aria-pressed", "true");
  await expect(outputs(page, "Bass line")).toHaveText(["Lowest", "Rest"]);
  await expect(notesCables(page)).toHaveCount(2);
  await expect(page.getByText("Unsaved changes")).toHaveCount(0);
  await expect(splitter(other, "Bass line")).toBeVisible();

  // The switch is turned in one page and in the other: the first output is called by the note it sends, and the session is unsaved.
  await pick(page, "Bass line", "highest").click();
  await expect(pick(page, "Bass line", "highest")).toHaveAttribute("aria-pressed", "true");
  await expect(pick(other, "Bass line", "highest")).toHaveAttribute("aria-pressed", "true");
  await expect(outputs(other, "Bass line")).toHaveText(["Highest", "Rest"]);
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await expect(page.getByRole("button", { name: "Undo let Bass line pick the highest note" })).toBeVisible();

  // A cable from the second output to the second lane is drawn from its menu, and the other page has it.
  await splitter(page, "Bass line").getByRole("button", { name: "Send the other notes from Bass line" }).click();
  await page.getByRole("menuitem", { name: "Oscillator 2" }).click();
  await expect(notesCables(page)).toHaveCount(3);
  await expect(notesCables(other)).toHaveCount(3);
  await expect(other.getByRole("button", { name: "Remove cable Bass line (rest) → Oscillator 2" })).toBeAttached();

  // The file has what the session has: the pick, and the second output's cable by its own place.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved).toContain("[[splitter]]");
  expect(saved).toContain('name = "Bass line"');
  expect(saved).toContain('pick = "highest"');
  expect(saved).toContain('from = "splitter[0].rest"');
  expect(saved).toContain('to = "instrument[1]"');
  expect(saved).toContain('from = "splitter[0]"');

  // Undo takes the cable back in both pages, and then the switch; the file has neither after the next save.
  await page.getByRole("button", { name: "Undo draw a cable of notes" }).click();
  await expect(notesCables(page)).toHaveCount(2);
  await expect(notesCables(other)).toHaveCount(2);
  await page.getByRole("button", { name: "Undo let Bass line pick the highest note" }).click();
  await expect(pick(other, "Bass line", "lowest")).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Unsaved changes")).toHaveCount(0);
  const again = readFileSync(own.file, "utf8");
  expect(again).not.toContain("splitter[0].rest");
  expect(again).not.toContain('pick = "highest"');
  await other.close();
});

plain("a splitter is made from the MIDI menu of the canvas on the real program, is renamed, and is taken out with its cables: the second page has each, and the file keeps them", async ({ page, browser, own }) => {
  await connect(page, own.url);
  const other = await browser.newPage();
  await connect(other, own.url);

  // The canvas's menu has the MIDI menu with the keys and the box and the splitter in it: the keys are the first of them while the session has none.
  const first = await emptySpot(page);
  await page.mouse.click(first.x, first.y, { button: "right" });
  await page.getByRole("menuitem", { name: "MIDI" }).click();
  // (The program is started with no MIDI input: there are none to list under the keys.)
  expect(await page.getByRole("menu", { name: "MIDI" }).getByRole("menuitem").allTextContents()).toEqual(["Keys", "Pattern box", "Note splitter"]);
  await page.getByRole("menuitem", { name: "Note splitter" }).click();
  await expect(splitter(page, "Splitter 1")).toBeVisible();
  await expect(splitter(other, "Splitter 1")).toBeVisible();

  // Its name is changed in place, and the other page has it.
  await page.getByRole("button", { name: "Rename Splitter 1" }).click();
  await page.getByRole("textbox", { name: "Name of Splitter 1" }).fill("Bass");
  await page.getByRole("textbox", { name: "Name of Splitter 1" }).press("Enter");
  await expect(splitter(other, "Bass")).toBeVisible();

  // The keys are added from the same menu, then a cable of notes from the keys into the splitter and one from the splitter to the lane, from the menus of the splitter.
  const second = await emptySpot(page);
  await page.mouse.click(second.x, second.y, { button: "right" });
  await page.getByRole("menuitem", { name: "MIDI" }).click();
  await page.getByRole("menuitem", { name: "Keys" }).click();
  await expect(page.getByText("Keys", { exact: true }).first()).toBeVisible();
  // (The menu that takes notes in is on the line of the node's badge, in its header; the menus that send them are with the outputs, in its body.)
  await node(page, "Bass").getByRole("button", { name: "Take notes into Bass" }).click();
  await page.getByRole("menuitem", { name: "Keys" }).click();
  await splitter(page, "Bass").getByRole("button", { name: "Send the lowest note from Bass" }).click();
  await page.getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(notesCables(page)).toHaveCount(2);
  await expect(notesCables(other)).toHaveCount(2);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved).toContain("[[splitter]]");
  expect(saved).toContain('name = "Bass"');
  expect(saved).toContain('from = "keys"');
  expect(saved).toContain('to = "splitter[0]"');
  expect(saved).toContain('from = "splitter[0]"');

  // It is taken out with its cables, in both pages, and the file has neither after the next save.
  await page.getByRole("button", { name: "Remove Bass" }).click();
  await expect(splitter(other, "Bass")).toHaveCount(0);
  await expect(notesCables(other)).toHaveCount(0);
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Unsaved changes")).toHaveCount(0);
  const gone = readFileSync(own.file, "utf8");
  expect(gone).not.toContain("[[splitter]]");
  expect(gone).not.toContain("splitter[0]");
  await other.close();
});
