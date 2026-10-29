import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { showAll } from "./canvas";
import { test, testWithGroups } from "./own";

// Mute and solo from the page, on the real program (a program of its own, on the null audio backend: the master's meter is what the engine mixed, and the file it saves is read). A
// lane or a group that is muted is never heard, and while anything is soloed only what is soloed is; the notes go on while it is silent, so that a held key is heard again when the
// node is; a node that cannot be heard is dimmed; and a press is a step of the history.

/** The loudest the master's meter is for `ms`, in decibels (its floor is -60). */
async function loudest(page: Page, ms: number): Promise<number> {
  const meter = page.getByRole("meter", { name: "Master level" });
  const until = Date.now() + ms;
  let top = Number.NEGATIVE_INFINITY;
  while (Date.now() < until) {
    top = Math.max(top, Number(await meter.getAttribute("aria-valuenow")));
    await page.waitForTimeout(25);
  }
  return top;
}

const nodeOf = (page: Page, name: string): Locator => page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: new RegExp(`^${name}$`) }) });

/** What the node draws as the whole of itself: the box that is dimmed when it cannot be heard. */
const boxOf = (page: Page, name: string): Locator => nodeOf(page, name).locator(".tang-node").first();

async function open(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
}

const level = (page: Page) => async (): Promise<number> => Number(await page.getByRole("meter", { name: "Master level" }).getAttribute("aria-valuenow"));

/** The lanes of a saved file, as the text of each. */
const lanesOf = (saved: string): string[] => saved.split("[[instrument]]\n").slice(1);

test("a lane is muted and soloed from its node: what is muted, or not soloed while something is, is silent at the master and dimmed, a key that is held is heard again when its lane is, and the saved file has the flags", async ({ page, own }) => {
  // (A good many waits for silence, each the better part of a second.)
  test.slow();
  await open(page, own.url);
  const master = level(page);
  /** A key that is held, heard at the master, and let go of: the master is quiet again before the next thing is looked at. */
  const heard = async (key: string, what: string): Promise<void> => {
    await page.keyboard.down(key);
    await expect.poll(master, { message: `${what} is heard` }).toBeGreaterThan(-50);
    await page.keyboard.up(key);
    await expect.poll(master, { message: "the master is quiet again" }).toBeLessThan(-50);
  };
  /** A key that is held and not heard, for the better part of a second. */
  const silent = async (key: string, what: string): Promise<void> => {
    await page.keyboard.down(key);
    expect(await loudest(page, 800), `${what} is silent`).toBeLessThan(-50);
    await page.keyboard.up(key);
  };

  // Two lanes that take notes of their own: the tracer's takes up to B3 (the computer's lower row, Z being a C3), the one that is added takes C4 and up (the upper row, Q being a C4).
  await page.getByRole("combobox", { name: "Highest key of Oscillator" }).selectOption({ label: "B3" });
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menu", { name: "Add an instrument" }).getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(nodeOf(page, "Oscillator 2")).toBeVisible();
  await page.getByRole("combobox", { name: "Lowest key of Oscillator 2" }).selectOption({ label: "C4" });
  await expect(page.getByRole("combobox", { name: "Lowest key of Oscillator 2" }).locator("option:checked")).toHaveText("C4");
  // (A choice has the focus, and a key typed in it is not for the piano: the focus goes to the page.)
  await page.getByRole("status").click();
  // The canvas moved to show the lane that was added, and the first lane's buttons are under the bar: both are shown.
  await showAll(page);

  // Neither is muted or soloed to begin with, nothing is dimmed, and each lane's note is heard (the master's meter falls slowly: a note is let go of, and the master is waited for until it is quiet).
  for (const name of ["Mute Oscillator 1", "Solo Oscillator 1", "Mute Oscillator 2", "Solo Oscillator 2"]) await expect(page.getByRole("button", { name, exact: true })).toHaveAttribute("aria-pressed", "false");
  await expect(boxOf(page, "Oscillator 1")).toHaveCSS("opacity", "1");
  await expect(boxOf(page, "Oscillator 2")).toHaveCSS("opacity", "1");
  await heard("q", "the second lane");

  // The second lane is muted: its node is dimmed and says so, and its key is silent, while the first lane's is still heard.
  const mute2 = page.getByRole("button", { name: "Mute Oscillator 2", exact: true });
  await mute2.click();
  await expect(mute2).toHaveAttribute("aria-pressed", "true");
  await expect(boxOf(page, "Oscillator 2")).toHaveCSS("opacity", "0.55");
  await expect(boxOf(page, "Oscillator 1")).toHaveCSS("opacity", "1");
  await silent("q", "a muted lane");
  await heard("z", "the first lane, which was not touched");

  // The notes go on while a lane is muted: a key that is held falls silent with it, and is heard the moment the lane is not muted.
  await page.keyboard.down("q");
  await expect.poll(master, { message: "a held key of a muted lane is silent" }).toBeLessThan(-50);
  await mute2.click();
  await expect(mute2).toHaveAttribute("aria-pressed", "false");
  await expect(boxOf(page, "Oscillator 2")).toHaveCSS("opacity", "1");
  await expect.poll(master, { message: "the held key is heard when the lane is unmuted" }).toBeGreaterThan(-50);
  await page.keyboard.up("q");
  await expect.poll(master, { message: "the master is quiet again" }).toBeLessThan(-50);

  // The first lane is soloed: the second is not heard, and is dimmed, as the first is not.
  const solo1 = page.getByRole("button", { name: "Solo Oscillator 1", exact: true });
  await solo1.click();
  await expect(solo1).toHaveAttribute("aria-pressed", "true");
  await expect(boxOf(page, "Oscillator 2")).toHaveCSS("opacity", "0.55");
  await expect(boxOf(page, "Oscillator 1")).toHaveCSS("opacity", "1");
  await silent("q", "a lane that is not soloed while another is");
  await heard("z", "the soloed lane");

  // Both are soloed: both are heard. The second is muted as well: a mute wins over a solo.
  const solo2 = page.getByRole("button", { name: "Solo Oscillator 2", exact: true });
  await solo2.click();
  await expect(solo2).toHaveAttribute("aria-pressed", "true");
  await expect(boxOf(page, "Oscillator 2")).toHaveCSS("opacity", "1");
  await heard("q", "a lane that is soloed with another");
  await mute2.click();
  await expect(mute2).toHaveAttribute("aria-pressed", "true");
  await expect(boxOf(page, "Oscillator 2")).toHaveCSS("opacity", "0.55");
  await silent("q", "a lane that is muted and soloed");

  // What is saved has the flags of the second lane, and the first has only its solo: they are written when they are on.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const lanes = lanesOf(readFileSync(own.file, "utf8"));
  expect(lanes).toHaveLength(2);
  expect(lanes[0]).toContain("solo = true\n");
  expect(lanes[0]).not.toContain("mute");
  expect(lanes[1]).toContain("mute = true\n");
  expect(lanes[1]).toContain("solo = true\n");
});

