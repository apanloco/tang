import { expect } from "@playwright/test";

import { node } from "./canvas";
import { test } from "./own";

// The panic from the page, on the real program (a program of its own, on the null audio backend: the oscillator's volume is put back to 1, so that a note is well over what a meter calls
// silence). A note that is held is ended by the button while the key is still down, which is what a note that hangs is, and the keys play again after it.

test("the Panic button ends a note that is held while the key is still down, and the key that is let go of after it owes nothing: the next note is heard", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await node(page, "Oscillator").getByRole("slider", { name: "Volume", exact: true }).dblclick();

  const meter = page.getByRole("meter", { name: "Master level" });
  const level = async (): Promise<number> => Number(await meter.getAttribute("aria-valuenow"));
  await page.keyboard.down("z");
  await expect.poll(level, { message: "a note that is held is heard" }).toBeGreaterThan(-40);

  await page.getByRole("button", { name: "Panic" }).click();
  // (The meter holds a peak for a moment and lets go of it, as it does after any note, so it is read until it has: its silence is -60 dB.)
  await expect.poll(level, { message: "and after the panic it is not, though the key is down" }).toBeLessThan(-59);

  // The key is let go of after the panic and pressed again: the note ended by the panic is not ended twice, and the new one sounds.
  await page.keyboard.up("z");
  await expect.poll(level, { message: "the key's release changes nothing" }).toBeLessThan(-59);
  await page.keyboard.down("z");
  await expect.poll(level, { message: "and the key plays again" }).toBeGreaterThan(-40);
  await page.keyboard.up("z");
});
