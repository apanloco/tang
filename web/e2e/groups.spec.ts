import { readFileSync } from "node:fs";

import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { node, open } from "./canvas";
import { testWithGroups } from "./own";

// Groups, from the page: the page has no way to make one (step 103), so that a session has one when it is loaded (the mock's `groups` scene, and a program that is
// started on the tracer's file with groups added). A lane is put in one and taken out of it, a group is named in place, and a lane or a group is taken out for
// good after the page has asked whether to. What the page does is the program's: a second page sees it, and the saved file has it.

const groupChoice = (page: Page, lane: string) => page.getByRole("combobox", { name: `Group of ${lane}` });

test("a group that the session has is drawn, a lane is put in it and taken out of it, the group is named in place and taken out after the page has asked", async ({ page }) => {
  await open(page, "groups");
  // The tracer has one lane, which has no group, and a session keeps its last lane: nothing takes the lane out, and its menu only offers to take the instrument out of it.
  await expect(groupChoice(page, "Oscillator")).toHaveValue("");
  await expect(page.getByRole("button", { name: /^Remove the lane of / })).toHaveCount(1);
  await page.getByRole("button", { name: "Remove the lane of Oscillator" }).click();
  await expect(page.getByRole("menuitem")).toHaveText(["Keep Oscillator", "Remove Oscillator only: its 1 effect stays in the lane"]);
  await page.keyboard.press("Escape");

  // The group of the scene: it is on the canvas, empty and with its own buttons, and the lane's choice offers it and nothing else (the bar has no button that adds one).
  const group = node(page, "Group 1");
  await expect(group).toBeVisible();
  await expect(group.getByRole("button", { name: "Add an effect to Group 1" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Add a group" })).toHaveCount(0);
  await expect(groupChoice(page, "Oscillator").locator("option")).toHaveText(["none", "Group 1"]);
  await groupChoice(page, "Oscillator").selectOption({ label: "Group 1" });
  await expect(groupChoice(page, "Oscillator").locator("option:checked")).toHaveText("Group 1");
  // The lane's last link, from its filter, goes into the group now and the group's goes to the master.
  await expect(page.locator('[id^="audio:"]')).toHaveCount(3);
  await expect(page.locator(`[id="audio:${await filterId(page)}>${await idOf(page, "Group 1")}"]`)).toHaveCount(1);

  // Named in place, with the keyboard: the button turns the name into a field, Enter takes it, and the focus goes back to the button.
  await group.getByRole("button", { name: "Rename Group 1" }).click();
  // (Found from the page: while the name is a field the node has no name to be found by.)
  const field = page.getByRole("textbox", { name: "Name of Group 1" });
  await expect(field).toBeFocused();
  await field.fill("Pad");
  await field.press("Enter");
  await expect(node(page, "Pad")).toBeVisible();
  await expect(node(page, "Pad").getByRole("button", { name: "Rename Pad" })).toBeFocused();
  await expect(groupChoice(page, "Oscillator").locator("option:checked")).toHaveText("Pad");
  // Escape leaves the name as it was.
  await node(page, "Pad").getByRole("button", { name: "Rename Pad" }).click();
  await page.getByRole("textbox", { name: "Name of Pad" }).fill("Nothing");
  await page.keyboard.press("Escape");
  await expect(node(page, "Pad")).toBeVisible();

  // A group that is taken out asks first: keeping it asks nothing, and removing it says what goes and what stays.
  await node(page, "Pad").getByRole("button", { name: "Remove Pad" }).click();
  const menu = page.getByRole("menu", { name: "Remove Pad" });
  await expect(menu.getByRole("menuitem", { name: "Keep Pad" })).toBeFocused();
  await expect(menu.getByRole("menuitem", { name: /^Remove Pad for good: its 1 lane stays, in no group$/ })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(node(page, "Pad")).toBeVisible();
  await node(page, "Pad").getByRole("button", { name: "Remove Pad" }).click();
  await page.getByRole("menuitem", { name: /^Remove Pad for good/ }).click();
  await expect(node(page, "Pad")).toHaveCount(0);
  // The last group is gone: the lane has no choice of group any more, and the focus is on the bar's button that adds an instrument.
  await expect(groupChoice(page, "Oscillator")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Add an instrument" })).toBeFocused();
});

test("the page has no way to make a group: the bar has no button for it, and a lane has no choice of group while the session has none", async ({ page }) => {
  await open(page);
  await expect(page.getByRole("button", { name: "Add an instrument" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Add a group" })).toHaveCount(0);
  await expect(page.getByRole("combobox", { name: /^Group of / })).toHaveCount(0);
  // The lane has its keys and its mute and solo, and no cell for a group.
  const lane = node(page, "Oscillator");
  await expect(lane.getByRole("combobox", { name: "Lowest key of Oscillator" })).toBeVisible();
  await expect(lane.getByRole("button", { name: "Mute Oscillator", exact: true })).toBeVisible();
  // A second lane has none either.
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  await expect(page.getByRole("combobox", { name: /^Group of / })).toHaveCount(0);
});

test("a lane is put in a group, a second lane is added and taken out again, and the last one cannot be", async ({ page }) => {
  await open(page, "groups");
  await groupChoice(page, "Oscillator").selectOption({ label: "Group 1" });
  await expect(node(page, "Group 1")).toBeVisible();
  await expect(groupChoice(page, "Oscillator").locator("option:checked")).toHaveText("Group 1");

  // A second lane: both can be taken out now, by a menu that asks; the new one, which has nothing in it, says only that it goes.
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(groupChoice(page, "Oscillator 2")).toBeVisible();
  await expect(page.getByRole("button", { name: /^Remove the lane of / })).toHaveCount(2);
  await page.getByRole("button", { name: "Remove the lane of Oscillator 2" }).click();
  await expect(page.getByRole("menuitem", { name: "Remove the lane of Oscillator 2 for good" })).toBeVisible();
  await page.getByRole("menuitem", { name: "Remove the lane of Oscillator 2 for good" }).click();
  await expect(groupChoice(page, "Oscillator")).toBeVisible();
  // (The last lane keeps its button: it can have its instrument taken out, and not be taken out.)
  await expect(page.getByRole("button", { name: /^Remove the lane of / })).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Add an instrument" })).toBeFocused();
});

test("the instrument of a lane is taken out alone: its filter and its LFO stay and stand by themselves, the lane has no card, and Undo puts the instrument back", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Remove the lane of Oscillator" }).click();
  await expect(page.getByRole("menuitem")).toHaveText(["Keep Oscillator", "Remove Oscillator only: its 1 effect stays in the lane"]);
  await page.getByRole("menuitem", { name: /^Remove Oscillator only/ }).click();
  // The lane has nothing left that a card shows (no instrument, no key range, no group): its filter and its LFO are on the canvas as they were, modules that stand by themselves, and the filter has
  // the lane's mute and solo. The focus is on the bar's button that adds an instrument.
  await expect(node(page, "Filter")).toBeVisible();
  await expect(node(page, "LFO")).toBeVisible();
  await expect(node(page, "\\(empty\\)")).toHaveCount(0);
  await expect(node(page, "Oscillator")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Add an instrument" })).toBeFocused();
  await expect(node(page, "Filter").getByRole("button", { name: "Mute Filter" })).toBeVisible();
  await expect(node(page, "Filter").getByRole("button", { name: "Solo Filter" })).toBeVisible();
  // It is the only lane, and has no instrument: nothing is left to take out of it, and it has no button for it.
  await expect(page.getByRole("button", { name: /^Remove the lane of / })).toHaveCount(0);

  // The history has it, in the words of what was done: Undo puts the instrument back, and Redo takes it out again.
  await page.getByRole("button", { name: "Undo take Oscillator out of its lane" }).click();
  await expect(node(page, "Oscillator")).toBeVisible();
  await expect(node(page, "Filter").getByRole("button", { name: "Mute Filter" })).toHaveCount(0);
  await page.getByRole("button", { name: "Redo take Oscillator out of its lane" }).click();
  await expect(node(page, "Oscillator")).toHaveCount(0);
  await expect(node(page, "Filter").getByRole("button", { name: "Mute Filter" })).toBeVisible();
  await expect(node(page, "LFO")).toBeVisible();
});

// (A program started on the tracer's file with two groups: the page has no way to make one.)
const grouped = testWithGroups(2);

grouped("what is done to groups is the program's: a second page sees it, and the saved file has the groups, their names and which lane is in which", async ({ page, browser, own: tang }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(tang.url);
  await expect(page.getByRole("status")).toHaveText("Connected");

  // The lane is put in the first group, which is named; the second is empty.
  await expect(node(page, "Group 2")).toBeVisible();
  await groupChoice(page, "Oscillator").selectOption({ label: "Group 1" });
  await node(page, "Group 1").getByRole("button", { name: "Rename Group 1" }).click();
  await page.getByRole("textbox", { name: "Name of Group 1" }).fill("Pad");
  await page.keyboard.press("Enter");
  await expect(node(page, "Pad")).toBeVisible();

  // A second page has all of it, and what it does shows on the first: the lane goes to the other group.
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(tang.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect(node(other, "Pad")).toBeVisible();
  await expect(node(other, "Group 2")).toBeVisible();
  await expect(groupChoice(other, "Oscillator").locator("option:checked")).toHaveText("Pad");
  await groupChoice(other, "Oscillator").selectOption({ label: "Group 2" });
  await expect(groupChoice(page, "Oscillator").locator("option:checked")).toHaveText("Group 2");

  // The saved file has both groups, the first by its name and the second with none (it is called by its place), and the lane in the second.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(tang.file, "utf8");
  expect(saved.match(/^\[\[group\]\]/gm), saved).toHaveLength(2);
  expect(saved).toMatch(/\[\[group\]\]\s*\nname = "Pad"/);
  expect(saved).toMatch(/^group = 1$/m);

  // Taken out by the second page, the first group is gone from the first page and from the file, and the lane is in the group that is the first now.
  await node(other, "Pad").getByRole("button", { name: "Remove Pad" }).click();
  await other.getByRole("menuitem", { name: /^Remove Pad for good/ }).click();
  await expect(node(page, "Pad")).toHaveCount(0);
  await expect(node(page, "Group 1")).toBeVisible();
  await expect(groupChoice(page, "Oscillator").locator("option:checked")).toHaveText("Group 1");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const after = readFileSync(tang.file, "utf8");
  expect(after.match(/^\[\[group\]\]/gm), after).toHaveLength(1);
  expect(after).not.toContain('name = "Pad"');
  expect(after).toMatch(/^group = 0$/m);
  await other.close();
});

/** The id of the node called `name`, which the canvas names an audio link by the nodes it joins with. */
async function idOf(page: Page, name: string): Promise<string> {
  const id = await node(page, name).getAttribute("data-id");
  if (id === null) throw new Error(`${name} has no id`);
  return id;
}

/** The tracer's filter, which ends the lane's chain. */
const filterId = (page: Page): Promise<string> => idOf(page, "Filter");
