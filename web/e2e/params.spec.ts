import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";

import { test } from "./own";

// All the parameters of a plugin from the page, on the real program (a program of its own: a reverb is put in the lane, which the shared one would keep).

test("a parameter that its node has no knob for is found in the panel by its name and turned, and the saved file has it", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");

  // A reverb has ten parameters, and its node shows the first eight as knobs: "lowcut" and "level" are not among them.
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  const reverb = page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: /^Reverb$/ }) });
  await expect(reverb).toBeVisible();
  await expect(reverb.getByRole("slider", { name: "width" })).toBeVisible();
  await expect(reverb.getByRole("slider", { name: "lowcut" })).toHaveCount(0);

  // Its button opens the panel, with the focus in the filter.
  await page.getByRole("button", { name: "Parameters of Reverb" }).click();
  const panel = page.getByRole("complementary", { name: "Parameters of Reverb" });
  await expect(panel.getByRole("searchbox", { name: "Filter the parameters of Reverb" })).toBeFocused();
  await expect(panel.getByText("10 parameters")).toBeVisible();
  await expect(panel.getByRole("slider")).toHaveCount(10);

  // Typed in the filter (letters that are keys of the piano), one is left, with its range and value in its own unit.
  await page.keyboard.type("low");
  await expect(panel.getByText("1 of 10 parameters")).toBeVisible();
  const lowcut = panel.getByRole("slider", { name: "lowcut" });
  await expect(panel.getByRole("slider")).toHaveCount(1);
  await expect(lowcut).toHaveValue("80");
  await expect(panel.getByText("80 Hz")).toBeVisible();

  // Turned by the keyboard: the page says so at once, the whole range at the end key, a hundredth of it (4.8 Hz) on an arrow.
  await lowcut.focus();
  await page.keyboard.press("End");
  await expect(lowcut).toHaveValue("500");
  await expect(panel.getByText("500 Hz")).toBeVisible();
  await page.keyboard.press("ArrowLeft");
  await expect(panel.getByText("495 Hz")).toBeVisible();
  await page.keyboard.press("End");
  await expect(panel.getByText("500 Hz")).toBeVisible();
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // What is saved has it as the user's own setting of the reverb.
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(own.file, "utf8")).toMatch(/lowcut\s*=\s*500/);

  // Escape in the panel closes it, and the focus is on the button that opened it.
  await lowcut.focus();
  await page.keyboard.press("Escape");
  await expect(panel).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Parameters of Reverb" })).toBeFocused();
});
