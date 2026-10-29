import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { cables, drag, node, open, output, port, showAll, still } from "./canvas";
import { modulatorsIn, testWithGroups } from "./own";

// The values that the host keeps, driven by a modulator as a plugin's parameters are (docs/web-ui.md D97, step 97; the user's "I cannot modulate everything, for example mix and some volume, is that fixable?"):
// the volume of a lane that has an instrument, the mix of an effect, and the volume and the pan of a group. Their knobs have a port on the top edge where a cable ends and a "~" button, as a parameter's knob has,
// and a cable to one has the ring and the dot that a parameter's has. The first tests are against the mock engine in the page, the last against the real program (what the file keeps, that the knob moves on the engine's own
// value, that Undo and a page opened again have it).

/** The knob called `name` of a node (a node can have several of one name: a lane's volume and its instrument's own, which is not capitalised). */
const knobOf = (plugin: Locator, name: string): Locator => plugin.locator(".tang-knob", { has: plugin.page().getByRole("slider", { name, exact: true }) });

/** The button that modulates the control called `name` of a node. */
const modulate = (plugin: Locator, name: string): Locator => plugin.getByRole("button", { name: `Modulate ${name}`, exact: true });

/** The cell of the control called `name` in a node. */
const cell = (plugin: Locator, name: string): Locator => plugin.locator(".tang-cell").filter({ has: plugin.page().locator(`[aria-label="${name}"]`) });

/** The button that says whose notes a modulator hears. */
const hears = (modulator: Locator): Locator => modulator.getByRole("button", { name: /^Whose notes / });

// (The page has no way to make a group: the real program is started on the tracer's file with a group, and the mock opens its groups scene.)
const test = testWithGroups(1);

/** The group that the session has, with the lane of the tracer put in it: its node, which has a volume and a pan. */
async function intoTheGroup(page: Page): Promise<Locator> {
  const group = node(page, "Group 1");
  await expect(group).toBeVisible();
  await page.getByRole("combobox", { name: "Group of Oscillator" }).selectOption({ label: "Group 1" });
  await expect(page.getByRole("combobox", { name: "Group of Oscillator" }).locator("option:checked")).toHaveText("Group 1");
  await still(group);
  return group;
}

mock("a cable is drawn from a modulator to the mix of an effect and to the volume of a lane, and each knob has the ring of what it can be swung, as a parameter's has", async ({ page }) => {
  await open(page);
  await showAll(page);
  const lfo = node(page, "LFO");
  const filter = node(page, "Filter");
  const oscillator = node(page, "Oscillator");
  await still(lfo);
  await still(filter);
  await still(oscillator);
  await expect(cables(page)).toHaveCount(1);
  // Nothing drives the mix or the volume yet: a port on each knob, and no ring.
  await expect(knobOf(filter, "Mix").locator(".tang-knob__ring")).toHaveCount(0);
  await expect(knobOf(oscillator, "Volume").locator(".tang-knob__ring")).toHaveCount(0);

  await drag(page, output(lfo), port(filter, "Mix"));
  await expect(cables(page)).toHaveCount(2);
  await expect(knobOf(filter, "Mix").locator(".tang-knob__ring")).toHaveCount(1);
  // (The cable is told by where it goes, in the words that the notices and the history use.)
  await expect(lfo.getByText("Filter: mix")).toBeVisible();
  await still(lfo);
  await drag(page, output(lfo), port(oscillator, "Volume"));
  await expect(cables(page)).toHaveCount(3);
  await expect(knobOf(oscillator, "Volume").locator(".tang-knob__ring")).toHaveCount(1);
  await expect(lfo.getByText("Oscillator: output volume")).toBeVisible();
  // The oscillator's own parameter called volume is another control and has no cable.
  await expect(knobOf(oscillator, "volume").locator(".tang-knob__ring")).toHaveCount(0);

  // Undo takes the last cable out, and its ring goes with it.
  await page.getByRole("button", { name: /^Undo / }).click();
  await expect(cables(page)).toHaveCount(2);
  await expect(knobOf(oscillator, "Volume").locator(".tang-knob__ring")).toHaveCount(0);
  await expect(knobOf(filter, "Mix").locator(".tang-knob__ring")).toHaveCount(1);
});

