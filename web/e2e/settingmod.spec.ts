import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { angleOf, arcPath, fractionOf, pointAt } from "../src/ui/knob-geometry";
import { addEnvelope, drag, node, output, setting, showAll, still } from "./canvas";
import { test, testWithGroups } from "./own";

// What a cable to a modulator's own setting looks like, on the real program (a program of its own, on the null audio backend): the rate of the
// LFO is a knob that has the ring and the dot of a parameter's knob once an envelope is drawn to it, and the real envelope moves the dot while a
// note is held (f132); the slider of the depth of a cable has a bar for what the engine makes of the depth once another cable drives it (f118).

async function open(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" })).toBeVisible();
}

/** The knob of a number of a modulator: what holds the dial, its ring and its dot. */
const knobOf = (modulator: Locator, name: string): Locator => modulator.locator(".tang-knob", { has: modulator.page().getByRole("slider", { name, exact: true }) });

/** Where the dot of a knob is, in the knob's own pixels. */
async function dotAt(dot: Locator): Promise<{ x: number; y: number }> {
  return { x: Number(await dot.getAttribute("cx")), y: Number(await dot.getAttribute("cy")) };
}

/**
 * Where the Rate knob (0.01 to 50 Hz, on decades) of the tracer's LFO, which the user set to 0.5 Hz, draws its dot when an envelope that is `open` (0 at rest, 1 at its peak) is drawn to it
 * at half depth: a setting that is turned on a log scale is moved a fraction of the knob's travel, so the envelope takes `0.5 x (1 - open)` of the way down the knob from the place of the 0.5 Hz.
 */
const rateWith = (open: number): { x: number; y: number } => pointAt(24, 18, angleOf((1 - 0.5 * (1 - open)) * fractionOf(0.5, 0.01, 50, "log")));

test("an envelope drawn to the rate of the LFO gives the Rate knob its ring and its dot, and the dot follows the real envelope while a note is held", async ({ page, own }) => {
  await open(page, own.url);
  const lfo = node(page, "LFO");
  const rate = knobOf(lfo, "Rate");
  // Nothing drives the rate: no ring, and a dot that has nothing to say.
  await expect(rate.locator(".tang-knob__ring")).toHaveCount(0);
  await expect(rate.locator(".tang-knob__dot")).toHaveAttribute("visibility", "hidden");

  const envelope = await addEnvelope(page);
  await drag(page, output(envelope), setting(lfo, "rate"));
  await expect(rate.locator(".tang-knob__ring")).toHaveCount(1);
  const dot = rate.locator(".tang-knob__dot");
  await expect(dot).toHaveAttribute("visibility", "visible");

  // At rest the envelope puts out nothing: the cable, at half depth, pulls the rate half the way down the knob from the place of the 0.5 Hz the user set.
  const rest = rateWith(0);
  await expect.poll(async () => (await dotAt(dot)).x).toBeCloseTo(rest.x, 1);
  expect((await dotAt(dot)).y).toBeCloseTo(rest.y, 1);

  // While a note is held the envelope rises to its sustain, 0.7, which pulls the rate only 0.3 of the way down what it does at rest.
  await page.keyboard.down("z");
  const held = rateWith(0.7);
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(held.x, 1);
  expect((await dotAt(dot)).y).toBeCloseTo(held.y, 1);
  expect(Math.hypot(held.x - rest.x, held.y - rest.y)).toBeGreaterThan(2);

  // And back, along the release, when it is let go. The knob itself is where the user set it all the while.
  await page.keyboard.up("z");
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(rest.x, 1);
  await expect(lfo.getByRole("slider", { name: "Rate", exact: true })).toHaveAttribute("aria-valuenow", "0.5");
});

test("an envelope drawn to the depth of the LFO's cable gives its slider a bar for the depth the engine makes of it, which follows a held note", async ({ page, own }) => {
  await open(page, own.url);
  // (Exactly: the slider of the cable that drives it is "Depth of ADSR → depth of LFO → Filter: cutoff", which holds the same words.)
  const depth = page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff", exact: true });
  const bar = page.getByRole("meter", { name: "Depth of LFO → Filter: cutoff, as modulated", exact: true });
  await expect(bar).toHaveCount(0);

  const envelope = await addEnvelope(page);
  await drag(page, output(envelope), node(page, "LFO").locator(".tang-cable-row .react-flow__handle"));
  await expect(bar).toBeVisible();
  const said = async (): Promise<number> => Number(await bar.getAttribute("aria-valuenow"));

  // At rest the cable that drives the depth, at half depth, pulls the 0.4 the user set half the way down to the lowest depth, -1: to -0.3.
  await expect.poll(said).toBeCloseTo(-0.3, 2);
  // With a note held the envelope is at its sustain, 0.7: 0.3 of the way down, which is 0.19.
  await page.keyboard.down("z");
  await expect.poll(said, { timeout: 8000 }).toBeCloseTo(0.19, 2);
  await page.keyboard.up("z");
  await expect.poll(said, { timeout: 8000 }).toBeCloseTo(-0.3, 2);
  // The slider is the user's all the while.
  await expect(depth).toHaveValue("0.4");
});

