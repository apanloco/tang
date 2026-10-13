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
