import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { addEnvelope, cables, drag, node, open, output, port, setting } from "./canvas";

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
  // The rate is off the list, and the LFO has nothing else to take, so its heading is too.
  await page.keyboard.press("ArrowDown");
  await expect(menu.locator(".tang-menu__heading")).toHaveText(["Oscillator", "Filter"]);
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
