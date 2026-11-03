import { expect } from "@playwright/test";
import type { Locator } from "@playwright/test";

import { node } from "./canvas";
import { test } from "./own";

// The built-in drive from the page, on the real program (a program of its own: an effect is put in the lane, which the shared one would keep). It starts fully wet, its type is a choice
// of four shapes, its four presets set its four knobs, and it is in the audio path: a level of nothing is silence, and a level is what is heard.

/** The knobs of the drive's node, each at the value the page says. */
async function expectKnobs(plugin: Locator, values: Record<string, string>): Promise<void> {
  for (const [name, value] of Object.entries(values)) await expect(plugin.getByRole("slider", { name, exact: true })).toHaveAttribute("aria-valuenow", value);
}

test("a drive from the page starts fully wet at its own values, has four presets that set its four knobs, and is in the audio path: its level is what is heard", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  // The tracer's lane is quiet on purpose (its volume is a tenth); it is put back to 1 so that what the drive does is well over what a meter calls silence.
  await node(page, "Oscillator").getByRole("slider", { name: "Volume", exact: true }).dblclick();

  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Drive" }).click();
  const drive = node(page, "Drive");
  await expect(drive).toBeVisible();
  // Its parameters with what the plugin starts at, and the mix that it is added at: all the way wet, since a drive is put in a chain as a filter is (its latency would put the dry sound out of step).
  await expect(drive.getByRole("combobox", { name: "type" })).toHaveValue("0");
  await expectKnobs(drive, { drive: "0.35", tone: "1", level: "0.5", Mix: "1" });

  // Its presets, in the menu of its node: each sets the type and the three knobs.
  await page.getByRole("button", { name: "Load a preset into Drive" }).click();
  const menu = page.getByRole("menu", { name: "Load a preset into Drive" });
  await expect(menu.getByRole("menuitem")).toHaveText(["Warm", "Crunch", "Fuzz", "Wavefolder"]);
  await menu.getByRole("menuitem", { name: "Wavefolder" }).click();
  await expect(drive.getByRole("combobox", { name: "type" })).toHaveValue("3");
  await expectKnobs(drive, { drive: "0.55", tone: "1", level: "0.5" });
  await page.getByRole("button", { name: "Load a preset into Drive" }).click();
  await menu.getByRole("menuitem", { name: "Fuzz" }).click();
  await expect(drive.getByRole("combobox", { name: "type" })).toHaveValue("1");
  await expectKnobs(drive, { drive: "0.85", tone: "0.6", level: "0.25" });

  // The type is a choice, which a person can turn by hand: the shape that was a hard clip is a tube.
  await drive.getByRole("combobox", { name: "type" }).selectOption({ label: "tube" });
  await expect(drive.getByRole("combobox", { name: "type" })).toHaveValue("2");

  // In the audio path: with the level all the way down a key is silence on the master, and with it all the way up it is heard.
  const meter = page.getByRole("meter", { name: "Master level" });
  const level = async (): Promise<number> => Number(await meter.getAttribute("aria-valuenow"));
  const gain = drive.getByRole("slider", { name: "level", exact: true });
  await gain.focus();
  await page.keyboard.press("Home");
  await expect(gain).toHaveAttribute("aria-valuenow", "0");
  await page.keyboard.down("z");
  // (The meter's silence is -60 dB. Read 0.2 s after the key it said -51 dB and at 0.4 s -60: it holds something from the first moments of the note, probably from before the level
  // was in the audio thread, and lets go of it. It is waited for until it has, and not read after a fixed time: the meter falls by the frames that the page is given, and a machine that is
  // loaded delays them (it read -58.6 dB after 1.2 s in one run of the suite, with the rest of the machine busy).)
  await expect.poll(level, { message: "a level of nothing is silence, with a note held" }).toBeLessThan(-59);
  await page.keyboard.up("z");
  await gain.focus();
  await page.keyboard.press("End");
  await expect(gain).toHaveAttribute("aria-valuenow", "1");
  await page.keyboard.down("z");
  await expect.poll(level, { message: "and a level of 1 is heard" }).toBeGreaterThan(-40);
  await page.keyboard.up("z");
});
