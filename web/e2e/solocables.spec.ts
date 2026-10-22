import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { audioIn, audioOut, drag, node, open, sends, showAll } from "./canvas";
import { test } from "./own";

// Mute and solo for what audio cables carry (docs/patch-graph.md §11.6, P3c-2b; D78 of docs/web-ui.md): a lane that is not soloed while another is can still be heard for what a cable from something
// that is heard sends into its effects, and then it puts none of its own sound into them. The first test is against the mock engine in the page (what the canvas dims), the last against the real
// program (what is heard at the master).

/** What the node draws as the whole of itself: the box that is dimmed when it cannot be heard. */
const boxOf = (page: Page, name: string): Locator => node(page, name).locator(".tang-node").first();

/** The nodes `names` are drawn dimmed, as what cannot be heard, or at full strength: one entry for each, in the order of the names. */
async function expectOpacities(page: Page, names: readonly string[], opacities: readonly ("0.55" | "1")[]): Promise<void> {
  for (const [place, name] of names.entries()) await expect(boxOf(page, name), name).toHaveCSS("opacity", opacities[place] ?? "?");
}

/** A second lane from the bar's menu, once the canvas has moved to show it. */
async function addLane(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menu", { name: "Add an instrument" }).getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  await showAll(page);
}

const solo = (page: Page, name: string): Locator => page.getByRole("button", { name: `Solo ${name}`, exact: true });
const mute = (page: Page, name: string): Locator => page.getByRole("button", { name: `Mute ${name}`, exact: true });

mock("a lane that is sent to by a lane that is soloed is heard through its filter and not for itself: its instrument is dimmed and its filter is not, until the cable has no level, the sender is muted or another lane is soloed", async ({ page }) => {
  await open(page);
  await addLane(page);
  await drag(page, audioOut(node(page, "Oscillator 2")), audioIn(node(page, "Filter")));
  await expect(sends(page)).toHaveCount(1);
  const level = page.getByRole("slider", { name: "Level of Oscillator 2 → Filter", exact: true });
  const names = ["Oscillator 1", "Filter", "Oscillator 2"] as const;
  // Nothing is soloed: everything is heard.
  await expectOpacities(page, names, ["1", "1", "1"]);

  // The second lane is soloed: it is heard, and so is the first lane's filter, for what the cable sends into it; the first lane's instrument is not.
  await solo(page, "Oscillator 2").click();
  await expect(solo(page, "Oscillator 2")).toHaveAttribute("aria-pressed", "true");
  await expectOpacities(page, names, ["0.55", "1", "1"]);

  // A cable that has no level carries nothing: the filter is not heard, and is when the cable has its level again.
  await level.fill("0");
  await expectOpacities(page, names, ["0.55", "0.55", "1"]);
  await level.fill("1");
  await expectOpacities(page, names, ["0.55", "1", "1"]);

  // A lane that is muted sends nothing, though it is soloed: nothing is heard, and the filter with it.
  await mute(page, "Oscillator 2").click();
  await expectOpacities(page, names, ["0.55", "0.55", "0.55"]);
  await mute(page, "Oscillator 2").click();
  await expectOpacities(page, names, ["0.55", "1", "1"]);

  // The first lane is soloed too: it is heard for what it is, and so is the second, which has its own solo.
  await solo(page, "Oscillator 1").click();
  await expectOpacities(page, names, ["1", "1", "1"]);
  // The second lane's solo is taken off: the first is the one that is heard, with its filter, and the second is not, though its cable is there.
  await solo(page, "Oscillator 2").click();
  await expect(solo(page, "Oscillator 2")).toHaveAttribute("aria-pressed", "false");
  await expectOpacities(page, names, ["1", "1", "0.55"]);
});

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

