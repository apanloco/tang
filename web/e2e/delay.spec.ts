import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { node } from "./canvas";
import { test } from "./own";

// The built-in delay from the page, on the real program (a program of its own: an effect is put in the lane, which the shared one would keep). It puts out its echoes only, one time after
// the sound, and the time is the preset's: what the scope of its node shows, in the pixels' time of a real browser.

/** The loudest sample that a scope has on its screen: the value of its meter (nothing is silence). */
const peakOf = async (scope: Locator): Promise<number> => Number(await scope.getAttribute("aria-valuenow"));

/** The knobs of the delay's node, each at the value the page says. */
async function expectKnobs(delay: Locator, values: Record<string, string>): Promise<void> {
  for (const [name, value] of Object.entries(values)) await expect(delay.getByRole("slider", { name, exact: true })).toHaveAttribute("aria-valuenow", value);
}

/**
 * A key held, and how long it is before `scope` shows more than a hundredth: the milliseconds from before the key to the first sound on the scope, as the page sees them. The program is a moment away
 * and so is the telemetry, so that the time is more than the delay's and never less: it is counted from before the key is pressed, which the page is told of a moment later still.
 */
async function firstHeard(page: Page, scope: Locator, key: string): Promise<number> {
  await expect.poll(() => peakOf(scope), { message: "silent before the key" }).toBeLessThan(1e-4);
  const started = Date.now();
  await page.keyboard.down(key);
  let first = Number.POSITIVE_INFINITY;
  while (Date.now() - started < 3000) {
    if ((await peakOf(scope)) > 0.01) {
      first = Date.now() - started;
      break;
    }
    await page.waitForTimeout(10);
  }
  await page.keyboard.up(key);
  return first;
}

test("a delay from the page puts out the echoes and not the sound, one time after a note, and a preset moves the time: its knobs take the preset's values and its scope shows the first echo where the time says", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  // The tracer's lane is quiet on purpose (its volume is a tenth); it is put back to 1 so that the echoes are well over what a scope calls silence.
  await node(page, "Oscillator").getByRole("slider", { name: "Volume", exact: true }).dblclick();

  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Delay" }).click();
  const delay = node(page, "Delay");
  await expect(delay).toBeVisible();
  // Its parameters, with what the plugin starts at, and the mix that an effect is added at.
  await expectKnobs(delay, { time: "0.375", feedback: "0.45", tone: "0.7", pingpong: "0", mod_rate: "0.5", mod_depth: "0", Mix: "0.5" });

  // The scope of a node is of what comes out of the node, which is the delay blended with the sound that went in by the mix: to see the echoes alone the mix is all the way up.
  const mix = delay.getByRole("slider", { name: "Mix", exact: true });
  await mix.focus();
  await page.keyboard.press("End");
  await expect(mix).toHaveAttribute("aria-valuenow", "1");

  // Its presets, in the menu of its node.
  await page.getByRole("button", { name: "Load a preset into Delay" }).click();
  const menu = page.getByRole("menu", { name: "Load a preset into Delay" });
  await expect(menu.getByRole("menuitem")).toHaveText(["Slapback", "Tape Echo", "Ping-Pong", "Dub Echo", "Chorus", "Flanger", "Doubler"]);
  await menu.getByRole("menuitem", { name: "Slapback" }).click();
  await expectKnobs(delay, { time: "0.09", feedback: "0.12", tone: "0.85", pingpong: "0", mod_rate: "0.5", mod_depth: "0" });

  // The scope of the delay's node is of what comes out of the delay: silent until a key, and then silent for the time (a slapback's is 0.09 s) and not the dry sound at once.
  await delay.getByRole("button", { name: "Scope of Delay" }).click();
  const scope = delay.getByRole("meter", { name: "Output of Delay" });
  await expect(scope).toBeVisible();
  const slapback = await firstHeard(page, scope, "z");
  expect(slapback, "no sound at once: the delay puts out echoes, and not the sound that is put in").toBeGreaterThan(80);
  expect(slapback, "and the first echo is heard").toBeLessThan(1500);

  // A longer time is a later echo (a tape echo's is 0.38 s): the same note, a good deal later.
  await expect.poll(() => peakOf(scope), { message: "the slapback has died away", timeout: 10_000 }).toBeLessThan(1e-4);
  await page.getByRole("button", { name: "Load a preset into Delay" }).click();
  await menu.getByRole("menuitem", { name: "Tape Echo" }).click();
  await expectKnobs(delay, { time: "0.38", feedback: "0.5", tone: "0.45", pingpong: "0", mod_rate: "0.6", mod_depth: "1.5" });
  const tape = await firstHeard(page, scope, "z");
  expect(tape, "the tape echo comes after its time").toBeGreaterThan(340);
  expect(tape, "and well after the slapback").toBeGreaterThan(slapback + 150);
  expect(tape, "and is heard").toBeLessThan(2500);

  // The presets that are not echoes: a chorus is a short delay that its own wobble swings, so that its first sound comes a few milliseconds after the key and not at the time of an echo.
  await expect.poll(() => peakOf(scope), { message: "the tape echo has died away", timeout: 20_000 }).toBeLessThan(1e-4);
  await page.getByRole("button", { name: "Load a preset into Delay" }).click();
  await menu.getByRole("menuitem", { name: "Chorus" }).click();
  await expectKnobs(delay, { time: "0.02", feedback: "0", tone: "1", pingpong: "0", mod_rate: "0.8", mod_depth: "3.5" });
  const chorus = await firstHeard(page, scope, "z");
  expect(chorus, "the chorus is heard well before a slapback's echo").toBeLessThan(slapback - 30);
});
