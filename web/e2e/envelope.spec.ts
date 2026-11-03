import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { test as own } from "./own";
import { node, open, still } from "./canvas";

// What an envelope's node shows of its settings: the shape of the four stages, redrawn when a knob is turned, and a dot that follows the
// envelope while a note is held (the real engine's envelope: the mock's is a step).

/** The numbers in a path's `d`, in order. */
const numbersOf = (d: string | null): number[] => (d?.match(/-?\d+(?:\.\d+)?/g) ?? []).map(Number);

/** The five corners of the shape that was drawn: the bottom left, the top, the start and the end of the hold, the bottom right. */
async function corners(shape: Locator): Promise<{ x: number; y: number }[]> {
  const numbers = numbersOf(await shape.locator(".tang-envelope__line").getAttribute("d"));
  if (numbers.length !== 10) throw new Error(`a path of ${String(numbers.length)} numbers: ${numbers.join(" ")}`);
  return [0, 2, 4, 6, 8].map((i) => ({ x: numbers[i] ?? 0, y: numbers[i + 1] ?? 0 }));
}

/** An envelope put in the lane from the menu, once the canvas has stopped moving to show it; its node and its shape. */
async function addEnvelope(page: Page): Promise<{ envelope: Locator; shape: Locator }> {
  await page.getByRole("button", { name: "Add a modulator to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Envelope" }).click();
  const envelope = node(page, "ADSR");
  await expect(envelope).toBeVisible();
  await still(envelope);
  return { envelope, shape: envelope.getByRole("img", { name: /^ADSR shape: / }) };
}

test("an envelope's node draws the shape of its four stages, and draws it again when a stage is turned", async ({ page }) => {
  await open(page);
  const { envelope, shape } = await addEnvelope(page);
  await expect(shape).toBeVisible();
  await expect(shape).toHaveAttribute("aria-label", "ADSR shape: attack 10 ms, decay 300 ms, sustain 0.7, release 500 ms");
  const [start, top, holdStart, holdEnd, end] = await corners(shape);
  if (!start || !top || !holdStart || !holdEnd || !end) throw new Error("no corners");
  // The attack is the shortest stage and the release the longest, as set; the sustain is between nothing and the top, and holds.
  expect(top.y).toBeLessThan(holdStart.y);
  expect(holdStart.y).toBeLessThan(start.y);
  expect(holdEnd.y).toBe(holdStart.y);
  expect(top.x - start.x).toBeLessThan(holdStart.x - top.x);
  expect(end.x - holdEnd.x).toBeGreaterThan(top.x - start.x);

  // The shape fits in its node, and the node is not taller than the layout allows for (a node that is would overlap what is below it).
  const box = await envelope.locator(".tang-node").boundingBox();
  const picture = await shape.boundingBox();
  if (!box || !picture) throw new Error("not on the page");
  expect(picture.x).toBeGreaterThanOrEqual(box.x);
  expect(picture.x + picture.width).toBeLessThanOrEqual(box.x + box.width);
  expect(picture.height).toBeGreaterThan(40);
  expect(box.height).toBeLessThanOrEqual(320);

  // A longer attack is a wider one: the knob is turned with the keyboard, and the shape and its words follow.
  const attack = envelope.getByRole("slider", { name: "attack" });
  await attack.focus();
  for (let i = 0; i < 25; i += 1) await page.keyboard.press("ArrowUp");
  await expect(shape).not.toHaveAttribute("aria-label", /attack 10 ms/);
  const [, longTop] = await corners(shape);
  if (!longTop) throw new Error("no corners");
  expect(longTop.x - start.x).toBeGreaterThan(top.x - start.x);
  // The dot is not there before a note: nothing puts out a level to follow... it rests at the start (the mock's envelope puts out nothing).
  await expect(shape.locator(".tang-envelope__dot")).toHaveAttribute("visibility", "visible");
});

