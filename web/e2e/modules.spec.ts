import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";

import { expect, test as mock } from "@playwright/test";
import type { Page } from "@playwright/test";

import { audioIn, audioOut, drag, node, open, showAll } from "./canvas";
import { test } from "./own";
import { heardAt, silentAt } from "./sound";

// A chain of effects saved as a module and added again (docs/web-ui.md D104): the button in the cell of a lane that saves its chain under a name, and the bar's "+ Effect", which lists what was saved and adds it in a lane
// of its own, with the effects it was saved with. The first test is against the mock engine in the page (what the canvas draws and says), the second against the real program, which keeps the module in a file.

/** The button in the bar that adds an effect. */
const addEffect = (page: Page) => page.getByRole("button", { name: "Add an effect", exact: true });

/** The names of the nodes of the canvas, in the order of the page. */
const names = (page: Page): Promise<string[]> => page.locator(".tang-node__name").allTextContents();

/** Save the chain of the lane called `lane` as `name`, with the field that its button opens. */
async function save(page: Page, lane: string, name: string): Promise<void> {
  await page.getByRole("button", { name: `Save the chain of ${lane} as a module` }).click();
  const dialog = page.getByRole("dialog", { name: `Save the chain of ${lane} as a module` });
  await expect(dialog.getByRole("textbox", { name: "Module name" })).toBeFocused();
  await dialog.getByRole("textbox", { name: "Module name" }).fill(name);
  await dialog.getByRole("textbox", { name: "Module name" }).press("Enter");
  await expect(page.getByText(`Saved the module "${name}": + Effect adds it`)).toBeVisible();
  await expect(dialog).toHaveCount(0);
}

mock("a lane that has a chain of effects saves it as a module from its cell, and the bar's + Effect adds it: a lane of its own with the effects it was saved with, which Undo takes out and Redo gives back", async ({ page }) => {
  await open(page);
  // The button is in the cell of the lane that has a chain, beside its mute and its solo.
  const cell = node(page, "Oscillator").locator(".tang-cell").filter({ has: page.getByRole("button", { name: "Mute Oscillator" }) });
  await expect(cell.getByRole("button")).toHaveCount(3);
  await expect(addEffect(page)).toBeEnabled();

  // The field takes a name; Escape leaves it and asks nothing; a name that is refused is said, and the field stays.
  await page.getByRole("button", { name: "Save the chain of Oscillator as a module" }).click();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Save the chain of Oscillator as a module" })).toBeFocused();
  await page.getByRole("button", { name: "Save the chain of Oscillator as a module" }).click();
  await page.getByRole("textbox", { name: "Module name" }).fill("a/b");
  // (The bar has a Save of its own: the field's is the one in the dialog.)
  await page.getByRole("dialog").getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText(/Could not save the chain of Oscillator as the module "a\/b": a file name has no folder in it/)).toBeVisible();
  await expect(page.getByRole("textbox", { name: "Module name" })).toBeVisible();
  await page.getByRole("button", { name: "Cancel" }).click();

  // Saved: the page says so, and the bar's menu has the module, first, under a heading of its own; the lane's own menu does not have it.
  await save(page, "Oscillator", "Tracer chain");
  await addEffect(page).click();
  const menu = page.getByRole("menu", { name: "Add an effect" });
  await expect(menu.getByRole("group", { name: "Module" }).getByRole("menuitem")).toHaveText(["Tracer chain"]);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await expect(page.getByRole("menu", { name: "Add an effect to Oscillator" }).getByRole("menuitem", { name: "Tracer chain" })).toHaveCount(0);
  await page.keyboard.press("Escape");

  // Added: a lane of its own with a filter, as the tracer's lane has one, with the lane's cell on it (and its button, since the chain is there).
  const before = await names(page);
  await addEffect(page).click();
  await page.getByRole("menuitem", { name: "Tracer chain", exact: true }).click();
  await expect.poll(() => names(page)).toHaveLength(before.length + 1);
  await expect(page.getByRole("button", { name: "Save the chain of Filter 2 as a module" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Undo add Tracer chain" })).toBeVisible();
  await page.getByRole("button", { name: "Undo add Tracer chain" }).click();
  await expect.poll(() => names(page)).toHaveLength(before.length);
  await page.getByRole("button", { name: "Redo add Tracer chain" }).click();
  await expect.poll(() => names(page)).toHaveLength(before.length + 1);
});

test("a chain of effects is saved as a file of the library of the real program, and added from it: heard through cables only, with the effects as they were saved, and the library is the program's: another program on it has the module", async ({ page, own, browser }) => {
  // (A good many waits for silence, each the better part of a second.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await heardAt(page, "the lane");

  // The tracer's lane has an oscillator and a filter whose cutoff an LFO moves: its chain, the filter, is saved as a module. The file is in the library, with the filter and no oscillator, and the session's own file has nothing of it.
  await save(page, "Oscillator", "Tracer filter");
  const module = readFileSync(join(own.library, "Tracer filter.toml"), "utf8");
  expect(module).toContain('plugin = "builtin:filter"');
  expect(module).not.toContain("builtin:osc");
  expect(existsSync(join(own.library, "Tracer filter.toml"))).toBe(true);

  // Cut the lane's way out (its filter is the link to the master): it is silent. The module is added in a lane of its own, bare: silent still, until cables say where. Then it is heard through them, and only through them.
  await showAll(page);
  await page.getByRole("button", { name: "Remove cable Filter → Master", exact: true }).click();
  await silentAt(page, "the lane, with its way out cut");
  await addEffect(page).click();
  await page.getByRole("menuitem", { name: "Tracer filter", exact: true }).click();
  await expect(node(page, "Filter 2")).toBeVisible();
  await showAll(page);
  await silentAt(page, "the module that is added, with no cable");
  await drag(page, audioOut(node(page, "Oscillator")), audioIn(node(page, "Filter 2")));
  await drag(page, audioOut(node(page, "Filter 2")), audioIn(node(page, "Master")));
  await heardAt(page, "the lane through the module");

  // The file of the session keeps the lane that the module made: no instrument, the filter, and the cable into it.
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved", { exact: true })).toBeVisible();
  const saved = readFileSync(own.file, "utf8");
  expect(saved.split("[[instrument]]")).toHaveLength(3);
  expect(saved).toContain('to = "instrument[1].effect[0]"');

  // The program's library is the one that it was started with, and a page that is opened again lists the module: it is a file, and not something the page has.
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await addEffect(page).click();
  await expect(page.getByRole("menuitem", { name: "Tracer filter", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  const second = await browser.newPage();
  await second.goto(own.url);
  await expect(second.getByRole("status")).toHaveText("Connected");
  await second.getByRole("button", { name: "Add an effect", exact: true }).click();
  await expect(second.getByRole("menuitem", { name: "Tracer filter", exact: true })).toBeVisible();
  await second.close();
});
