import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { node, open, showAll } from "./canvas";
import { test } from "./own";
import { loudest, masterLevel } from "./sound";

// The master's limiter, volume and readout from the page (docs/web-ui.md step 102; the user's "we often reach the clip-lvl and its hard to control" and, of the default, "yes limiter default on"): the master's
// card has its volume, the ceiling of the limiter that holds the output mix under it, a button that puts the limiter in the sound or takes it out (it is in the sound from the start, which a session that says
// nothing of it has too) and how far the limiter has brought the sound down. The first test is against the mock engine in the page (what the card draws and what the history says), the second against the real
// program on the null audio backend (what is heard at the master, whether it clips, what the file keeps and what a page that is opened again draws).

const master = (page: Page): Locator => node(page, "Master");
const limiter = (page: Page): Locator => master(page).getByRole("button", { name: "Limiter", exact: true });
const volume = (page: Page): Locator => master(page).getByRole("slider", { name: "Master volume", exact: true });
const ceiling = (page: Page): Locator => master(page).getByRole("slider", { name: "Ceiling", exact: true });
const clip = (page: Page): Locator => master(page).getByRole("img", { name: "Clip indicator" });

/** How far the limiter is bringing the sound down now, in decibels: 0 for none, and for a bar that has no value yet. */
const reduction = async (page: Page): Promise<number> => Number((await master(page).getByRole("meter", { name: "Limiter reduction", exact: true }).getAttribute("aria-valuenow")) ?? "0");

mock("the master has a volume, a limiter that is in the sound from the start with its ceiling, and a readout of how far it brings the sound down: a note that is held is held at the ceiling, taking the limiter out lets it through, and each press is a step of the history", async ({ page }) => {
  await open(page);
  await showAll(page);

  // What the session has: the limiter is in the sound (the default), the ceiling is -1 dB and the volume is 1; the master's card is the one that has them.
  await expect(limiter(page)).toHaveAttribute("aria-pressed", "true");
  await expect(limiter(page)).toHaveText("On");
  await expect(volume(page)).toHaveAttribute("aria-valuenow", "1");
  await expect(ceiling(page)).toHaveAttribute("aria-valuenow", "-1");
  await expect(ceiling(page)).toHaveAttribute("aria-valuetext", "-1.0 dB");
  await expect(page.getByRole("button", { name: "Limiter", exact: true })).toHaveCount(1);
  await expect(page.getByRole("slider", { name: "Ceiling", exact: true })).toHaveCount(1);
  await expect(page.getByRole("meter", { name: "Limiter reduction", exact: true })).toHaveCount(1);
  // (Nothing drives the output mix's volume: it has no port where a cable ends, as a lane's volume has.)
  await expect(master(page).locator('.react-flow__handle[data-handleid="volume"]')).toHaveCount(0);

  // A note that is held is far under a ceiling of -1 dB: the meter reads what it is, and the limiter has nothing to do.
  await page.keyboard.down("z");
  await expect.poll(() => masterLevel(page), { message: "the note is heard" }).toBeGreaterThan(-20);
  await expect.poll(() => reduction(page), { message: "nothing is brought down" }).toBe(0);
  await expect(master(page).locator(".tang-reduction__value")).toHaveText("0 dB");
  await page.keyboard.up("z");
  await expect.poll(() => masterLevel(page), { message: "the master is quiet again" }).toBeLessThan(-50);

  // The ceiling all the way down (-24 dB, a step of the history in its own words): the same note is held at the ceiling, which the meter reads, and the readout says how far the mix was brought down.
  await ceiling(page).focus();
  await page.keyboard.press("Home");
  await expect(ceiling(page)).toHaveAttribute("aria-valuenow", "-24");
  await expect(ceiling(page)).toHaveAttribute("aria-valuetext", "-24.0 dB");
  await expect(page.getByRole("button", { name: /^Undo/ })).toHaveAccessibleName("Undo set the limiter's ceiling");
  await page.keyboard.down("z");
  await expect.poll(() => masterLevel(page), { message: "the note is held at the ceiling" }).toBeCloseTo(-24, 0);
  await expect.poll(() => reduction(page), { message: "the limiter says how far it brings the sound down" }).toBeGreaterThan(10);
  await expect(master(page).locator(".tang-reduction__value")).toHaveText(/^-\d+\.\d dB$/);

  // The limiter taken out of the sound: the note is let through as it is and nothing is brought down (the readout falls to 0).
  await limiter(page).click();
  await expect(limiter(page)).toHaveAttribute("aria-pressed", "false");
  await expect(limiter(page)).toHaveText("Off");
  await expect.poll(() => masterLevel(page), { message: "the note is let through" }).toBeGreaterThan(-20);
  await expect.poll(() => reduction(page), { message: "nothing is brought down" }).toBe(0);
  await page.keyboard.up("z");
  await expect(page.getByRole("button", { name: /^Undo/ })).toHaveAccessibleName("Undo turn the limiter off");

  // Undo puts the limiter back in the sound, Redo takes it out again.
  await page.getByRole("button", { name: /^Undo/ }).click();
  await expect(limiter(page)).toHaveAttribute("aria-pressed", "true");
  await page.getByRole("button", { name: /^Redo/ }).click();
  await expect(limiter(page)).toHaveAttribute("aria-pressed", "false");

  // The volume turns the mix: all the way down is silence at the meter (the limiter being out of the sound, it is the volume and nothing else), and a double click puts it back to 1.
  await page.keyboard.down("z");
  await volume(page).focus();
  await page.keyboard.press("Home");
  await expect(volume(page)).toHaveAttribute("aria-valuenow", "0");
  await expect(page.getByRole("button", { name: /^Undo/ })).toHaveAccessibleName("Undo set the master volume");
  await expect.poll(() => masterLevel(page), { message: "a volume of 0 is silence" }).toBeLessThan(-50);
  await volume(page).dblclick();
  await expect(volume(page)).toHaveAttribute("aria-valuenow", "1");
  await expect.poll(() => masterLevel(page), { message: "the note is heard again" }).toBeGreaterThan(-20);
  await page.keyboard.up("z");
});

