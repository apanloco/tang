import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

import { test } from "./own";

// The keys of a lane from the page, on the real program (a program of its own: the lane's keys are in the file it saves).

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

test("the keys of a lane are chosen on its node: a note outside them is silent and one inside is heard, and the saved file has them", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const meter = page.getByRole("meter", { name: "Master level" });
  const level = async (): Promise<number> => Number(await meter.getAttribute("aria-valuenow"));
  const lowest = page.getByRole("combobox", { name: "Lowest key of Oscillator" });
  const highest = page.getByRole("combobox", { name: "Highest key of Oscillator" });

  // The lane takes every note to begin with: both ends say any, and the lowest key of the computer (Z, a C) is heard.
  await expect(lowest).toHaveValue("");
  await expect(highest).toHaveValue("");
  await expect(lowest.locator("option:checked")).toHaveText("any");
  await page.keyboard.down("z");
  await expect.poll(level, { message: "the note is heard" }).toBeGreaterThan(-50);
  await page.keyboard.up("z");
  await expect.poll(level, { message: "the master is quiet again" }).toBeLessThan(-50);

  // C4 to B4 are chosen on the node, one end at a time: what is chosen shows at once, and the session is unsaved.
  await lowest.selectOption({ label: "C4" });
  await expect(lowest.locator("option:checked")).toHaveText("C4");
  await expect(highest.locator("option:checked")).toHaveText("any");
  await highest.selectOption({ label: "B4" });
  await expect(highest.locator("option:checked")).toHaveText("B4");
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  // (A choice has the focus, and a key typed in it is not for the piano: the focus goes to the page.)
  await page.getByRole("status").click();

  // Z is the C below the keys: the lane does not play it, and the master stays quiet. Q is the C4: inside, and heard.
  await page.keyboard.down("z");
  expect(await loudest(page, 800), "a note below the keys is silent").toBeLessThan(-50);
  await page.keyboard.up("z");
  await page.keyboard.down("q");
  await expect.poll(level, { message: "a note inside the keys is heard" }).toBeGreaterThan(-50);
  await page.keyboard.up("q");
  await expect.poll(level, { message: "the master is quiet again" }).toBeLessThan(-50);

  // What is saved has the keys as the TUI writes them.
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(own.file, "utf8")).toContain('range = "C4-B4"');

  // Any at both ends is every note again, and the note that was silent is heard.
  await lowest.selectOption({ label: "any" });
  await highest.selectOption({ label: "any" });
  await expect(lowest.locator("option:checked")).toHaveText("any");
  await expect(highest.locator("option:checked")).toHaveText("any");
  await page.getByRole("status").click();
  await page.keyboard.down("z");
  await expect.poll(level, { message: "the note that was outside the keys is heard" }).toBeGreaterThan(-50);
  await page.keyboard.up("z");
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(own.file, "utf8")).not.toContain("range =");
});
