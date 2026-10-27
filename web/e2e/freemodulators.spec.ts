import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { cables, drag, expectNodesApart, flowBoxes, node, open, output, port, setting, showAll, still } from "./canvas";
import { test } from "./own";
import { knob } from "./support";

// A modulator that is in no lane and no group, from the page (docs/web-ui.md D92, P5b-3): the bar's "+ Modulator" adds one, a node of its own in a row above the lanes, that a cable can be drawn from to a parameter of
// any plugin of the session (one LFO can move a knob of two lanes), and that the "~" menu of any knob offers to attach. The first tests are against the mock engine in the page (what the canvas draws, says and keeps
// where it was), the last against the real program (what the file keeps, and that a knob moves).

/** The button in the bar that adds a modulator of its own. */
const addModulator = (page: Page) => page.getByRole("button", { name: "Add a modulator", exact: true });

/** Add the modulator of the kind `kind` (as the menu says it) that is in no lane, from the bar, and wait for its node, which `name` is called. */
async function add(page: Page, kind: string, name: string): Promise<Locator> {
  await addModulator(page).click();
  await page.getByRole("menuitem", { name: kind, exact: true }).click();
  const added = node(page, name);
  await expect(added).toBeVisible();
  await still(added);
  return added;
}

/** The button that modulates the control called `name`. */
const modulate = (plugin: Locator, name: string): Locator => plugin.getByRole("button", { name: `Modulate ${name}`, exact: true });

/** The cell of the control called `name` in a node. */
const cell = (plugin: Locator, name: string): Locator => plugin.locator(".tang-cell").filter({ has: plugin.page().locator(`[aria-label="${name}"]`) });

