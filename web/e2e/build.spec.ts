import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { CABLE_ROW_HEIGHT, CABLES_GAP, MODULATOR_SETTINGS_HEIGHT, NODE_WIDTH } from "../src/app/patch/toFlow";
import { addEnvelope, cablePoints, cables, centre, drag, expectNodesApart, node, nodeBoxes, open, output, port, setting, within } from "./canvas";

// What a person does to build modulation on the page, against the mock engine in the page (so that a test
// can have a session of its own: the tracer, or two lanes): add a modulator from the menu, draw a cable by
// dragging it from a modulator's output to a parameter's or a setting's port, and take a cable or a modulator out.

test("a cable is drawn by dragging it from a modulator's output to a parameter, and has a depth to turn at once", async ({ page }) => {
  await open(page);
  await expect(cables(page)).toHaveCount(1);
  await drag(page, output(node(page, "LFO")), port(node(page, "Oscillator"), "detune"));
  await expect(cables(page)).toHaveCount(2);
  const depth = page.getByRole("slider", { name: "Depth of LFO → Oscillator: detune" });
  await expect(depth).toHaveValue("0.5");
  await expect(port(node(page, "Oscillator"), "detune")).toHaveClass(/tang-port--driven/);
  await expect(page.locator(".tang-port--takes")).toHaveCount(0); // the marks go when the cable is dropped
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

test("the parameters that take the cable are marked while it is dragged, and not the one that is driven already", async ({ page }) => {
  await open(page);
  await drag(page, output(node(page, "LFO")), port(node(page, "Oscillator"), "detune"), false);
  // The oscillator's eight and the filter's three (the cutoff is driven by this LFO).
  await expect(page.locator(".tang-port--takes")).toHaveCount(11);
  await expect(port(node(page, "Filter"), "cutoff")).not.toHaveClass(/tang-port--takes/);
  await expect(port(node(page, "Oscillator"), "detune")).toHaveClass(/valid/);
  await page.mouse.up();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.locator(".tang-port--takes")).toHaveCount(0);
});

/** A point `down` of the way (0 to 1) down the cell of the control called `name`, a third of the way across. */
async function over(plugin: Locator, name: string, down: number): Promise<{ x: number; y: number }> {
  const box = await plugin.locator(".tang-cell").filter({ has: plugin.page().locator(`[aria-label="${name}"]`) }).boundingBox();
  if (!box) throw new Error(`no control called ${name}`);
  return { x: box.x + box.width / 3, y: box.y + box.height * down };
}

test("a cable let go anywhere over a parameter's control is drawn to that parameter, and not to the one above or below it", async ({ page }) => {
  await open(page);
  const oscillator = node(page, "Oscillator");
  // Near the top of the cell of one, in the middle of another, near the foot of a third (they are in a column, so
  // the foot of one is a few pixels from the port of the next), and in the gap beside the knob.
  const drops: [string, number][] = [
    ["decay", 0.04],
    ["pan", 0.15],
    ["volume", 0.5],
    ["attack", 0.88],
    ["release", 0.97],
  ];
  let drawn = 1;
  for (const [name, down] of drops) {
    await drag(page, output(node(page, "LFO")), await over(oscillator, name, down));
    drawn += 1;
    await expect(cables(page), `a cable let go over ${name}`).toHaveCount(drawn);
    await expect(page.getByRole("slider", { name: `Depth of LFO → Oscillator: ${name}` }), name).toBeVisible();
  }
  // (Five cables, each to the parameter it was let go over: not one to a neighbour.)
  await expect(page.locator(".tang-cable--modulation")).toHaveCount(6);
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

test("a cable dropped where it does not belong is not drawn: on the empty canvas, or on a parameter that is driven already", async ({ page }) => {
  await open(page);
  await drag(page, output(node(page, "LFO")), { x: 1500, y: 900 });
  await expect(cables(page)).toHaveCount(1);
  await drag(page, output(node(page, "LFO")), port(node(page, "Filter"), "cutoff"));
  await expect(cables(page)).toHaveCount(1);
  await expect(page.locator(".tang-notice")).toHaveCount(0);
  // And a cable cannot be started at a parameter.
  await drag(page, port(node(page, "Oscillator"), "detune"), port(node(page, "Filter"), "resonance"));
  await expect(cables(page)).toHaveCount(1);
});

test("a cable cannot be dropped on a lane the modulator does not belong to", async ({ page }) => {
  await open(page, "lanes");
  const lane = (n: number) => node(page, `Oscillator ${String(n)}`);
  await expect(page.locator(".tang-cable--modulation")).toHaveCount(2);
  const lfo = (n: number) => page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: `LFO ${String(n)}` }) });
  // The first LFO to the first lane's oscillator, which is its own, and to the second's, which is not.
  await drag(page, output(lfo(1)), port(lane(1), "decay"));
  await expect(cables(page)).toHaveCount(3);
  await drag(page, output(lfo(1)), port(lane(2), "decay"));
  await expect(cables(page)).toHaveCount(3);
  await drag(page, output(lfo(2)), port(lane(2), "decay"));
  await expect(cables(page)).toHaveCount(4);
  // (Dropped out of reach, a cable is not even asked for: there is nothing for the page to say it was refused.)
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

test("a modulator added from the menu has an output to drag from at once", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Add a modulator to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Envelope" }).click();
  const envelope = node(page, "ADSR");
  await expect(envelope).toBeVisible();
  await drag(page, output(envelope), port(node(page, "Filter"), "resonance"));
  await expect(page.getByRole("slider", { name: "Depth of ADSR → Filter: resonance" })).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
});

