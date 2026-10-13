import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

// What a person does to build modulation on the page, against the mock engine in the page (so that a test
// can have a session of its own: the tracer, or two lanes): add a modulator from the menu, draw a cable by
// dragging it from a modulator's output to a parameter's port, and take a cable or a modulator out.

async function open(page: Page, scene = "tracer"): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(`/?engine=mock&scene=${scene}`);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.locator(".react-flow__node").first()).toBeVisible();
}

/** The node of the canvas whose name is exactly `name`. */
const node = (page: Page, name: string): Locator => page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: new RegExp(`^${name}$`) }) });

/** The output of a modulator, where a cable is started. */
const output = (modulator: Locator): Locator => modulator.locator('.react-flow__handle[data-handleid="modOut"]');

/** The port of the control of a parameter, where a cable ends. */
const port = (plugin: Locator, name: string): Locator => plugin.locator(".tang-cell").filter({ has: plugin.page().locator(`[aria-label="${name}"]`) }).locator(".react-flow__handle");

async function centre(target: Locator): Promise<{ x: number; y: number }> {
  const box = await target.boundingBox();
  if (!box) throw new Error("not on the page");
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** Press on `from` and move to `to`, and let go there unless `release` is false: a cable dragged by hand. */
async function drag(page: Page, from: Locator, to: Locator | { x: number; y: number }, release = true): Promise<void> {
  const start = await centre(from);
  const end = "x" in to ? to : await centre(to);
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
  await page.mouse.move((start.x + end.x) / 2, (start.y + end.y) / 2, { steps: 8 });
  await page.mouse.move(end.x, end.y, { steps: 8 });
  if (release) await page.mouse.up();
}

const cables = (page: Page): Locator => page.locator(".tang-cable--modulation");

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
  const picker = page.getByRole("combobox", { name: "Cable from LFO to" });
  await picker.selectOption({ label: "detune" });
  await expect(page.getByRole("slider", { name: "Depth of LFO → Oscillator: detune" })).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
  // The parameter is off the list now, and the list is back at its first line.
  await expect(picker.locator("option", { hasText: "detune" })).toHaveCount(0);
  await expect(picker).toHaveValue("");
});

test("a cable and a modulator are taken out by their buttons", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Remove cable LFO → Filter: cutoff" }).click();
  await expect(cables(page)).toHaveCount(0);
  await expect(node(page, "LFO")).toBeVisible();
  await page.getByRole("button", { name: "Remove LFO" }).click();
  await expect(node(page, "LFO")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Add a modulator to Oscillator" })).toBeFocused();
});
