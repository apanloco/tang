import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";

import { node, open } from "./canvas";
import { test } from "./own";

// Undo and Redo from the page, on the real program (a program of its own: the history is the program's, whatever page changed what, so what
// other tests did would be on it).

test("a knob turned in one go is put back by one Undo, by the button and by the keys, a second page has the same history and does it again, and the saved file has what is done", async ({ page, browser, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  await expect(undo).toBeDisabled();
  await expect(redo).toBeDisabled();

  // Five presses of an arrow, a moment apart, are one thing that was done: the program makes one step of them.
  const detune = node(page, "Oscillator").getByRole("slider", { name: "detune", exact: true });
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await detune.focus();
  for (let press = 0; press < 5; press += 1) await page.keyboard.press("ArrowRight");
  const turned = (await detune.getAttribute("aria-valuenow")) ?? "";
  expect(Number(turned)).toBeGreaterThan(0);
  await expect(undo).toBeEnabled();
  await expect(undo).toHaveAccessibleName("Undo set detune");
  await expect(redo).toBeDisabled();
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // The button puts all of it back at once, and says what it would do again.
  await undo.click();
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await expect(undo).toBeDisabled();
  await expect(redo).toHaveAccessibleName("Redo set detune");

  // The keys do it again and put it back, whichever way they are written (Control, or Command on a Mac).
  await page.keyboard.press("ControlOrMeta+Shift+Z");
  await expect(detune).toHaveAttribute("aria-valuenow", turned);
  await expect(redo).toBeDisabled();
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await expect(redo).toBeEnabled();

  // A second page is told what can be done when it comes, shows what the session has, and what it does is on the first page as what anybody does.
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect(other.getByRole("button", { name: /^Redo/ })).toHaveAccessibleName("Redo set detune");
  await expect(other.getByRole("button", { name: /^Undo/ })).toBeDisabled();
  await expect(node(other, "Oscillator").getByRole("slider", { name: "detune", exact: true })).toHaveAttribute("aria-valuenow", "0");
  await other.getByRole("button", { name: /^Redo/ }).click();
  await expect(detune).toHaveAttribute("aria-valuenow", turned);
  await expect(undo).toBeEnabled();
  await expect(redo).toBeDisabled();

  // What is saved is what is done: the setting is in the file; put back, it is not the value it was.
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(Number(/^detune\s*=\s*(\S+)$/m.exec(readFileSync(own.file, "utf8"))?.[1])).toBeCloseTo(Number(turned), 5);
  await undo.click();
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(/^detune\s*=\s*(?!0(\.0*)?$)\S+$/m.test(readFileSync(own.file, "utf8")), readFileSync(own.file, "utf8")).toBe(false);
  await other.close();
});

test("a key typed in a field that takes text is the field's own undo and not the session's, and something that cannot be undone yet empties the history", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const detune = node(page, "Oscillator").getByRole("slider", { name: "detune", exact: true });
  await detune.focus();
  await page.keyboard.press("End");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  await expect(undo).toBeEnabled();

  // In the filter of the panel of parameters, Control+Z is the field's: the text goes back, the detune stays where it was put.
  await page.getByRole("button", { name: "Parameters of Oscillator" }).click();
  const search = page.getByRole("searchbox", { name: "Filter the parameters of Oscillator" });
  await expect(search).toBeFocused();
  await page.keyboard.type("ab");
  await expect(search).toHaveValue("ab");
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(search).not.toHaveValue("ab");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  await expect(undo).toBeEnabled();

  // An effect that is added is a step of its own, and so is a lane that is put in a group; a group made of a lane that is in one is a change that nothing puts back yet (the
  // lane leaves its group), and it empties the history: there is nothing to undo, and the keys leave Control+Z alone.
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  await expect(node(page, "Reverb")).toBeVisible();
  await expect(undo).toHaveAccessibleName("Undo add Reverb");
  await page.getByRole("button", { name: "Add a group" }).click();
  await expect(node(page, "Group 1")).toBeVisible();
  const inGroup = page.getByRole("combobox", { name: "Group of Oscillator" });
  await inGroup.selectOption({ label: "Group 1" });
  await expect(undo).toHaveAccessibleName("Undo put a lane in a group");
  await inGroup.selectOption({ label: "new group" });
  await expect(undo).toBeDisabled();
  await detune.focus();
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
});