test("the depth that the user sets for a cable whose depth another cable drives is what that cable works from: the bar follows it, and the engine does not write the old one back", async ({ page, own }) => {
  await open(page, own.url);
  const depth = page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff", exact: true });
  const bar = page.getByRole("meter", { name: "Depth of LFO → Filter: cutoff, as modulated", exact: true });
  const envelope = await addEnvelope(page);
  await drag(page, output(envelope), node(page, "LFO").locator(".tang-cable-row .react-flow__handle"));
  await expect(bar).toBeVisible();
  const said = async (): Promise<number> => Number(await bar.getAttribute("aria-valuenow"));
  await expect.poll(said).toBeCloseTo(-0.3, 2);

  // The slider is turned to 0.8 with the keyboard (forty steps of a hundredth). At rest the envelope pulls the depth half the way from there to the lowest, -1: to -0.1.
  // (Not to -0.3, which is what the cable works from if the engine keeps the 0.4 that it began with.)
  await depth.focus();
  for (let i = 0; i < 40; i += 1) await page.keyboard.press("ArrowRight");
  await expect(depth).toHaveValue("0.8");
  await expect.poll(said, { timeout: 8000 }).toBeCloseTo(-0.1, 2);
  // And with a note held, from 0.8 too: 0.3 of the way down, which is 0.8 - 0.5 x 0.3 x 1.8 = 0.53.
  await page.keyboard.down("z");
  await expect.poll(said, { timeout: 8000 }).toBeCloseTo(0.53, 2);
  await page.keyboard.up("z");
  await expect.poll(said, { timeout: 8000 }).toBeCloseTo(-0.1, 2);
});

// (A program started on the tracer's file with a group: the page has no way to make one.)
const grouped = testWithGroups(1);

grouped("a group's envelope drawn to the rate of a member's LFO gives the Rate knob its ring and its dot, which follow the real engine: the member's modulator is told by the group", async ({ page, own }) => {
  await open(page, own.url);
  // The group that the file has, with the lane in it, and an envelope in the group (a group's modulator reaches the modulators of its members).
  await expect(node(page, "Group 1")).toBeVisible();
  await page.getByRole("combobox", { name: "Group of Oscillator" }).selectOption({ label: "Group 1" });
  await expect(page.getByRole("combobox", { name: "Group of Oscillator" }).locator("option:checked")).toHaveText("Group 1");
  await page.getByRole("button", { name: "Add a modulator to Group 1" }).click();
  await page.getByRole("menuitem", { name: "Envelope" }).click();
  const envelope = node(page, "ADSR");
  await expect(envelope).toBeVisible();

  const lfo = node(page, "LFO");
  const rate = knobOf(lfo, "Rate");
  // (The view has moved to show the envelope, which is above the group where the layout put it, and the LFO, which stayed, may be out of it.)
  await showAll(page);
  await still(envelope);
  await still(lfo);
  await drag(page, output(envelope), setting(lfo, "rate"));
  await expect(rate.locator(".tang-knob__ring")).toHaveCount(1);
  const dot = rate.locator(".tang-knob__dot");
  await expect(dot).toHaveAttribute("visibility", "visible");
  // At rest, as for a lane's own envelope: the cable, at half depth, pulls the 0.5 Hz half the way down the knob.
  const rest = rateWith(0);
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(rest.x, 1);
  expect((await dotAt(dot)).y).toBeCloseTo(rest.y, 1);
  // With a note held the group's envelope (which a note of its member opens) rises to its sustain, and the dot goes up with it.
  await page.keyboard.down("z");
  const held = rateWith(0.7);
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(held.x, 1);
  await page.keyboard.up("z");
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(rest.x, 1);
});

/** How far round its sweep, from 0 at the lowest value to 1 at the highest, a knob's dot at (`x`, `y`) is: the angle from straight up, clockwise (the knob turns through 270 degrees from the lower left). */
const placeOfDot = (x: number, y: number): number => (Math.atan2(x - 24, 24 - y) * (180 / Math.PI) + 135) / 270;

test("an LFO drawn to the Attack of an envelope swings it by a fraction of the knob's travel: as far up the knob as the depth says, and no further down than the lowest value", async ({ page, own }) => {
  await open(page, own.url);
  const envelope = await addEnvelope(page);
  const attack = knobOf(envelope, "Attack");
  await drag(page, output(node(page, "LFO")), setting(envelope, "attack"));
  const ring = attack.locator(".tang-knob__ring");
  await expect(ring).toHaveCount(1);
  // The attack is 10 ms, a quarter of the way up a knob of 0.001 to 10 s (four decades), and the cable is at half depth: a half of the travel either way, which is three quarters up and
  // past the lowest value down. (It was a half of the range in seconds, which took the ring to 92% of the knob, 5 seconds, with the pointer at a quarter: the dot sat at the lowest value
  // for half of each turn of the LFO and then went to the top.)
  await expect(ring).toHaveAttribute("d", arcPath(24, 21.5, 0, 0.75));
  const dot = attack.locator(".tang-knob__dot");
  await expect(dot).toHaveAttribute("visibility", "visible");

  // The tracer's LFO turns at 0.5 Hz: in two and a half seconds the dot has been to both ends of the ring, and not past them.
  const places: number[] = [];
  const began = Date.now();
  while (Date.now() - began < 2500) {
    const { x, y } = await dotAt(dot);
    places.push(placeOfDot(x, y));
    await page.waitForTimeout(40);
  }
  expect(Math.max(...places)).toBeGreaterThan(0.7);
  expect(Math.max(...places)).toBeLessThan(0.78);
  expect(Math.min(...places)).toBeLessThan(0.02);
});
