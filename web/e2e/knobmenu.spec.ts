import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { addEnvelope, cables, centre, drag, expectNodesApart, node, open, output, port, setting } from "./canvas";

// Modulating a control from the control, against the mock engine in the page: the button on a knob (and on a row of the panel and of a modulator's cables),
// and its menu of a new LFO, a new envelope or a new MIDI control that learns, made for the control with a cable to it in one change, and of the
// modulators that are there, which get a cable.

/** The cell of the control called `name` in a node. */
const cell = (plugin: Locator, name: string): Locator => plugin.locator(".tang-cell").filter({ has: plugin.page().locator(`[aria-label="${name}"]`) });

/** The button that modulates the control called `name`. */
const modulate = (plugin: Locator, name: string): Locator => plugin.getByRole("button", { name: `Modulate ${name}`, exact: true });

/** The names of the items of the menu that is open, in order. */
const items = (page: Page): Promise<string[]> => page.getByRole("menuitem").allTextContents();

const undo = (page: Page, what: string): Locator => page.getByRole("button", { name: `Undo ${what}`, exact: true });

test("the button is quiet until the pointer or the focus is on the control, and a control that is chosen from a list has none", async ({ page }) => {
  await open(page);
  const filter = node(page, "Filter");
  const resonance = modulate(filter, "resonance");
  await expect(resonance).toHaveCSS("opacity", "0");
  await cell(filter, "resonance").hover();
  await expect(resonance).toHaveCSS("opacity", "1");
  // It is over the corner of its own knob, and what is drawn at its centre is the button: not the control beside it, which a cell's corner is next to.
  expect(await resonance.evaluate((button) => {
    const box = button.getBoundingClientRect();
    return document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2) === button;
  })).toBe(true);
  // It is at the top right corner of the knob: a few pixels above it, and not over the control beside it.
  const corner = await resonance.evaluate((button) => {
    const knob = button.closest(".tang-cell")?.querySelector(".tang-knob")?.getBoundingClientRect();
    const box = button.getBoundingClientRect();
    return knob ? { above: knob.top - box.top, inside: knob.right - box.right } : null;
  });
  expect(corner?.above).toBeGreaterThanOrEqual(0);
  expect(corner?.above).toBeLessThanOrEqual(8);
  expect(Math.abs(corner?.inside ?? 99)).toBeLessThanOrEqual(1);
  // The pointer goes away; the knob, focused, shows it as well.
  await page.mouse.move(5, 500);
  await expect(resonance).toHaveCSS("opacity", "0");
  await filter.getByRole("slider", { name: "resonance", exact: true }).focus();
  await expect(resonance).toHaveCSS("opacity", "1");
  // The waveform of the oscillator, the type of the filter and the wave of the LFO are chosen from lists: a cable can be dragged to them, and no button is drawn.
  await expect(modulate(node(page, "Oscillator"), "waveform")).toHaveCount(0);
  await expect(modulate(filter, "type")).toHaveCount(0);
  await expect(page.getByRole("button", { name: /^Modulate wave/i })).toHaveCount(0);
  // The knobs have it: the oscillator's seven, the filter's three and the LFO's rate (the filter's mix is no parameter of the plugin), and the row of the LFO's cable.
  await expect(page.getByRole("button", { name: /^Modulate / })).toHaveCount(7 + 3 + 1 + 1);
});

test("a cable let go over the button of a control is drawn to that control: the buttons give way while a cable is dragged", async ({ page }) => {
  await open(page);
  const oscillator = node(page, "Oscillator");
  await expect(cables(page)).toHaveCount(1);
  const target = await centre(modulate(oscillator, "volume"));
  await drag(page, output(node(page, "LFO")), target);
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of LFO → Oscillator: volume" })).toHaveValue("0.5");
  await expect(page.locator(".tang-notice")).toHaveCount(0);
  // The buttons are there again when the cable is dropped.
  await cell(oscillator, "pan").hover();
  await modulate(oscillator, "pan").click();
  await expect(page.getByRole("menu", { name: "Modulate pan" })).toBeVisible();
});

