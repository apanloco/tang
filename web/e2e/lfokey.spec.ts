import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

import { node } from "./canvas";
import { modulatorsIn, test } from "./own";

// The key of an LFO from the page, on the real program (a program of its own: the file it saves is read, and the history is the program's): a choice of whether a note starts the LFO again
// and a knob for how long it takes to come in after one, which the program applies (the file it saves has them by their names) and which undo puts back.

const save = async (page: Page, file: string): Promise<string> => {
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  return readFileSync(file, "utf8");
};

test("an LFO's retrigger and fade are set from its node, saved in the file by the names the file has, and put back by undo", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const lfo = node(page, "LFO");
  const retrigger = lfo.getByRole("combobox", { name: "Retrigger" });
  const fade = lfo.getByRole("slider", { name: "Fade", exact: true });
  // It runs free and does not fade in, as the file that was loaded says by saying nothing of it; and no cable ends at either.
  await expect(retrigger).toHaveValue("0");
  await expect(fade).toHaveAttribute("aria-valuenow", "0");
  await expect(fade).toHaveAttribute("aria-valuetext", "none");
  // (Nothing is changed yet, so there is nothing to save: the file is the one that was loaded.)
  expect(modulatorsIn(readFileSync(own.file, "utf8"))[0] ?? "", "a free LFO has neither key in its file").not.toMatch(/retrigger|fade/);

  await retrigger.selectOption({ label: "on" });
  await expect(page.getByRole("button", { name: "Undo set retrigger", exact: true })).toBeVisible();
  // The top of the knob, by the keyboard, is the longest fade there can be: five seconds.
  await fade.focus();
  await page.keyboard.press("End");
  await expect(fade).toHaveAttribute("aria-valuenow", "5");
  await expect(fade).toHaveAttribute("aria-valuetext", "5 s");
  const keyed = modulatorsIn(await save(page, own.file))[0] ?? "";
  expect(keyed).toContain("retrigger = true\n");
  expect(keyed).toContain("fade = 5.0\n");

  // Undo puts the fade back, and then the switch.
  await page.getByRole("button", { name: "Undo set fade", exact: true }).click();
  await expect(fade).toHaveAttribute("aria-valuenow", "0");
  await expect(retrigger).toHaveValue("1");
  await page.getByRole("button", { name: "Undo set retrigger", exact: true }).click();
  await expect(retrigger).toHaveValue("0");
  expect(modulatorsIn(await save(page, own.file))[0] ?? "").not.toMatch(/retrigger|fade/);
});