test("a press of the mute is a step of the history, undone and done again from the bar, and a second page sees the lane go quiet and come back", async ({ page, browser, own }) => {
  await open(page, own.url);
  const master = level(page);
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");

  const mute = page.getByRole("button", { name: "Mute Oscillator", exact: true });
  await mute.click();
  await expect(page.getByRole("button", { name: "Undo mute the lane of Oscillator", exact: true })).toBeVisible();
  // The other page has the lane's button pressed and the node dimmed, and the first lane's note is silent for both.
  await expect(other.getByRole("button", { name: "Mute Oscillator", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(boxOf(other, "Oscillator")).toHaveCSS("opacity", "0.55");
  await page.keyboard.down("z");
  expect(await loudest(page, 800), "the only lane is muted").toBeLessThan(-50);

  // Undo unmutes it, in both pages, and the held key is heard; redo mutes it again.
  await page.getByRole("button", { name: "Undo mute the lane of Oscillator", exact: true }).click();
  await expect(mute).toHaveAttribute("aria-pressed", "false");
  await expect(other.getByRole("button", { name: "Mute Oscillator", exact: true })).toHaveAttribute("aria-pressed", "false");
  await expect(boxOf(other, "Oscillator")).toHaveCSS("opacity", "1");
  await expect.poll(master, { message: "the lane is heard again" }).toBeGreaterThan(-50);
  await page.getByRole("button", { name: "Redo mute the lane of Oscillator", exact: true }).click();
  await expect(mute).toHaveAttribute("aria-pressed", "true");
  await expect.poll(master, { message: "the lane is silent again" }).toBeLessThan(-50);
  await page.keyboard.up("z");
  await other.close();
});

// (A program started on the tracer's file with a group: the page has no way to make one.)
const grouped = testWithGroups(1);

grouped("a group is muted from its node: the lanes in it are silent at the master and dimmed with it, and the group's flag is in the saved file", async ({ page, own }) => {
  await open(page, own.url);
  const master = level(page);

  // The tracer's lane in the group that the file has, chosen on the lane.
  await page.getByRole("combobox", { name: "Group of Oscillator" }).selectOption({ label: "Group 1" });
  await expect(nodeOf(page, "Group 1")).toBeVisible();
  await page.getByRole("status").click();
  await page.keyboard.down("z");
  await expect.poll(master, { message: "the lane is heard through its group" }).toBeGreaterThan(-50);

  // The group is muted: the bus is cut, so the lane in it is silent, and both nodes are dimmed; the group's button is pressed and the lane's is not (a lane's flags are its own).
  const mute = page.getByRole("button", { name: "Mute Group 1", exact: true });
  await mute.click();
  await expect(mute).toHaveAttribute("aria-pressed", "true");
  await expect.poll(master, { message: "a muted group is silent" }).toBeLessThan(-50);
  await expect(boxOf(page, "Group 1")).toHaveCSS("opacity", "0.55");
  await expect(boxOf(page, "Oscillator")).toHaveCSS("opacity", "0.55");
  await expect(page.getByRole("button", { name: "Mute Oscillator", exact: true })).toHaveAttribute("aria-pressed", "false");

  // Soloed as well, it is still muted; unmuted, the held key is heard again.
  await page.getByRole("button", { name: "Solo Group 1", exact: true }).click();
  await expect(page.getByRole("button", { name: "Solo Group 1", exact: true })).toHaveAttribute("aria-pressed", "true");
  expect(await loudest(page, 800), "a mute wins over a solo, for a group too").toBeLessThan(-50);
  await mute.click();
  await expect(mute).toHaveAttribute("aria-pressed", "false");
  await expect.poll(master, { message: "the group is heard again" }).toBeGreaterThan(-50);
  await expect(boxOf(page, "Group 1")).toHaveCSS("opacity", "1");
  await expect(boxOf(page, "Oscillator")).toHaveCSS("opacity", "1");
  await page.keyboard.up("z");

  // The file has the group's solo, in the group's table (which follows the lanes'), and nothing in the lane's.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const [lane, group] = readFileSync(own.file, "utf8").split("[[group]]\n");
  expect(group).toContain("solo = true\n");
  expect(group).not.toContain("mute");
  expect(lane).toContain("group = 0\n");
  expect(lane).not.toContain("solo");
  expect(lane).not.toContain("mute");
});
