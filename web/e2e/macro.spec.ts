import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { angleOf, fractionOf, pointAt } from "../src/ui/knob-geometry";
import { node } from "./canvas";
import { modulatorsIn, test } from "./own";

// A macro on the real program (a program of its own, on the null audio backend: the file it saves is read, and the history is the program's): made from a knob
// with the knob's menu, it is a node with one knob, its value, which adds to what it drives as it is turned, in the engine, one change of the history for the
// macro and one for each value it is turned to, and in the saved file.

async function open(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" })).toBeVisible();
}

/** The cell of the control called `name` in a node, and the button on it that modulates it. */
const cell = (plugin: Locator, name: string): Locator => plugin.locator(".tang-cell").filter({ has: plugin.page().locator(`[aria-label="${name}"]`) });
const modulate = (plugin: Locator, name: string): Locator => plugin.getByRole("button", { name: `Modulate ${name}`, exact: true });

async function choose(plugin: Locator, name: string, item: string): Promise<void> {
  await cell(plugin, name).hover();
  await modulate(plugin, name).click();
  await plugin.page().getByRole("menuitem", { name: item, exact: true }).click();
}

/** The knob of a parameter: what holds the dial, its ring and its dot. */
const knobOf = (plugin: Locator, name: string): Locator => plugin.locator(".tang-knob", { has: plugin.page().getByRole("slider", { name, exact: true }) });

/** Where the dot of a knob is, in the knob's own pixels. */
async function dotAt(dot: Locator): Promise<{ x: number; y: number }> {
  return { x: Number(await dot.getAttribute("cx")), y: Number(await dot.getAttribute("cy")) };
}

/** Where the dot of the filter's resonance knob (0.5 to 10, turned evenly) is when the engine has set the resonance to `resonance`. */
const dotFor = (resonance: number): { x: number; y: number } => pointAt(24, 18, angleOf(fractionOf(resonance, 0.5, 10, "linear")));

const save = async (page: Page, file: string): Promise<string> => {
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  return readFileSync(file, "utf8");
};

test("a macro made from the resonance knob moves the engine's resonance as it is turned, which the knob's dot shows, and the file has its value", async ({ page, own }) => {
  await open(page, own.url);
  const filter = node(page, "Filter");
  await choose(filter, "resonance", "New macro");
  const macro = node(page, "Macro");
  await expect(macro).toBeVisible();
  const value = macro.getByRole("slider", { name: "Value", exact: true });
  // It is at rest, with a cable to the resonance at half depth, and no cable can end at its value.
  await expect(value).toHaveAttribute("aria-valuenow", "0");
  await expect(page.getByRole("slider", { name: "Depth of Macro → Filter: resonance" })).toHaveValue("0.5");
  await expect(macro.locator('.react-flow__handle[data-handleid="value"]')).toHaveCount(0);
  await expect(modulate(macro, "value")).toHaveCount(0);

  // At rest it adds nothing: the resonance is where the user set it, 2.
  const dot = knobOf(filter, "resonance").locator(".tang-knob__dot");
  await expect(dot).toHaveAttribute("visibility", "visible");
  await expect.poll(async () => (await dotAt(dot)).x).toBeCloseTo(dotFor(2).x, 1);
  expect((await dotAt(dot)).y).toBeCloseTo(dotFor(2).y, 1);

  // Turned all the way up it adds the depth of the range of the resonance, half of 9.5, to the 2: 6.75. The knob of the resonance is the user's all the while.
  await value.focus();
  await page.keyboard.press("End");
  await expect(value).toHaveAttribute("aria-valuenow", "1");
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(dotFor(6.75).x, 1);
  expect((await dotAt(dot)).y).toBeCloseTo(dotFor(6.75).y, 1);
  await expect(filter.getByRole("slider", { name: "resonance", exact: true })).toHaveAttribute("aria-valuenow", "2");

  // Half way up it adds half of that: 4.375.
  for (let i = 0; i < 50; i += 1) await page.keyboard.press("ArrowDown");
  await expect(value).toHaveAttribute("aria-valuenow", "0.5");
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(dotFor(4.375).x, 1);

  // The file has it, as a macro with the value it is turned to, the lane it hears and its cable to the filter (the second modulator, after the tracer's LFO).
  await page.keyboard.press("End");
  await expect(value).toHaveAttribute("aria-valuenow", "1");
  const saved = await save(page, own.file);
  const parts = modulatorsIn(saved);
  expect(parts).toHaveLength(2);
  expect(parts[1]).toBe('type = "macro"\nvalue = 1.0\nhears = "instrument[0]"\n\n[[modulator.target]]\nparam = "resonance"\nto = "instrument[0].effect[0]"\ndepth = 0.5\n\n');
});

test("the value of a macro is a step of the history that Undo puts back, and the macro and its cable are a step of their own: Undo takes them out and Redo makes them again", async ({ page, own }) => {
  await open(page, own.url);
  const filter = node(page, "Filter");
  await choose(filter, "resonance", "New macro");
  const macro = node(page, "Macro");
  const value = macro.getByRole("slider", { name: "Value", exact: true });
  await expect(page.getByRole("button", { name: "Undo modulate resonance", exact: true })).toBeVisible();
  await value.focus();
  await page.keyboard.press("End");
  await expect(value).toHaveAttribute("aria-valuenow", "1");
  await expect(page.getByRole("button", { name: "Undo set value", exact: true })).toBeVisible();

  // The first Undo puts the value back, and the macro stays.
  await page.getByRole("button", { name: "Undo set value", exact: true }).click();
  await expect(value).toHaveAttribute("aria-valuenow", "0");
  await expect(page.getByRole("button", { name: "Undo modulate resonance", exact: true })).toBeVisible();
  // The second takes the macro out with its cable.
  await page.getByRole("button", { name: "Undo modulate resonance", exact: true }).click();
  await expect(macro).toHaveCount(0);
  expect(await save(page, own.file)).not.toContain('type = "macro"');
  // Redo makes it again, at rest, and then puts the value up again.
  await page.getByRole("button", { name: "Redo modulate resonance", exact: true }).click();
  await expect(node(page, "Macro")).toBeVisible();
  await expect(node(page, "Macro").getByRole("slider", { name: "Value", exact: true })).toHaveAttribute("aria-valuenow", "0");
  await page.getByRole("button", { name: "Redo set value", exact: true }).click();
  await expect(node(page, "Macro").getByRole("slider", { name: "Value", exact: true })).toHaveAttribute("aria-valuenow", "1");
  expect(await save(page, own.file)).toContain('type = "macro"\nvalue = 1.0');
});

test("a macro that the lane's menu adds is at rest, and a second page sees its value as it is turned", async ({ page, browser, own }) => {
  await open(page, own.url);
  await page.getByRole("button", { name: "Add a modulator to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Macro", exact: true }).click();
  const macro = node(page, "Macro");
  await expect(macro).toBeVisible();
  await expect(macro.getByRole("slider", { name: "Value", exact: true })).toHaveAttribute("aria-valuenow", "0");
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect(node(other, "Macro")).toBeVisible();
  await macro.getByRole("slider", { name: "Value", exact: true }).focus();
  await page.keyboard.press("End");
  await expect(node(other, "Macro").getByRole("slider", { name: "Value", exact: true })).toHaveAttribute("aria-valuenow", "1");
  await other.close();
});
