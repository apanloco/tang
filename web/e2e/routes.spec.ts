import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { audioIn, audioOut, centre, drag, node, nodeBoxes, open, sends, still } from "./canvas";
import { test } from "./own";
import { heardAt, silentAt } from "./sound";

// The way out of a lane and of a group from the page (docs/patch-graph.md §11.6, P3c-2a, and step 88): it is a cable, the link from the end of the lane's chain (or the group's) to its group's bus or the master,
// amber, with the button that takes it out at its middle and no slider (it has no level). A node that has no cable out sends nowhere: there is no button on the node that says where its audio goes. The first tests
// are against the mock engine in the page (what the canvas draws and says), the last against the real program (what is heard, what the file keeps and what the history does).

/** The links of the audio path that the canvas draws: cables with no level (the amber ones that have a slider are the cables that a person drew). */
const links = (page: Page) => page.locator(".tang-cable--audio");

/** The button that takes a link out, by the words of the link (`Filter → Master`). */
const removeLink = (page: Page, words: string) => page.getByRole("button", { name: `Remove cable ${words}`, exact: true });

/** The menu that sends the audio of the node `name`, which draws a cable from it, a link that was taken out among them. */
const send = (page: Page, name: string) => page.getByRole("button", { name: `Send audio from ${name}`, exact: true });

/** How far the middle of `button` is from the link `link` (a path), in pixels: the least distance to a point along it. */
async function distanceFromLink(button: Locator, link: Locator): Promise<number> {
  const middle = await centre(button);
  return link.evaluate((path: SVGPathElement, at) => {
    const matrix = path.getScreenCTM();
    if (!matrix) throw new Error("the link is not drawn");
    const length = path.getTotalLength();
    let least = Number.POSITIVE_INFINITY;
    for (let i = 0; i <= 400; i += 1) {
      const point = path.getPointAtLength((length * i) / 400);
      const x = matrix.a * point.x + matrix.c * point.y + matrix.e;
      const y = matrix.b * point.x + matrix.d * point.y + matrix.f;
      least = Math.min(least, Math.hypot(x - at.x, y - at.y));
    }
    return least;
  }, middle);
}

mock("the link from the end of a lane's chain to the master is a cable with a button on it that takes it out: the way out is cut, the focus goes to the menu that sends from the filter, nothing on the canvas moves, and the menu draws the link again", async ({ page }) => {
  await open(page);
  const link = removeLink(page, "Filter → Master");
  await expect(link).toBeVisible();
  // (The tracer's lane has its instrument and a filter: a link from the one to the other, which has a button of its own that cuts the filter's input, and one from the filter to the master.)
  await expect(links(page)).toHaveCount(2);
  // It is on the link, in the middle of it, as the buttons of the cables that a person draws are.
  expect(await distanceFromLink(link, links(page).last())).toBeLessThan(4);
  const before = await nodeBoxes(page);

  await link.click();
  await expect(links(page)).toHaveCount(1);
  await expect(link).toHaveCount(0);
  await expect(send(page, "Filter")).toBeFocused();
  expect(await nodeBoxes(page)).toEqual(before);

  // The menu has the master to offer, and draws the link: the way out given back, and no cable of a person's (it has no slider).
  await page.keyboard.press("Enter");
  await page.getByRole("menuitem", { name: "Master" }).click();
  await expect(links(page)).toHaveCount(2);
  await expect(sends(page)).toHaveCount(0);
  await expect(link).toBeVisible();
  expect(await nodeBoxes(page)).toEqual(before);
});

mock("a lane in a group has its link to the group, and a group its link to the master, and each is taken out apart from the other", async ({ page }) => {
  await open(page, "groups");
  await page.getByRole("combobox", { name: "Group of Oscillator" }).selectOption({ label: "Group 1" });
  await expect(node(page, "Group 1")).toBeVisible();
  // The instrument to the filter, the filter to the group, the group to the master.
  await expect(links(page)).toHaveCount(3);
  await expect(removeLink(page, "Filter → Group 1")).toBeVisible();
  await expect(removeLink(page, "Group 1 → Master")).toBeVisible();

  // The group's is taken out: its link to the master goes, and the lane's is as it was.
  await removeLink(page, "Group 1 → Master").click();
  await expect(links(page)).toHaveCount(2);
  await expect(removeLink(page, "Group 1 → Master")).toHaveCount(0);
  await expect(removeLink(page, "Filter → Group 1")).toBeVisible();

  // The lane's is taken out as well: the link from the filter to the group goes too.
  await removeLink(page, "Filter → Group 1").click();
  await expect(links(page)).toHaveCount(1);
});