test("a cable is drawn by dragging it from a modulator's output to the rate of another, and has a depth to turn at once", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  await drag(page, output(envelope), setting(node(page, "LFO"), "rate"));
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of ADSR → LFO: rate" })).toHaveValue("0.5");
  await expect(setting(node(page, "LFO"), "rate")).toHaveClass(/tang-port--driven/);
  await expect(setting(envelope, "attack")).not.toHaveClass(/tang-port--driven/);
  await expect(page.locator(".tang-port--takes")).toHaveCount(0); // the marks go when the cable is dropped
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

test("a cable is drawn from an LFO to a stage of an envelope", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  await drag(page, output(node(page, "LFO")), setting(envelope, "release"));
  await expect(page.getByRole("slider", { name: "Depth of LFO → ADSR: release" })).toHaveValue("0.5");
  await expect(setting(envelope, "release")).toHaveClass(/tang-port--driven/);
  await expect(cables(page)).toHaveCount(2);
});

test("the settings that take the cable are marked while it is dragged from a modulator: the others', and not its own", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  const lfo = node(page, "LFO");
  const stages = ["attack", "decay", "sustain", "release"];
  await drag(page, output(envelope), setting(lfo, "rate"), false);
  await expect(setting(lfo, "rate")).toHaveClass(/tang-port--takes/);
  for (const stage of stages) await expect(setting(envelope, stage), stage).not.toHaveClass(/tang-port--takes/);
  // (Let go on the empty canvas: nothing is drawn.)
  await page.mouse.move(1500, 900, { steps: 4 });
  await page.mouse.up();
  await expect(cables(page)).toHaveCount(1);
  // And from the LFO it is the envelope's stages that take it.
  await drag(page, output(lfo), setting(envelope, "attack"), false);
  for (const stage of stages) await expect(setting(envelope, stage), stage).toHaveClass(/tang-port--takes/);
  await expect(setting(lfo, "rate")).not.toHaveClass(/tang-port--takes/);
  await page.mouse.move(1500, 900, { steps: 4 });
  await page.mouse.up();
  await expect(cables(page)).toHaveCount(1);
  await expect(page.locator(".tang-port--takes")).toHaveCount(0);
});

