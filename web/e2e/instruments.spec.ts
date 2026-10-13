import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

import { test } from "./own";

// Instruments from the page, on the real program (a program of its own: a lane cannot be taken out again).

/** The loudest the master's meter is for `ms`, in decibels (its floor is -60). */
async function loudest(page: Page, ms: number): Promise<number> {
  const meter = page.getByRole("meter", { name: "Master level" });
  const until = Date.now() + ms;
  let top = Number.NEGATIVE_INFINITY;
  while (Date.now() < until) {
    top = Math.max(top, Number(await meter.getAttribute("aria-valuenow")));
    await page.waitForTimeout(25);
  }
  return top;
}

test("an instrument is put in a lane from its menu, another is added in a lane of its own from the bar and heard, and the saved file has both lanes", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const lane = (name: string) => page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: new RegExp(`^${name}$`) }) });
  const meter = page.getByRole("meter", { name: "Master level" });
  const level = async (): Promise<number> => Number(await meter.getAttribute("aria-valuenow"));

  // The tracer's lane has its saw, and a new instrument put in it by its menu starts as a sine: the lane keeps its filter and its LFO.
  const waveform = lane("Oscillator").getByRole("combobox", { name: "waveform" });
  await expect(waveform).toHaveValue("3");
  await page.getByRole("button", { name: "Change the instrument of Oscillator" }).click();
  const instruments = page.getByRole("menu", { name: "Change the instrument of Oscillator" }).getByRole("group", { name: "Built-in" });
  await expect(instruments.getByRole("menuitem")).toHaveText(["Oscillator"]);
  await instruments.getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(waveform).toHaveValue("0");
  await expect(lane("Filter")).toBeVisible();
  await expect(lane("LFO")).toBeVisible();
  await expect(page.getByRole("button", { name: "Change the instrument of Oscillator" })).toBeFocused();
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // The lane is quiet on purpose (its volume is 0.1, which the lane keeps): a note, and the level it makes.
  await page.keyboard.down("z");
  await expect.poll(level, { message: "the note is heard" }).toBeGreaterThan(-50);
  const before = await loudest(page, 600);
  await page.keyboard.up("z");
  await expect.poll(level, { message: "the master is quiet again" }).toBeLessThan(-50);

  // The bar's menu has what the program found installed: the built-ins are always there.
  await page.getByRole("button", { name: "Add an instrument" }).click();
  const builtins = page.getByRole("menu", { name: "Add an instrument" }).getByRole("group", { name: "Built-in" });
  await expect(builtins.getByRole("menuitem")).toHaveText(["Oscillator"]);
  await builtins.getByRole("menuitem", { name: "Oscillator" }).click();

  // A lane of its own is on the canvas, called by a number now that there are two, with the buttons of a lane. It was added below
  // the first, which fills the screen: the canvas has moved to show it.
  await expect(lane("Oscillator 2")).toBeVisible();
  await expect(lane("Oscillator 2")).toBeInViewport({ ratio: 1 });
  await expect(page.getByRole("button", { name: "Change the instrument of Oscillator 2" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Add an effect to Oscillator 2" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Add an instrument" })).toBeFocused();

  // The next note is heard through it too: the new oscillator is at its own full volume, far louder than the first lane.
  await page.keyboard.down("z");
  await expect.poll(level, { message: "the note through both lanes" }).toBeGreaterThan(before + 10);
  await page.keyboard.up("z");

  // What is saved has both lanes, and the first has none of the saw it had.
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved.match(/\[\[instrument\]\]/g)).toHaveLength(2);
  expect(saved.match(/plugin = "builtin:osc"/g)).toHaveLength(2);
  expect(saved).not.toContain("waveform = 3");
  expect(saved).toContain('plugin = "builtin:filter"');
  expect(saved).toContain("volume = 0.1");
});