test("the limiter on the real program holds a sound that would clip under its ceiling and says how far it brings it down, the ceiling and the volume are what is heard, taking the limiter out lets the sound clip, the file keeps what is not the default, and a page that is opened again draws what the program has", async ({ page, own }) => {
  // (A good many waits for the meter to fall, each the better part of a second.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await showAll(page);
  const oscillator = node(page, "Oscillator");

  // A chord of three keys (C, E and G of the computer's lower row): one note of the tracer is not far over full scale, and three of them are.
  const keys = ["z", "c", "b"];
  const press = async (): Promise<void> => {
    for (const key of keys) await page.keyboard.down(key);
  };
  const release = async (): Promise<void> => {
    for (const key of keys) await page.keyboard.up(key);
  };
  /**
   * A chord that is held for the better part of a second: the loudest the master's meter is while it is, and the most that the limiter brings the sound down by while it is. The readout is read all the while, as the meter is, and
   * not once at the end: an LFO opens and shuts the tracer's filter in two seconds, so for about half a second of every turn the chord is 10 dB under the ceiling, the limiter has nothing to do and the readout, which falls
   * 40 dB a second, is at 0 by the end of that stretch. One reading at the end of the window found it there for a quarter of the starting phases, and a runner that is slower than a person's computer found it there every time.
   */
  const held = async (): Promise<{ level: number; down: number }> => {
    await press();
    let level = Number.NEGATIVE_INFINITY;
    let down = 0;
    const until = Date.now() + 800;
    while (Date.now() < until) {
      level = Math.max(level, await masterLevel(page));
      down = Math.max(down, await reduction(page));
      await page.waitForTimeout(25);
    }
    await release();
    await expect.poll(() => masterLevel(page), { message: "the master is quiet again", timeout: 15_000 }).toBeLessThan(-50);
    return { level, down };
  };
  const saved = async (): Promise<string> => {
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };

  // A session that says nothing of its master has the limiter in the sound, at a ceiling of -1 dB and a volume of 1 (the tracer's file has no `[master]` table); at rest it brings nothing down.
  await expect(limiter(page)).toHaveAttribute("aria-pressed", "true");
  await expect(ceiling(page)).toHaveAttribute("aria-valuenow", "-1");
  await expect(volume(page)).toHaveAttribute("aria-valuenow", "1");
  await expect.poll(() => reduction(page), { message: "nothing is brought down at rest" }).toBe(0);
  expect(readFileSync(own.file, "utf8")).not.toContain("[master]");

  // The tracer is quiet on purpose; the volume of its lane at 4 and its oscillator's at the top make a chord of it louder than full scale (as the part with the limiter out of the sound shows).
  await oscillator.getByRole("slider", { name: "Volume", exact: true }).focus();
  await page.keyboard.press("End");
  await expect(oscillator.getByRole("slider", { name: "Volume", exact: true })).toHaveAttribute("aria-valuenow", "4");
  await oscillator.getByRole("slider", { name: "volume", exact: true }).focus();
  await page.keyboard.press("End");

  // With the limiter in the sound the hot sound is held at the ceiling (-1 dB: the meter reads it there, never above it), the limiter says that it brings it down by a good deal and the CLIP light stays out.
  const limited = await held();
  expect(limited.level, "held at the ceiling, and not over it").toBeLessThanOrEqual(-0.95);
  expect(limited.level, "held at the ceiling, not just quiet").toBeGreaterThan(-3);
  expect(limited.down, "the limiter says it brings the sound down").toBeGreaterThan(3);
  await expect(clip(page)).not.toHaveAttribute("data-lit");

  // Taken out of the sound, the same notes clip: the meter reads over full scale, the CLIP light comes on and nothing is brought down.
  await limiter(page).click();
  await expect(limiter(page)).toHaveAttribute("aria-pressed", "false");
  await press();
  await expect.poll(() => masterLevel(page), { message: "the sound clips with the limiter out of it", timeout: 15_000 }).toBeGreaterThan(0);
  await expect(clip(page)).toHaveAttribute("data-lit");
  await release();
  await expect.poll(() => masterLevel(page), { message: "the master is quiet again", timeout: 15_000 }).toBeLessThan(-50);
  await expect.poll(() => reduction(page), { message: "nothing is brought down with the limiter out of the sound" }).toBe(0);

  // Put back in the sound, it holds it again (the light goes out two seconds after the last clip).
  await limiter(page).click();
  await expect(limiter(page)).toHaveAttribute("aria-pressed", "true");
  expect((await held()).level, "held at the ceiling again").toBeLessThanOrEqual(-0.95);
  await expect(clip(page)).not.toHaveAttribute("data-lit", { timeout: 15_000 });

  // The ceiling is where the sound is held: all the way down is -24 dB, and Undo gives -1 back.
  await ceiling(page).focus();
  await page.keyboard.press("Home");
  await expect(ceiling(page)).toHaveAttribute("aria-valuenow", "-24");
  const low = await held();
  expect(low.level, "held at a ceiling of -24 dB").toBeLessThanOrEqual(-23.5);
  expect(low.level, "held at a ceiling of -24 dB, not just quiet").toBeGreaterThan(-26);
  expect(low.down, "the limiter brings it down further").toBeGreaterThan(limited.down);
  await expect(page.getByRole("button", { name: /^Undo/ })).toHaveAccessibleName("Undo set the limiter's ceiling");
  await page.getByRole("button", { name: /^Undo/ }).click();
  await expect(ceiling(page)).toHaveAttribute("aria-valuenow", "-1");
  expect((await held()).level, "held at -1 dB again").toBeGreaterThan(-3);

  // The volume is in front of the limiter: all the way down is silence, whatever the ceiling is, and a double click gives it back.
  await volume(page).focus();
  await page.keyboard.press("Home");
  await expect(volume(page)).toHaveAttribute("aria-valuenow", "0");
  await press();
  expect(await loudest(page, 800), "a volume of 0 is silence").toBeLessThan(-50);
  await release();
  await volume(page).dblclick();
  await expect(volume(page)).toHaveAttribute("aria-valuenow", "1");
  expect((await held()).level, "heard again").toBeGreaterThan(-3);

  // The file keeps what is not the default, and nothing else: the ceiling and the volume are at -1 and 1 again and the limiter is in the sound, so there is no table; with the ceiling at -24 and the limiter out
  // of the sound there is one.
  expect(await saved()).not.toContain("[master]");
  await ceiling(page).focus();
  await page.keyboard.press("Home");
  await expect(ceiling(page)).toHaveAttribute("aria-valuenow", "-24");
  await limiter(page).click();
  await expect(limiter(page)).toHaveAttribute("aria-pressed", "false");
  expect(await saved()).toContain("[master]\nlimiter = false\nceiling = -24.0\n");

  // A page that is opened again draws what the program has.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(limiter(page)).toHaveAttribute("aria-pressed", "false");
  await expect(ceiling(page)).toHaveAttribute("aria-valuenow", "-24");
  await expect(volume(page)).toHaveAttribute("aria-valuenow", "1");
});