test("a cable dropped on a setting where it does not belong is not drawn: the modulator's own, one that it drives already, another lane's", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  await drag(page, output(envelope), setting(envelope, "attack"));
  await expect(cables(page)).toHaveCount(1);
  await drag(page, output(envelope), setting(node(page, "LFO"), "rate"));
  await expect(cables(page)).toHaveCount(2);
  await drag(page, output(envelope), setting(node(page, "LFO"), "rate"));
  await expect(cables(page)).toHaveCount(2);
  await expect(page.locator(".tang-notice")).toHaveCount(0);
  // A cable cannot be started at a setting.
  await drag(page, setting(envelope, "decay"), setting(node(page, "LFO"), "rate"));
  await expect(cables(page)).toHaveCount(2);
});

test("a cable cannot be dropped on a modulator of another lane", async ({ page }) => {
  await open(page, "lanes");
  const lfo = (n: number) => page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: `LFO ${String(n)}` }) });
  await expect(cables(page)).toHaveCount(2);
  await drag(page, output(lfo(1)), setting(lfo(2), "rate"));
  await expect(cables(page)).toHaveCount(2);
  await drag(page, output(lfo(2)), setting(lfo(1), "rate"));
  await expect(cables(page)).toHaveCount(2);
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

test("a cable to the setting of a modulator is drawn from the keyboard, from the list at the foot of another", async ({ page }) => {
  await open(page);
  await addEnvelope(page);
  const picker = page.getByRole("button", { name: "Add a cable from ADSR" });
  await picker.focus();
  await page.keyboard.press("ArrowDown");
  const menu = page.getByRole("menu", { name: "Add a cable from ADSR" });
  // (The plugins' parameters, under a heading for each, and then the settings of the other modulators of the lane, under a heading for each.)
  await expect(menu.locator(".tang-menu__heading")).toHaveText(["Oscillator", "Filter", "LFO"]);
  await page.keyboard.type("rat");
  await expect(menu.getByRole("menuitem", { name: "rate", exact: true })).toBeFocused();
  await expect(cables(page)).toHaveCount(1);
  await page.keyboard.press("Enter");
  await expect(page.getByRole("slider", { name: "Depth of ADSR → LFO: rate" })).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
  await expect(picker).toBeFocused();
  // The rate is off the list, and the depth of the LFO's cable is not.
  await page.keyboard.press("ArrowDown");
  await expect(menu.locator(".tang-menu__heading")).toHaveText(["Oscillator", "Filter", "LFO"]);
  await expect(menu.getByRole("group", { name: "LFO" }).getByRole("menuitem")).toHaveText(["depth → Filter: cutoff"]);
});

test("a cable to a setting is taken out by its button, and the focus goes to the list of the modulator that drove it", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  await drag(page, output(envelope), setting(node(page, "LFO"), "rate"));
  await expect(cables(page)).toHaveCount(2);
  await page.getByRole("button", { name: "Remove cable ADSR → LFO: rate" }).click();
  await expect(cables(page)).toHaveCount(1);
  await expect(setting(node(page, "LFO"), "rate")).not.toHaveClass(/tang-port--driven/);
  await expect(page.getByRole("button", { name: "Add a cable from ADSR" })).toBeFocused();
  // The modulator it drove can be taken out with a cable to it: the cable goes with it.
  await drag(page, output(envelope), setting(node(page, "LFO"), "rate"));
  await expect(cables(page)).toHaveCount(2);
  await page.getByRole("button", { name: "Remove LFO" }).click();
  await expect(node(page, "LFO")).toHaveCount(0);
  await expect(cables(page)).toHaveCount(0);
});

/** What has the focus, in words. */
const focused = (page: Page): Promise<string> =>
  page.evaluate(() => {
    const element = document.activeElement;
    return `${element?.tagName ?? "nothing"} ${element?.getAttribute("aria-label") ?? element?.textContent ?? ""}`.trim();
  });

test("Tab out of the add menu goes on from its button, as Tab does when the menu is closed", async ({ page }) => {
  await open(page);
  const add = page.getByRole("button", { name: "Add a modulator to Oscillator" });
  // What Tab, and Shift+Tab, come to from the button with the menu closed.
  await add.focus();
  await page.keyboard.press("Tab");
  const after = await focused(page);
  await add.focus();
  await page.keyboard.press("Shift+Tab");
  const before = await focused(page);
  expect([after, before]).not.toContain("nothing");
  expect(after).not.toBe(before);
  // And from the open menu: the menu is in the page, not in the node, so Tab from it would have gone to
  // whatever comes after the last thing in the page.
  for (const [keys, expected] of [["Tab", after], ["Shift+Tab", before]] as const) {
    await add.focus();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("menu")).toBeVisible();
    await page.keyboard.press(keys);
    await expect(page.getByRole("menu")).toHaveCount(0);
    expect(await focused(page), keys).toBe(expected);
  }
});

