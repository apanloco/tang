import { expect } from "@playwright/test";

import { cables, drag, node, output, port, still } from "./canvas";
import { test } from "./own";
import { knob, sampleCutoff } from "./support";

// The ring of a knob where a lane's modulator and its group's both drive one parameter, on the real program (a program of its own, which the test puts a group
// into; on the null audio backend). The engine applies a group's modulators before its lanes render and a lane's own later, each from the value that the user
// set, so what a lane's modulators make of the parameter stands and the group's offset is not added to it: the ring is where the lane's modulators swing it, and
// the dot, which is the engine's own value, goes no further than the ring and as far as it.

test("the ring of a parameter that a lane's LFO and its group's envelope both drive is the LFO's alone, and the dot goes as far as it and no further", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const ringOf = (name: string) => knob(page, name).locator(".tang-knob__ring");
  // The tracer's LFO drives the cutoff, which is set to 0.5, at a depth of 0.4: the ring is from 0.1 to 0.9. The resonance has none.
  await expect(ringOf("cutoff")).toHaveCount(1);
  const lfoAlone = await ringOf("cutoff").getAttribute("d");
  await expect(ringOf("resonance")).toHaveCount(0);
  await expect(cables(page)).toHaveCount(1);

  // A group with the lane in it, and an envelope in the group.
  await page.getByRole("button", { name: "Add a group", exact: true }).click();
  await expect(node(page, "Group 1")).toBeVisible();
  await page.getByRole("combobox", { name: "Group of Oscillator" }).selectOption({ label: "Group 1" });
  await expect(page.getByRole("combobox", { name: "Group of Oscillator" }).locator("option:checked")).toHaveText("Group 1");
  await page.getByRole("button", { name: "Add a modulator to Group 1" }).click();
  await page.getByRole("menuitem", { name: "Envelope" }).click();
  const envelope = node(page, "ADSR");
  await expect(envelope).toBeVisible();
  await still(envelope);
  const filter = node(page, "Filter");
  await still(filter);

  // Its cable to the resonance, which no modulator of the lane drives: the group's cable counts there, and the knob has a ring.
  await drag(page, output(envelope), port(filter, "resonance"));
  await expect(cables(page)).toHaveCount(2);
  await expect(ringOf("resonance")).toHaveCount(1);

  // Its cable to the cutoff, which the lane's LFO drives too: the ring is what it was.
  await drag(page, output(envelope), port(filter, "cutoff"));
  await expect(cables(page)).toHaveCount(3);
  expect(await ringOf("cutoff").getAttribute("d")).toBe(lfoAlone);

  // And the engine does what the ring says: over a whole turn of the LFO (two seconds) the cutoff goes from 0.1 to 0.9, as it did before the group's cable. (The
  // two added would take the idle envelope's quarter off the low end and stop at 0.)
  const dots = (await sampleCutoff(page, 2600)).flatMap((sample) => (sample.dot === null ? [] : [sample.dot]));
  expect(dots.length).toBeGreaterThan(20);
  expect(Math.min(...dots)).toBeCloseTo(0.1, 1);
  expect(Math.max(...dots)).toBeCloseTo(0.9, 1);
});
