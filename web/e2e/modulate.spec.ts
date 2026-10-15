import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { addEnvelope, cables, node } from "./canvas";
import { test } from "./own";

// Modulating a control from the control, on the real program (a program of its own: the file it saves is read, and the history is the program's): a new LFO
// or a new MIDI control made for a knob is in the server's session and the file as one change, which one Undo takes out and one Redo makes again, and a
// second page sees it; a modulator that is there is attached by the same menu.

async function open(page: Page, url: string): Promise<void> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.getByRole("slider", { name: "Depth of LFO → Filter: cutoff" })).toBeVisible();
}

/** The cell of the control called `name` in a node, and the button on it that modulates it. */
const cell = (plugin: Locator, name: string): Locator => plugin.locator(".tang-cell").filter({ has: plugin.page().locator(`[aria-label="${name}"]`) });
const modulate = (plugin: Locator, name: string): Locator => plugin.getByRole("button", { name: `Modulate ${name}`, exact: true });

async function choose(plugin: Locator, name: string, item: string): Promise<void> {
  await cell(plugin, name).hover();
  await modulate(plugin, name).click();
  await plugin.page().getByRole("menuitem", { name: item, exact: true }).click();
}

const save = async (page: Page, file: string): Promise<string> => {
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  return readFileSync(file, "utf8");
};

/** What the file has for the cables that start at the modulators of the lane, one string for each modulator, as written. */
const modulators = (saved: string): string[] => saved.split("[[instrument.modulator]]\n").slice(1).map((part) => part.split("[[instrument.effect]]")[0] ?? "");

test("a new LFO made from a knob is in the server's session and the saved file, one Undo takes it out and one Redo makes it again, and a second page sees what is done", async ({ page, browser, own }) => {
  await open(page, own.url);
  const filter = node(page, "Filter");
  await choose(filter, "resonance", "New LFO");
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of LFO 2 → Filter: resonance" })).toHaveValue("0.5");
  await expect(page.getByRole("button", { name: "Undo modulate resonance", exact: true })).toBeVisible();

  // The server has it: a page that is opened again draws it, from the session the server tells.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(page.getByRole("slider", { name: "Depth of LFO 2 → Filter: resonance" })).toHaveValue("0.5");

  // The file has it: a second modulator of the lane, an LFO at its default rate, with a cable to the filter's resonance at half depth.
  const saved = await save(page, own.file);
  expect(modulators(saved)).toHaveLength(2);
  expect(modulators(saved)[1]).toBe('waveform = "sine"\nrate = 1.0\n\n[[instrument.modulator.target]]\nparam = "resonance"\nslot = 1\ndepth = 0.5\n\n');

  // A second page has the same history, and what it does is on the first as what anybody does.
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(own.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect(other.getByRole("button", { name: "Undo modulate resonance", exact: true })).toBeVisible();

  // One Undo takes the modulator and its cable out together, and the file is as it was; one Redo makes both again, under the same node.
  await page.getByRole("button", { name: "Undo modulate resonance", exact: true }).click();
  await expect(node(page, "LFO 2")).toHaveCount(0);
  await expect(cables(page)).toHaveCount(1);
  await expect(node(other, "LFO 2")).toHaveCount(0);
  expect(modulators(await save(page, own.file))).toHaveLength(1);
  await expect(page.getByRole("button", { name: "Redo modulate resonance", exact: true })).toBeVisible();
  await other.getByRole("button", { name: "Redo modulate resonance", exact: true }).click();
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of LFO 2 → Filter: resonance" })).toHaveValue("0.5");
  expect(modulators(await save(page, own.file))[1]).toContain('param = "resonance"');
  await other.close();
});

test("a new MIDI control made from a knob is armed to learn on the program and its cable is at the whole range, which the file keeps as a control that is not bound", async ({ page, own }) => {
  await open(page, own.url);
  const oscillator = node(page, "Oscillator");
  await choose(oscillator, "detune", "New MIDI control");
  const control = node(page, "MIDI \\(learning…\\)");
  await expect(control).toBeVisible();
  await expect(control.getByText("learning", { exact: false })).toBeVisible();
  await expect(page.getByRole("slider", { name: "Depth of MIDI (learning…) → Oscillator: detune" })).toHaveValue("1");
  await expect(page.getByRole("button", { name: "Undo learn detune", exact: true })).toBeVisible();

  // (No controller is moved here: a program with no MIDI port cannot be told one. The control waits, and the file has it, unbound, to be armed again when it is loaded.)
  const saved = await save(page, own.file);
  expect(modulators(saved)[1]).toBe('type = "midi"\n\n[[instrument.modulator.target]]\nparam = "detune"\ndepth = 1.0\n\n');

  await page.getByRole("button", { name: "Undo learn detune", exact: true }).click();
  await expect(control).toHaveCount(0);
  await expect(cables(page)).toHaveCount(1);
  expect(modulators(await save(page, own.file))).toHaveLength(1);
  await page.getByRole("button", { name: "Redo learn detune", exact: true }).click();
  await expect(control).toBeVisible();
  await expect(page.getByRole("slider", { name: "Depth of MIDI (learning…) → Oscillator: detune" })).toHaveValue("1");
});

test("a modulator that is there is attached by the same menu, with a cable and no new node, as one step of its own", async ({ page, own }) => {
  await open(page, own.url);
  await addEnvelope(page);
  const filter = node(page, "Filter");
  await choose(filter, "resonance", "ADSR");
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of ADSR → Filter: resonance" })).toHaveValue("0.5");
  await expect(page.locator(".tang-node__name", { hasText: /^ADSR$/ })).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Undo draw a cable", exact: true })).toBeVisible();
  const saved = await save(page, own.file);
  expect(modulators(saved)).toHaveLength(2);
  expect(modulators(saved)[1]).toContain('param = "resonance"');
  await page.getByRole("button", { name: "Undo draw a cable", exact: true }).click();
  await expect(cables(page)).toHaveCount(1);
  await expect(page.locator(".tang-node__name", { hasText: /^ADSR$/ })).toHaveCount(1);
});
