import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

// What the tests of the canvas against the mock engine in the page have in common: the page opened on a scene, the nodes, their
// ports, and a cable dragged by hand.

export async function open(page: Page, scene = "tracer"): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(`/?engine=mock&scene=${scene}`);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.locator(".react-flow__node").first()).toBeVisible();
}

/** The node of the canvas whose name is exactly `name`. */
export const node = (page: Page, name: string): Locator => page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: new RegExp(`^${name}$`) }) });

/** The output of a modulator, where a cable is started. */
export const output = (modulator: Locator): Locator => modulator.locator('.react-flow__handle[data-handleid="modOut"]');

/** The port of the control of a parameter, where a cable ends. */
export const port = (plugin: Locator, name: string): Locator => plugin.locator(".tang-cell").filter({ has: plugin.page().locator(`[aria-label="${name}"]`) }).locator(".react-flow__handle");

export async function centre(target: Locator): Promise<{ x: number; y: number }> {
  const box = await target.boundingBox();
  if (!box) throw new Error("not on the page");
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** Press on `from` and move to `to`, and let go there unless `release` is false: a cable dragged by hand. */
export async function drag(page: Page, from: Locator, to: Locator | { x: number; y: number }, release = true): Promise<void> {
  const start = await centre(from);
  const end = "x" in to ? to : await centre(to);
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
  await page.mouse.move((start.x + end.x) / 2, (start.y + end.y) / 2, { steps: 8 });
  await page.mouse.move(end.x, end.y, { steps: 8 });
  if (release) await page.mouse.up();
}

export const cables = (page: Page): Locator => page.locator(".tang-cable--modulation");

/** The port of a setting of a modulator (an LFO's rate, a stage of an envelope), where a cable from another modulator ends. */
export const setting = (modulator: Locator, name: string): Locator => modulator.locator(`.react-flow__handle[data-handleid="${name}"]`);

/** An envelope put in the oscillator's lane from its menu (the tracer's LFO is in it already). */
export async function addEnvelope(page: Page): Promise<Locator> {
  await page.getByRole("button", { name: "Add a modulator to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Envelope" }).click();
  const envelope = node(page, "ADSR");
  await expect(envelope).toBeVisible();
  return envelope;
}
