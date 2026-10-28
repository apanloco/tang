import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { audioIn, audioInSide, audioOut, drag, node, open, sends, showAll } from "./canvas";
import { test } from "./own";
import { heardAt, silentAt } from "./sound";

// An effect that has a second input of audio, the Math (docs/web-ui.md D95): it has two inputs, A and B, one above the other on its left edge, each a place where a cable ends. The first test is against the mock
// engine in the page (what the canvas draws and says), the second against the real program (what is heard when B has a cable and when it has none, what the file keeps and what a page that is opened again draws).

/** Add the effect `name` that stands by itself, from the bar. */
async function add(page: Page, name: string): Promise<void> {
  await page.getByRole("button", { name: "Add an effect", exact: true }).click();
  await page.getByRole("menuitem", { name, exact: true }).click();
  await expect(node(page, name)).toBeVisible();
}

/** The names of the knobs of a node that are dimmed because they have no effect now (`data-unused`: step 95b), in the order of the node. */
const dimmed = (math: Locator): Promise<(string | null | undefined)[]> =>
  math.locator(".tang-cell[data-unused]").evaluateAll((cells) => cells.map((cell) => cell.querySelector(".tang-knob__label, .tang-choice__label")?.textContent));

// A ring reads neither `amount`, `bits`, `attack` nor `release`; and B is the effect's own carrier while nothing is patched there, so that `pitch` and `wave` have no effect while a cable is.
const RING = ["amount", "bits", "attack", "release"];
const RING_AT_B = ["amount", "bits", "pitch", "wave", "attack", "release"];

mock("a math effect has two inputs, A and B, one above the other, and a cable is dragged to each, and the keyboard's menu has both", async ({ page }) => {
  await open(page);
  await add(page, "Math");
  await showAll(page);
  const math = node(page, "Math");

  // Two inputs, named beside their ports, the second below the first; an effect that has one input has the one.
  await expect(math.locator(".tang-port-label")).toHaveText(["A", "B"]);
  const [first, second] = [await audioIn(math).boundingBox(), await audioInSide(math).boundingBox()];
  expect(first && second && second.y > first.y + 10).toBe(true);
  await expect(audioInSide(node(page, "Filter"))).toHaveCount(0);
  await expect(audioIn(node(page, "Filter"))).toHaveCount(1);

  // The knobs that have no effect now are dimmed, and say why to the pointer.
  await expect.poll(() => dimmed(math)).toEqual(RING);
  await expect(math.locator(".tang-cell[data-unused]").first()).toHaveAttribute("title", "No effect now: only in the modes am, fold, warp, compare, hold, follow");

  // A cable to B, and one to A from the same oscillator: two cables, each named for the input it ends at.
  await drag(page, audioOut(node(page, "Oscillator")), audioInSide(math));
  await expect(sends(page)).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Remove cable Oscillator → Math: B" })).toBeVisible();
  // With a cable at B the carrier is not heard: its two knobs are dimmed as well, and the first of them says why.
  await expect.poll(() => dimmed(math)).toEqual(RING_AT_B);
  await expect(math.locator(".tang-cell[data-unused]").nth(2)).toHaveAttribute("title", "No effect now: only while nothing is patched at B");
  // The mode that is chosen decides which of the others have an effect: xor reads `bits`.
  await math.getByRole("combobox", { name: "mode", exact: true }).selectOption("xor");
  await expect.poll(() => dimmed(math)).toEqual(["amount", "pitch", "wave", "attack", "release"]);
  await math.getByRole("combobox", { name: "mode", exact: true }).selectOption("ring");
  await expect.poll(() => dimmed(math)).toEqual(RING_AT_B);
  await drag(page, audioOut(node(page, "Oscillator")), audioIn(math));
  await expect(sends(page)).toHaveCount(2);
  await expect(page.getByRole("button", { name: "Remove cable Oscillator → Math: A" })).toBeVisible();
  // (A second cable between the same oscillator and the same input is not drawn: the drop is not taken.)
  await drag(page, audioOut(node(page, "Oscillator")), audioInSide(math));
  await expect(sends(page)).toHaveCount(2);

  // The menu on the filter has each of the inputs of the math effect, by name, and the cable that is chosen is at that input.
  await page.getByRole("button", { name: "Send audio from Filter" }).click();
  const menu = page.getByRole("menu");
  // (Not the master: the filter's way out is there already, a cable that is drawn.)
  await expect(menu.getByRole("menuitem")).toHaveText(["Math (A)", "Math (B)"]);
  await menu.getByRole("menuitem", { name: "Math (B)" }).click();
  await expect(page.getByRole("button", { name: "Remove cable Filter → Math: B" })).toBeVisible();
  await expect(sends(page)).toHaveCount(3);

  // Undo takes the last cable out, and Redo draws it again; taking a cable out leaves the other input's cable of the same oscillator.
  await page.getByRole("button", { name: "Undo draw an audio cable" }).click();
  await expect(sends(page)).toHaveCount(2);
  await page.getByRole("button", { name: "Redo draw an audio cable" }).click();
  await expect(sends(page)).toHaveCount(3);
  await page.getByRole("button", { name: "Remove cable Oscillator → Math: B" }).click();
  await expect(sends(page)).toHaveCount(2);
  await expect(page.getByRole("button", { name: "Remove cable Oscillator → Math: A" })).toBeVisible();
  // (The filter's cable is at B still: the carrier is not heard until the last cable there is out.)
  await expect.poll(() => dimmed(math)).toEqual(RING_AT_B);
  await page.getByRole("button", { name: "Remove cable Filter → Math: B" }).click();
  await expect.poll(() => dimmed(math)).toEqual(RING);
});

