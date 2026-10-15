import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { NODE_SCOPE_HEIGHT, NODE_WIDTH } from "../src/app/patch/toFlow";
import { expectNodesApart, node, open, still } from "./canvas";
import { test } from "./own";

// The scope of a node (docs/web-ui.md §9.3, D47): a button in the header of an instrument, an effect and a group shows what comes out of that node, as a wave or a spectrum, below its
// controls. The sizes are tried on the page's own mock engine and the signals on the real program, on the null audio backend (a program of its own for each test that plays a note).

/** How much a pixel of the layout is on the screen: the canvas is zoomed to fit the window. */
async function scaleOf(target: Locator): Promise<number> {
  const box = await target.locator(".tang-node").boundingBox();
  if (!box) throw new Error("not on the page");
  return box.width / NODE_WIDTH;
}

/** How tall a node is, in the pixels of the layout. */
async function layoutHeight(target: Locator): Promise<number> {
  const box = await target.locator(".tang-node").boundingBox();
  if (!box) throw new Error("not on the page");
  return box.height / (await scaleOf(target));
}

/** Where the top of a node is on the screen. */
async function topOf(target: Locator): Promise<number> {
  const box = await target.boundingBox();
  if (!box) throw new Error("not on the page");
  return box.y;
}

test("a node that shows its scope is as much taller as the layout assumes, with the scope below its controls, and the lane below makes room for it so that no node lies over another", async ({ page }) => {
  await open(page, "lanes");
  const first = node(page, "Oscillator 1");
  const below = node(page, "Oscillator 2");
  const [height, lower, upper] = [await layoutHeight(first), await topOf(below), await topOf(first)];

  await first.getByRole("button", { name: "Scope of Oscillator 1" }).click();
  const scope = first.getByRole("meter", { name: "Output of Oscillator 1" });
  await expect(scope).toBeVisible();
  await still(below);
  // The node is as much taller as the layout says (a scope and the choices of its view and its time), and the lane below is that much further down: it does not lie under the scope.
  expect((await layoutHeight(first)) - height).toBeCloseTo(NODE_SCOPE_HEIGHT, 0);
  const scale = await scaleOf(first);
  expect(await topOf(first)).toBeCloseTo(upper, 0);
  expect(((await topOf(below)) - lower) / scale).toBeCloseTo(NODE_SCOPE_HEIGHT, 0);
  // The scope is under the node's controls, and in the node.
  const [controls, shown, outer] = await Promise.all([first.locator(".tang-node__controls").boundingBox(), scope.boundingBox(), first.locator(".tang-node").boundingBox()]);
  if (!controls || !shown || !outer) throw new Error("a part of the node is not on the page");
  expect(shown.y).toBeGreaterThanOrEqual(controls.y + controls.height - 1);
  expect(shown.y + shown.height).toBeLessThanOrEqual(outer.y + outer.height + 1);
  await expectNodesApart(page);

  // Taken away again, the node is as tall as it was, and the lane below stays where it is until the nodes are arranged.
  await first.getByRole("button", { name: "Scope of Oscillator 1" }).click();
  await expect(scope).toHaveCount(0);
  expect(await layoutHeight(first)).toBeCloseTo(height, 0);
  expect(((await topOf(below)) - lower) / scale).toBeCloseTo(NODE_SCOPE_HEIGHT, 0);
  await expectNodesApart(page);
});

test("a scope that is opened where it cannot be seen moves the canvas to show it, and a scope that is in the view moves nothing", async ({ page }) => {
  await open(page, "lanes");
  const pane = page.locator(".tang-patch__canvas");
  const [first, second] = [node(page, "Oscillator 1"), node(page, "Oscillator 2")];
  // The scope of the first lane is in the view: the canvas stays where it is (the lane below is moved down for it, on the canvas).
  const before = await first.boundingBox();
  await first.getByRole("button", { name: "Scope of Oscillator 1" }).click();
  await expect(first.getByRole("meter", { name: "Output of Oscillator 1" })).toBeVisible();
  await still(second);
  // (The node is taller by the scope, and where it is and how wide it is, which the zoom says, are what they were.)
  const after = await first.boundingBox();
  expect([after?.x, after?.y, after?.width]).toEqual([before?.x, before?.y, before?.width]);

  // The second lane is at the foot of the view, and its scope would be under it: the canvas moves, and all of the scope is in the view after.
  await second.getByRole("button", { name: "Scope of Oscillator 2" }).click();
  const scope = second.getByRole("meter", { name: "Output of Oscillator 2" });
  await expect(scope).toBeVisible();
  await still(second);
  const [area, shown] = [await pane.boundingBox(), await scope.boundingBox()];
  if (!area || !shown) throw new Error("not on the page");
  expect(shown.y, "the scope is not above the view").toBeGreaterThanOrEqual(area.y);
  expect(shown.y + shown.height, "nor below it").toBeLessThanOrEqual(area.y + area.height);
  expect(shown.x).toBeGreaterThanOrEqual(area.x);
  expect(shown.x + shown.width).toBeLessThanOrEqual(area.x + area.width);
});