own("the dot of an envelope follows it while a note is held: up the attack, down the decay, along the hold, and down the release when it is let go", async ({ page, own: tang }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(tang.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const { envelope, shape } = await addEnvelope(page);

  // The attack is 10 ms, which is less than the time between two frames of the telemetry (30 a second): a dot seen on it would be luck. So it is made
  // longer first, with the keyboard as a person would (a page key is ten steps, about two and a half times as long), until the shape's words say that
  // it is a quarter of a second or more; the shape is drawn again with it, and its corners are read after.
  const attackMs = async (): Promise<number> => {
    const words = (await shape.getAttribute("aria-label")) ?? "";
    const found = /attack ([\d.]+) (ms|s),/.exec(words);
    if (!found) throw new Error(`no attack in "${words}"`);
    return Number(found[1]) * (found[2] === "s" ? 1000 : 1);
  };
  const slider = envelope.getByRole("slider", { name: "attack" });
  await slider.focus();
  for (let ms = await attackMs(); ms < 250; ms = await attackMs()) {
    await page.keyboard.press("PageUp");
    await expect.poll(attackMs).toBeGreaterThan(ms);
  }
  // (The focus is let go: a key on a knob is the knob's, and the note below is the page's.)
  await slider.blur();

  const [start, top, holdStart, holdEnd, end] = await corners(shape);
  if (!start || !top || !holdStart || !holdEnd || !end) throw new Error("no corners");
  const dot = shape.locator(".tang-envelope__dot");
  const at = async (): Promise<{ x: number; y: number }> => ({ x: Number(await dot.getAttribute("cx")), y: Number(await dot.getAttribute("cy")) });

  // At rest it is at the start of the shape.
  await expect.poll(async () => Math.abs((await at()).x - start.x)).toBeLessThan(0.5);

  // The dot's places on every frame of the page for two seconds, while a note is pressed and then let go: the sampler runs in the page, so that nothing
  // between two frames is missed.
  const sampling = shape.evaluate(async (svg) => {
    const circle = svg.querySelector(".tang-envelope__dot");
    const samples: { t: number; x: number; y: number }[] = [];
    const began = performance.now();
    while (performance.now() - began < 2600) {
      await new Promise((resolve) => requestAnimationFrame(resolve));
      samples.push({ t: performance.now() - began, x: Number(circle?.getAttribute("cx")), y: Number(circle?.getAttribute("cy")) });
    }
    return samples;
  });
  await page.waitForTimeout(200);
  await page.keyboard.down("z");
  await page.waitForTimeout(1300);
  await page.keyboard.up("z");
  const samples = await sampling;

  // It was on the attack (between the start and the top), on the decay (between the top and the start of the hold: the decay is 0.3 s), on the hold
  // (the sustain, 0.7), and on the release (past the hold). The dot at rest is at the start, which is not on the attack.
  const between = (a: number, b: number) => (s: { x: number }) => s.x > a + 0.5 && s.x < b - 0.5;
  expect(samples.some(between(start.x, top.x)), "a sample on the attack").toBe(true);
  expect(samples.some(between(top.x, holdStart.x)), "a sample on the decay").toBe(true);
  const held = samples.filter((s) => Math.abs(s.x - (holdStart.x + holdEnd.x) / 2) < 1 && Math.abs(s.y - holdStart.y) < 1);
  expect(held.length, "samples on the hold").toBeGreaterThan(10);
  expect(samples.some(between(holdEnd.x, end.x)), "a sample on the release").toBe(true);
  // The order is the envelope's: the attack before the decay, the decay before the hold, the hold before the release.
  const first = (match: (s: { x: number }) => boolean) => samples.findIndex(match);
  expect(first(between(start.x, top.x))).toBeLessThan(first(between(top.x, holdStart.x)));
  expect(first(between(top.x, holdStart.x))).toBeLessThan(first(between(holdStart.x, holdEnd.x)));
  expect(first(between(holdStart.x, holdEnd.x))).toBeLessThan(first(between(holdEnd.x, end.x)));
  // And at the end it is at rest again.
  await expect.poll(async () => Math.abs((await at()).x - start.x)).toBeLessThan(0.5);
});
