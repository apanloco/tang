import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { expectNodesApart, node, nodeBoxes, open } from "./canvas";

// A node that is added where a person has put another, against the mock engine in the page (docs/web-ui.md §9.3, f80): the layout puts what is added where it
// would go if nothing were moved, which can be on a node that was moved there, and hide it. It moves it clear instead.

/** Where the canvas draws a node, in its own pixels (not the screen's: the view can be panned or zoomed, and is when a node is added that is not in it). */
async function placeOf(target: Locator): Promise<{ x: number; y: number }> {
  const transform = await target.evaluate((element) => (element as HTMLElement).style.transform);
  const found = /translate\((-?[\d.e-]+)px,\s*(-?[\d.e-]+)px\)/.exec(transform);
  if (!found?.[1] || !found[2]) throw new Error(`the node is not placed: ${transform}`);
  return { x: Number(found[1]), y: Number(found[2]) };
}

/** A place as the server keeps it, to a tenth of a pixel (`NodeUi::tidy`): what the canvas takes when the server's answer is in. */
const tidy = (n: number): number => Math.round(n * 10) / 10;

/** Take a node by the name in its header and put it down `dx` pixels to the right and `dy` down. */
async function dragBy(page: Page, target: Locator, dx: number, dy: number): Promise<void> {
  const header = await target.locator(".tang-node__name").boundingBox();
  if (!header) throw new Error("the node has no header");
  const from = { x: header.x + 6, y: header.y + header.height / 2 };
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(from.x + dx, from.y + dy, { steps: 12 });
  await page.mouse.up();
}

test("an effect that is added where a node was put by hand does not hide it: it is moved clear, below, and nothing lies over anything", async ({ page }) => {
  await open(page);
  const oscillator = node(page, "Oscillator");
  const filter = node(page, "Filter");
  const [oscillatorBox, filterBox] = await Promise.all([oscillator.boundingBox(), filter.boundingBox()]);
  if (!oscillatorBox || !filterBox) throw new Error("a node is not drawn");
  const laidOut = { oscillator: await placeOf(oscillator), filter: await placeOf(filter) };
  const column = laidOut.filter.x - laidOut.oscillator.x;

  // The filter is put where the next effect of the lane would go: a column to the right of where it is.
  await dragBy(page, filter, filterBox.x - oscillatorBox.x, 0);
  // (It is put down a little short of where the pointer is: the canvas takes a drag from a few pixels on. Most of the way is what matters, and the server's answer is in.)
  await expect
    .poll(async () => {
      const place = await placeOf(filter);
      return place.x > laidOut.filter.x + column * 0.8 && place.x === tidy(place.x) && place.y === tidy(place.y);
    })
    .toBe(true);
  const put = await placeOf(filter);
  await expectNodesApart(page);

  // An effect is added at the end of the lane's chain.
  await oscillator.getByRole("button", { name: /^Add an effect/ }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  const reverb = node(page, "Reverb");
  await expect(reverb).toBeVisible();

  // It is not on the filter, nor on anything else, and the filter is where it was put.
  await expectNodesApart(page);
  expect(await placeOf(filter)).toEqual(put);
  // The reverb is in the column where the layout puts the next effect, which is two from the oscillator's, and below the filter.
  const added = await placeOf(reverb);
  expect(added.x).toBeCloseTo(laidOut.oscillator.x + 2 * column, 0);
  const boxes = await nodeBoxes(page);
  const still = boxes.find((box) => box.name === "Filter");
  const below = boxes.find((box) => box.name === "Reverb");
  expect(below?.top ?? 0).toBeGreaterThan((still?.top ?? 0) + (still?.height ?? 0));
  await expect(reverb).toBeInViewport();
});

test("a node that is put down on one that the layout placed moves that one clear: nothing is hidden when a node is dropped on another either", async ({ page }) => {
  await open(page);
  const oscillator = node(page, "Oscillator");
  const filter = node(page, "Filter");
  const [oscillatorBox, filterBox] = await Promise.all([oscillator.boundingBox(), filter.boundingBox()]);
  if (!oscillatorBox || !filterBox) throw new Error("a node is not drawn");

  // The oscillator is dropped on the filter, which the layout placed (nobody moved it).
  const column = filterBox.x - oscillatorBox.x;
  await dragBy(page, oscillator, column, 0);
  await expect.poll(async () => (await oscillator.boundingBox())?.x ?? 0).toBeGreaterThan(oscillatorBox.x + column * 0.8);
  // (The filter was not moved by anyone, and is below the oscillator now.)
  await expect.poll(async () => ((await filter.boundingBox())?.y ?? 0) > oscillatorBox.y + oscillatorBox.height).toBe(true);
  await expectNodesApart(page);
});