/** The loudest sample that a scope has on its screen: the value of its meter (nothing is silence). */
const peakOf = async (scope: Locator): Promise<number> => Number(await scope.getAttribute("aria-valuenow"));

/** The page of the real program, with the connection made. */
async function openProgram(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.locator(".react-flow__node").first()).toBeVisible();
}

test("the scope of an instrument is of what it puts out and the scope of an effect is of what comes out of that effect: with the filter shut the one is loud, and the filter's and the master's are nearly silent", async ({ page, own }) => {
  await openProgram(page, own.url);
  const [lane, filter] = [node(page, "Oscillator"), node(page, "Filter")];
  // The sound first, while every node is in the view (a scope that is opened moves the canvas to show its node, and the LFO is out of the view then). The tracer is quiet on purpose
  // (the lane's volume is a tenth): it is put back to 1. The LFO is taken out, which gives the cutoff the value that was set, and that is turned down all the way: a low pass at 20 Hz
  // leaves very little of a note a hundred and thirty hertz up.
  await lane.getByRole("slider", { name: "Volume", exact: true }).dblclick();
  await page.getByRole("button", { name: "Remove LFO" }).click();
  await expect(page.getByRole("button", { name: "Remove LFO" })).toHaveCount(0);
  // (The focus goes to the button that adds a modulator when the server's answer is in, a moment after the node has gone: a key that is pressed before that goes to the button.)
  await expect(lane.getByRole("button", { name: "Add a modulator to Oscillator" })).toBeFocused();
  const cutoff = filter.getByRole("slider", { name: "cutoff" });
  await cutoff.focus();
  await page.keyboard.press("Home");
  await expect.poll(async () => Number(await cutoff.getAttribute("aria-valuenow")), { message: "the filter is shut" }).toBe(0);

  await lane.getByRole("button", { name: "Scope of Oscillator" }).click();
  await filter.getByRole("button", { name: "Scope of Filter" }).click();
  const [laneScope, filterScope, masterScope] = [lane.getByRole("meter", { name: "Output of Oscillator" }), filter.getByRole("meter", { name: "Output of Filter" }), page.getByRole("meter", { name: "Output scope" })];
  await expect(laneScope).toBeVisible();
  await expect(filterScope).toBeVisible();
  await expect.poll(async () => [await peakOf(laneScope), await peakOf(filterScope)], { message: "silence to begin with" }).toEqual([0, 0]);

  await page.keyboard.down("z");
  await expect
    .poll(
      async () => {
        const [lanePeak, filterPeak, masterPeak] = [await peakOf(laneScope), await peakOf(filterScope), await peakOf(masterScope)];
        return { loud: lanePeak > 0.1, filterShut: filterPeak < lanePeak / 5, masterShut: masterPeak < lanePeak / 5 };
      },
      { message: "the instrument is loud where the filter is not" },
    )
    .toEqual({ loud: true, filterShut: true, masterShut: true });

  // The note ends, and every scope goes quiet (a filter rings out for a while, in numbers too small to hear or see: the master was seen at 1e-42, and the test's limit is a millionth).
  await page.keyboard.up("z");
  await expect
    .poll(async () => [await peakOf(laneScope), await peakOf(filterScope), await peakOf(masterScope)].map((peak) => peak < 1e-6), { message: "silence again" })
    .toEqual([true, true, true]);
});

test("the signal of a node comes to every page that shows its scope, for as long as one does: a page that takes its scope away leaves the other's drawn", async ({ page, browser, own }) => {
  await openProgram(page, own.url);
  const other = await browser.newPage();
  try {
    await openProgram(other, own.url);
    const scopes = [] as Locator[];
    for (const shown of [page, other]) {
      const lane = node(shown, "Oscillator");
      await lane.getByRole("button", { name: "Scope of Oscillator" }).click();
      scopes.push(lane.getByRole("meter", { name: "Output of Oscillator" }));
    }
    const [mine, theirs] = scopes;
    if (!mine || !theirs) throw new Error("no scopes");
    await node(page, "Oscillator").getByRole("slider", { name: "Volume", exact: true }).dblclick();

    await page.keyboard.down("z");
    await expect.poll(async () => [await peakOf(mine), await peakOf(theirs)].map((peak) => peak > 0.1), { message: "the note on both pages" }).toEqual([true, true]);

    // One page takes its scope away: the other's goes on being drawn, which a note that ends shows: it goes quiet only if the frames still come.
    await node(page, "Oscillator").getByRole("button", { name: "Scope of Oscillator" }).click();
    await expect(mine).toHaveCount(0);
    await page.keyboard.up("z");
    await expect.poll(async () => peakOf(theirs), { message: "the other page's scope goes quiet with the note" }).toBe(0);
  } finally {
    await other.close();
  }
});