test("a cable let go over the button of a row of cables is drawn to the depth of that cable", async ({ page }) => {
  await open(page);
  const envelope = await addEnvelope(page);
  const button = node(page, "LFO").locator(".tang-cable-row").getByRole("button", { name: "Modulate depth of LFO → Filter: cutoff", exact: true });
  await drag(page, output(envelope), await centre(button));
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of ADSR → depth of LFO → Filter: cutoff" })).toHaveValue("0.5");
  await expect(page.locator(".tang-notice")).toHaveCount(0);
});

test("a new LFO for a knob is made with a cable to it in one change: the node, the cable and the port that is driven, and one Undo takes them out", async ({ page }) => {
  await open(page);
  const filter = node(page, "Filter");
  await expect(cables(page)).toHaveCount(1);
  await cell(filter, "resonance").hover();
  await modulate(filter, "resonance").click();
  await expect(page.getByRole("menu", { name: "Modulate resonance" })).toBeVisible();
  expect(await items(page)).toEqual(["New LFO", "New envelope", "New macro", "New MIDI control", "New velocity", "New key", "LFO"]);
  await page.getByRole("menuitem", { name: "New LFO" }).click();

  // The tracer's LFO and the new one are numbered, and the new one drives the resonance at half depth.
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(node(page, "LFO 1")).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
  await expect(port(filter, "resonance")).toHaveClass(/tang-port--driven/);
  await expect(page.getByRole("slider", { name: "Depth of LFO 2 → Filter: resonance" })).toHaveValue("0.5");
  await expect(page.locator(".tang-notice")).toHaveCount(0);
  await expectNodesApart(page);

  // It is one thing that was done: one Undo takes the node and its cable out, and one Redo makes them again.
  await expect(undo(page, "modulate resonance")).toBeVisible();
  await undo(page, "modulate resonance").click();
  await expect(node(page, "LFO 2")).toHaveCount(0);
  await expect(node(page, "LFO")).toBeVisible();
  await expect(cables(page)).toHaveCount(1);
  await expect(port(filter, "resonance")).not.toHaveClass(/tang-port--driven/);
  await page.getByRole("button", { name: "Redo modulate resonance", exact: true }).click();
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of LFO 2 → Filter: resonance" })).toHaveValue("0.5");
});

test("a new MIDI control is armed to learn and its cable is at the whole range, and Undo says it learns", async ({ page }) => {
  await open(page);
  const oscillator = node(page, "Oscillator");
  await cell(oscillator, "detune").hover();
  await modulate(oscillator, "detune").click();
  await page.getByRole("menuitem", { name: "New MIDI control" }).click();
  const control = node(page, "MIDI \\(learning…\\)");
  await expect(control).toBeVisible();
  await expect(page.getByRole("slider", { name: "Depth of MIDI (learning…) → Oscillator: detune" })).toHaveValue("1");
  await expect(port(oscillator, "detune")).toHaveClass(/tang-port--driven/);
  await expect(undo(page, "learn detune")).toBeVisible();
  await undo(page, "learn detune").click();
  await expect(control).toHaveCount(0);
  await expect(cables(page)).toHaveCount(1);
});

test("a modulator that is there is attached from the menu: a cable and no new node, and the one that drives the knob already is not offered", async ({ page }) => {
  await open(page);
  await addEnvelope(page);
  const filter = node(page, "Filter");
  // (The tracer's LFO drives the cutoff, so for the cutoff the envelope is the one that is offered.)
  await cell(filter, "cutoff").hover();
  await modulate(filter, "cutoff").click();
  expect(await items(page)).toEqual(["New LFO", "New envelope", "New macro", "New MIDI control", "New velocity", "New key", "ADSR"]);
  await page.keyboard.press("Escape");
  await expect(modulate(filter, "cutoff")).toBeFocused();

  await cell(filter, "resonance").hover();
  await modulate(filter, "resonance").click();
  expect(await items(page)).toEqual(["New LFO", "New envelope", "New macro", "New MIDI control", "New velocity", "New key", "LFO", "ADSR"]);
  await page.getByRole("menuitem", { name: "ADSR" }).click();
  await expect(cables(page)).toHaveCount(2);
  await expect(page.getByRole("slider", { name: "Depth of ADSR → Filter: resonance" })).toHaveValue("0.5");
  await expect(page.locator(".tang-node__name", { hasText: /^ADSR$/ })).toHaveCount(1);
  await expect(undo(page, "draw a cable")).toBeVisible();
});

