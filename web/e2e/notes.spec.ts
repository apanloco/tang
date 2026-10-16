import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { angleOf, fractionOf, pointAt } from "../src/ui/knob-geometry";
import { node } from "./canvas";
import { test } from "./own";

// A velocity and a key on the real program (a program of its own, on the null audio backend: the file it saves is read, and the history is the program's): controls that follow the notes the lane
// plays, made from a knob with the knob's menu or from the lane's. The keys of the computer play at a velocity of 100, and the engine's control puts the notes it hears into what it drives.

async function open(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" })).toBeVisible();
}

/** The cell of the control called `name` in a node, and the button on it that modulates it. */
const cell = (plugin: Locator, name: string): Locator => plugin.locator(".tang-cell").filter({ has: plugin.page().locator(`[aria-label="${name}"]`) });
const modulate = (plugin: Locator, name: string): Locator => plugin.getByRole("button", { name: `Modulate ${name}`, exact: true });

async function choose(plugin: Locator, name: string, item: string): Promise<void> {
  await cell(plugin, name).hover();
  await modulate(plugin, name).click();
  await plugin.page().getByRole("menuitem", { name: item, exact: true }).click();
}

/** The knob of a parameter: what holds the dial, its ring and its dot. */
const knobOf = (plugin: Locator, name: string): Locator => plugin.locator(".tang-knob", { has: plugin.page().getByRole("slider", { name, exact: true }) });

/** Where the dot of a knob is, in the knob's own pixels. */
async function dotAt(dot: Locator): Promise<{ x: number; y: number }> {
  return { x: Number(await dot.getAttribute("cx")), y: Number(await dot.getAttribute("cy")) };
}

/** Where the dot of the filter's resonance knob (0.5 to 10, turned evenly) is when the engine has set the resonance to `resonance`. */
const dotFor = (resonance: number): { x: number; y: number } => pointAt(24, 18, angleOf(fractionOf(resonance, 0.5, 10, "linear")));

const save = async (page: Page, file: string): Promise<string> => {
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  return readFileSync(file, "utf8");
};

/** The modulators of the saved lane, as the text of each. */
const modulatorsOf = (saved: string): string[] => saved.split("[[instrument.modulator]]\n").slice(1).map((part) => part.split("[[instrument.effect]]")[0] ?? "");

/** A key of the computer pressed and let go: the note is played, and what the controls heard of it stays. */
async function play(page: Page, key: string): Promise<void> {
  await page.keyboard.down(key);
  await page.keyboard.up(key);
}

test("a velocity made from the resonance knob adds the velocity of the notes that are played to what the user set, which the knob's dot shows, and the file has it bound", async ({ page, own }) => {
  await open(page, own.url);
  const filter = node(page, "Filter");
  await choose(filter, "resonance", "New velocity");
  const velocity = node(page, "Velocity");
  await expect(velocity).toBeVisible();
  await expect(velocity).toContainText("velocity of the last note");
  await expect(page.getByRole("slider", { name: "Depth of Velocity → Filter: resonance" })).toHaveValue("0.5");
  await expect(page.getByRole("button", { name: "Undo modulate resonance", exact: true })).toBeVisible();

  // Before a note it is at rest and adds nothing: the resonance is where the user set it, 2.
  const dot = knobOf(filter, "resonance").locator(".tang-knob__dot");
  await expect(dot).toHaveAttribute("visibility", "visible");
  await expect.poll(async () => (await dotAt(dot)).x).toBeCloseTo(dotFor(2).x, 1);
  expect((await dotAt(dot)).y).toBeCloseTo(dotFor(2).y, 1);

  // A key of the computer plays at a velocity of 100, and 100 of 127 of the depth, half, of the range of the resonance, 9.5, is added to the 2.
  await play(page, "z");
  const played = 2 + 0.5 * 9.5 * (100 / 127);
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(dotFor(played).x, 1);
  expect((await dotAt(dot)).y).toBeCloseTo(dotFor(played).y, 1);
  // The knob is the user's all the while, and the note's end leaves the velocity where it was.
  await expect(filter.getByRole("slider", { name: "resonance", exact: true })).toHaveAttribute("aria-valuenow", "2");
  await page.waitForTimeout(400);
  expect((await dotAt(dot)).x).toBeCloseTo(dotFor(played).x, 1);

  // The file has a control that is bound to the velocity of the notes, with its cable.
  const parts = modulatorsOf(await save(page, own.file));
  expect(parts).toHaveLength(2);
  expect(parts[1]).toBe('type = "midi"\nmidi_velocity = true\n\n[[instrument.modulator.target]]\nparam = "resonance"\nslot = 1\ndepth = 0.5\n\n');
});

