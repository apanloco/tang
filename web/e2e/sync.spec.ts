import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

import { node } from "./canvas";
import { test } from "./own";

// The note length that an LFO follows the tempo by, from the page, on the real program (a program of its own: the file it saves is read, and the history is the program's): a choice in the LFO's
// node that the program applies (the file it saves has it by its name, and the rate the LFO keeps), a knob of the rate that shows what the tempo makes of it and follows the tempo, and an undo.

const save = async (page: Page, file: string): Promise<string> => {
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  return readFileSync(file, "utf8");
};

/** The text of the first modulator of the saved lane. */
const firstModulator = (saved: string): string => (saved.split("[[instrument.modulator]]\n")[1] ?? "").split("[[instrument.effect]]")[0] ?? "";

test("an LFO's note length is chosen from its node, makes its rate the tempo's and follows the tempo, is saved by its name with the rate it keeps, and is put back by undo", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const lfo = node(page, "LFO");
  const sync = lfo.getByRole("combobox", { name: "Sync" });
  const rate = lfo.getByRole("slider", { name: "Rate", exact: true });
  // It goes at its own rate, as the file that was loaded says by saying nothing of it: 0.5 Hz, and the knob can be turned.
  await expect(sync.locator("option:checked")).toHaveText("free");
  await expect(rate).toHaveAttribute("aria-valuetext", "0.5 Hz");
  await expect(rate).not.toHaveAttribute("aria-disabled", "true");
  expect(firstModulator(readFileSync(own.file, "utf8")), "a free LFO has no sync key").not.toMatch(/sync/);

  // An eighth note at the 120 beats a minute of a session that says nothing of its tempo is 4 Hz, and the knob shows it and cannot be turned.
  await sync.selectOption({ label: "1/8" });
  await expect(page.getByRole("button", { name: "Undo set sync", exact: true })).toBeVisible();
  await expect(rate).toHaveAttribute("aria-valuetext", "4 Hz");
  await expect(rate).toHaveAttribute("aria-disabled", "true");
  const synced = firstModulator(await save(page, own.file));
  expect(synced).toContain('sync = "1/8"\n');
  expect(synced, "the rate that it keeps").toContain("rate = 0.5\n");

  // A page that is opened after it has the note length from the snapshot it is given, and the knob of the rate where the tempo has it.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "LFO").getByRole("combobox", { name: "Sync" }).locator("option:checked")).toHaveText("1/8");
  await expect(node(page, "LFO").getByRole("slider", { name: "Rate", exact: true })).toHaveAttribute("aria-valuetext", "4 Hz");

  // The tempo is the knob's: half of it, half the rate.
  await page.getByRole("spinbutton", { name: "Tempo" }).fill("60");
  await page.getByRole("spinbutton", { name: "Tempo" }).press("Enter");
  await expect(rate).toHaveAttribute("aria-valuetext", "2 Hz");

  // Undo puts the tempo back, and then the note length: the knob is the LFO's own again.
  await page.getByRole("button", { name: "Undo set the tempo", exact: true }).click();
  await expect(rate).toHaveAttribute("aria-valuetext", "4 Hz");
  await page.getByRole("button", { name: "Undo set sync", exact: true }).click();
  await expect(sync.locator("option:checked")).toHaveText("free");
  await expect(rate).toHaveAttribute("aria-valuetext", "0.5 Hz");
  await expect(rate).not.toHaveAttribute("aria-disabled", "true");
  expect(firstModulator(await save(page, own.file)), "put back: no sync key, and the rate it had").not.toMatch(/sync/);
});