test("the rate of an LFO is modulated from its knob: the new envelope drives it, is a sibling in the lane, and is laid out above it", async ({ page }) => {
  await open(page);
  const lfo = node(page, "LFO");
  await cell(lfo, "Rate").hover();
  await modulate(lfo, "rate").click();
  await page.getByRole("menuitem", { name: "New envelope" }).click();
  const envelope = node(page, "ADSR");
  await expect(envelope).toBeVisible();
  await expect(setting(lfo, "rate")).toHaveClass(/tang-port--driven/);
  await expect(page.getByRole("slider", { name: "Depth of ADSR → LFO: rate" })).toHaveValue("0.5");
  const [above, below] = [await envelope.boundingBox(), await lfo.boundingBox()];
  if (!above || !below) throw new Error("not on the page");
  expect(above.y + above.height).toBeLessThanOrEqual(below.y + 1);
  await expectNodesApart(page);
  await expect(undo(page, "modulate rate")).toBeVisible();
});

test("the row of a cable of a modulator has the button: the depth of the cable is modulated by a new modulator", async ({ page }) => {
  await open(page);
  const lfo = node(page, "LFO");
  const row = lfo.locator(".tang-cable-row");
  await row.hover();
  await row.getByRole("button", { name: "Modulate depth of LFO → Filter: cutoff", exact: true }).click();
  await page.getByRole("menuitem", { name: "New envelope" }).click();
  await expect(node(page, "ADSR")).toBeVisible();
  await expect(row).toHaveClass(/tang-cable-row--driven/);
  await expect(page.getByRole("slider", { name: "Depth of ADSR → depth of LFO → Filter: cutoff" })).toHaveValue("0.5");
  await expect(undo(page, "modulate depth")).toBeVisible();
  await expect(lfo.locator(".tang-cable-row")).toHaveCount(1);
});

test("the panel's rows have the button, and a parameter that no knob shows is modulated from there, and then has its knob", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  const reverb = node(page, "Reverb");
  await expect(reverb).toBeVisible();
  await expect(reverb.getByRole("slider", { name: "lowcut" })).toHaveCount(0);
  await page.getByRole("button", { name: "Parameters of Reverb" }).click();
  const panel = page.getByRole("complementary", { name: "Parameters of Reverb" });
  await page.keyboard.type("low");
  await expect(panel.getByText("1 of 10 parameters")).toBeVisible();
  await panel.getByRole("button", { name: "Modulate lowcut", exact: true }).click();
  await page.getByRole("menuitem", { name: "New LFO" }).click();
  // The lowcut is driven now, which is a parameter that its node shows (a cable that ends at a parameter is shown, as long as it is there).
  await expect(node(page, "LFO 2")).toBeVisible();
  await expect(reverb.getByRole("slider", { name: "lowcut" })).toBeVisible();
  await expect(page.getByRole("slider", { name: "Depth of LFO 2 → Reverb: lowcut" })).toHaveValue("0.5");
});

test("the menu is used from the keyboard: the button is reached from the knob, an arrow opens the menu, Enter chooses, and the focus is on the button again", async ({ page }) => {
  await open(page);
  const filter = node(page, "Filter");
  await filter.getByRole("slider", { name: "resonance", exact: true }).focus();
  await page.keyboard.press("Tab");
  await expect(modulate(filter, "resonance")).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(page.getByRole("menuitem", { name: "New LFO" })).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(node(page, "ADSR")).toBeVisible();
  await expect(modulate(filter, "resonance")).toBeFocused();
  await expect(page.getByRole("slider", { name: "Depth of ADSR → Filter: resonance" })).toHaveValue("0.5");
});