test("a cable is drawn from the keyboard, from the list at the foot of a modulator", async ({ page }) => {
  await open(page);
  const picker = page.getByRole("button", { name: "Add a cable from LFO" });
  // From the keyboard alone: an arrow key opens the list, letters find a parameter, Enter chooses it.
  await picker.focus();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("menu", { name: "Add a cable from LFO" })).toBeVisible();
  await page.keyboard.type("det");
  await expect(page.getByRole("menuitem", { name: "detune", exact: true })).toBeFocused();
  // (Nothing is drawn until Enter: not by opening the list, nor by finding a parameter in it.)
  await expect(cables(page)).toHaveCount(1);
  await page.keyboard.press("Enter");
  await expect(page.getByRole("slider", { name: "Depth of LFO → Oscillator: detune" })).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
  // The list is closed, with the focus on its button, and the parameter is off it.
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(picker).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("menuitem", { name: "volume", exact: true })).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "detune", exact: true })).toHaveCount(0);
});

/** The ports of the rows of the LFO's cables. */
const lfoRowPorts = (page: Page): Locator => node(page, "LFO").locator(".tang-cable-row .react-flow__handle");

/** How tall a modulator's node and each of its rows of cables are, in the pixels of the layout (the canvas is zoomed to fit the window), with where each row's port is. */
async function listing(modulator: Locator) {
  const box = await modulator.locator(".tang-node").boundingBox();
  if (!box) throw new Error("not on the page");
  const scale = box.width / NODE_WIDTH;
  const rows = await Promise.all(
    (await modulator.locator(".tang-cable-row").all()).map(async (row) => {
      const [at, dot] = [await row.boundingBox(), await row.locator(".react-flow__handle").boundingBox()];
      if (!at || !dot) throw new Error("a row with no port");
      return { height: at.height / scale, portX: (dot.x + dot.width / 2 - box.x) / scale, portY: (dot.y + dot.height / 2 - at.y) / scale };
    }),
  );
  return { height: box.height / scale, rows };
}

test("a modulator lists a row for each cable that starts at it, each as tall as the layout assumes, with its port on the node's left edge", async ({ page }) => {
  await open(page);
  const lfo = node(page, "LFO");
  await expect(lfo.locator(".tang-cable-row__label")).toHaveText(["Filter: cutoff"]);
  const one = await listing(lfo);
  // The node is as tall as the layout assumes for a row (and not much less), and the row is as high as it assumes.
  const assumedOne = MODULATOR_SETTINGS_HEIGHT + CABLES_GAP + CABLE_ROW_HEIGHT;
  expect(one.height).toBeLessThanOrEqual(assumedOne);
  expect(one.height).toBeGreaterThan(assumedOne - 12);
  expect(one.rows.map((row) => row.height)).toEqual([expect.closeTo(CABLE_ROW_HEIGHT, 0)]);
  // The port is where the node's left edge is, level with the middle of its row.
  expect(one.rows[0]?.portX).toBeCloseTo(0, 0);
  expect(one.rows[0]?.portY).toBeCloseTo(CABLE_ROW_HEIGHT / 2, 0);
  // A second cable, from the list: a row more, and the node as much taller, with the others where they were.
  const picker = page.getByRole("button", { name: "Add a cable from LFO" });
  await picker.focus();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.type("det");
  await page.keyboard.press("Enter");
  await expect(lfo.locator(".tang-cable-row__label")).toHaveText(["Filter: cutoff", "Oscillator: detune"]);
  const two = await listing(lfo);
  expect(two.height - one.height).toBeCloseTo(CABLE_ROW_HEIGHT, 0);
  expect(two.rows.map((row) => row.height)).toEqual([expect.closeTo(CABLE_ROW_HEIGHT, 0), expect.closeTo(CABLE_ROW_HEIGHT, 0)]);
  expect(two.rows[1]?.portX).toBeCloseTo(0, 0);
  await expectNodesApart(page);
});

