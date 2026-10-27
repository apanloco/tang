import { expect } from "@playwright/test";

import { cables, drag, node, output, port, still } from "./canvas";
import { test } from "./own";
import { knob, sampleCutoff } from "./support";

// The ring of a knob where two modulators drive one parameter, a modulator that hears a lane and one that hears its group, on the real program (a program of its own,
// which the test puts a group into; on the null audio backend). Every modulator is the session's own (step 94: the program moves a loaded session's lane and group
// modulators into the session's rack, and what the page makes is of the session too), so that what two of them do to one parameter is added, as the engine adds
// the offsets of the session's own (before step 94 a lane's modulator won over a group's, which were not added). The ring is where they swing it together, and the
// dot, which is the engine's own value, goes no further than the ring.

/** The two ends of the arc that a ring is drawn as: where it starts and where it stops, as the path of the ring says them. */
const arcOf = (d: string | null): { from: string; to: string } => {
  const parts = (d ?? "").split(" ");
  return { from: parts.slice(1, 3).join(" "), to: parts.slice(-2).join(" ") };
};

test("the ring of a parameter that a lane's LFO and a group's envelope both drive is the two added: the LFO's, taken down by the idle envelope's quarter, and the dot goes as far as it and no further", async ({ page, own }) => {
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

  // Its cable to the cutoff, which the lane's LFO drives too: the two are added. The envelope is at its peak when it is open, where it adds nothing, so the top of the ring is where it was, and
  // its low end is lower: the envelope at rest takes a quarter off (the user's 0.5, half of the way down to the bottom, at a depth of a half), which takes the ring from 0.1 down to nothing.
  await drag(page, output(envelope), port(filter, "cutoff"));
  await expect(cables(page)).toHaveCount(3);
  await expect(async () => {
    expect(arcOf(await ringOf("cutoff").getAttribute("d")).from).not.toBe(arcOf(lfoAlone).from);
  }).toPass();
  expect(arcOf(await ringOf("cutoff").getAttribute("d")).to).toBe(arcOf(lfoAlone).to);

  // And the engine does what the ring says: over a whole turn of the LFO (two seconds) the cutoff, with the envelope at rest, goes from the bottom (the sum goes below it, and is held to it) to 0.65
  // (0.5 and the LFO's 0.4 and the envelope's quarter off): the two added, where a lane's modulator alone had it go from 0.1 to 0.9.
  const dots = (await sampleCutoff(page, 2600)).flatMap((sample) => (sample.dot === null ? [] : [sample.dot]));
  expect(dots.length).toBeGreaterThan(20);
  expect(Math.min(...dots)).toBeCloseTo(0, 1);
  expect(Math.max(...dots)).toBeCloseTo(0.65, 1);
});