test("an effect that is added is taken out by Undo and put back by Redo, with what was done to it, and the saved file has what is done", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  const save = async (): Promise<string> => {
    await page.getByRole("button", { name: /^Save/ }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };

  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  const reverb = node(page, "Reverb");
  await expect(reverb).toBeVisible();
  await expect(undo).toHaveAccessibleName("Undo add Reverb");

  // Something is done to the effect that was added.
  const damping = reverb.getByRole("slider", { name: "damping", exact: true });
  const before = (await damping.getAttribute("aria-valuenow")) ?? "";
  await damping.focus();
  await page.keyboard.press("End");
  const turned = (await damping.getAttribute("aria-valuenow")) ?? "";
  expect(turned).not.toBe(before);
  await expect(undo).toHaveAccessibleName("Undo set damping");
  expect(await save()).toContain("builtin:reverb");

  // The last thing done is put back first, and then the effect is taken out: the file that is saved then has no reverb.
  await undo.click();
  await expect(damping).toHaveAttribute("aria-valuenow", before);
  await expect(undo).toHaveAccessibleName("Undo add Reverb");
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(reverb).toHaveCount(0);
  await expect(undo).toBeDisabled();
  await expect(redo).toHaveAccessibleName("Redo add Reverb");
  expect(await save()).not.toContain("builtin:reverb");

  // And done again: the effect comes back with the id it had, so that what was done to it finds it, and the knob is where it was turned.
  await page.keyboard.press("ControlOrMeta+Shift+Z");
  await expect(reverb).toBeVisible();
  await expect(redo).toHaveAccessibleName("Redo set damping");
  await redo.click();
  await expect(reverb.getByRole("slider", { name: "damping", exact: true })).toHaveAttribute("aria-valuenow", turned);
  await expect(redo).toBeDisabled();
  const saved = await save();
  expect(saved).toContain("builtin:reverb");
  expect(Number(/^damping\s*=\s*(\S+)$/m.exec(saved)?.[1])).toBeCloseTo(Number(turned), 5);
});

test("a cable that is taken out is put back by Undo with the depth it had, and taken out again by Redo, and the saved file has what is done", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  const save = async (): Promise<string> => {
    await page.getByRole("button", { name: /^Save/ }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };
  const depth = page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" });
  const remove = page.getByRole("button", { name: "Remove cable LFO → Filter: cutoff" });

  // The cable is turned first, so that what comes back is the cable as it was and not one as a new cable is.
  await expect(depth).toHaveValue("0.4");
  await depth.focus();
  await page.keyboard.press("End");
  await expect(depth).toHaveValue("1");
  await expect(undo).toHaveAccessibleName("Undo set the depth of a cable");
  expect(await save()).toContain('param = "cutoff"');

  // Taken out with its button: it is a step of its own, and the file that is saved then has no cable.
  await remove.click();
  await expect(depth).toHaveCount(0);
  await expect(undo).toHaveAccessibleName("Undo remove a cable");
  expect(await save()).not.toContain('param = "cutoff"');

  // Put back by Undo: the cable has the depth it had, and what was done before it is the next to be undone.
  await undo.click();
  await expect(depth).toHaveValue("1");
  await expect(undo).toHaveAccessibleName("Undo set the depth of a cable");
  await expect(redo).toHaveAccessibleName("Redo remove a cable");
  expect(await save()).toContain('param = "cutoff"');

  // Taken out again by Redo, and put back by the keys; then the depth goes back to where it was before it was turned.
  await redo.click();
  await expect(depth).toHaveCount(0);
  expect(await save()).not.toContain('param = "cutoff"');
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(depth).toHaveValue("1");
  await undo.click();
  await expect(depth).toHaveValue("0.4");
  await expect(undo).toBeDisabled();
});