test("a cable is drawn by dragging it from a modulator's output to the depth of a cable, to the port of its row, and the modulator is laid out above the one whose cable it is", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  const lfo = node(page, "LFO");
  const rowPort = lfo.locator(".tang-cable-row .react-flow__handle");
  await expect(rowPort).toHaveCount(1);
  await expect(rowPort).not.toHaveClass(/tang-port--driven/);
  await drag(page, output(envelope), rowPort, false);
  // While it is dragged the port of the row says that it takes the cable, and is the one it is over (with the LFO's rate and the parameters it can drive).
  await expect(rowPort).toHaveClass(/tang-port--takes/);
  await expect(rowPort).toHaveClass(/valid/);
  await expect(page.locator(".tang-port--takes")).toHaveCount(8 + 4 + 1 + 1); // the oscillator's eight, the filter's four, the LFO's rate and the depth of its cable
  await page.mouse.up();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of ADSR → depth of LFO → Filter: cutoff" })).toHaveValue("0.5");
  await expect(rowPort).toHaveClass(/tang-port--driven/);
  await expect(page.locator(".tang-port--takes")).toHaveCount(0);
  await expect(page.locator(".tang-notice")).toHaveCount(0);
  // The envelope is above the LFO, in its column and clear of it (the cable runs down), and no node lies over another.
  const [above, below] = [await envelope.boundingBox(), await lfo.boundingBox()];
  if (!above || !below) throw new Error("not on the page");
  expect(above.y + above.height).toBeLessThan(below.y);
  expect(Math.abs(above.x - below.x)).toBeLessThan(2);
  await expectNodesApart(page);
});

test("a cable to the depth of a cable goes round the node it ends at, and has its slider in the room under the modulator that drives it, over no node", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  await drag(page, output(envelope), node(page, "LFO").locator(".tang-cable-row .react-flow__handle"));
  await expect(cables(page)).toHaveCount(2);
  const depth = page.getByRole("slider", { name: "Depth of ADSR → depth of LFO → Filter: cutoff" });
  await expect(depth).toBeVisible();
  // No point of the cable is inside a node: it leaves the foot of the envelope, runs along under it and down beside the LFO, and comes in at its left edge.
  const [points, boxes] = [await cablePoints(page), await nodeBoxes(page)];
  const lfo = boxes.find((box) => box.name === "LFO");
  const adsr = boxes.find((box) => box.name === "ADSR");
  if (!lfo || !adsr) throw new Error("the nodes are not on the page");
  for (const box of boxes) expect(points.filter((point) => within(point, box)), `the cable is in ${box.name}`).toEqual([]);
  expect(Math.min(...points.map((point) => point.x))).toBeLessThan(lfo.left - 10);
  // (It ends at the port, which is at the node's edge: within its own size of it, and in the lower half of the node, where the row is.)
  const end = points.at(-1);
  expect(end && Math.abs(end.x - lfo.left)).toBeLessThan(8);
  expect(end?.y).toBeGreaterThan(lfo.top + lfo.height / 2);
  // The slider and the button that takes the cable out are between the two nodes, not over either.
  const anchor = depth.locator("xpath=ancestor::div[contains(@class, 'tang-depth-anchor')]");
  const pill = await anchor.boundingBox();
  if (!pill) throw new Error("the slider is not on the page");
  expect(pill.y).toBeGreaterThanOrEqual(adsr.top + adsr.height);
  expect(pill.y + pill.height).toBeLessThanOrEqual(lfo.top);
});

