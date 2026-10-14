import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";

import { node, open } from "./canvas";
import { test } from "./own";

// Undo and Redo from the page, on the real program (a program of its own: the history is the program's, whatever page changed what, so what
// other tests did would be on it).

test("a knob turned in one go is put back by one Undo, by the button and by the keys, a second page has the same history and does it again, and the saved file has what is done", async ({ page, browser, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  await expect(undo).toBeDisabled();
  await expect(redo).toBeDisabled();

  // Five presses of an arrow, a moment apart, are one thing that was done: the program makes one step of them.
  const detune = node(page, "Oscillator").getByRole("slider", { name: "detune", exact: true });
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await detune.focus();
  for (let press = 0; press < 5; press += 1) await page.keyboard.press("ArrowRight");
  const turned = (await detune.getAttribute("aria-valuenow")) ?? "";
  expect(Number(turned)).toBeGreaterThan(0);
  await expect(undo).toBeEnabled();
  await expect(undo).toHaveAccessibleName("Undo set detune");
  await expect(redo).toBeDisabled();
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // The button puts all of it back at once, and says what it would do again.
  await undo.click();
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await expect(undo).toBeDisabled();
  await expect(redo).toHaveAccessibleName("Redo set detune");

  // The keys do it again and put it back, whichever way they are written (Control, or Command on a Mac).
  await page.keyboard.press("ControlOrMeta+Shift+Z");
  await expect(detune).toHaveAttribute("aria-valuenow", turned);
  await expect(redo).toBeDisabled();
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await expect(redo).toBeEnabled();

  // A second page is told what can be done when it comes, shows what the session has, and what it does is on the first page as what anybody does.
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect(other.getByRole("button", { name: /^Redo/ })).toHaveAccessibleName("Redo set detune");
  await expect(other.getByRole("button", { name: /^Undo/ })).toBeDisabled();
  await expect(node(other, "Oscillator").getByRole("slider", { name: "detune", exact: true })).toHaveAttribute("aria-valuenow", "0");
  await other.getByRole("button", { name: /^Redo/ }).click();
  await expect(detune).toHaveAttribute("aria-valuenow", turned);
  await expect(undo).toBeEnabled();
  await expect(redo).toBeDisabled();

  // What is saved is what is done: the setting is in the file; put back, it is not the value it was.
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(Number(/^detune\s*=\s*(\S+)$/m.exec(readFileSync(own.file, "utf8"))?.[1])).toBeCloseTo(Number(turned), 5);
  await undo.click();
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(/^detune\s*=\s*(?!0(\.0*)?$)\S+$/m.test(readFileSync(own.file, "utf8")), readFileSync(own.file, "utf8")).toBe(false);
  await other.close();
});

test("a key typed in a field that takes text is the field's own undo and not the session's, and something that cannot be undone yet empties the history", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const detune = node(page, "Oscillator").getByRole("slider", { name: "detune", exact: true });
  await detune.focus();
  await page.keyboard.press("End");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  await expect(undo).toBeEnabled();

  // In the filter of the panel of parameters, Control+Z is the field's: the text goes back, the detune stays where it was put.
  await page.getByRole("button", { name: "Parameters of Oscillator" }).click();
  const search = page.getByRole("searchbox", { name: "Filter the parameters of Oscillator" });
  await expect(search).toBeFocused();
  await page.keyboard.type("ab");
  await expect(search).toHaveValue("ab");
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(search).not.toHaveValue("ab");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  await expect(undo).toBeEnabled();

  // A change that nothing puts back yet (an effect added) empties it: there is nothing to undo, and the keys leave Control+Z alone.
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  await expect(node(page, "Reverb")).toBeVisible();
  await expect(undo).toBeDisabled();
  await detune.focus();
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
});

test("the mock engine in the page has the same buttons and keys, and they do what the program's do", async ({ page }) => {
  await open(page);
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  await expect(undo).toBeDisabled();
  const detune = node(page, "Oscillator").getByRole("slider", { name: "detune", exact: true });
  await detune.focus();
  await page.keyboard.press("End");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  await expect(undo).toHaveAccessibleName("Undo set detune");
  await undo.click();
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await expect(redo).toHaveAccessibleName("Redo set detune");
  // (Control+Y, on a Mac too: Command+Y is the browser's own History.)
  await page.keyboard.press("Control+Y");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
});
