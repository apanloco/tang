import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

import { test } from "./own";

// The piano of the page, on the real program (a program of its own: the scale and the lock are in the file it saves).

/** The loudest the master's meter is for `ms`, in decibels (its floor is -60). */
async function loudest(page: Page, ms: number): Promise<number> {
  const meter = page.getByRole("meter", { name: "Master level" });
  const until = Date.now() + ms;
  let top = Number.NEGATIVE_INFINITY;
  while (Date.now() < until) {
    top = Math.max(top, Number(await meter.getAttribute("aria-valuenow")));
    await page.waitForTimeout(25);
  }
  return top;
}

test("the piano is under the patch: a key played with the pointer lights and sounds, the scale colours the keys, the lock drops the notes outside it, and the saved file has both", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const level = async (): Promise<number> => Number(await page.getByRole("meter", { name: "Master level" }).getAttribute("aria-valuenow"));
  const key = (note: number) => page.locator(`[data-note="${String(note)}"]`);
  await expect(page.getByRole("group", { name: "Piano keys" })).toBeVisible();

  // Six octaves at this width, from the one below the keys of the computer; named as the TUI names them.
  await expect(page.locator("[data-note]")).toHaveCount(72);
  await expect(key(60)).toHaveAccessibleName("C4");
  // C major to begin with: C is the root, D is in the scale, C# is not.
  await expect(key(60)).toHaveAttribute("data-role", "root");
  await expect(key(62)).toHaveAttribute("data-role", "scale");
  await expect(key(61)).toHaveAttribute("data-role", "off");
  // The page and the piano are one: the piano is below the patch, which keeps the room that is left.
  const [patch, piano] = await Promise.all([page.locator(".tang-app__patch").boundingBox(), page.getByRole("region", { name: "Piano" }).boundingBox()]);
  expect((patch?.y ?? 0) + (patch?.height ?? 0)).toBeLessThanOrEqual(piano?.y ?? 0);

  // A key pressed with the mouse is lit while it is down, sounds, and ends when it comes up.
  const box = await key(60).boundingBox();
  if (!box) throw new Error("C4 has no box");
  await page.mouse.move(box.x + box.width / 2, box.y + box.height - 14);
  await page.mouse.down();
  await expect(key(60)).toHaveAttribute("data-held", "");
  await expect.poll(level, { message: "the note is heard" }).toBeGreaterThan(-50);
  // Slid along to E4 (64), the note of C4 ends and E4 plays: a glissando.
  const target = await key(64).boundingBox();
  if (!target) throw new Error("E4 has no box");
  await page.mouse.move(target.x + target.width / 2, target.y + target.height - 14, { steps: 12 });
  await expect(key(64)).toHaveAttribute("data-held", "");
  await expect(key(60)).not.toHaveAttribute("data-held", "");
  await page.mouse.up();
  await expect(key(64)).not.toHaveAttribute("data-held", "");
  await expect.poll(level, { message: "the master is quiet again" }).toBeLessThan(-50);

  // The keys of the computer light the piano too (Q is the C of the octave above the lower row: C4), and the bracket keys move the window.
  await page.keyboard.down("q");
  await expect(key(60)).toHaveAttribute("data-held", "");
  await page.keyboard.up("q");
  await expect(key(60)).not.toHaveAttribute("data-held", "");
  await page.keyboard.press("]");
  await expect(key(24)).toHaveCount(0);
  await expect(key(96)).toHaveCount(1);
  await page.keyboard.press("[");

  // The scale F# dorian, chosen on the piano: F# is the root, C# is in it, and C is not.
  await page.getByRole("combobox", { name: "Scale root" }).selectOption({ label: "F#" });
  await page.getByRole("combobox", { name: "Scale kind" }).selectOption({ label: "Dorian" });
  await expect(key(66)).toHaveAttribute("data-role", "root");
  await expect(key(61)).toHaveAttribute("data-role", "scale");
  await expect(key(60)).toHaveAttribute("data-role", "off");
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // Not locked, a note outside the scale is played, lit as the wrong note...
  await page.keyboard.down("q");
  await expect(key(60)).toHaveAttribute("data-held", "");
  await expect.poll(level, { message: "a note outside the scale is heard while the piano is not locked" }).toBeGreaterThan(-50);
  await page.keyboard.up("q");
  await expect.poll(level, { message: "the master is quiet again" }).toBeLessThan(-50);

  // ...and locked, it is dropped where it is sent: not heard, not lit, whichever way it is played; one inside the scale is played.
  await page.getByRole("button", { name: "Lock to scale" }).click();
  await expect(page.getByRole("button", { name: "Lock to scale" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("group", { name: "Piano keys" })).toHaveAttribute("data-locked", "");
  await page.keyboard.down("q");
  expect(await loudest(page, 800), "a note outside the scale is silent").toBeLessThan(-50);
  await expect(key(60)).not.toHaveAttribute("data-held", "");
  await page.keyboard.up("q");
  const outside = await key(60).boundingBox();
  if (!outside) throw new Error("C4 has no box");
  await page.mouse.move(outside.x + outside.width / 2, outside.y + outside.height - 14);
  await page.mouse.down();
  expect(await loudest(page, 800), "a key outside the scale does nothing under the pointer").toBeLessThan(-50);
  await page.mouse.up();
  // (Digit 2 is the C# above, which is in the scale.)
  await page.keyboard.down("2");
  await expect(key(61)).toHaveAttribute("data-held", "");
  await expect.poll(level, { message: "a note inside the scale is heard" }).toBeGreaterThan(-50);
  await page.keyboard.up("2");

  // What is saved has the scale and the lock, as the TUI writes them.
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved).toContain("[piano]");
  expect(saved).toContain('scale = "F# dorian"');
  expect(saved).toContain("locked = true");
});