test("a cable let go over the row of a cable, anywhere in it, is drawn to the depth of that cable, and one let go where it does not belong is not", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  const lfo = node(page, "LFO");
  const row = lfo.locator(".tang-cable-row");
  const box = await row.boundingBox();
  if (!box) throw new Error("no row");
  // The right end of the row, far from its port: while the cable is dragged the whole row is where it can be let go.
  await drag(page, output(envelope), { x: box.x + box.width - 8, y: box.y + box.height / 2 });
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of ADSR → depth of LFO → Filter: cutoff" })).toBeVisible();
  // The cable is let go over the same row again: it is driven already, and the row does not take it.
  await drag(page, output(envelope), { x: box.x + box.width - 8, y: box.y + box.height / 2 });
  await expect(cables(page)).toHaveCount(2);
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

test("a cable can be dropped on the depth of a cable of a modulator of its own lane, and not on the depth of a cable of another lane's", async ({ page }) => {
  await open(page, "lanes");
  const lfo = (n: number) => node(page, `LFO ${String(n)}`);
  // An envelope in the first lane drives a parameter of the lane's oscillator: it has a cable, and a row of it.
  await page.getByRole("button", { name: "Add a modulator to Oscillator 1" }).click();
  await page.getByRole("menuitem", { name: "Envelope" }).click();
  const envelope = node(page, "ADSR");
  await expect(envelope).toBeVisible();
  await drag(page, output(envelope), port(node(page, "Oscillator 1"), "decay"));
  await expect(cables(page)).toHaveCount(3);
  await expect(envelope.locator(".tang-cable-row")).toHaveCount(1);
  // The first LFO to the depth of the envelope's cable (its own lane's) is drawn, and to the depth of the second LFO's cable (another lane's) is not.
  await drag(page, output(lfo(1)), envelope.locator(".tang-cable-row .react-flow__handle"));
  await expect(cables(page)).toHaveCount(4);
  await drag(page, output(lfo(1)), lfo(2).locator(".tang-cable-row .react-flow__handle"));
  await expect(cables(page)).toHaveCount(4);
  // (Dropped out of reach, a cable is not even asked for: there is nothing for the page to say it was refused.)
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

test("a cable to the depth of a cable is drawn from the keyboard, from the list at the foot of a modulator, under the modulator whose cable it is, and is taken out by its button", async ({ page }) => {
  await open(page);
  await addEnvelope(page);
  const picker = page.getByRole("button", { name: "Add a cable from ADSR" });
  await picker.focus();
  await page.keyboard.press("ArrowDown");
  const menu = page.getByRole("menu", { name: "Add a cable from ADSR" });
  await expect(menu.getByRole("group", { name: "LFO" }).getByRole("menuitem")).toHaveText(["rate", "depth → Filter: cutoff"]);
  await page.keyboard.type("depth");
  await expect(menu.getByRole("menuitem", { name: "depth → Filter: cutoff" })).toBeFocused();
  await expect(cables(page)).toHaveCount(1);
  await page.keyboard.press("Enter");
  await expect(page.getByRole("slider", { name: "Depth of ADSR → depth of LFO → Filter: cutoff" })).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(picker).toBeFocused();
  // That depth is off the list; the cable is taken out by its button, and the focus goes to the list of the modulator that drove it.
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("menuitem", { name: "depth → Filter: cutoff" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Remove cable ADSR → depth of LFO → Filter: cutoff" }).click();
  await expect(cables(page)).toHaveCount(1);
  await expect(picker).toBeFocused();
  await expect(lfoRowPorts(page)).not.toHaveClass(/tang-port--driven/);
});

test("the list of what a modulator can be drawn to has a heading for each plugin, and stays on the page", async ({ page }) => {
  await open(page);
  const picker = page.getByRole("button", { name: "Add a cable from LFO" });
  // (The button is a field as wide as the node's body, not a glyph.)
  const field = await picker.boundingBox();
  const lfo = await node(page, "LFO").boundingBox();
  expect(field).not.toBeNull();
  expect(lfo).not.toBeNull();
  if (field && lfo) expect(field.width).toBeGreaterThan(lfo.width * 0.7);
  await picker.click();
  const menu = page.getByRole("menu", { name: "Add a cable from LFO" });
  await expect(menu).toBeVisible();
  // (The tracer's list is eleven parameters under two headings; one that is longer scrolls, at 360 px.)
  const box = await menu.boundingBox();
  const size = page.viewportSize();
  expect(box).not.toBeNull();
  expect(size).not.toBeNull();
  if (box && size) {
    expect(box.y).toBeGreaterThanOrEqual(0);
    expect(box.y + box.height).toBeLessThanOrEqual(size.height);
    expect(box.height).toBeLessThanOrEqual(360);
  }
  await expect(menu.getByRole("group", { name: "Oscillator" })).toBeVisible();
  await expect(menu.getByRole("group", { name: "Filter" })).toBeVisible();
  // (And a heading drawn for each, which a screen reader does not read twice.)
  await expect(menu.locator(".tang-menu__heading")).toHaveText(["Oscillator", "Filter"]);
});

test("a cable and a modulator are taken out by their buttons", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Remove cable LFO → Filter: cutoff" }).click();
  await expect(cables(page)).toHaveCount(0);
  await expect(node(page, "LFO")).toBeVisible();
  // The cable's button went with it: the focus is on the list of its modulator, where the next one is drawn from.
  await expect(page.getByRole("button", { name: "Add a cable from LFO" })).toBeFocused();
  await page.getByRole("button", { name: "Remove LFO" }).click();
  await expect(node(page, "LFO")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Add a modulator to Oscillator" })).toBeFocused();
});

test("an effect is moved along its chain by its buttons: the canvas lays the chain out in the new order, the cable stays on its plugin, the buttons fit the node, and the focus stays where it was", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  const [filter, reverb] = [node(page, "Filter"), node(page, "Reverb")];
  await expect(reverb).toBeVisible();
  const leftOf = async (a: Locator, b: Locator): Promise<boolean> => (await centre(a)).x < (await centre(b)).x;
  // The chain is the filter and then the reverb, left to right; the first can go no earlier and the last no later.
  expect(await leftOf(filter, reverb)).toBe(true);
  const button = (name: string) => page.getByRole("button", { name });
  await expect(button("Move Filter earlier in its chain")).toHaveAttribute("aria-disabled", "true");
  await expect(button("Move Filter later in its chain")).toHaveAttribute("aria-disabled", "false");
  await expect(button("Move Reverb earlier in its chain")).toHaveAttribute("aria-disabled", "false");
  await expect(button("Move Reverb later in its chain")).toHaveAttribute("aria-disabled", "true");

  // The reverb goes before the filter: the canvas has them the other way round, and the focus is on the button that was pressed, which
  // has nowhere left to go.
  await button("Move Reverb earlier in its chain").click();
  await expect.poll(() => leftOf(reverb, filter), { message: "the reverb is first now" }).toBe(true);
  await expect(button("Move Reverb earlier in its chain")).toBeFocused();
  await expect(button("Move Reverb earlier in its chain")).toHaveAttribute("aria-disabled", "true");
  await expect(button("Move Reverb later in its chain")).toHaveAttribute("aria-disabled", "false");
  // The LFO's cable is on the filter's cutoff still: it is the plugin that the cable names, not the place.
  await expect(cables(page)).toHaveCount(1);
  await expect(button("Remove cable LFO → Filter: cutoff")).toBeVisible();
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // The buttons of an effect fit in its node, whatever order they are in.
  for (const effect of [filter, reverb]) {
    const box = await effect.locator(".tang-node").boundingBox();
    if (!box) throw new Error("not on the page");
    for (const control of await effect.locator(".tang-node__actions button").all()) {
      const at = await control.boundingBox();
      if (!at) throw new Error("not on the page");
      expect(at.x, "inside the node on the left").toBeGreaterThanOrEqual(box.x);
      expect(at.x + at.width, "inside the node on the right").toBeLessThanOrEqual(box.x + box.width);
    }
  }

  // And from the keyboard: Enter on the button that moves it later puts it back.
  await button("Move Reverb later in its chain").focus();
  await page.keyboard.press("Enter");
  await expect.poll(() => leftOf(filter, reverb), { message: "the filter is first again" }).toBe(true);
  await expect(button("Move Reverb later in its chain")).toBeFocused();
});
