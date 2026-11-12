import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { notesCables } from "./canvas";
import { test as plain, testWithPatterns } from "./own";

// The pattern boxes on the real program, on the null audio backend (a program of its own: the file it saves is read, and a second page is told): a file that has a box and a cable of notes from it, a recording on the real engine, which
// the real player makes (the page's mock holds to it: `patternBoxTakes.test.ts`), the box's notes in both pages and in the file, and what each page asks of it.
const test = testWithPatterns();

const box = (page: Page, name: string): Locator => page.getByRole("group", { name: `Pattern box ${name}` });
const record = (page: Page, name: string): Locator => box(page, name).getByRole("button", { name: new RegExp(`^(Record into|Finish recording) ${name}$`) });
const play = (page: Page, name: string): Locator => box(page, name).getByRole("button", { name: new RegExp(`^(Play|Stop) ${name}$`) });
const line = (page: Page, name: string): Locator => box(page, name).locator(".tang-pattern__line");
const marks = (page: Page, name: string): Locator => box(page, name).locator(".tang-roll__note");
/** The notes that a saved file has in its boxes, a line each (`{ at = …, len = …, note = …, vel = … }`). */
const notesOf = (saved: string): string[] => saved.split("\n").filter((each) => /^\s*\{ at = /.test(each));

test("a box that the file has is on the page with its note and its cable, records on the real engine, both pages and the saved file have what it recorded, and Undo takes it back", async ({ page, browser, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");

  // The box of the file: four beats, one note, a cable of notes from it to the lane, and (no cable of notes ends at it) it records every source.
  await expect(box(page, "Bass")).toBeVisible();
  await expect(line(page, "Bass")).toHaveText("4 beats · 1 note");
  await expect(marks(page, "Bass")).toHaveCount(1);
  await expect(notesCables(page)).toHaveCount(1);
  await expect(page.getByText("Unsaved changes")).toHaveCount(0);

  // 150 beats a minute: a count-in and a pass of four beats take a second and six tenths each. A second page is told what the first does.
  await page.getByRole("spinbutton", { name: "Tempo" }).fill("150");
  await page.getByRole("spinbutton", { name: "Tempo" }).press("Enter");
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect(line(other, "Bass")).toHaveText("4 beats · 1 note");

  // It counts in, in both pages, and then takes a pass; a key of the computer that is played in it is the box's note (a key that is played in the count-in is not).
  await record(page, "Bass").click();
  await expect(record(page, "Bass")).toHaveText("■ Finish");
  await expect(line(page, "Bass")).toHaveText("Counting in, then 4 beats: play now");
  await expect(line(other, "Bass")).toHaveText("Counting in, then 4 beats: play now");
  await expect(line(page, "Bass")).toHaveText("Recording 4 beats: play now", { timeout: 15_000 });
  await page.keyboard.down("z");
  await page.keyboard.up("z");

  // When the pass is over both pages have the notes that it took (it overwrites: the note of the file is gone) and the box plays them; the session is unsaved.
  await expect(line(page, "Bass")).toHaveText("Playing · 4 beats · 1 note", { timeout: 15_000 });
  await expect(line(other, "Bass")).toHaveText("Playing · 4 beats · 1 note");
  await expect(play(page, "Bass")).toHaveText("■ Stop");
  await expect(marks(other, "Bass")).toHaveCount(1);
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await expect(page.getByRole("button", { name: "Undo record into Bass" })).toBeVisible();

  // The file has the box with the note that was recorded, where the note of the file was.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved).toContain("[[pattern]]");
  expect(saved).toContain('name = "Bass"');
  expect(saved).toContain("length_beats = 4.0");
  expect(saved).toContain('from = "pattern[0]"');
  // (One note, and not the note of the file, which was at the start of the loop and half a beat long: the key was played in the pass, and a key that comes up as it went down is the shortest note.)
  expect(notesOf(saved)).toHaveLength(1);
  expect(saved).not.toContain("{ at = 0.0, len = 0.5, note = \"C3\", vel = 100 }");
  expect(saved).toContain("len = 0.015625");

  // Stop is the engine's too: the box stops in both pages. Undo gives the note of the file back, in both.
  await play(page, "Bass").click();
  await expect(line(page, "Bass")).toHaveText("4 beats · 1 note");
  await expect(line(other, "Bass")).toHaveText("4 beats · 1 note");
  await page.getByRole("button", { name: "Undo record into Bass" }).click();
  await expect(line(page, "Bass")).toHaveText("4 beats · 1 note");
  await expect(line(other, "Bass")).toHaveText("4 beats · 1 note");
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  // (One note again, and the file says which: the one it had.)
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Unsaved changes")).toHaveCount(0);
  expect(readFileSync(own.file, "utf8")).toContain("{ at = 0.0, len = 0.5, note = \"C3\", vel = 100 }");
  await other.close();
});

plain("a box is made on the page, its settings and its notes are the session's: the second page has them and the file keeps them, and it is taken out with its cables", async ({ page, browser, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");

  await page.getByRole("button", { name: "Add a pattern box" }).click();
  await expect(box(page, "Pattern 1")).toBeVisible();
  await expect(box(other, "Pattern 1")).toBeVisible();
  await expect(line(other, "Pattern 1")).toHaveText("4 beats · no notes");

  // A setting that is turned in one page is turned in the other, and the file has it.
  await box(page, "Pattern 1").getByRole("button", { name: "Overdub into Pattern 1" }).click();
  await expect(box(other, "Pattern 1").getByRole("button", { name: "Overdub into Pattern 1" })).toHaveAttribute("aria-pressed", "true");
  await box(page, "Pattern 1").getByRole("button", { name: "Click while Pattern 1 records" }).click();
  await box(page, "Pattern 1").getByRole("spinbutton", { name: "Length of Pattern 1" }).fill("8");
  await box(page, "Pattern 1").getByRole("spinbutton", { name: "Length of Pattern 1" }).press("Enter");
  await expect(line(other, "Pattern 1")).toHaveText("8 beats · no notes");

  // A cable of notes from it to the lane is drawn from its menu, and the lane has it in the other page too.
  await page.getByRole("button", { name: "Send notes from Pattern 1" }).click();
  await page.getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(notesCables(page)).toHaveCount(1);
  await expect(notesCables(other)).toHaveCount(1);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved).toContain("[[pattern]]");
  expect(saved).toContain("length_beats = 8.0");
  expect(saved).toContain("overdub = true");
  expect(saved).toContain("click = false");
  expect(saved).toContain('from = "pattern[0]"');

  // It is taken out with its cable, in both pages, and the file has neither after the next save.
  await page.getByRole("button", { name: "Remove Pattern 1" }).click();
  await expect(box(other, "Pattern 1")).toHaveCount(0);
  await expect(notesCables(other)).toHaveCount(0);
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Unsaved changes")).toHaveCount(0);
  expect(readFileSync(own.file, "utf8")).not.toContain("[[pattern]]");
  await other.close();
});
