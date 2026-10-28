import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { addEnvelope, drag, expectNodesApart, nodeBoxes, output, setting, still } from "./canvas";
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
  // (A session is opened with its nodes in rows, which is not the arrangement that the button makes: it is there to be pressed.)
  await expect(page.getByRole("button", { name: "Arrange the nodes again" })).toBeEnabled();

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

  // Arranged again, the places that were given are given back (the file has none) and every node is where the arrangement that makes the cables short has it: not where the filter was put, and not where the
  // rows had it. (The arrangement is the page's own: a second page has the nodes that were given back where the rows have them.)
  await page.getByRole("button", { name: "Arrange the nodes again" }).click();
  await expect.poll(async () => (await placeOf(nodeNamed(page, "Filter"))).x, { message: "the filter is no longer where it was put" }).not.toBe(moved.x);
  await expect(page.getByRole("button", { name: "Arrange the nodes again" })).toBeDisabled();
  await expectNodesApart(page);
  await expect.poll(async () => placeOf(nodeNamed(other, "Filter"))).toEqual(laidOut);
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(own.file, "utf8")).not.toMatch(/^ui = /m);
  await other.close();
});

/** Where the canvas draws each node, in its own pixels, by the name in its header (the view pans and zooms, so the screen's are not the node's). */
async function placesByName(page: Page): Promise<Record<string, { x: number; y: number }>> {
  return page.evaluate(() => {
    const places: Record<string, { x: number; y: number }> = {};
    for (const element of document.querySelectorAll<HTMLElement>(".react-flow__node")) {
      const found = /translate\((-?[\d.e-]+)px,\s*(-?[\d.e-]+)px\)/.exec(element.style.transform);
      const name = element.querySelector(".tang-node__name")?.textContent;
      if (name && found?.[1] && found[2]) places[name] = { x: Number(found[1]), y: Number(found[2]) };
    }
    return places;
  });
}

test("a cable drawn to the setting of another modulator leaves every node where it is, and the button that says Arrange puts them where the arrangement has them", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const lfo = nodeNamed(page, "LFO");
  await expect(lfo).toBeVisible();

  // The button is there, says what it does, is bright (the nodes are in rows, which is not the arrangement), and is over none of the nodes.
  const arrange = page.getByRole("button", { name: "Arrange the nodes again" });
  await expect(arrange).toHaveText("Arrange");
  await expect(arrange).toBeEnabled();
  const button = await arrange.boundingBox();
  if (!button) throw new Error("the button is not on the page");
  for (const other of await nodeBoxes(page)) {
    const apart = button.x + button.width <= other.left || other.left + other.width <= button.x || button.y + button.height <= other.top || other.top + other.height <= button.y;
    expect(apart, `the button and ${other.name}`).toBe(true);
  }

  const first = await placesByName(page);
  expect(Object.keys(first).sort()).toEqual(["LFO", "Filter", "Master", "Oscillator"].sort());
  const envelope = await addEnvelope(page);
  await still(envelope);

  // A cable from the LFO to the Attack of the envelope: the layout would put the LFO a row above the envelope, and move the strip down for the room that makes.
  // The nodes stay: the canvas is not rearranged under the hand that drew the cable.
  const added = await placesByName(page);
  await drag(page, output(lfo), setting(envelope, "attack"));
  await expect(page.locator(".tang-cable--modulation")).toHaveCount(2);
  await still(envelope);
  const drawn = await placesByName(page);
  expect(drawn).toEqual(added);
  for (const [name, place] of Object.entries(first)) expect(drawn[name], name).toEqual(place);

  // The nodes can be put where the arrangement has them: the button is bright, and the nodes go, each to its place in the arrangement, which has the LFO above the envelope that it drives and the nodes apart.
  await expect(arrange).toBeEnabled();
  await arrange.click();
  await expect(arrange).toBeDisabled();
  await expect.poll(async () => JSON.stringify(await placesByName(page)), { message: "the nodes have gone to the places of the arrangement" }).not.toBe(JSON.stringify(drawn));
  await expectNodesApart(page);
  const arranged = await placesByName(page);
  expect(arranged.LFO?.y).toBeLessThan(arranged.ADSR?.y ?? 0);

  // The canvas is moved to show them: the layout is a row taller than what was in the view, and every node is in it.
  const view = await page.locator(".react-flow").boundingBox();
  if (!view) throw new Error("the canvas is not on the page");
  await expect
    .poll(
      async () => (await nodeBoxes(page)).filter((box) => box.left < view.x - 1 || box.top < view.y - 1 || box.left + box.width > view.x + view.width + 1 || box.top + box.height > view.y + view.height + 1).map((box) => box.name),
      { message: "every node is in the view" },
    )
    .toEqual([]);
});
