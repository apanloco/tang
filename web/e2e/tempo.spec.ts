import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { test } from "./own";

// The tempo on the real program (a program of its own, on the null audio backend: the file it saves is read, and the history is the program's): set in the field of the bar it
// is the session's, which a second page sees, the saved file has (as `bpm`: a session that has not had one set has none) and one Undo puts back; the arrow keys turn it, the
// turns of a held key are one step of the history, and a tempo beyond the tempos there can be is held to them by the program.

async function open(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(tempo(page)).toBeVisible();
}

const tempo = (page: Page): Locator => page.getByRole("spinbutton", { name: "Tempo" });
const undoTempo = (page: Page): Locator => page.getByRole("button", { name: "Undo set the tempo", exact: true });

const save = async (page: Page, file: string): Promise<string> => {
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  return readFileSync(file, "utf8");
};

test("a tempo typed into the field is the session's: a second page has it, the file has it, and one Undo puts it back", async ({ page, browser, own }) => {
  await open(page, own.url);
  await expect(tempo(page)).toHaveValue("120");
  await tempo(page).fill("90");
  await tempo(page).press("Enter");
  await expect(tempo(page)).toHaveValue("90");
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await expect(undoTempo(page)).toBeVisible();

  const other = await browser.newPage();
  await open(other, own.url);
  await expect(tempo(other)).toHaveValue("90");

  // (It comes before every table in the file.)
  expect((await save(page, own.file)).startsWith("bpm = 90.0\n")).toBe(true);

  await undoTempo(page).click();
  await expect(tempo(page)).toHaveValue("120");
  await expect(tempo(other)).toHaveValue("120");
  await other.close();
});

test("the arrow keys turn the tempo, the turns of a held key are one step of the history, and a tempo beyond the tempos there can be is held to them", async ({ page, own }) => {
  await open(page, own.url);
  await tempo(page).focus();
  for (let turn = 0; turn < 5; turn += 1) await page.keyboard.press("ArrowUp");
  await expect(tempo(page)).toHaveValue("125");
  await page.keyboard.press("Shift+ArrowDown");
  await expect(tempo(page)).toHaveValue("115");

  // The six turns are one step: one Undo goes back to where they began.
  await undoTempo(page).click();
  await expect(tempo(page)).toHaveValue("120");

  await tempo(page).fill("1000");
  await tempo(page).press("Enter");
  await expect(tempo(page)).toHaveValue("300");
  await tempo(page).fill("5");
  await tempo(page).press("Enter");
  await expect(tempo(page)).toHaveValue("20");
});
