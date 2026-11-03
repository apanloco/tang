import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { NODE_WIDTH, STEPS_NODE_HEIGHT } from "../src/app/patch/toFlow";
import { node, still } from "./canvas";
import { test } from "./own";

// A step sequencer on the real program (a program of its own, on the null audio backend: the file it saves is read, and the history is the program's): added from the
// lane's menu, it is a node with four controls and sixteen bars, a playhead on the step that the engine says it is at, a stroke across the bars that is one step of the
// history, and a note that starts it again at its first step.

async function open(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" })).toBeVisible();
}

/** A step sequencer added to the oscillator's lane, and its node. */
async function addSequencer(page: Page): Promise<Locator> {
  await page.getByRole("button", { name: "Add a modulator to Oscillator", exact: true }).click();
  await page.getByRole("menuitem", { name: "Step sequencer", exact: true }).click();
  const sequencer = node(page, "Steps");
  await expect(sequencer).toBeVisible();
  // (The canvas pans to a node that was added, which takes a quarter of a second: a pointer that goes where the bars were a moment before misses them.)
  await still(sequencer);
  return sequencer;
}

const barsOf = (sequencer: Locator): Locator => sequencer.getByRole("group", { name: "Steps of Steps" });

/** What each bar says its value is, from the first. */
async function valuesOf(sequencer: Locator): Promise<number[]> {
  return (await barsOf(sequencer).getByRole("slider").evaluateAll((bars) => bars.map((bar) => Number(bar.getAttribute("aria-valuenow")))));
}

/** The number (from 1) of the bar that the playhead is on, or none. */
async function playing(sequencer: Locator): Promise<number | undefined> {
  const index = await barsOf(sequencer).getByRole("slider").evaluateAll((bars) => bars.findIndex((bar) => bar.hasAttribute("data-playing")));
  return index < 0 ? undefined : index + 1;
}

const save = async (page: Page, file: string): Promise<string> => {
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  return readFileSync(file, "utf8");
};

/** The steps that a saved file has for its step sequencer. */
function savedSteps(saved: string): number[] {
  const list = /type = "steps"[\s\S]*?steps = \[([^\]]*)\]/.exec(saved)?.[1];
  if (list === undefined) throw new Error(`the file has no steps:\n${saved}`);
  return list.split(",").map((value) => Number(value.trim()));
}

const STAIRCASE = Array.from({ length: 16 }, (_, step) => step / 8 - 1);

test("a step sequencer that is added has sixteen bars as a staircase, a node as tall as the layout assumes, and a playhead that goes through the steps", async ({ page, own }) => {
  await open(page, own.url);
  const sequencer = await addSequencer(page);
  expect(await valuesOf(sequencer)).toEqual(STAIRCASE);
  await expect(sequencer.getByRole("slider", { name: "Rate", exact: true })).toHaveAttribute("aria-valuenow", "8");
  await expect(sequencer.getByRole("slider", { name: "Length", exact: true })).toHaveAttribute("aria-valuenow", "16");
  await expect(sequencer.getByRole("slider", { name: "Glide", exact: true })).toHaveAttribute("aria-valuenow", "0");
  await expect(sequencer.getByRole("combobox", { name: "Retrigger" })).toHaveValue("1");

  // Its node is as tall as the layout allowed for (and not much taller than it needs): the canvas is zoomed to fit the window, so the scale is how much of the layout's width it is.
  const box = await sequencer.boundingBox();
  const scale = (box?.width ?? NODE_WIDTH) / NODE_WIDTH;
  expect((box?.height ?? 0) / scale).toBeLessThanOrEqual(STEPS_NODE_HEIGHT);
  expect((box?.height ?? 0) / scale).toBeGreaterThan(STEPS_NODE_HEIGHT - 12);

  // The playhead is on one bar, and goes through the steps at eight a second: a couple of seconds show it on all of the sixteen... or most of them (a frame is a thirtieth of a second).
  const seen = new Set<number>();
  const until = Date.now() + 3000;
  while (Date.now() < until) {
    const now = await playing(sequencer);
    if (now !== undefined) seen.add(now);
    await page.waitForTimeout(40);
  }
  expect(seen.size, `the bars it was seen on: ${[...seen].join(", ")}`).toBeGreaterThanOrEqual(12);
});

