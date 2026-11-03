import { readFileSync } from "node:fs";

import { expect } from "@playwright/test";

import { test } from "./own";

// Presets from the page, on the real program (a program of its own: a reverb is put in the lane, which the shared one would keep).

test("a preset is loaded into a reverb from its menu: the knobs take its values, the menu marks it and opens on it, and the saved file names it", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const reverb = page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: /^Reverb$/ }) });

  // The tracer has no plugin with presets: the oscillator and the filter have none, and the page offers nothing to load.
  await expect(page.getByRole("button", { name: /^Load a preset into / })).toHaveCount(0);

  // A reverb in the lane's chain (the built-ins are always installed): it has presets, and so a button to load them.
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  await expect(reverb).toBeVisible();
  const decay = reverb.getByRole("slider", { name: "decay" });
  await expect(decay).toHaveAttribute("aria-valuenow", "0.88");
  await page.getByRole("button", { name: "Load a preset into Reverb" }).click();
  const menu = page.getByRole("menu", { name: "Load a preset into Reverb" });
  await expect(menu.getByRole("menuitem")).toHaveText(["Arcadia Dream Hall", "Cathedral", "Vintage Plate", "Small Room", "Ambient Wash"]);
  // None is the preset yet: the menu opens on the first.
  await expect(menu.getByRole("menuitem").first()).toBeFocused();
  await expect(menu.locator('[aria-current="true"]')).toHaveCount(0);

  // The small room has a decay of 0.55, a size of 0.4 and a pre-delay of 0.01: the knobs say so as soon as the server says it is loaded.
  await menu.getByRole("menuitem", { name: "Small Room" }).click();
  await expect(decay).toHaveAttribute("aria-valuenow", "0.55");
  await expect(reverb.getByRole("slider", { name: "size" })).toHaveAttribute("aria-valuenow", "0.4");
  await expect(reverb.getByRole("slider", { name: "predelay" })).toHaveAttribute("aria-valuenow", "0.01");
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await expect(page.getByRole("button", { name: "Load a preset into Reverb" })).toBeFocused();

  // The menu marks the one that is loaded, and opens on it.
  await page.getByRole("button", { name: "Load a preset into Reverb" }).click();
  await expect(menu.locator('[aria-current="true"]')).toHaveText("Small Room");
  await expect(menu.getByRole("menuitem", { name: "Small Room" })).toBeFocused();
  // Another preset by the keyboard alone: a letter finds it, Enter loads it.
  await page.keyboard.press("c");
  await expect(menu.getByRole("menuitem", { name: "Cathedral" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(decay).toHaveAttribute("aria-valuenow", "0.96");
  await expect(menu).toHaveCount(0);

  // What is saved names the preset, and has none of its values as the user's own: a knob turned afterwards is.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved).toContain('preset = "Cathedral"');
  expect(saved).not.toContain("decay = 0.96");
});
