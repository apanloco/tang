import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { addEnvelope, arrange, cables, drag, expectNodesApart, node, open, output, setting, still } from "./canvas";

// A modulator that drives the settings of another is drawn above it when the nodes are arranged, with the cable running down to it (docs/web-ui.md §9.3): a
// chain of modulators is a cascade, one row for each step, over the strip they end at. A cable does not arrange the nodes: they stay where they are until
// the button of the canvas is pressed.

/** Where a node is on the screen. */
async function boxOf(target: Locator): Promise<{ left: number; top: number; right: number; bottom: number }> {
  const box = await target.boundingBox();
  if (!box) throw new Error("not on the page");
  return { left: box.x, top: box.y, right: box.x + box.width, bottom: box.y + box.height };
}

/** A second LFO in the oscillator's lane, from its menu (the tracer's is "LFO 1" from then on), once the canvas has stopped moving to show it. */
async function addLfo(page: Page): Promise<Locator> {
  await page.getByRole("button", { name: "Add a modulator to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "LFO" }).click();
  const added = node(page, "LFO 2");
  await expect(added).toBeVisible();
  await still(added);
  return added;
}

test("a modulator is drawn above the one it drives when the nodes are arranged, and no node lies over another", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  const lfo = node(page, "LFO");
  // Before it drives anything the envelope is in a row with the LFO, beside it.
  const beside = [await boxOf(envelope), await boxOf(lfo)];
  expect(beside[0]?.bottom).toBeGreaterThan(beside[1]?.top ?? Infinity);
  await drag(page, output(envelope), setting(lfo, "rate"));
  await expect(cables(page)).toHaveCount(2);
  // The cable does not rearrange the canvas: the nodes are where they were (the envelope is taller by the row of the cable that it lists).
  await still(envelope);
  const drawn = [await boxOf(envelope), await boxOf(lfo)];
  for (const [i, box] of drawn.entries()) {
    for (const edge of ["left", "top", "right"] as const) expect(box[edge], `${i === 0 ? "the envelope" : "the LFO"}: ${edge}`).toBeCloseTo(beside[i]?.[edge] ?? Number.NaN, 0);
  }
  expect(drawn[0]?.bottom).toBeGreaterThan(beside[0]?.bottom ?? Infinity);
  expect(drawn[1]?.bottom).toBeCloseTo(beside[1]?.bottom ?? Number.NaN, 0);
  // Arranged, the envelope's bottom is above the LFO's top, with room for the cable between.
  await arrange(page);
  const [above, below] = [await boxOf(envelope), await boxOf(lfo)];
  expect(above.bottom).toBeLessThan(below.top);
  expect(below.top - above.bottom).toBeGreaterThan(20);
  // In the same column.
  expect(above.left).toBeCloseTo(below.left, 0);
  await expectNodesApart(page);
});

test("a chain of modulators is a cascade of rows when the nodes are arranged, each above the one it drives, and the nodes keep from one another", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  await drag(page, output(envelope), setting(node(page, "LFO"), "rate"));
  await expect(cables(page)).toHaveCount(2);
  const second = await addLfo(page);
  const lfo = node(page, "LFO 1");
  await drag(page, output(second), setting(envelope, "release"));
  await expect(cables(page)).toHaveCount(3);
  // The nodes keep from one another as the cables are drawn and the modulator is added, and arranged again.
  await expectNodesApart(page);
  await arrange(page);
  // The second LFO drives the envelope, which drives the first LFO, which drives the filter: three rows, in that order.
  expect((await boxOf(second)).bottom).toBeLessThan((await boxOf(envelope)).top);
  expect((await boxOf(envelope)).bottom).toBeLessThan((await boxOf(lfo)).top);
  expect((await boxOf(lfo)).bottom).toBeLessThan((await boxOf(node(page, "Filter"))).top);
  // No node lies over another, nor does a node keep a size the layout does not know: they are all the sizes the layout assumes.
  const nodes = await page.evaluate(() =>
    [...document.querySelectorAll(".react-flow__node")].map((element) => {
      const box = element.getBoundingClientRect();
      return { name: element.querySelector(".tang-node__name")?.textContent ?? "?", left: box.left, top: box.top, width: box.width, height: box.height };
    }),
  );
  expect(nodes).toHaveLength(6);
  for (const a of nodes) {
    for (const b of nodes) {
      if (a === b) continue;
      const apart = a.left + a.width <= b.left + 1 || b.left + b.width <= a.left + 1 || a.top + a.height <= b.top + 1 || b.top + b.height <= a.top + 1;
      expect(apart, `${a.name} and ${b.name}`).toBe(true);
    }
  }
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

test("a cable taken out leaves the modulator where it is, and arranged again it is back in the row it was in", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  const lfo = node(page, "LFO");
  await drag(page, output(envelope), setting(lfo, "rate"));
  await expect(cables(page)).toHaveCount(2);
  await arrange(page);
  expect((await boxOf(envelope)).bottom).toBeLessThan((await boxOf(lfo)).top);
  await page.getByRole("button", { name: "Remove cable ADSR → LFO: rate" }).click();
  await expect(cables(page)).toHaveCount(1);
  // The canvas is not rearranged for the cable that goes: the envelope stays above the LFO.
  await still(envelope);
  expect((await boxOf(envelope)).bottom).toBeLessThan((await boxOf(lfo)).top);
  // Arranged again, the two are in a row.
  await arrange(page);
  expect((await boxOf(envelope)).top).toBeCloseTo((await boxOf(lfo)).top, 0);
});
