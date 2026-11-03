import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

import { open } from "./canvas";
import { test } from "./own";

// The header of a node (docs/web-ui.md §9.3): its name is shown whole whatever buttons it has, and what kind of plugin it is goes under it, in small type, where a badge beside it left
// the name 40 pixels in a lane that has a number in its name and a "×" ("Os…"), and 30 in a lane of Pianoteq 9 ("Pian…", f148).

/** What each header of the page has: whether its name is cut (it has more text than room: the browser shows an ellipsis then), and where its badge is against the name. */
async function headers(page: Page): Promise<{ name: string; cut: boolean; badge: { under: boolean; aligned: boolean } | null }[]> {
  return page.evaluate(() =>
    [...document.querySelectorAll(".react-flow__node")].map((node) => {
      const name = node.querySelector<HTMLElement>(".tang-node__name");
      const badge = node.querySelector<HTMLElement>(".tang-node__badge");
      if (!name) throw new Error("a node with no name");
      const [at, under] = [name.getBoundingClientRect(), badge?.getBoundingClientRect()];
      return {
        name: name.textContent,
        cut: name.scrollWidth > name.clientWidth,
        badge: under ? { under: under.top >= at.bottom - 2, aligned: Math.abs(under.left - at.left) < 1 } : null,
      };
    }),
  );
}

test("no name is cut in the header of a node of two lanes, each with a number in its name and the buttons of a lane, and a badge is under the name", async ({ page }) => {
  await open(page, "lanes");
  const all = await headers(page);
  expect(all.map((header) => header.name)).toEqual(expect.arrayContaining(["Oscillator 1", "Oscillator 2", "Filter 1", "Filter 2", "LFO 1", "LFO 2", "Master"]));
  expect(all.filter((header) => header.cut).map((header) => header.name), "the names that are cut").toEqual([]);
  // What kind of plugin it is is under the name, and level with it.
  const badges = all.filter((header) => header.badge);
  expect(badges.length).toBeGreaterThanOrEqual(4);
  for (const header of badges) expect(header.badge, header.name).toEqual({ under: true, aligned: true });
});

test("no name is cut in the header of a node of the tracer, with a badge under it, on the real program too", async ({ page, own }) => {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.locator(".react-flow__node").first()).toBeVisible();
  const all = await headers(page);
  expect(all.map((header) => header.name)).toEqual(expect.arrayContaining(["Oscillator", "Filter", "LFO", "Master"]));
  expect(all.filter((header) => header.cut).map((header) => header.name)).toEqual([]);
  for (const header of all.filter((node) => node.badge)) expect(header.badge, header.name).toEqual({ under: true, aligned: true });
});