mock("the \"~\" button of the mix knob makes a modulator that hears the lane of the effect, with a cable to the mix, and offers the modulators that are there to attach", async ({ page }) => {
  await open(page);
  await showAll(page);
  const filter = node(page, "Filter");
  await still(filter);
  await cell(filter, "Mix").hover();
  await modulate(filter, "mix").click();
  // (The tracer's LFO is the session's own and reaches the mix as every plugin's value: it is among those to attach, and the new ones are above it.)
  await expect(page.getByRole("group", { name: "Attach" }).getByRole("menuitem", { name: "LFO", exact: true })).toBeVisible();
  await page.getByRole("menuitem", { name: "New envelope", exact: true }).click();
  const envelope = node(page, "ADSR");
  await expect(envelope).toBeVisible();
  await expect(hears(envelope)).toHaveAttribute("aria-label", "Whose notes ADSR hears: Oscillator");
  await expect(cables(page)).toHaveCount(2);
  await expect(envelope.getByText("Filter: mix")).toBeVisible();
  await expect(knobOf(filter, "Mix").locator(".tang-knob__ring")).toHaveCount(1);
  // It drives the mix, so it is not offered there again, and the tracer's LFO still is.
  await cell(filter, "Mix").hover();
  await modulate(filter, "mix").click();
  await expect(page.getByRole("group", { name: "Attach" }).getByRole("menuitem", { name: "ADSR", exact: true })).toHaveCount(0);
  await expect(page.getByRole("group", { name: "Attach" }).getByRole("menuitem", { name: "LFO", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
});

mock("a group has a volume and a pan that a modulator drives, and a modulator that is made from the pan knob hears the group", async ({ page }) => {
  await open(page, "groups");
  const group = await intoTheGroup(page);
  await showAll(page);
  const lfo = node(page, "LFO");
  await still(lfo);
  await expect(knobOf(group, "Pan").locator(".tang-knob__ring")).toHaveCount(0);
  await drag(page, output(lfo), port(group, "Pan"));
  await expect(cables(page)).toHaveCount(2);
  await expect(knobOf(group, "Pan").locator(".tang-knob__ring")).toHaveCount(1);
  await expect(lfo.getByText("Group 1: pan")).toBeVisible();
  // Another, from the "~" button of the volume: a modulator that hears the group.
  await cell(group, "Volume").hover();
  await modulate(group, "volume").click();
  await page.getByRole("menuitem", { name: "New LFO", exact: true }).click();
  const second = node(page, "LFO 2");
  await expect(second).toBeVisible();
  await expect(hears(second)).toHaveAttribute("aria-label", "Whose notes LFO 2 hears: Group 1");
  await expect(cables(page)).toHaveCount(3);
  await expect(knobOf(group, "Volume").locator(".tang-knob__ring")).toHaveCount(1);
});

test("a cable to a mix, a volume and a pan, in the real program: the knobs move on the engine's own values, the file names what each drives, and Undo and a page opened again have them", async ({ page, own }) => {
  // (A good many waits, and three cables.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  /** Where the knob called `name` of the node `of` is, as the fraction of its sweep that its dot is at, looked at on every frame for `ms` milliseconds (the dot is the engine's own value). */
  const dots = (of: Locator, name: string, ms: number): Promise<number[]> =>
    of.locator(`[role="slider"][aria-label="${name}"]`).evaluate(
      (slider, duration) =>
        new Promise<number[]>((resolve) => {
          const dot = slider.closest(".tang-knob")?.querySelector(".tang-knob__dot") ?? null;
          const found: number[] = [];
          const start = performance.now();
          const look = (now: number): void => {
            if (dot?.getAttribute("visibility") === "visible") {
              const x = Number(dot.getAttribute("cx")) - 24;
              const y = 24 - Number(dot.getAttribute("cy"));
              found.push(((Math.atan2(x, y) * 180) / Math.PI + 135) / 270);
            }
            if (now - start < duration) requestAnimationFrame(look);
            else resolve(found);
          };
          requestAnimationFrame(look);
        }),
      ms,
    );
  const spread = (values: number[]): number => Math.max(...values) - Math.min(...values);
  const saved = async (): Promise<string> => {
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };

  const group = await intoTheGroup(page);
  await showAll(page);
  const lfo = node(page, "LFO");
  const filter = node(page, "Filter");
  const oscillator = node(page, "Oscillator");
  await still(lfo);
  await still(filter);
  await still(oscillator);
  await expect(cables(page)).toHaveCount(1);

  // A cable to the group's pan, to the filter's mix and to the lane's volume, each from the tracer's LFO (a sine of 0.5 Hz): each knob has a ring.
  await drag(page, output(lfo), port(group, "Pan"));
  await expect(cables(page)).toHaveCount(2);
  await still(lfo);
  await drag(page, output(lfo), port(filter, "Mix"));
  await expect(cables(page)).toHaveCount(3);
  await still(lfo);
  await drag(page, output(lfo), port(oscillator, "Volume"));
  await expect(cables(page)).toHaveCount(4);
  await expect(knobOf(group, "Pan").locator(".tang-knob__ring")).toHaveCount(1);
  await expect(knobOf(filter, "Mix").locator(".tang-knob__ring")).toHaveCount(1);
  await expect(knobOf(oscillator, "Volume").locator(".tang-knob__ring")).toHaveCount(1);

  // The dots are the engine's own values: the pan, which the user set to the middle, is swung to the two sides (a depth of a half of its range of -1 to 1 is the whole of the knob) and the mix goes down from its top and back.
  const pan = await dots(group, "Pan", 2600);
  expect(pan.length).toBeGreaterThan(20);
  expect(spread(pan), "the group's pan moves").toBeGreaterThan(0.5);
  const mix = await dots(filter, "Mix", 2600);
  expect(mix.length).toBeGreaterThan(20);
  expect(spread(mix), "the filter's mix moves").toBeGreaterThan(0.2);

  // The file has the three cables in the tracer's own LFO's table, each naming whose value it drives by its place in the session and which of its values it is.
  let file = await saved();
  const table = modulatorsIn(file)[0] ?? "";
  expect(table.match(/\[\[modulator\.target\]\]/g)).toHaveLength(4);
  expect(table).toMatch(/to = "group\[0\]"\ngain = "pan"\ndepth = /);
  expect(table).toMatch(/to = "instrument\[0\]\.effect\[0\]"\ngain = "mix"\ndepth = /);
  expect(table).toMatch(/to = "instrument\[0\]"\ngain = "volume"\ndepth = /);

  // Undo takes the last cable out (the lane's volume), and the file has no more than the others; Redo draws it again.
  await page.getByRole("button", { name: /^Undo / }).click();
  await expect(cables(page)).toHaveCount(3);
  await expect(knobOf(oscillator, "Volume").locator(".tang-knob__ring")).toHaveCount(0);
  expect(modulatorsIn(await saved())[0]?.match(/\[\[modulator\.target\]\]/g)).toHaveLength(3);
  await page.getByRole("button", { name: /^Redo / }).click();
  await expect(cables(page)).toHaveCount(4);
  await expect(knobOf(oscillator, "Volume").locator(".tang-knob__ring")).toHaveCount(1);

  // A page opened again has them: the session is the server's, and the file that the program saves is read as it is written.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(cables(page)).toHaveCount(4);
  await expect(knobOf(node(page, "Group 1"), "Pan").locator(".tang-knob__ring")).toHaveCount(1);
  await expect(knobOf(node(page, "Filter"), "Mix").locator(".tang-knob__ring")).toHaveCount(1);
  file = await saved();
  expect(modulatorsIn(file)[0]?.match(/gain = "/g)).toHaveLength(3);
});
