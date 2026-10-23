import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

// What the tests of the real program have in common for what is heard: the master's meter, read for a while, and a key held while it is looked at (the computer's `z`).

/** The loudest the master's meter is for `ms`, in decibels (its floor is -60). */
export async function loudest(page: Page, ms: number): Promise<number> {
  const meter = page.getByRole("meter", { name: "Master level" });
  const until = Date.now() + ms;
  let top = Number.NEGATIVE_INFINITY;
  while (Date.now() < until) {
    top = Math.max(top, Number(await meter.getAttribute("aria-valuenow")));
    await page.waitForTimeout(25);
  }
  return top;
}

/** The level of the master's meter now, in decibels (its floor is -60). */
export const masterLevel = async (page: Page): Promise<number> => Number(await page.getByRole("meter", { name: "Master level" }).getAttribute("aria-valuenow"));

/** A key that is held, heard at the master, and let go of: the master is quiet again before the next thing is looked at. */
export async function heardAt(page: Page, what: string): Promise<void> {
  await page.keyboard.down("z");
  await expect.poll(() => masterLevel(page), { message: `${what} is heard` }).toBeGreaterThan(-50);
  await page.keyboard.up("z");
  await expect.poll(() => masterLevel(page), { message: "the master is quiet again" }).toBeLessThan(-50);
}

/** A key that is held and not heard, for the better part of a second. */
export async function silentAt(page: Page, what: string): Promise<void> {
  await page.keyboard.down("z");
  expect(await loudest(page, 800), `${what} is silent`).toBeLessThan(-50);
  await page.keyboard.up("z");
  await expect.poll(() => masterLevel(page), { message: "the master is quiet again" }).toBeLessThan(-50);
}
