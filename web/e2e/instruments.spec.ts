import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";

import { node, sends, showAll } from "./canvas";
import { test } from "./own";
import { heardAt, loudest, silentAt } from "./sound";

// Instruments from the page, on the real program (a program of its own: a lane cannot be taken out again).

test("an instrument is put in a lane from its menu, another is added in a lane of its own from the bar and heard, and the saved file has both lanes", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const lane = (name: string) => page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: new RegExp(`^${name}$`) }) });
  const meter = page.getByRole("meter", { name: "Master level" });
  const level = async (): Promise<number> => Number(await meter.getAttribute("aria-valuenow"));

  // The tracer's lane has its saw, and a new instrument put in it by its menu starts as a sine: the lane keeps its filter and its LFO.
  const waveform = lane("Oscillator").getByRole("combobox", { name: "waveform" });
  await expect(waveform).toHaveValue("3");
  await page.getByRole("button", { name: "Change the instrument of Oscillator" }).click();
  const instruments = page.getByRole("menu", { name: "Change the instrument of Oscillator" }).getByRole("group", { name: "Built-in" });
  await expect(instruments.getByRole("menuitem")).toHaveText(["Oscillator"]);
  await instruments.getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(waveform).toHaveValue("0");
  await expect(lane("Filter")).toBeVisible();
  await expect(lane("LFO")).toBeVisible();
  await expect(page.getByRole("button", { name: "Change the instrument of Oscillator" })).toBeFocused();
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // The lane is quiet on purpose (its volume is 0.1, which the lane keeps): a note, and the level it makes.
  await page.keyboard.down("z");
  await expect.poll(level, { message: "the note is heard" }).toBeGreaterThan(-50);
  const before = await loudest(page, 600);
  await page.keyboard.up("z");
  await expect.poll(level, { message: "the master is quiet again" }).toBeLessThan(-50);

  // The bar's menu has what the program found installed: the built-ins are always there.
  await page.getByRole("button", { name: "Add an instrument" }).click();
  const builtins = page.getByRole("menu", { name: "Add an instrument" }).getByRole("group", { name: "Built-in" });
  await expect(builtins.getByRole("menuitem")).toHaveText(["Oscillator"]);
  await builtins.getByRole("menuitem", { name: "Oscillator" }).click();

  // A lane of its own is on the canvas, called by a number now that there are two, with the buttons of a lane. It was added below
  // the first, which fills the screen: the canvas has moved to show it.
  await expect(lane("Oscillator 2")).toBeVisible();
  // (All of it is in the window: the page is asked for the box, since the ratio of the viewport is a 32-bit number that is a hair short of 1 when the canvas is zoomed to a fraction.)
  await expect
    .poll(async () => {
      const box = await lane("Oscillator 2").boundingBox();
      const window = page.viewportSize();
      return box !== null && window !== null && box.x >= 0 && box.y >= 0 && box.x + box.width <= window.width && box.y + box.height <= window.height;
    })
    .toBe(true);
  await expect(page.getByRole("button", { name: "Change the instrument of Oscillator 2" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Add an effect to Oscillator 2" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Add an instrument" })).toBeFocused();

  // The next note is heard through it too: the new oscillator is at its own full volume, far louder than the first lane.
  await page.keyboard.down("z");
  await expect.poll(level, { message: "the note through both lanes" }).toBeGreaterThan(before + 10);
  await page.keyboard.up("z");

  // What is saved has both lanes, and the first has none of the saw it had.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved.match(/\[\[instrument\]\]/g)).toHaveLength(2);
  expect(saved.match(/plugin = "builtin:osc"/g)).toHaveLength(2);
  expect(saved).not.toContain("waveform = 3");
  expect(saved).toContain('plugin = "builtin:filter"');
  expect(saved).toContain("volume = 0.1");
});

test("the instrument of a lane is taken out alone and its filter stays and runs: what another lane sends into it by a cable is heard through it, the file keeps the lane with no instrument, and Undo puts it back", async ({ page, own }) => {
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const saved = async (): Promise<string> => {
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };
  await heardAt(page, "the lane");

  // A second lane, which is heard through the cables that start at it only: its link to the master is taken out. The first lane is heard still.
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menu", { name: "Add an instrument" }).getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  const link = page.getByRole("button", { name: "Remove cable Oscillator 2 → Master", exact: true });
  await link.click();
  await expect(link).toHaveCount(0);
  await heardAt(page, "the first lane");

  // The instrument of the first lane is taken out alone: the lane is left with its filter, and the LFO that hears it (the session's own: it is not in the lane) stays too, and nothing is heard (the second lane has no way out).
  // (The canvas moved to show the lane that was added, and the first is above it: the view is fitted to all of it.)
  await showAll(page);
  await page.getByRole("button", { name: "Remove the lane of Oscillator 1" }).click();
  await page.getByRole("menuitem", { name: /^Remove Oscillator 1 only: its 1 effect stays in the lane$/ }).click();
  await expect(node(page, "\\(empty\\)")).toHaveCount(0);
  await expect(node(page, "Filter")).toBeVisible();
  await expect(node(page, "LFO")).toBeVisible();
  await silentAt(page, "a session whose lane that has a way out has no instrument");
  // The file has the lane, with its filter and no instrument, and the other lane with its; and the LFO, a table of its own after the lanes, that hears the lane that has no instrument.
  const file = await saved();
  const lanes = file.split("[[instrument]]").slice(1);
  expect(lanes).toHaveLength(2);
  expect(lanes[0]).not.toContain("builtin:osc");
  expect(lanes[0]).toContain('plugin = "builtin:filter"');
  expect(lanes[0]).not.toContain("modulator");
  expect(file).toMatch(/\[\[modulator\]\]\n[^[]*hears = "instrument\[0\]"/);
  expect(lanes[1]).toContain('plugin = "builtin:osc"');

  // The second lane sends its sound into the filter that stands by itself (the other lane is called what it is now): the filter runs, and it is heard through it.
  await showAll(page);
  await page.getByRole("button", { name: "Send audio from Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Filter" }).click();
  await expect(sends(page)).toHaveCount(1);
  await heardAt(page, "what is sent into the filter of a lane that has no instrument");

  // The history has both: Undo takes the cable out (silent), and puts the instrument back (the first lane is heard again).
  await page.getByRole("button", { name: "Undo draw an audio cable" }).click();
  await expect(sends(page)).toHaveCount(0);
  await silentAt(page, "the filter of a lane that has no instrument and is sent nothing");
  await page.getByRole("button", { name: "Undo take Oscillator out of its lane" }).click();
  await expect(node(page, "Oscillator 1")).toBeVisible();
  await heardAt(page, "the first lane, with its instrument put back by Undo");
  expect((await saved()).split("[[instrument]]")[1]).toContain('plugin = "builtin:osc"');
});
