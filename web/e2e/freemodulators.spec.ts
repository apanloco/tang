import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { cables, drag, expectNodesApart, flowBoxes, node, open, output, port, setting, showAll, still } from "./canvas";
import { modulatorsIn, test } from "./own";
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
  // It has a cable's worth of nothing yet, and what it can be drawn to is what is in the session: the oscillator's parameters and the filter's, and the settings of the other modulator, the tracer's LFO, which is
  // the session's own as well (every modulator is: step 94) and has a cable of its own, whose depth is one of them.
  await lfo.getByRole("button", { name: /^Add a cable from / }).click();
  await expect(page.getByRole("menuitem", { name: "detune" })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "cutoff", exact: true })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "rate", exact: true })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "depth → Filter: cutoff" })).toBeVisible();
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
  // (The tracer's LFO hears the first lane and reaches the second lane's oscillator as well: every modulator is the session's own.)
  await expect(page.getByRole("group", { name: "Attach" }).getByRole("menuitem", { name: "LFO 1", exact: true })).toBeVisible();
  await page.getByRole("group", { name: "Attach" }).getByRole("menuitem", { name: "LFO 2", exact: true }).click();
  await expect(cables(page)).toHaveCount(4);
  await expect(lfo.getByText("Oscillator 2: volume")).toBeVisible();
  // The parameter it drives is not offered again: the one that does not drive it still is.
  await cell(second, "volume").hover();
  await modulate(second, "volume").click();
  await expect(page.getByRole("group", { name: "Attach" }).getByRole("menuitem", { name: "LFO 2", exact: true })).toHaveCount(0);
  await expect(page.getByRole("group", { name: "Attach" }).getByRole("menuitem", { name: "LFO 1", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
});

mock("a modulator made from the \"+\" of a lane or from one of its knobs hears that lane and is above its row, and is drawn to a knob of another lane: it is the session's own", async ({ page }) => {
  await open(page);
  await addLane(page);
  await showAll(page);
  const first = node(page, "Oscillator 1");
  const second = node(page, "Oscillator 2");
  await still(first);
  await still(second);
  // From the "+" of the second lane: it hears that lane, and is above its row, which is below the first lane's: not in the row above the first lane that a modulator that hears every note has.
  await page.getByRole("button", { name: "Add a modulator to Oscillator 2" }).click();
  await page.getByRole("menuitem", { name: "LFO", exact: true }).click();
  const fromPlus = node(page, "LFO 2");
  await expect(fromPlus).toBeVisible();
  await expect(hears(fromPlus)).toHaveAttribute("aria-label", "Whose notes LFO 2 hears: Oscillator 2");
  await expect(hears(fromPlus)).toHaveClass(/tang-node__notes--set/);
  await showAll(page);
  const boxes = await flowBoxes(page);
  const box = (name: string) => boxes.find((found) => found.name === name);
  expect((box("LFO 2")?.y ?? 0) + (box("LFO 2")?.height ?? 0), "above the second lane's row").toBeLessThanOrEqual(box("Oscillator 2")?.y ?? 0);
  expect(box("LFO 2")?.y ?? 0, "below the first lane's row").toBeGreaterThan(box("Oscillator 1")?.y ?? 0);
  // From a knob of the first lane: it hears the first lane, and it is drawn to a knob of the second, which "a lane's LFO" could not be.
  await cell(first, "volume").hover();
  await modulate(first, "volume").click();
  await page.getByRole("menuitem", { name: "New LFO", exact: true }).click();
  const fromKnob = node(page, "LFO 3");
  await expect(fromKnob).toBeVisible();
  await expect(hears(fromKnob)).toHaveAttribute("aria-label", "Whose notes LFO 3 hears: Oscillator 1");
  await still(fromKnob);
  await drag(page, output(fromKnob), port(second, "detune"));
  await expect(page.getByRole("slider", { name: "Depth of LFO 3 → Oscillator 2: detune" })).toBeVisible();
  // (The tracer's cable, the one that "New LFO" drew to the knob it was made from, and this.)
  await expect(cables(page)).toHaveCount(3);
  // The one that hears the second lane is drawn to the first lane's knob as well.
  await still(fromPlus);
  await drag(page, output(fromPlus), port(first, "detune"));
  await expect(page.getByRole("slider", { name: "Depth of LFO 2 → Oscillator 1: detune" })).toBeVisible();
  await expect(cables(page)).toHaveCount(4);
  await expect(page.locator(".tang-notice")).toHaveCount(0);
  await expectNodesApart(page);
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

mock("a modulator that is in no lane can drive a setting of another, which is a row above it as a lane's is, and so can the tracer's, which hears a lane", async ({ page }) => {
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
  // The tracer's LFO hears a lane and is drawn to from the one that hears none, like the other: a group of its settings, and its rate among them.
  await lfo.getByRole("button", { name: /^Add a cable from / }).click();
  await expect(page.getByRole("menu").getByRole("group")).toHaveCount(4);
  await expect(page.getByRole("menuitem", { name: "rate", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
});

mock("the canvas arranged again has the modulators that are in no lane among the nodes that have cables, with no node over another", async ({ page }) => {
  await open(page);
  await addLane(page);
  await add(page, "LFO", "LFO 2");
  await add(page, "Envelope", "ADSR");
  await showAll(page);
  const arranged = page.getByRole("button", { name: "Arrange the nodes again" });
  await arranged.click();
  await expect(arranged).toBeDisabled();
  await still(page.locator(".react-flow__node").first());
  // (A modulator that hears every note and has no cable is held by nothing: the arrangement puts it by the others, and not in a row of its own at the top as the rows' layout does.)
  const boxes = await flowBoxes(page);
  for (const name of ["LFO 2", "ADSR", "Oscillator 1", "Oscillator 2", "Filter", "LFO 1"]) expect(boxes.some((box) => box.name === name), name).toBe(true);
  await expectNodesApart(page);
});

/** The button on a modulator's node that chooses whose notes it hears. */
const hears = (of: Locator): Locator => of.getByRole("button", { name: /^Whose notes / });

mock("a modulator that is in no lane chooses whose notes it hears from a menu on its node: lit while it hears a lane, and Undo and Redo have the words of what it did", async ({ page }) => {
  await open(page);
  await addLane(page);
  const lfo = await add(page, "LFO", "LFO 2");
  // (The tracer's own LFO has the menu too, lit: it hears the lane it was in.)
  await expect(hears(node(page, "LFO 1"))).toHaveAttribute("aria-label", "Whose notes LFO 1 hears: Oscillator 1");
  await expect(hears(node(page, "LFO 1"))).toHaveClass(/tang-node__notes--set/);
  await expect(hears(lfo)).toHaveAttribute("aria-label", "Whose notes LFO 2 hears: every note");
  await expect(hears(lfo)).not.toHaveClass(/tang-node__notes--set/);
  await hears(lfo).click();
  const menu = page.getByRole("menu");
  await expect(menu.getByRole("menuitem")).toHaveText(["Every note", "Oscillator 1", "Oscillator 2"]);
  // (The one that it hears is marked: a check mark is in its name.)
  await expect(menu.getByRole("menuitem", { name: /Every note$/ })).toHaveAttribute("aria-current", "true");
  await menu.getByRole("menuitem", { name: "Oscillator 2", exact: true }).click();
  await expect(hears(lfo)).toHaveAttribute("aria-label", "Whose notes LFO 2 hears: Oscillator 2");
  await expect(hears(lfo)).toHaveClass(/tang-node__notes--set/);
  // The menu opens on what it hears, and every note is a choice.
  await hears(lfo).click();
  await expect(page.getByRole("menuitem", { name: /Oscillator 2$/ })).toHaveAttribute("aria-current", "true");
  await page.getByRole("menuitem", { name: "Every note", exact: true }).click();
  await expect(hears(lfo)).not.toHaveClass(/tang-node__notes--set/);
  // Undo goes back through them, in the words of what was done.
  await page.getByRole("button", { name: "Undo make an LFO hear every note", exact: true }).click();
  await expect(hears(lfo)).toHaveClass(/tang-node__notes--set/);
  await page.getByRole("button", { name: "Undo make an LFO hear the lane of Oscillator", exact: true }).click();
  await expect(hears(lfo)).not.toHaveClass(/tang-node__notes--set/);
  await page.getByRole("button", { name: "Redo make an LFO hear the lane of Oscillator", exact: true }).click();
  await expect(hears(lfo)).toHaveAttribute("aria-label", "Whose notes LFO 2 hears: Oscillator 2");
  // Taking the lane out leaves the modulator hearing every note, and Undo has it hear the lane again.
  await showAll(page);
  await node(page, "Oscillator 2").getByRole("button", { name: /^Remove /, exact: false }).first().click();
  await page.getByRole("menuitem", { name: /^Remove the lane of Oscillator 2 for good/ }).click();
  await expect(node(page, "Oscillator 2")).toHaveCount(0);
  await expect(hears(lfo)).toHaveAttribute("aria-label", "Whose notes LFO 2 hears: every note");
  await page.getByRole("button", { name: /^Undo remove the lane of Oscillator/ }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  await expect(hears(lfo)).toHaveAttribute("aria-label", "Whose notes LFO 2 hears: Oscillator 2");
});

test("a modulator that is in no lane, in the real program, hears the lane that it is made to, the file keeps it as the place of the lane, and a page opened again has it", async ({ page, own }) => {
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const saved = async (): Promise<string> => {
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };
  await addLane(page);
  const envelope = await add(page, "Envelope", "ADSR");
  // (The tracer's LFO is the session's own since the program loaded it, and hears the lane it was in: it is the one ear that the file has. The envelope hears every note.)
  let file = await saved();
  expect(file.match(/^hears = /gm)).toHaveLength(1);
  expect(file).toContain('hears = "instrument[0]"');
  // The second lane's notes: the file says which lane by its place among the lanes.
  await hears(envelope).click();
  await page.getByRole("menuitem", { name: "Oscillator 2", exact: true }).click();
  await expect(hears(envelope)).toHaveAttribute("aria-label", "Whose notes ADSR hears: Oscillator 2");
  file = await saved();
  expect(file.match(/^hears = /gm)).toHaveLength(2);
  expect(file).toContain('hears = "instrument[1]"');
  expect(file.indexOf("[[modulator]]")).toBeLessThan(file.indexOf('hears = "instrument[1]"'));
  // A page opened again has it: the session is the server's.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(hears(node(page, "ADSR"))).toHaveAttribute("aria-label", "Whose notes ADSR hears: Oscillator 2");
  await expect(hears(node(page, "ADSR"))).toHaveClass(/tang-node__notes--set/);
  // Undo takes it back, in the file too.
  await page.getByRole("button", { name: "Undo make an envelope hear the lane of Oscillator", exact: true }).click();
  await expect(hears(node(page, "ADSR"))).not.toHaveClass(/tang-node__notes--set/);
  file = await saved();
  expect(file.match(/^hears = /gm)).toHaveLength(1);
  expect(file).not.toContain('hears = "instrument[1]"');
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
  // (The modulator is in the file at once, with no cable: a table of its own after the tracer's LFO, the session's own since the program loaded it, which has its one cable to the cutoff.)
  let file = await saved();
  expect(file.match(/^\[\[modulator\]\]$/gm)).toHaveLength(2);
  expect(file.match(/\[\[modulator\.target\]\]/g)).toHaveLength(1);

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

  // The file has a table that is no lane's, with the cables that name the plugins by their places in the session, after the tracer's own LFO as it was.
  file = await saved();
  expect(file.match(/^\[\[modulator\]\]$/gm)).toHaveLength(2);
  const table = file.slice(file.lastIndexOf("[[modulator]]"));
  expect(table.match(/\[\[modulator\.target\]\]/g)).toHaveLength(3);
  expect(table).toMatch(/param = "detune"\nto = "instrument\[0\]"\n/);
  expect(table).toMatch(/param = "detune"\nto = "instrument\[1\]"\n/);
  expect(table).toMatch(/param = "resonance"\nto = "instrument\[0\]\.effect\[0\]"\n/);
  expect(file.match(/\[\[instrument\.modulator\]\]/g)).toBeNull();

  // The "×" of the modulator takes it out with its cables: the knobs stop moving and the file has the tracer's LFO only. Undo puts it back, and the knobs move again.
  await lfo.getByRole("button", { name: /^Remove LFO/ }).click();
  await expect(node(page, "LFO 2")).toHaveCount(0);
  await expect(cables(page)).toHaveCount(1);
  await expect(addModulator(page)).toBeFocused();
  expect((await saved()).match(/^\[\[modulator\]\]$/gm)).toHaveLength(1);
  await page.getByRole("button", { name: "Undo remove an LFO", exact: true }).click();
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(cables(page)).toHaveCount(4);
  expect(spread(await dots(node(page, "Oscillator 1"), "detune", 2600))).toBeGreaterThan(0.2);
  expect((await saved()).match(/\[\[modulator\.target\]\]/g)).toHaveLength(4);

  // A page opened again has it: the session is the server's.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(cables(page)).toHaveCount(4);
});

test("a modulator made from a knob, in the real program, hears the knob's lane and drives a knob of another lane, which moves, and the file has it hearing the first and driving both", async ({ page, own }) => {
  // (A good many waits, and a lane added.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
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
  await addLane(page);
  await showAll(page);
  const first = node(page, "Oscillator 1");
  const second = node(page, "Oscillator 2");
  await still(first);
  await still(second);

  // "New LFO" from the detune knob of the second lane: it hears that lane. Its cable is to that knob; it is drawn to the first lane's detune as well, which is the thing that a lane's LFO could not do.
  await cell(second, "detune").hover();
  await modulate(second, "detune").click();
  await page.getByRole("menuitem", { name: "New LFO", exact: true }).click();
  const lfo = node(page, "LFO 2");
  await expect(lfo).toBeVisible();
  await expect(hears(lfo)).toHaveAttribute("aria-label", "Whose notes LFO 2 hears: Oscillator 2");
  await still(lfo);
  await expect(cables(page)).toHaveCount(2);
  await drag(page, output(lfo), port(first, "detune"));
  await expect(cables(page)).toHaveCount(3);
  await expect(lfo.getByText("Oscillator 1: detune")).toBeVisible();
  await expect(lfo.getByText("Oscillator 2: detune")).toBeVisible();

  // Both knobs move on the engine's own telemetry, as the one LFO turns.
  const firstDots = await dots(first, "detune", 2600);
  const secondDots = await dots(second, "detune", 2600);
  expect(firstDots.length).toBeGreaterThan(20);
  expect(spread(firstDots), "the first lane's detune moves").toBeGreaterThan(0.1);
  expect(spread(secondDots), "the second lane's detune moves").toBeGreaterThan(0.1);

  // The file has the table, with the lane that it hears by its place among the lanes, after the tracer's own, and the two cables by the places of the plugins they drive.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const mine = modulatorsIn(readFileSync(own.file, "utf8"))[1] ?? "";
  expect(mine).toContain('hears = "instrument[1]"');
  expect(mine.match(/\[\[modulator\.target\]\]/g)).toHaveLength(2);
  expect(mine).toMatch(/param = "detune"\nto = "instrument\[1\]"\n/);
  expect(mine).toMatch(/param = "detune"\nto = "instrument\[0\]"\n/);
});
