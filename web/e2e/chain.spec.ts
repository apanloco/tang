import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";

import { test } from "./own";

// The order of the effects of a lane from the page, on the real program (a program of its own: a reverb is put in the lane, which the shared one would keep).

test("an effect is moved before another from its button, the program has the new order and the note is still heard, and so has the file that is saved", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const effect = (name: string) => page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: new RegExp(`^${name}$`) }) });
  const meter = page.getByRole("meter", { name: "Master level" });
  const level = async (): Promise<number> => Number(await meter.getAttribute("aria-valuenow"));
  const leftOf = async (a: ReturnType<typeof effect>, b: ReturnType<typeof effect>): Promise<boolean> => {
    const [boxA, boxB] = [await a.boundingBox(), await b.boundingBox()];
    if (!boxA || !boxB) throw new Error("not on the page");
    return boxA.x < boxB.x;
  };

  // A reverb at the end of the lane's chain: the filter, then the reverb.
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  await expect(effect("Reverb")).toBeVisible();
  expect(await leftOf(effect("Filter"), effect("Reverb"))).toBe(true);

  // A note, held: it is heard through the chain as it is, and goes on being heard through it in the other order.
  await page.keyboard.down("z");
  await expect.poll(level, { message: "the note is heard" }).toBeGreaterThan(-50);

  await page.getByRole("button", { name: "Move Reverb earlier in its chain" }).click();
  await expect.poll(() => leftOf(effect("Reverb"), effect("Filter")), { message: "the reverb is first on the canvas" }).toBe(true);
  await expect(page.getByRole("button", { name: "Move Reverb earlier in its chain" })).toBeFocused();
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await page.waitForTimeout(600);
  await expect.poll(level, { message: "the note is still heard through the reordered chain" }).toBeGreaterThan(-50);
  await page.keyboard.up("z");
  await expect.poll(level, { message: "and dies away" }).toBeLessThan(-50);

  // A page that opens now is given the order the program has, not the one the first page laid out.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(effect("Reverb")).toBeVisible();
  expect(await leftOf(effect("Reverb"), effect("Filter"))).toBe(true);
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // What is saved has the reverb before the filter, and the LFO's cable on the filter's new slot, the second of the lane.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved.indexOf('plugin = "builtin:reverb"')).toBeGreaterThan(-1);
  expect(saved.indexOf('plugin = "builtin:reverb"')).toBeLessThan(saved.indexOf('plugin = "builtin:filter"'));
  expect(saved).toContain("slot = 2");
});