/** Another lane, from the bar: an oscillator, which is called "Oscillator 2" then (the first is "Oscillator 1"). */
async function addLane(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Add an instrument", exact: true }).click();
  await page.getByRole("menu", { name: "Add an instrument" }).getByRole("menuitem", { name: "Oscillator", exact: true }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
}

mock("a modulator that is added from the bar stands by itself: a node above the lanes, and nothing else on the canvas moves", async ({ page }) => {
  await open(page);
  const before = (await flowBoxes(page)).filter((box) => box.name !== "Master");
  const lfo = await add(page, "LFO", "LFO 2");
  // (The tracer's LFO is "LFO 1" now: two of one name are told apart by their places.)
  const after = await flowBoxes(page);
  for (const box of before) {
    const name = box.name === "LFO" ? "LFO 1" : box.name;
    expect(after.find((found) => found.name === name), name).toEqual({ ...box, name });
  }
  // It is above all of the lane, the modulator that the lane has too, with the room for a cable under it.
  const added = after.find((box) => box.name === "LFO 2");
  expect(added).toBeDefined();
  expect((added?.y ?? 0) + (added?.height ?? 0)).toBeLessThanOrEqual(Math.min(...before.map((box) => box.y)));
  await expectNodesApart(page);
  // It has a cable's worth of nothing yet, and what it can be drawn to is what is in the session: the oscillator's parameters and the filter's, and not the LFO that the lane has.
  await lfo.getByRole("button", { name: /^Add a cable from / }).click();
  await expect(page.getByRole("menuitem", { name: "detune" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "cutoff" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "rate" })).toHaveCount(0);
  await page.keyboard.press("Escape");
});

mock("one modulator that is in no lane drives a knob of two lanes, and the \"~\" menu of a knob of the second offers it to attach: the cables are from the one node", async ({ page }) => {
  await open(page);
  await addLane(page);
  const lfo = await add(page, "LFO", "LFO 2");
  await showAll(page);
  await expect(cables(page)).toHaveCount(1);
  const first = node(page, "Oscillator 1");
  const second = node(page, "Oscillator 2");
  await still(first);
  await still(second);
  // A cable to the detune of each: a lane's LFO could have drawn to one of them.
  await drag(page, output(lfo), port(first, "detune"));
  await expect(cables(page)).toHaveCount(2);
  await drag(page, output(lfo), port(second, "detune"));
  await expect(cables(page)).toHaveCount(3);
  await expect(lfo.getByText("Oscillator 1: detune")).toBeVisible();
  await expect(lfo.getByText("Oscillator 2: detune")).toBeVisible();
  // The knob of another parameter of the second lane: its "~" menu has the modulator among the ones that are there, and choosing it is a cable.
  await cell(second, "volume").hover();
  await modulate(second, "volume").click();
  await expect(page.getByRole("group", { name: "Attach" }).getByRole("menuitem", { name: "LFO 2", exact: true })).toBeVisible();
  // (The tracer's LFO is a lane's, which does not reach the second lane's oscillator.)
  await expect(page.getByRole("group", { name: "Attach" }).getByRole("menuitem", { name: "LFO 1", exact: true })).toHaveCount(0);
  await page.getByRole("group", { name: "Attach" }).getByRole("menuitem", { name: "LFO 2", exact: true }).click();
  await expect(cables(page)).toHaveCount(4);
  await expect(lfo.getByText("Oscillator 2: volume")).toBeVisible();
  // The parameter it drives is not offered again.
  await cell(second, "volume").hover();
  await modulate(second, "volume").click();
  await expect(page.getByRole("group", { name: "Attach" })).toHaveCount(0);
  await page.keyboard.press("Escape");
});

mock("a modulator that is in no lane is taken out by its \"×\" with its cables, the focus goes to the bar's button, and Undo and Redo have the words of the button that added it", async ({ page }) => {
  await open(page);
  const lfo = await add(page, "LFO", "LFO 2");
  await showAll(page);
  await still(node(page, "Filter"));
  await drag(page, output(lfo), port(node(page, "Filter"), "resonance"));
  await expect(cables(page)).toHaveCount(2);
  await lfo.getByRole("button", { name: /^Remove LFO/ }).click();
  await expect(node(page, "LFO 2")).toHaveCount(0);
  await expect(cables(page)).toHaveCount(1);
  await expect(addModulator(page)).toBeFocused();
  await page.getByRole("button", { name: "Undo remove an LFO", exact: true }).click();
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
  await page.getByRole("button", { name: "Redo remove an LFO", exact: true }).click();
  await expect(node(page, "LFO 2")).toHaveCount(0);
  // Back through the steps before the removal: it, the cable that was drawn, and then the modulator that was added.
  await page.getByRole("button", { name: "Undo remove an LFO", exact: true }).click();
  await expect(node(page, "LFO 2")).toBeVisible();
  await page.keyboard.press("Control+z");
  await expect(cables(page)).toHaveCount(1);
  await expect(node(page, "LFO 2")).toBeVisible();
  await page.keyboard.press("Control+z");
  await expect(node(page, "LFO 2")).toHaveCount(0);
  await page.keyboard.press("Control+Shift+z");
  await expect(node(page, "LFO 2")).toBeVisible();
});

mock("a modulator that is in no lane can drive a setting of another, which is a row above it as a lane's is, and the lanes do not reach them", async ({ page }) => {
  await open(page);
  const lfo = await add(page, "LFO", "LFO 2");
  const envelope = await add(page, "Envelope", "ADSR");
  await showAll(page);
  await still(lfo);
  await still(envelope);
  await drag(page, output(lfo), setting(envelope, "attack"));
  await expect(cables(page)).toHaveCount(2);
  // (A cable that is drawn moves nothing: the two are side by side above the lanes, over none of them, until the canvas is arranged again.)
  const placed = await flowBoxes(page);
  const laneTop = Math.min(...placed.filter((box) => /^(Oscillator|Filter|LFO 1)/.test(box.name)).map((box) => box.y));
  for (const name of ["LFO 2", "ADSR"]) {
    const box = placed.find((found) => found.name === name);
    expect((box?.y ?? 0) + (box?.height ?? 0), name).toBeLessThanOrEqual(laneTop);
  }
  const arranged = page.getByRole("button", { name: "Arrange the nodes again" });
  await arranged.click();
  await expect(arranged).toBeDisabled();
  await still(page.locator(".react-flow__node").first());
  // The one that drives is a row above the one it drives, in its column, as in a lane's rack.
  const boxes = await flowBoxes(page);
  const [driver, driven] = ["LFO 2", "ADSR"].map((name) => boxes.find((box) => box.name === name));
  expect(driver?.y ?? 0).toBeLessThan(driven?.y ?? 0);
  expect(driver?.x).toBe(driven?.x);
  await expectNodesApart(page);
  // The tracer's LFO (a lane's) cannot be drawn to from the one that has none: no group of its settings.
  await lfo.getByRole("button", { name: /^Add a cable from / }).click();
  await expect(page.getByRole("menu").getByRole("group")).toHaveCount(3);
  await expect(page.getByRole("menuitem", { name: "rate" })).toHaveCount(0);
  await page.keyboard.press("Escape");
});

mock("the canvas arranged again has the row of the modulators that are in no lane at the top, with the lanes below it, and no node over another", async ({ page }) => {
  await open(page);
  await addLane(page);
  await add(page, "LFO", "LFO 2");
  await add(page, "Envelope", "ADSR");
  await showAll(page);
  const arranged = page.getByRole("button", { name: "Arrange the nodes again" });
  await arranged.click();
  await expect(arranged).toBeDisabled();
  await still(page.locator(".react-flow__node").first());
  const boxes = await flowBoxes(page);
  const top = (name: string): number => boxes.find((box) => box.name === name)?.y ?? Number.NaN;
  // Both of the session's own are at the very top, one column each from the first, and above everything of the lanes.
  expect(top("LFO 2")).toBe(top("ADSR"));
  expect(top("LFO 2")).toBe(Math.min(...boxes.map((box) => box.y)));
  for (const name of ["Oscillator 1", "Oscillator 2", "Filter", "LFO 1"]) expect(top(name), name).toBeGreaterThan(top("LFO 2"));
  const xs = ["LFO 2", "ADSR"].map((name) => boxes.find((box) => box.name === name)?.x);
  expect(xs[0]).toBeLessThan(xs[1] ?? 0);
  await expectNodesApart(page);
});

test("a modulator that is in no lane, in the real program, drives knobs of two lanes, which move, and the file keeps it as a table of its own that names the plugins it drives by their places, and Undo puts it back", async ({ page, own }) => {
  // (A good many waits, and a lane added.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const saved = async (): Promise<string> => {
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };
  /** Where the knob called `name` of the node `of` is, as the fraction of its sweep that its dot is at, looked at on every frame for `ms` milliseconds (the dot is the engine's own value). */
  const dots = (of: Locator, name: string, ms: number): Promise<number[]> =>
    of.locator(`[role="slider"][aria-label="${name}"]`).evaluate(
      (slider, duration) =>
        new Promise<number[]>((resolve) => {
          const dot = slider.closest(".tang-knob")?.querySelector(".tang-knob__dot") ?? null;
          const found: number[] = [];
          const start = performance.now();
          const look = (now: number): void => {
            if (dot?.getAttribute("visibility") === "visible") {
              const x = Number(dot.getAttribute("cx")) - 24;
              const y = 24 - Number(dot.getAttribute("cy"));
              found.push(((Math.atan2(x, y) * 180) / Math.PI + 135) / 270);
            }
            if (now - start < duration) requestAnimationFrame(look);
            else resolve(found);
          };
          requestAnimationFrame(look);
        }),
      ms,
    );
  const spread = (values: number[]): number => Math.max(...values) - Math.min(...values);

  // A second lane, and a modulator of the session's own. The file has the tracer's lane and the second, and no table of its own for a modulator yet.
  await addLane(page);
  const lfo = await add(page, "LFO", "LFO 2");
  await showAll(page);
  const first = node(page, "Oscillator 1");
  const second = node(page, "Oscillator 2");
  await still(first);
  await still(second);
  // (The modulator is in the file at once, with no cable: a table of its own, and the tracer's own LFO is the lane's.)
  let file = await saved();
  expect(file.match(/^\[\[modulator\]\]$/gm)).toHaveLength(1);
  expect(file).not.toContain("[[modulator.target]]");

  // Its cables to the detune of each of the two lanes, and to the resonance of the filter (which nothing drives yet): each knob has a ring, and the dot of each moves on the engine's own telemetry as the LFO turns.
  await drag(page, output(lfo), port(first, "detune"));
  await expect(cables(page)).toHaveCount(2);
  await still(lfo);
  await drag(page, output(lfo), port(second, "detune"));
  await expect(cables(page)).toHaveCount(3);
  await still(lfo);
  await drag(page, output(lfo), port(node(page, "Filter"), "resonance"));
  await expect(cables(page)).toHaveCount(4);
  await expect(knob(page, "resonance").locator(".tang-knob__ring")).toHaveCount(1);
  const firstDots = await dots(first, "detune", 2600);
  const secondDots = await dots(second, "detune", 2600);
  expect(firstDots.length).toBeGreaterThan(20);
  expect(spread(firstDots), "the first lane's detune moves").toBeGreaterThan(0.2);
  expect(spread(secondDots), "the second lane's detune moves").toBeGreaterThan(0.2);
  // (One LFO: the two knobs go together, to the step of a frame or two: the middle of the sweep is the same.)
  expect(Math.abs((Math.max(...firstDots) + Math.min(...firstDots)) / 2 - (Math.max(...secondDots) + Math.min(...secondDots)) / 2)).toBeLessThan(0.05);

  // The file has a table that is no lane's, with the cables that name the plugins by their places in the session, and the tracer's own LFO as it was.
  file = await saved();
  expect(file.match(/^\[\[modulator\]\]$/gm)).toHaveLength(1);
  const table = file.slice(file.indexOf("[[modulator]]"));
  expect(table.match(/\[\[modulator\.target\]\]/g)).toHaveLength(3);
  expect(table).toMatch(/param = "detune"\nto = "instrument\[0\]"\n/);
  expect(table).toMatch(/param = "detune"\nto = "instrument\[1\]"\n/);
  expect(table).toMatch(/param = "resonance"\nto = "instrument\[0\]\.effect\[0\]"\n/);
  expect(file.match(/\[\[instrument\.modulator\]\]/g)).toHaveLength(1);

  // The "×" of the modulator takes it out with its cables: the knobs stop moving and the file has no table of its own. Undo puts it back, and the knobs move again.
  await lfo.getByRole("button", { name: /^Remove LFO/ }).click();
  await expect(node(page, "LFO 2")).toHaveCount(0);
  await expect(cables(page)).toHaveCount(1);
  await expect(addModulator(page)).toBeFocused();
  expect(await saved()).not.toMatch(/^\[\[modulator\]\]/m);
  await page.getByRole("button", { name: "Undo remove an LFO", exact: true }).click();
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(cables(page)).toHaveCount(4);
  expect(spread(await dots(node(page, "Oscillator 1"), "detune", 2600))).toBeGreaterThan(0.2);
  expect((await saved()).match(/\[\[modulator\.target\]\]/g)).toHaveLength(3);

  // A page opened again has it: the session is the server's.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(cables(page)).toHaveCount(4);
});
