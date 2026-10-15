import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { test } from "./own";

// Moving the nodes of the patch, on the real program (a program of its own: where a node is put is in the file it saves).

/** Where the canvas draws a node, in its own pixels (not the screen's: the view can be zoomed and panned, and is fitted to what is in it). */
async function placeOf(node: Locator): Promise<{ x: number; y: number }> {
  const transform = await node.evaluate((element) => (element as HTMLElement).style.transform);
  const found = /translate\((-?[\d.e-]+)px,\s*(-?[\d.e-]+)px\)/.exec(transform);
  if (!found?.[1] || !found[2]) throw new Error(`the node is not placed: ${transform}`);
  return { x: Number(found[1]), y: Number(found[2]) };
}

/** A place as the server keeps it, to a tenth of a pixel (`NodeUi::tidy`): what the canvas takes when the server's answer is in. */
const tidy = (n: number): number => Math.round(n * 10) / 10;

/** The node of the canvas with this name in its header. */
const nodeNamed = (page: Page, name: string): Locator => page.locator(".react-flow__node", { has: page.locator(".tang-node__name", { hasText: new RegExp(`^${name}$`) }) });

test("a node dragged on the canvas stays where it is put down, a page that is opened again and a second page have it there, the saved file has it, and the canvas can be arranged again", async ({ page, browser, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const filter = nodeNamed(page, "Filter");
  await expect(filter).toBeVisible();
  const laidOut = await placeOf(filter);
  await expect(page.getByRole("button", { name: "Arrange the nodes again" })).toBeDisabled();

  // The filter is taken by its header (by the name: the buttons at the other end are controls) and put 220 pixels to the right and 90 down.
  const header = await filter.locator(".tang-node__name").boundingBox();
  if (!header) throw new Error("the filter has no header");
  const from = { x: header.x + 6, y: header.y + header.height / 2 };
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + 220, from.y + 90, { steps: 12 });
  await page.mouse.up();

  // It stays: the session has the place, which the page is told, and what is unsaved is said. (Where the pointer put it down is where the node is
  // until the server's answer is in, a moment later, and then it is where the server kept it, to a tenth of a pixel: that is the place to compare.)
  await expect
    .poll(
      async () => {
        const place = await placeOf(filter);
        return place.x > laidOut.x + 100 && place.x === tidy(place.x) && place.y === tidy(place.y);
      },
      { message: "the filter has moved to the right, and the server's answer is in" },
    )
    .toBe(true);
  const moved = await placeOf(filter);
  expect(moved.y).toBeGreaterThan(laidOut.y + 40);
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await expect(page.getByRole("button", { name: "Arrange the nodes again" })).toBeEnabled();

  // The other nodes did not move, and the cable from the LFO still ends at the filter's cutoff (it is drawn between the nodes where they are).
  await expect(page.locator(".react-flow__edge")).toHaveCount(3);

  // A second page is given the place in its snapshot...
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect.poll(async () => placeOf(nodeNamed(other, "Filter"))).toEqual(moved);
  // ... and is told of the next one as it is made: the oscillator is moved by the first page and the second page draws it there.
  const oscillator = nodeNamed(page, "Oscillator");
  const before = await placeOf(nodeNamed(other, "Oscillator"));
  const name = await oscillator.locator(".tang-node__name").boundingBox();
  if (!name) throw new Error("the oscillator has no header");
  await page.mouse.move(name.x + 6, name.y + name.height / 2);
  await page.mouse.down();
  await page.mouse.move(name.x + 6 - 40, name.y + name.height / 2 + 160, { steps: 8 });
  await page.mouse.up();
  await expect.poll(async () => (await placeOf(nodeNamed(other, "Oscillator"))).y, { message: "the second page draws the oscillator where it was moved" }).toBeGreaterThan(before.y + 60);

  // A page that is opened again has them where they were put.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect.poll(async () => placeOf(nodeNamed(page, "Filter"))).toEqual(moved);

  // The saved file has the two places, each on its own node, and only those.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved.match(/^ui = \{ x = -?[\d.]+, y = -?[\d.]+ \}$/gm), saved).toHaveLength(2);
  expect(saved).not.toContain("[ui]");

  // Arranged again, every node is where the layout has it, and the file has none.
  await page.getByRole("button", { name: "Arrange the nodes again" }).click();
  await expect.poll(async () => placeOf(nodeNamed(page, "Filter"))).toEqual(laidOut);
  await expect(page.getByRole("button", { name: "Arrange the nodes again" })).toBeDisabled();
  await expect.poll(async () => placeOf(nodeNamed(other, "Filter"))).toEqual(laidOut);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(own.file, "utf8")).not.toMatch(/^ui = /m);
  await other.close();
});