test("a modulator that is taken out is put back by Undo with its cable and the depth the cable had, and taken out again by Redo, and the saved file has what is done", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  const save = async (): Promise<string> => {
    await page.getByRole("button", { name: /^Save/ }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };
  const lfo = node(page, "LFO");
  const depth = page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" });

  // The cable is turned first, so that what comes back is the cable as it was and not one as a new cable is.
  await expect(depth).toHaveValue("0.4");
  await depth.focus();
  await page.keyboard.press("End");
  await expect(depth).toHaveValue("1");
  const before = await save();
  expect(before).toContain("[[instrument.modulator]]");

  // Taken out with its button, the modulator goes with its cable: it is a step of its own, and the file that is saved then has neither.
  await page.getByRole("button", { name: "Remove LFO" }).click();
  await expect(lfo).toHaveCount(0);
  await expect(depth).toHaveCount(0);
  await expect(undo).toHaveAccessibleName("Undo remove an LFO");
  const taken = await save();
  expect(taken).not.toContain("[[instrument.modulator]]");

  // Put back by Undo: it is where it was with its cable at the depth it had, and the file is the one that was saved before it went.
  await undo.click();
  await expect(lfo).toBeVisible();
  await expect(depth).toHaveValue("1");
  await expect(undo).toHaveAccessibleName("Undo set the depth of a cable");
  await expect(redo).toHaveAccessibleName("Redo remove an LFO");
  expect(await save()).toBe(before);

  // Taken out again by Redo, and put back by the keys; then the depth goes back to where it was before it was turned.
  await redo.click();
  await expect(lfo).toHaveCount(0);
  expect(await save()).toBe(taken);
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(lfo).toBeVisible();
  await expect(depth).toHaveValue("1");
  await undo.click();
  await expect(depth).toHaveValue("0.4");
  await expect(undo).toBeDisabled();
});

test("an effect that is taken out is put back by Undo with the values it had and the cable that drove it, and taken out again by Redo, and the saved file has what is done", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  const save = async (): Promise<string> => {
    await page.getByRole("button", { name: /^Save/ }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };
  const filter = node(page, "Filter");
  const resonance = filter.getByRole("slider", { name: "resonance", exact: true });
  const depth = page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" });

  // A value of the effect and the depth of the cable that drives it are turned first, so that what comes back is the effect as it was set and not as a new one is.
  const first = (await resonance.getAttribute("aria-valuenow")) ?? "";
  await resonance.focus();
  await page.keyboard.press("End");
  const turned = (await resonance.getAttribute("aria-valuenow")) ?? "";
  expect(turned).not.toBe(first);
  await depth.focus();
  await page.keyboard.press("End");
  await expect(depth).toHaveValue("1");
  const before = await save();
  expect(before).toContain("builtin:filter");

  // Taken out with its button, the effect goes with the cable that drives it: it is a step of its own, and the file that is saved then has neither.
  await page.getByRole("button", { name: "Remove Filter", exact: true }).click();
  await expect(filter).toHaveCount(0);
  await expect(depth).toHaveCount(0);
  await expect(undo).toHaveAccessibleName("Undo remove Filter");
  const taken = await save();
  expect(taken).not.toContain("builtin:filter");
  expect(taken).not.toContain('param = "cutoff"');

  // Put back by Undo: it is in its place with the values it had and its cable at the depth it had, and the file is the one that was saved before it went.
  await undo.click();
  await expect(filter).toBeVisible();
  await expect(resonance).toHaveAttribute("aria-valuenow", turned);
  await expect(depth).toHaveValue("1");
  await expect(undo).toHaveAccessibleName("Undo set the depth of a cable");
  await expect(redo).toHaveAccessibleName("Redo remove Filter");
  expect(await save()).toBe(before);

  // Taken out again by Redo, and put back by the keys; then the cable's depth and the effect's value go back to where they were before they were turned.
  await redo.click();
  await expect(filter).toHaveCount(0);
  expect(await save()).toBe(taken);
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(filter).toBeVisible();
  await expect(depth).toHaveValue("1");
  await undo.click();
  await expect(depth).toHaveValue("0.4");
  await undo.click();
  await expect(resonance).toHaveAttribute("aria-valuenow", first);
  await expect(undo).toBeDisabled();
});