test("a math effect in the real program multiplies by its own carrier until a cable is patched to B, and then by what the cable brings: a muted lane at B silences it, taking the cable out brings the carrier back, the file keeps the input and a page that is opened again draws it", async ({
  page,
  own,
}) => {
  // (A good many waits for silence, each the better part of a second.)
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

  // The lane's way out is cut, and a math effect of its own is added: it has two inputs (the tracer's oscillator is fed to the first by a cable) and is a module that sends nowhere until a cable says where.
  await add(page, "Math");
  await showAll(page);
  await page.getByRole("button", { name: "Remove cable Filter → Master", exact: true }).click();
  await expect(page.getByRole("button", { name: "Remove cable Filter → Master", exact: true })).toHaveCount(0);
  await drag(page, audioOut(node(page, "Oscillator")), audioIn(node(page, "Math")));
  await expect(page.getByRole("button", { name: "Remove cable Oscillator → Math: A" })).toBeVisible();
  await silentAt(page, "a math effect that is fed and sends nowhere");

  // A cable from the math effect to the master gives its lane its way out: the oscillator is heard multiplied by the effect's own carrier, since nothing is patched to B.
  await drag(page, audioOut(node(page, "Math")), audioIn(node(page, "Master")));
  await expect(page.getByRole("button", { name: "Remove cable Math → Master", exact: true })).toBeVisible();
  await heardAt(page, "the oscillator times the math effect's own carrier");
  // (The carrier is heard, so its knobs are not dimmed: the program says what has an effect, and the page dims by it.)
  await expect.poll(() => dimmed(node(page, "Math"))).toEqual(RING);

  // A second lane, which is muted, is patched to B: a B that is patched and silent multiplies the sound by nothing. The first input is fed still.
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menu", { name: "Add an instrument" }).getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  await node(page, "Oscillator 2").getByRole("button", { name: "Mute Oscillator 2" }).click();
  await expect(node(page, "Oscillator 2").getByRole("button", { name: "Mute Oscillator 2" })).toHaveAttribute("aria-pressed", "true");
  await showAll(page);
  await drag(page, audioOut(node(page, "Oscillator 2")), audioInSide(node(page, "Math")));
  await expect(page.getByRole("button", { name: "Remove cable Oscillator 2 → Math: B" })).toBeVisible();
  await silentAt(page, "a math effect whose B is patched to a muted lane");
  await expect.poll(() => dimmed(node(page, "Math"))).toEqual(RING_AT_B);

  // The file keeps the input the cable ends at, by the word `side` after the effect's place.
  const file = await saved();
  expect(file).toMatch(/\[\[cable\]\]\nfrom = "instrument\[0\]"\nto = "instrument\[1\]\.effect\[0\]"\n/);
  expect(file).toMatch(/\[\[cable\]\]\nfrom = "instrument\[2\]"\nto = "instrument\[1\]\.effect\[0\]\.side"\n/);

  // The cable taken out: nothing is patched to B, and the carrier is back.
  await page.getByRole("button", { name: "Remove cable Oscillator 2 → Math: B" }).click();
  await expect(sends(page)).toHaveCount(1);
  await heardAt(page, "the math effect's own carrier, with nothing patched to B");
  await expect.poll(() => dimmed(node(page, "Math"))).toEqual(RING);

  // Undo draws the cable at B again (it is the second input that comes back, and silence with it), and a page that is opened again draws the cable at B.
  await page.getByRole("button", { name: "Undo remove an audio cable" }).click();
  await expect(sends(page)).toHaveCount(2);
  await silentAt(page, "a math effect whose B is patched to a muted lane, again");
  await expect.poll(() => dimmed(node(page, "Math"))).toEqual(RING_AT_B);
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByRole("button", { name: "Remove cable Oscillator 2 → Math: B" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Remove cable Oscillator 1 → Math: A" })).toBeVisible();
  // (A page that is opened again dims the carrier too.)
  await expect.poll(() => dimmed(node(page, "Math"))).toEqual(RING_AT_B);

  // The lane that is unmuted sends its own sound to B, and is heard itself: the master has the oscillator multiplied by it as well.
  await node(page, "Oscillator 2").getByRole("button", { name: "Mute Oscillator 2" }).click();
  await expect(node(page, "Oscillator 2").getByRole("button", { name: "Mute Oscillator 2" })).toHaveAttribute("aria-pressed", "false");
  await heardAt(page, "the oscillator times the second oscillator at B");
});
