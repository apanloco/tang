import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Page } from "@playwright/test";

import { audioIn, audioOut, drag, flowBoxes, node, nodeBoxes, open, output, port, sends, showAll } from "./canvas";
import { test } from "./own";
import { heardAt, silentAt } from "./sound";

// An effect that stands by itself, from the page (docs/web-ui.md D87): the bar's "+ Effect" adds an effect in a lane of its own that has no instrument and no card on the canvas, so that what the canvas
// has is the effect, a module like any other: it is fed by the cables that end at it, it has the lane's mute and solo, and its "×" takes it out with the lane. The first tests are against the mock
// engine in the page (what the canvas draws and says), the last against the real program (what is heard, what the file keeps and what the history does).

/** The button in the bar that adds an effect. */
const addEffect = (page: Page) => page.getByRole("button", { name: "Add an effect", exact: true });

/** Add the effect `name` that stands by itself, from the bar. */
async function add(page: Page, name: string): Promise<void> {
  await addEffect(page).click();
  await page.getByRole("menuitem", { name, exact: true }).click();
  await expect(node(page, name)).toBeVisible();
}

mock("an effect that is added from the bar stands by itself: a module with the lane's cells and no card of a lane, and nothing else on the canvas moves", async ({ page }) => {
  await open(page);
  const before = (await flowBoxes(page)).filter((box) => box.name !== "Master");
  await add(page, "Reverb");
  const reverb = node(page, "Reverb");
  // (No lane that has no instrument is drawn: the canvas has the module, and the cells of the lane are on it.)
  await expect(node(page, "\\(empty\\)")).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^Choose an instrument for / })).toHaveCount(0);
  await expect(reverb.getByRole("button", { name: "Mute Reverb" })).toBeVisible();
  await expect(reverb.getByRole("button", { name: "Solo Reverb" })).toBeVisible();
  // A module that stands by itself sends nowhere until a cable says where (step 88): it is made with no cable at all, none into it from the lane (which has no sound) and none out of it to the master.
  await expect(reverb.getByRole("button", { name: "Add a modulator to Reverb" })).toBeVisible();
  await expect(page.getByRole("button", { name: /^Remove cable .*Reverb/ })).toHaveCount(0);
  await expect(reverb.getByRole("button", { name: /^Cut the (way out|input) of / })).toHaveCount(0);
  // The nodes that were there are where they were on the canvas, and the module is below them, clear of them.
  const after = await flowBoxes(page);
  for (const box of before) expect(after.find((found) => found.name === box.name), box.name).toEqual(box);
  const added = after.find((box) => box.name === "Reverb");
  const lowest = Math.max(...before.map((box) => box.y + box.height));
  expect(added?.y ?? 0).toBeGreaterThanOrEqual(lowest);
});

mock("an effect that stands by itself takes a cable from an instrument, is muted and soloed by its own buttons, and is taken out by its \"×\", with nothing left behind, and Undo and Redo have the words of the button that adds it", async ({ page }) => {
  await open(page);
  await add(page, "Reverb");
  await showAll(page);
  const reverb = node(page, "Reverb");

  // A cable from the instrument of the lane to the reverb: the reverb is fed by it.
  await drag(page, audioOut(node(page, "Oscillator")), audioIn(reverb));
  await expect(sends(page)).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Remove cable Oscillator → Reverb" })).toBeVisible();

  // Its mute dims it, as the mute of a lane dims its effects, and a press again brings it back.
  await reverb.getByRole("button", { name: "Mute Reverb" }).click();
  await expect(reverb.getByRole("button", { name: "Mute Reverb" })).toHaveAttribute("aria-pressed", "true");
  await expect(reverb.locator(".tang-node--unheard")).toHaveCount(1);
  await reverb.getByRole("button", { name: "Mute Reverb" }).click();
  await expect(reverb.locator(".tang-node--unheard")).toHaveCount(0);

  // The "×" takes the effect out with the lane that holds it, and the cable that went to it; the focus goes to the bar's button that adds one.
  await reverb.getByRole("button", { name: "Remove Reverb" }).click();
  await expect(node(page, "Reverb")).toHaveCount(0);
  await expect(sends(page)).toHaveCount(0);
  await expect(addEffect(page)).toBeFocused();
  // The history has the removal in the words of the effect, and the add before it: Undo puts it back, and the cable with it.
  await page.getByRole("button", { name: "Undo remove Reverb" }).click();
  await expect(node(page, "Reverb")).toBeVisible();
  await expect(sends(page)).toHaveCount(1);
  await expect(node(page, "\\(empty\\)")).toHaveCount(0);
  await page.getByRole("button", { name: "Redo remove Reverb" }).click();
  await expect(node(page, "Reverb")).toHaveCount(0);
});

mock("an effect that stands by itself has a \"+\" that adds a modulator to it, which drives its parameters like any lane's: the LFO is over it and a cable can be dragged to a knob", async ({ page }) => {
  await open(page);
  await add(page, "Reverb");
  await showAll(page);
  const reverb = node(page, "Reverb");
  await reverb.getByRole("button", { name: "Add a modulator to Reverb" }).click();
  await page.getByRole("menuitem", { name: "LFO" }).click();
  const lfo = node(page, "LFO 2");
  await expect(lfo).toBeVisible();
  const boxes = await nodeBoxes(page);
  const [top, below] = ["LFO 2", "Reverb"].map((name) => boxes.find((box) => box.name === name));
  expect((top?.top ?? 0) + (top?.height ?? 0)).toBeLessThan(below?.top ?? 0);
  await showAll(page);
  await drag(page, output(lfo), port(reverb, "decay"));
  await expect(page.locator(".tang-cable--modulation")).toHaveCount(2);
});