test("a key made from the resonance knob rests at middle C and adds the distance of the last note from it, five octaves being the whole of the depth, and the file has it bound", async ({ page, own }) => {
  await open(page, own.url);
  const filter = node(page, "Filter");
  await choose(filter, "resonance", "New key");
  const key = node(page, "Key");
  await expect(key).toBeVisible();
  await expect(key).toContainText("pitch of the last note");
  const dot = knobOf(filter, "resonance").locator(".tang-knob__dot");
  await expect(dot).toHaveAttribute("visibility", "visible");
  await expect.poll(async () => (await dotAt(dot)).x).toBeCloseTo(dotFor(2).x, 1);

  // The lower row of the keys of the computer starts at C3, 12 semitones under middle C (a fifth of the five octaves: the depth, half of 9.5, a fifth of it below the 2), and the upper row at C4.
  await play(page, "z");
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(dotFor(2 - (12 / 60) * 0.5 * 9.5).x, 1);
  expect((await dotAt(dot)).y).toBeCloseTo(dotFor(2 - (12 / 60) * 0.5 * 9.5).y, 1);
  // The `i` of the upper row is C5, an octave over middle C.
  await play(page, "i");
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(dotFor(2 + (12 / 60) * 0.5 * 9.5).x, 1);
  // And the `q`, which is middle C, adds nothing.
  await play(page, "q");
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(dotFor(2).x, 1);

  const parts = modulatorsOf(await save(page, own.file));
  expect(parts).toHaveLength(2);
  expect(parts[1]).toBe('type = "midi"\nmidi_key = true\n\n[[instrument.modulator.target]]\nparam = "resonance"\nslot = 1\ndepth = 0.5\n\n');
});

test("a velocity that the lane's menu adds has no knob, is a step of the history in the words of its own, and a second page sees it come and go", async ({ page, browser, own }) => {
  await open(page, own.url);
  await page.getByRole("button", { name: "Add a modulator to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Velocity", exact: true }).click();
  const velocity = node(page, "Velocity");
  await expect(velocity).toBeVisible();
  await expect(velocity.getByRole("slider")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Undo add a velocity control", exact: true })).toBeVisible();

  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect(node(other, "Velocity")).toBeVisible();

  // Undo takes it out, in both pages, and Redo makes it again, bound.
  await page.getByRole("button", { name: "Undo add a velocity control", exact: true }).click();
  await expect(velocity).toHaveCount(0);
  await expect(node(other, "Velocity")).toHaveCount(0);
  await page.getByRole("button", { name: "Redo add a velocity control", exact: true }).click();
  await expect(node(page, "Velocity")).toContainText("velocity of the last note");
  await expect(node(other, "Velocity")).toContainText("velocity of the last note");
  expect(modulatorsOf(await save(page, own.file)).filter((m) => m.includes("midi_velocity = true"))).toHaveLength(1);
  await other.close();
});

test("a key of the page's piano plays harder lower down, which a velocity control follows: near its top edge a note is soft, near its front edge hard", async ({ page, own }) => {
  await open(page, own.url);
  const filter = node(page, "Filter");
  await choose(filter, "resonance", "New velocity");
  await expect(node(page, "Velocity")).toBeVisible();
  const dot = knobOf(filter, "resonance").locator(".tang-knob__dot");
  await expect.poll(async () => (await dotAt(dot)).x).toBeCloseTo(dotFor(2).x, 1);

  // Middle C of the piano, pressed a tenth of the way down its height and then nine tenths of the way: velocities of 31 and 116 (20 and 107 steps over the key).
  const key = page.locator('[data-note="60"]');
  const box = await key.boundingBox();
  if (!box) throw new Error("the key is on the page");
  const pressAt = async (fraction: number): Promise<void> => {
    await page.mouse.move(box.x + box.width / 2, box.y + box.height * fraction);
    await page.mouse.down();
    await page.mouse.up();
  };
  const resonanceFor = (velocity: number): number => 2 + 0.5 * 9.5 * (velocity / 127);

  await pressAt(0.1);
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(dotFor(resonanceFor(31)).x, 0);
  await pressAt(0.9);
  await expect.poll(async () => (await dotAt(dot)).x, { timeout: 8000 }).toBeCloseTo(dotFor(resonanceFor(116)).x, 0);
  // (The two dots are about 25 pixels apart on the knob, and the dot is looked for to half a pixel: a velocity of two steps more or less would be seen. A hard note is not a soft one.)
  expect(Math.abs(dotFor(resonanceFor(116)).x - dotFor(resonanceFor(31)).x)).toBeGreaterThan(20);
});
