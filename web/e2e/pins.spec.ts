import { readFileSync } from "node:fs";

import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { node, open } from "./canvas";
import { test as own } from "./own";

// Pinning parameters to a node: the pins in a plugin's panel decide which of its parameters the node shows as knobs, a node shows the first
// eight until something is pinned, and what is pinned is the session's (the program keeps it, saves it by name and tells every page).

/** A reverb put in the oscillator's lane from its menu: the built-in one has ten parameters, and its node shows the first eight. */
async function addReverb(page: Page): Promise<Locator> {
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Reverb" }).click();
  const reverb = node(page, "Reverb");
  await expect(reverb).toBeVisible();
  return reverb;
}

const pinOf = (panel: Locator, name: string): Locator => panel.getByRole("button", { name: `Show ${name} on the node` });
const giveBack = (panel: Locator): Locator => panel.getByRole("button", { name: "Show the first eight parameters of Reverb again" });

test("a parameter is pinned to a node from its panel, taken off with the keyboard, and the node is given back to the first eight", async ({ page }) => {
  await open(page);
  const reverb = await addReverb(page);
  await expect(reverb.getByRole("slider", { name: "width" })).toBeVisible();
  await expect(reverb.getByRole("slider", { name: "lowcut" })).toHaveCount(0);

  await page.getByRole("button", { name: "Parameters of Reverb" }).click();
  const panel = page.getByRole("complementary", { name: "Parameters of Reverb" });
  await panel.getByRole("searchbox").fill("low");
  await expect(pinOf(panel, "lowcut")).toHaveAttribute("aria-pressed", "false");
  await expect(giveBack(panel)).toHaveCount(0);

  // Pinned, it is a knob on the node and the first eight stay.
  await pinOf(panel, "lowcut").click();
  await expect(pinOf(panel, "lowcut")).toHaveAttribute("aria-pressed", "true");
  await expect(reverb.getByRole("slider", { name: "lowcut" })).toBeVisible();
  await expect(reverb.getByRole("slider", { name: "width" })).toBeVisible();
  await expect(giveBack(panel)).toBeVisible();

  // A pin is a button: the keyboard takes one off, and the focus stays on it.
  await panel.getByRole("searchbox").fill("width");
  await pinOf(panel, "width").focus();
  await page.keyboard.press("Space");
  await expect(reverb.getByRole("slider", { name: "width" })).toHaveCount(0);
  await expect(pinOf(panel, "width")).toHaveAttribute("aria-pressed", "false");
  await expect(pinOf(panel, "width")).toBeFocused();
  await expect(reverb.getByRole("slider", { name: "lowcut" })).toBeVisible();

  // And the button under the count gives the node back to the first eight.
  await giveBack(panel).click();
  await expect(reverb.getByRole("slider", { name: "width" })).toBeVisible();
  await expect(reverb.getByRole("slider", { name: "lowcut" })).toHaveCount(0);
  await expect(giveBack(panel)).toHaveCount(0);
});

own("what is pinned to a node is the program's: a second page and a page that is opened again have it, and the saved file has it by name, in order", async ({ page, browser, own: tang }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(tang.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const reverb = await addReverb(page);
  await expect(reverb.getByRole("slider", { name: "lowcut" })).toHaveCount(0);
  await page.getByRole("button", { name: "Parameters of Reverb" }).click();
  const panel = page.getByRole("complementary", { name: "Parameters of Reverb" });
  await panel.getByRole("searchbox").fill("low");
  await pinOf(panel, "lowcut").click();
  await expect(reverb.getByRole("slider", { name: "lowcut" })).toBeVisible();
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // A second page is given it in its snapshot, and so is a page that is opened again; the node has the first eight and the new one on both.
  const other = await browser.newPage();
  await other.setViewportSize({ width: 1600, height: 1000 });
  await other.goto(tang.url);
  await expect(other.getByRole("status")).toHaveText("Connected");
  await expect(node(other, "Reverb").getByRole("slider", { name: "lowcut" })).toBeVisible();
  await expect(node(other, "Reverb").getByRole("slider", { name: "width" })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(node(page, "Reverb").getByRole("slider", { name: "lowcut" })).toBeVisible();

  // What the second page does shows on the first, as a pin that somebody else makes: it takes the last of the first eight off.
  await other.getByRole("button", { name: "Parameters of Reverb" }).click();
  const otherPanel = other.getByRole("complementary", { name: "Parameters of Reverb" });
  await otherPanel.getByRole("searchbox").fill("width");
  await pinOf(otherPanel, "width").click();
  await expect(node(page, "Reverb").getByRole("slider", { name: "width" })).toHaveCount(0);
  await expect(node(page, "Reverb").getByRole("slider", { name: "lowcut" })).toBeVisible();

  // The saved file has the pinned parameters of the reverb by name, in the order they were pinned, and nowhere else.
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(tang.file, "utf8");
  expect(saved.match(/^pinned = /gm), saved).toHaveLength(1);
  const names = /^pinned = \[(.*)\]$/m.exec(saved)?.[1]?.split(", ").map((name) => name.replaceAll('"', ""));
  expect(names, saved).toHaveLength(8);
  expect(names?.at(-1)).toBe("lowcut");
  expect(names).not.toContain("width");
  expect(names?.slice(0, 3)).toEqual(["decay", "size", "damping"]);

  // Given back to the first eight (by the second page, whose panel is still open: the first page's was closed by the reload) it is gone from
  // the file the next time it is saved.
  await giveBack(otherPanel).click();
  await expect(node(page, "Reverb").getByRole("slider", { name: "width" })).toBeVisible();
  await expect(node(page, "Reverb").getByRole("slider", { name: "lowcut" })).toHaveCount(0);
  await page.getByRole("button", { name: /^Save/ }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  expect(readFileSync(tang.file, "utf8")).not.toContain("pinned");
  await other.close();
});