test("a stroke across the bars paints them, is one step of the history that Undo puts back and Redo paints again, and the file has the steps", async ({ page, own }) => {
  await open(page, own.url);
  const sequencer = await addSequencer(page);
  const bars = barsOf(sequencer);
  const box = await bars.boundingBox();
  if (!box) throw new Error("the bars have no box");
  const bar = box.width / 16;
  // From near the top of the third bar to near the bottom of the eighth, with the pointer moving through the bars between.
  await page.mouse.move(box.x + 2.5 * bar, box.y + 2);
  await page.mouse.down();
  await page.mouse.move(box.x + 7.5 * bar, box.y + box.height - 2, { steps: 12 });
  await page.mouse.up();

  const painted = await valuesOf(sequencer);
  // (A bar has the value of the last place of the pointer in it: the pointer is on its way down when it leaves the third bar, and stops at the bottom of the eighth.)
  expect(painted[2], "the third bar is high").toBeGreaterThan(0.5);
  expect(painted[7], "the eighth is near the bottom").toBeLessThan(-0.85);
  // The bars between are along the line from one to the other: each lower than the one before.
  for (let step = 3; step < 7; step += 1) expect(painted[step]).toBeLessThan(painted[step - 1] ?? 0);
  // The others are as they were.
  expect([...painted.slice(0, 2), ...painted.slice(8)]).toEqual([...STAIRCASE.slice(0, 2), ...STAIRCASE.slice(8)]);

  // The stroke is one step of the history, which is words of the button of Undo; one Undo puts all of it back.
  await expect(page.getByRole("button", { name: "Undo set steps", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Undo set steps", exact: true }).click();
  await expect.poll(() => valuesOf(sequencer)).toEqual(STAIRCASE);
  await expect(page.getByRole("button", { name: "Undo add a step sequencer", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Redo set steps", exact: true }).click();
  await expect.poll(() => valuesOf(sequencer)).toEqual(painted);

  // The file has the steps as they are painted, and a step sequencer as a modulator of the lane.
  const saved = await save(page, own.file);
  expect(saved).toContain('type = "steps"');
  expect(savedSteps(saved)).toEqual(painted);
});

test("a note starts the step sequencer again at its first step when it is set to, and the choice is in the file", async ({ page, own }) => {
  await open(page, own.url);
  const sequencer = await addSequencer(page);
  // A rate at the bottom of its knob (a step every hundred seconds): the playhead stands where it is, at a step that is not the first, when it is waited for.
  await expect.poll(async () => (await playing(sequencer)) ?? 0, { timeout: 5000 }).toBeGreaterThanOrEqual(4);
  await sequencer.getByRole("slider", { name: "Rate", exact: true }).focus();
  await page.keyboard.press("Home");
  await expect(sequencer.getByRole("slider", { name: "Rate", exact: true })).toHaveAttribute("aria-valuenow", "0.01");
  await page.waitForTimeout(400);
  const stands = await playing(sequencer);
  expect(stands).toBeGreaterThanOrEqual(4);
  await page.waitForTimeout(300);
  expect(await playing(sequencer), "it stands").toBe(stands);

  // A note, with the choice on (it starts on), puts it on the first step, where it stays.
  await page.keyboard.down("z");
  await expect.poll(() => playing(sequencer), { message: "the playhead is on the first step at the note" }).toBe(1);
  await page.keyboard.up("z");

  // The choice of off is the sequencer's, and the file's.
  await sequencer.getByRole("combobox", { name: "Retrigger" }).selectOption({ label: "off" });
  await expect(sequencer.getByRole("combobox", { name: "Retrigger" })).toHaveValue("0");
  const saved = await save(page, own.file);
  expect(saved).toMatch(/type = "steps"[\s\S]*retrigger = false/);
});