mock("a lane that has nothing but effects is drawn as them whichever way it came to be: the instrument of a lane taken out alone leaves its filter (and the LFO that hears the lane), and the last lane of the session is a card again when its last effect goes", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Remove the lane of Oscillator" }).click();
  await page.getByRole("menuitem", { name: /^Remove Oscillator only/ }).click();
  await expect(node(page, "\\(empty\\)")).toHaveCount(0);
  await expect(node(page, "Filter").getByRole("button", { name: "Mute Filter" })).toBeVisible();
  // The lane is the only one of the session, which a session keeps: the filter's "×" takes the filter out and no more, and the lane that has nothing has its card. The LFO, which only hears the lane, is as it was.
  await node(page, "Filter").getByRole("button", { name: "Remove Filter" }).click();
  await expect(node(page, "Filter")).toHaveCount(0);
  await expect(node(page, "\\(empty\\)")).toBeVisible();
  await expect(node(page, "LFO")).toBeVisible();
});

test("an effect that stands by itself in the real program is fed by a cable only, and is heard through it: the lane that has an instrument is silent with its way out cut, the cable brings its sound to the effect, the file keeps the lane that holds it, and Undo puts it back", async ({ page, own }) => {
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

  // A reverb of its own is added. Nothing is fed to it, and it puts out nothing: the lane is heard as before, and when its way out is cut, which leaves its sound nowhere, a key is silent.
  await add(page, "Reverb");
  await heardAt(page, "the lane, with a reverb beside it that is fed nothing");
  await showAll(page);
  await page.getByRole("button", { name: "Remove cable Filter → Master", exact: true }).click();
  await expect(page.getByRole("button", { name: "Remove cable Filter → Master", exact: true })).toHaveCount(0);
  await silentAt(page, "a lane with no way out, and a reverb that is fed nothing");

  // A cable from the instrument to the reverb: the reverb is fed, and is silent still, since a module that no cable leaves sends nowhere (its way out was cut when it was made).
  await drag(page, audioOut(node(page, "Oscillator")), audioIn(node(page, "Reverb")));
  await expect(sends(page)).toHaveCount(1);
  await silentAt(page, "a reverb that is fed by a cable and has no cable out");

  // A cable from the reverb to the master is the link that its lane's cut way out would be (step 88: every link is a cable): it gives the way out back and makes no cable of its own, and the sound is heard through the
  // reverb, and only through it.
  await drag(page, audioOut(node(page, "Reverb")), audioIn(node(page, "Master")));
  await expect(page.getByRole("button", { name: "Remove cable Reverb → Master", exact: true })).toBeVisible();
  await expect(sends(page)).toHaveCount(1);
  await heardAt(page, "the lane through its cable to the reverb that stands by itself");

  // The file has the lane that holds the reverb, with no instrument, and the cable to it, and no cable from it (its way out is given: nothing is written).
  const file = await saved();
  const lanes = file.split("[[instrument]]").slice(1);
  expect(lanes).toHaveLength(2);
  expect(lanes[0]).toContain('plugin = "builtin:osc"');
  expect(lanes[0]).toContain('route = "none"');
  expect(lanes[1]).not.toContain("builtin:osc");
  expect(lanes[1]).toContain('plugin = "builtin:reverb"');
  expect(lanes[1]).not.toContain("route");
  expect(file.split("[[cable]]")).toHaveLength(2);
  expect(file).toMatch(/\[\[cable\]\]\nfrom = "instrument\[0\]"\nto = "instrument\[1\]\.effect\[0\]"\n/);

  // The cable is taken out, and the reverb is fed nothing again: silent. The reverb's "×" takes it out with its lane, the file has one lane again, and Undo puts the effect back as it was.
  await page.getByRole("button", { name: "Remove cable Oscillator → Reverb" }).click();
  await expect(sends(page)).toHaveCount(0);
  await silentAt(page, "a reverb that is fed nothing, with no cable");
  await node(page, "Reverb").getByRole("button", { name: "Remove Reverb" }).click();
  await expect(node(page, "Reverb")).toHaveCount(0);
  expect((await saved()).split("[[instrument]]")).toHaveLength(2);
  await page.getByRole("button", { name: "Undo remove Reverb" }).click();
  await expect(node(page, "Reverb")).toBeVisible();
  await expect(node(page, "\\(empty\\)")).toHaveCount(0);
  expect((await saved()).split("[[instrument]]")).toHaveLength(3);

  // A page that is opened again draws what the program has: the reverb as a module of its own, and no card of a lane that has no instrument.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "Reverb")).toBeVisible();
  await expect(node(page, "\\(empty\\)")).toHaveCount(0);
  await expect(node(page, "Reverb").getByRole("button", { name: "Mute Reverb" })).toBeVisible();
});