test("a lane that is soloed and has no way out is heard through the cable that it sends into the filter of a lane that is not, whose own notes are not heard; and not without the cable, or when either lane is muted", async ({ page, own }) => {
  // (A good many waits for silence, each the better part of a second.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const master = async (): Promise<number> => Number(await page.getByRole("meter", { name: "Master level" }).getAttribute("aria-valuenow"));
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
    await expect.poll(master, { message: "the master is quiet again" }).toBeLessThan(-50);
  };

  // Two lanes that take notes of their own: the tracer's takes up to B3 (the computer's lower row, Z being a C3), the one that is added takes C4 and up (the upper row, Q being a C4).
  await page.getByRole("combobox", { name: "Highest key of Oscillator" }).selectOption({ label: "B3" });
  await addLane(page);
  await page.getByRole("combobox", { name: "Lowest key of Oscillator 2" }).selectOption({ label: "C4" });
  await expect(page.getByRole("combobox", { name: "Lowest key of Oscillator 2" }).locator("option:checked")).toHaveText("C4");
  // (A choice has the focus, and a key typed in it is not for the piano: the focus goes to the page.)
  await page.getByRole("status").click();
  await showAll(page);

  // The second lane has no way out, so that what it sends is heard by the cable and by nothing else: from its instrument to the filter of the first lane (the tracer's low-pass).
  const cut = page.getByRole("button", { name: "Cut the way out of Oscillator 2", exact: true });
  await cut.click();
  await expect(cut).toHaveText("no out");
  await drag(page, audioOut(node(page, "Oscillator 2")), audioIn(node(page, "Filter")));
  await expect(sends(page)).toHaveCount(1);
  // Nothing is soloed: both lanes are heard, the second only through the cable.
  await heard("z", "the first lane");
  await heard("q", "the second lane, through its cable to the filter");

  // The second lane is soloed: its notes are heard still, through the filter of a lane that is not soloed, and the first lane's own notes are not heard. The canvas draws it so.
  await solo(page, "Oscillator 2").click();
  await expect(solo(page, "Oscillator 2")).toHaveAttribute("aria-pressed", "true");
  await expectOpacities(page, ["Oscillator 1", "Filter", "Oscillator 2"], ["0.55", "1", "1"]);
  await heard("q", "a lane that is soloed, through the filter of a lane that is not");
  await silent("z", "the notes of a lane that is not soloed, whose filter is heard");

  // A lane that is muted is not heard for what it is sent: the first lane's filter is cut, and so are the notes that came through it. And so is what a muted lane would send.
  await mute(page, "Oscillator 1").click();
  await expect(mute(page, "Oscillator 1")).toHaveAttribute("aria-pressed", "true");
  await expectOpacities(page, ["Oscillator 1", "Filter", "Oscillator 2"], ["0.55", "0.55", "1"]);
  await silent("q", "what a lane that is muted is sent");
  await mute(page, "Oscillator 1").click();
  await expect(mute(page, "Oscillator 1")).toHaveAttribute("aria-pressed", "false");
  await heard("q", "the notes again, when the first lane is not muted");
  await mute(page, "Oscillator 2").click();
  await expectOpacities(page, ["Oscillator 1", "Filter", "Oscillator 2"], ["0.55", "0.55", "0.55"]);
  await silent("q", "a lane that is muted and soloed");
  await mute(page, "Oscillator 2").click();
  await expect(mute(page, "Oscillator 2")).toHaveAttribute("aria-pressed", "false");

  // The cable is taken out: the second lane has no way out and no cable, and the filter is not heard for anything; undo puts the cable back, and the notes are heard again.
  await page.getByRole("button", { name: "Remove cable Oscillator 2 → Filter" }).click();
  await expect(sends(page)).toHaveCount(0);
  await expectOpacities(page, ["Oscillator 1", "Filter", "Oscillator 2"], ["0.55", "0.55", "1"]);
  await silent("q", "a lane with no way out and no cable");
  await page.getByRole("button", { name: "Undo remove an audio cable", exact: true }).click();
  await expect(sends(page)).toHaveCount(1);
  await expectOpacities(page, ["Oscillator 1", "Filter", "Oscillator 2"], ["0.55", "1", "1"]);
  await heard("q", "the notes, with the cable put back by undo");
});