test("a lane that is taken out is put back by Undo with its instrument, its effect and its place, and taken out again by Redo, and the saved file has what is done", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  const save = async (): Promise<string> => {
    await page.getByRole("button", { name: /^Save/ }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };

  // A second lane with an effect and a note range of its own (a session keeps its last lane, so there has to be one to take out).
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menuitem", { name: "Oscillator" }).click();
  const second = node(page, "Oscillator 2");
  await expect(second).toBeVisible();
  await page.getByRole("button", { name: "Add an effect to Oscillator 2" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  await expect(node(page, "Reverb")).toBeVisible();
  const detune = second.getByRole("slider", { name: "detune", exact: true });
  await detune.focus();
  await page.keyboard.press("End");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  const before = await save();
  expect(before.match(/\[\[instrument\]\]/g)).toHaveLength(2);

  // Taken out from its menu, which asks first: the lane goes with its effect, and is one step.
  await page.getByRole("button", { name: "Remove the lane of Oscillator 2" }).click();
  await page.getByRole("menuitem", { name: /^Remove the lane of Oscillator 2 for good/ }).click();
  await expect(second).toHaveCount(0);
  await expect(node(page, "Reverb")).toHaveCount(0);
  await expect(undo).toHaveAccessibleName("Undo remove the lane of Oscillator");
  const taken = await save();
  expect(taken.match(/\[\[instrument\]\]/g)).toHaveLength(1);

  // Put back by Undo: the lane, its effect and what was set on it, and the file is the one that was saved before it went.
  await undo.click();
  await expect(second).toBeVisible();
  await expect(node(page, "Reverb")).toBeVisible();
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  await expect(undo).toHaveAccessibleName("Undo set detune");
  await expect(redo).toHaveAccessibleName("Redo remove the lane of Oscillator");
  expect(await save()).toBe(before);

  // Taken out again by Redo, and put back by the keys; then what was done before it is put back, one step at a time, to the lane that was added.
  await redo.click();
  await expect(second).toHaveCount(0);
  expect(await save()).toBe(taken);
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(second).toBeVisible();
  await undo.click();
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await undo.click();
  await expect(node(page, "Reverb")).toHaveCount(0);
  await undo.click();
  await expect(second).toHaveCount(0);
  await expect(undo).toBeDisabled();
});

test("a group that is taken out is put back by Undo with its member and its bus effect, and taken out again by Redo, and the saved file has what is done", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  const save = async (): Promise<string> => {
    await page.getByRole("button", { name: /^Save/ }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };
  const group = node(page, "Group 1");
  const choice = page.getByRole("combobox", { name: "Group of Oscillator" });

  // A group with the lane in it, and a reverb on its bus.
  await page.getByRole("button", { name: "Add a group" }).click();
  await expect(group).toBeVisible();
  await choice.selectOption({ label: "Group 1" });
  await expect(choice.locator("option:checked")).toHaveText("Group 1");
  await group.getByRole("button", { name: "Add an effect to Group 1" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  await expect(node(page, "Reverb")).toBeVisible();
  const before = await save();
  expect(before).toContain("[[group]]");
  expect(before).toContain("group = 0");

  // Taken out from its menu, which asks first: the group goes with its reverb, the lane stays in none, and it is one step.
  await group.getByRole("button", { name: "Remove Group 1" }).click();
  await page.getByRole("menuitem", { name: /^Remove Group 1 for good/ }).click();
  await expect(group).toHaveCount(0);
  await expect(node(page, "Reverb")).toHaveCount(0);
  await expect(choice.locator("option:checked")).toHaveText("none");
  await expect(undo).toHaveAccessibleName("Undo remove Group 1");
  const taken = await save();
  expect(taken).not.toContain("[[group]]");
  expect(taken).not.toContain("group = 0");

  // Put back by Undo: the group, its reverb and its member, and the file is the one that was saved before it went.
  await undo.click();
  await expect(group).toBeVisible();
  await expect(node(page, "Reverb")).toBeVisible();
  await expect(choice.locator("option:checked")).toHaveText("Group 1");
  await expect(undo).toHaveAccessibleName("Undo add Reverb");
  await expect(redo).toHaveAccessibleName("Redo remove Group 1");
  expect(await save()).toBe(before);

  // Taken out again by Redo, and put back by the keys; then what was done before it is put back, one step at a time, to the group that was added.
  await redo.click();
  await expect(group).toHaveCount(0);
  expect(await save()).toBe(taken);
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(group).toBeVisible();
  await undo.click();
  await expect(node(page, "Reverb")).toHaveCount(0);
  await undo.click();
  await expect(choice.locator("option:checked")).toHaveText("none");
  await undo.click();
  await expect(group).toHaveCount(0);
  await expect(undo).toBeDisabled();
});

test("a preset that is loaded is put back by Undo with the preset and the values the plugin had, and loaded again by Redo, and the saved file has what is done", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  const save = async (): Promise<string> => {
    await page.getByRole("button", { name: /^Save/ }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };

  // A reverb (the tracer has none) with a preset loaded and a value set over it: what comes back is the plugin as it was, and not as a preset leaves it.
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  const decay = node(page, "Reverb").getByRole("slider", { name: "decay" });
  await page.getByRole("button", { name: "Load a preset into Reverb" }).click();
  await page.getByRole("menuitem", { name: "Vintage Plate" }).click();
  await expect(decay).toHaveAttribute("aria-valuenow", "0.8");
  await expect(undo).toHaveAccessibleName("Undo load preset Vintage Plate");
  await decay.focus();
  await page.keyboard.press("End");
  await expect(decay).toHaveAttribute("aria-valuenow", "0.99");
  const before = await save();
  expect(before).toContain('preset = "Vintage Plate"');

  // Another preset is a step of its own: it has its values, and the one that was set over the first is gone.
  await page.getByRole("button", { name: "Load a preset into Reverb" }).click();
  await page.getByRole("menuitem", { name: "Small Room" }).click();
  await expect(decay).toHaveAttribute("aria-valuenow", "0.55");
  await expect(undo).toHaveAccessibleName("Undo load preset Small Room");
  const loaded = await save();
  expect(loaded).toContain('preset = "Small Room"');
  expect(loaded).not.toContain("Vintage Plate");

  // Put back by Undo: the preset it had, and the decay set over it, and the file is the one that was saved before it was loaded.
  await undo.click();
  await expect(decay).toHaveAttribute("aria-valuenow", "0.99");
  await expect(undo).toHaveAccessibleName("Undo set decay");
  await expect(redo).toHaveAccessibleName("Redo load preset Small Room");
  expect(await save()).toBe(before);

  // Loaded again by Redo, and put back by the keys; then what was done before it is put back, one step at a time.
  await redo.click();
  await expect(decay).toHaveAttribute("aria-valuenow", "0.55");
  expect(await save()).toBe(loaded);
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(decay).toHaveAttribute("aria-valuenow", "0.99");
  await undo.click();
  await expect(decay).toHaveAttribute("aria-valuenow", "0.8");
  await undo.click();
  await expect(decay).toHaveAttribute("aria-valuenow", "0.88");
  await undo.click();
  await expect(node(page, "Reverb")).toHaveCount(0);
  await expect(undo).toBeDisabled();
});

test("an instrument that is replaced is put back by Undo with the values it had and the cable that drove it, and replaced again by Redo, and the saved file has what is done", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  const save = async (): Promise<string> => {
    await page.getByRole("button", { name: /^Save/ }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };
  const oscillator = node(page, "Oscillator");
  const waveform = oscillator.getByRole("combobox", { name: "waveform" });
  const detune = oscillator.getByRole("slider", { name: "detune", exact: true });
  const depth = page.getByRole("slider", { name: "Depth of LFO → Oscillator: detune" });

  // The instrument has a value of its own set and a cable from the LFO to its detune, at a depth that is turned: what comes back is what was, and not what a new one is.
  await expect(waveform).toHaveValue("3");
  await detune.focus();
  await page.keyboard.press("End");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  const picker = page.getByRole("button", { name: "Add a cable from LFO" });
  await picker.focus();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.type("det");
  await page.keyboard.press("Enter");
  await expect(depth).toBeVisible();
  await depth.focus();
  await page.keyboard.press("End");
  await expect(depth).toHaveValue("1");
  const before = await save();
  expect(before).toContain("builtin:osc");

  // Another instrument put in the lane from its menu is a step of its own: it starts as a sine at its defaults, the cable that drove the old one is gone, and the filter stays.
  await page.getByRole("button", { name: "Change the instrument of Oscillator" }).click();
  await page.getByRole("menu", { name: "Change the instrument of Oscillator" }).getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(waveform).toHaveValue("0");
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await expect(depth).toHaveCount(0);
  await expect(node(page, "Filter")).toBeVisible();
  await expect(undo).toHaveAccessibleName("Undo change the instrument to Oscillator");
  const replaced = await save();
  expect(replaced).not.toBe(before);

  // Put back by Undo: the instrument has its saw, its detune and its cable at the depth it had, and the file is the one that was saved before it went.
  await undo.click();
  await expect(waveform).toHaveValue("3");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  await expect(depth).toHaveValue("1");
  await expect(undo).toHaveAccessibleName("Undo set the depth of a cable");
  await expect(redo).toHaveAccessibleName("Redo change the instrument to Oscillator");
  expect(await save()).toBe(before);

  // Replaced again by Redo, and put back by the keys; then what was done before it is put back, one step at a time.
  await redo.click();
  await expect(waveform).toHaveValue("0");
  expect(await save()).toBe(replaced);
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(waveform).toHaveValue("3");
  await expect(depth).toHaveValue("1");
  await undo.click();
  await expect(depth).toHaveValue("0.5");
  await undo.click();
  await expect(depth).toHaveCount(0);
  await undo.click();
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await expect(undo).toBeDisabled();
});

test("the mock engine in the page has the same buttons and keys, and they do what the program's do", async ({ page }) => {
  await open(page);
  const undo = page.getByRole("button", { name: /^Undo/ });
  const redo = page.getByRole("button", { name: /^Redo/ });
  await expect(undo).toBeDisabled();
  const detune = node(page, "Oscillator").getByRole("slider", { name: "detune", exact: true });
  await detune.focus();
  await page.keyboard.press("End");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  await expect(undo).toHaveAccessibleName("Undo set detune");
  await undo.click();
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  await expect(redo).toHaveAccessibleName("Redo set detune");
  // (Control+Y, on a Mac too: Command+Y is the browser's own History.)
  await page.keyboard.press("Control+Y");
  await expect(detune).toHaveAttribute("aria-valuenow", "24");
  await page.keyboard.press("ControlOrMeta+Z");
  await expect(detune).toHaveAttribute("aria-valuenow", "0");
  // What is added is taken out and put back too.
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  await expect(node(page, "Reverb")).toBeVisible();
  await expect(undo).toHaveAccessibleName("Undo add Reverb");
  await undo.click();
  await expect(node(page, "Reverb")).toHaveCount(0);
  await redo.click();
  await expect(node(page, "Reverb")).toBeVisible();
  // And so is a cable that is taken out.
  const depth = page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" });
  await page.getByRole("button", { name: "Remove cable LFO → Filter: cutoff" }).click();
  await expect(depth).toHaveCount(0);
  await expect(undo).toHaveAccessibleName("Undo remove a cable");
  await undo.click();
  await expect(depth).toHaveValue("0.4");
  await redo.click();
  await expect(depth).toHaveCount(0);
  // And a modulator, with what it drives.
  await undo.click();
  await expect(depth).toHaveValue("0.4");
  await page.getByRole("button", { name: "Remove LFO" }).click();
  await expect(node(page, "LFO")).toHaveCount(0);
  await expect(depth).toHaveCount(0);
  await expect(undo).toHaveAccessibleName("Undo remove an LFO");
  await undo.click();
  await expect(node(page, "LFO")).toBeVisible();
  await expect(depth).toHaveValue("0.4");
  await redo.click();
  await expect(node(page, "LFO")).toHaveCount(0);
  // And an effect, with what drives it.
  await undo.click();
  await expect(depth).toHaveValue("0.4");
  await page.getByRole("button", { name: "Remove Filter", exact: true }).click();
  await expect(node(page, "Filter")).toHaveCount(0);
  await expect(depth).toHaveCount(0);
  await expect(undo).toHaveAccessibleName("Undo remove Filter");
  await undo.click();
  await expect(node(page, "Filter")).toBeVisible();
  await expect(depth).toHaveValue("0.4");
  await redo.click();
  await expect(node(page, "Filter")).toHaveCount(0);
  // And a preset that is loaded, with the values the plugin had.
  await undo.click();
  await expect(node(page, "Filter")).toBeVisible();
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  const decay = node(page, "Reverb 2").getByRole("slider", { name: "decay" });
  const first = (await decay.getAttribute("aria-valuenow")) ?? "";
  await page.getByRole("button", { name: "Load a preset into Reverb 2" }).click();
  await page.getByRole("menuitem", { name: "Small Room" }).click();
  await expect(decay).toHaveAttribute("aria-valuenow", "0.55");
  await expect(undo).toHaveAccessibleName("Undo load preset Small Room");
  await undo.click();
  await expect(decay).toHaveAttribute("aria-valuenow", first);
  await redo.click();
  await expect(decay).toHaveAttribute("aria-valuenow", "0.55");
  // And an instrument that is replaced, with the cables that drove it. (The canvas went to the reverb that was added: the whole of it is in the view again.)
  await undo.click();
  await undo.click();
  await expect(node(page, "Reverb 2")).toHaveCount(0);
  await page.locator(".react-flow__controls-fitview").click();
  const waveform = node(page, "Oscillator").getByRole("combobox", { name: "waveform" });
  await expect(waveform).toHaveValue("3");
  await page.getByRole("button", { name: "Change the instrument of Oscillator" }).click();
  await page.getByRole("menu", { name: "Change the instrument of Oscillator" }).getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(waveform).toHaveValue("0");
  await expect(undo).toHaveAccessibleName("Undo change the instrument to Oscillator");
  await undo.click();
  await expect(waveform).toHaveValue("3");
  await redo.click();
  await expect(waveform).toHaveValue("0");
  // And a lane, with what it has.
  await undo.click();
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  await page.getByRole("button", { name: "Remove the lane of Oscillator 2" }).click();
  await page.getByRole("menuitem", { name: /^Remove the lane of Oscillator 2 for good/ }).click();
  await expect(node(page, "Oscillator 2")).toHaveCount(0);
  await expect(undo).toHaveAccessibleName("Undo remove the lane of Oscillator");
  await undo.click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  await redo.click();
  await expect(node(page, "Oscillator 2")).toHaveCount(0);
  // And a group, with what it has.
  await undo.click();
  await page.getByRole("button", { name: "Add a group" }).click();
  await expect(node(page, "Group 1")).toBeVisible();
  await node(page, "Group 1").getByRole("button", { name: "Remove Group 1" }).click();
  await page.getByRole("menuitem", { name: /^Remove Group 1 for good/ }).click();
  await expect(node(page, "Group 1")).toHaveCount(0);
  await expect(undo).toHaveAccessibleName("Undo remove Group 1");
  await undo.click();
  await expect(node(page, "Group 1")).toBeVisible();
  await redo.click();
  await expect(node(page, "Group 1")).toHaveCount(0);
});