mock("a lane that is added has a link to the master, and a button on it that takes it out: the lane goes nowhere, the other lane's link is as it was, and nothing moves", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menu", { name: "Add an instrument" }).getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  await still(node(page, "Oscillator 2"));
  // (The tracer's lane has a link from its instrument to its filter and one from the filter to the master; the new lane, which has no effect, has one from its instrument to the master.)
  await expect(links(page)).toHaveCount(3);
  const link = removeLink(page, "Oscillator 2 → Master");
  await expect(link).toBeVisible();
  expect(await distanceFromLink(link, links(page).last())).toBeLessThan(4);
  const before = await nodeBoxes(page);

  await link.click();
  await expect(links(page)).toHaveCount(2);
  await expect(link).toHaveCount(0);
  await expect(removeLink(page, "Filter → Master")).toBeVisible();
  expect(await nodeBoxes(page)).toEqual(before);
});

mock("the history has the way out in the words of the controller's: cut when its link is taken out, and given back when a cable draws the link again", async ({ page }) => {
  await open(page);
  const [undo, redo] = [page.getByRole("button", { name: /^Undo/ }), page.getByRole("button", { name: /^Redo/ })];
  await removeLink(page, "Filter → Master").click();
  await expect(undo).toHaveAccessibleName("Undo cut the way out of the lane of Oscillator");
  await send(page, "Filter").click();
  await page.getByRole("menuitem", { name: "Master" }).click();
  await expect(undo).toHaveAccessibleName("Undo give the lane of Oscillator its way out");
  await expect(links(page)).toHaveCount(2);
  await undo.click();
  await expect(links(page)).toHaveCount(1);
  await expect(redo).toHaveAccessibleName("Redo give the lane of Oscillator its way out");
  await undo.click();
  await expect(links(page)).toHaveCount(2);
  await redo.click();
  await expect(links(page)).toHaveCount(1);
});

test("a lane whose link to the master is taken out is silent at the master, and is heard through a cable that is drawn from it and only through that; the file keeps the way out, a cable drawn where the link was gives it back, and undo and redo go through it", async ({ page, own }) => {
  // (A good many waits for silence, each the better part of a second.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const heard = (what: string): Promise<void> => heardAt(page, what);
  const silent = (what: string): Promise<void> => silentAt(page, what);
  const saved = async (): Promise<string> => {
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };

  // The lane goes to the master, and a key is heard.
  await expect(links(page)).toHaveCount(2);
  await heard("the lane");

  // Its link to the master is taken out: it is gone from the canvas, and the same key is silent.
  await removeLink(page, "Filter → Master").click();
  await expect(links(page)).toHaveCount(1);
  await silent("a lane with no way out");

  // A cable drawn from its instrument to the master is the way that it is heard by (the filter that the lane's chain has is not in it), and the key is heard again.
  await drag(page, audioOut(node(page, "Oscillator")), audioIn(node(page, "Master")));
  await expect(sends(page)).toHaveCount(1);
  await heard("the lane through its cable");
  expect(await saved()).toMatch(/route = "none"\n[\s\S]*\[\[cable\]\]\nfrom = "instrument\[0\]"\nto = "master"\n/);

  // The cable is taken out, and the lane is silent again; the file has the way out cut, and no cable.
  await page.getByRole("button", { name: "Remove cable Oscillator → Master" }).click();
  await expect(sends(page)).toHaveCount(0);
  await silent("a lane with no way out and no cable");
  const cut = await saved();
  expect(cut).toContain('route = "none"\n');
  expect(cut).not.toContain("[[cable]]");

  // A cable drawn from the filter to the master is the link that was taken out (step 88: every link is a cable): it gives the way out back and makes no cable of its own, the key is heard, and the file has no
  // word of a way out.
  await drag(page, audioOut(node(page, "Filter")), audioIn(node(page, "Master")));
  await expect(links(page)).toHaveCount(2);
  await expect(sends(page)).toHaveCount(0);
  await heard("the lane, given its way out by a cable");
  expect(await saved()).not.toContain("route");

  // The history has it: undo takes the link out again, the key is silent, and redo gives it back.
  await page.getByRole("button", { name: "Undo give the lane of Oscillator its way out", exact: true }).click();
  await expect(links(page)).toHaveCount(1);
  await silent("the lane, with its way out cut by undo");
  await page.getByRole("button", { name: "Redo give the lane of Oscillator its way out", exact: true }).click();
  await expect(links(page)).toHaveCount(2);
  await heard("the lane, given its way out by redo");

  // The button on the link takes it out again, in the real program: the key is silent, the file keeps the way out cut, and undo, in the words of what was done, gives it back and the key is heard.
  await removeLink(page, "Filter → Master").click();
  await expect(links(page)).toHaveCount(1);
  await silent("the lane, disconnected again");
  expect(await saved()).toContain('route = "none"\n');
  await page.getByRole("button", { name: "Undo cut the way out of the lane of Oscillator", exact: true }).click();
  await expect(links(page)).toHaveCount(2);
  await heard("the lane, with its way out given back by undo");

  // A page that is opened again draws what the program has: the way out, cut.
  await removeLink(page, "Filter → Master").click();
  await expect(links(page)).toHaveCount(1);
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(links(page)).toHaveCount(1);
  await expect(removeLink(page, "Oscillator → Filter")).toBeVisible();
  await expect(removeLink(page, "Filter → Master")).toHaveCount(0);
});
