import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { audioIn, audioOut, centre, drag, node, nodeBoxes, open, sends, showAll } from "./canvas";
import { test } from "./own";
import { heardAt, silentAt } from "./sound";

// The link into an effect from the stage before it in its chain, from the page (docs/web-ui.md D86 and step 88): it is a cable, amber, with the button that takes it out at its middle and no slider (it has no
// level). Taking it out cuts the input of the effect, which is then fed by the cables that end at it and by nothing else, and the chain goes on from it; the menu that sends the audio of the node the link started
// at, or a drag, draws it again (a cable between the two ends of a link that is cut is the link given back). There is no button on the effect that says where it is fed from. The first tests are against the mock
// engine in the page (what the canvas draws and says), the last against the real program (what is heard, what the file keeps and what the history does).

/** The links of the audio path that the canvas draws: cables with no level (the amber ones that have a slider are the cables that a person drew). */
const links = (page: Page) => page.locator(".tang-cable--audio");

/** The button that takes a link out, by the words of the link (`Oscillator → Filter`). */
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

mock("the link from the instrument to the filter is a cable with a button on it that takes it out: the filter's input is cut, the link out of the filter is there still, the focus goes to the menu that sends from the instrument, nothing moves, and the menu draws the link again", async ({ page }) => {
  await open(page);
  const link = removeLink(page, "Oscillator → Filter");
  await expect(link).toBeVisible();
  // (The tracer's lane has its instrument and a filter: a link from the one to the other, and one from the filter to the master, which has a button of its own that cuts the way out: `routes.spec.ts`.)
  await expect(links(page)).toHaveCount(2);
  // It is on the link, in the middle of it, as the buttons of the cables that a person draws are.
  expect(await distanceFromLink(link, links(page).first())).toBeLessThan(4);
  const before = await nodeBoxes(page);

  await link.click();
  await expect(links(page)).toHaveCount(1);
  await expect(link).toHaveCount(0);
  // The link out of the filter is there still: the chain goes on from it.
  await expect(removeLink(page, "Filter → Master")).toBeVisible();
  await expect(send(page, "Oscillator")).toBeFocused();
  expect(await nodeBoxes(page)).toEqual(before);

  // The menu has the filter to offer, and draws the link: the input given back, and no cable of a person's (it has no slider).
  await page.keyboard.press("Enter");
  await page.getByRole("menuitem", { name: "Filter" }).click();
  await expect(links(page)).toHaveCount(2);
  await expect(sends(page)).toHaveCount(0);
  await expect(link).toBeVisible();
  expect(await nodeBoxes(page)).toEqual(before);
});

mock("an effect that is added is fed by the stage before it, and the link into it has a button: taking out the link into the first leaves the second fed, and the cut goes with an effect that is moved", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Delay" }).click();
  await expect(node(page, "Delay")).toBeVisible();
  // The instrument to the filter, the filter to the delay, the delay to the master.
  await expect(links(page)).toHaveCount(3);
  await expect(removeLink(page, "Filter → Delay")).toBeVisible();

  // The link into the filter is taken out: the delay is still fed by the filter.
  await removeLink(page, "Oscillator → Filter").click();
  await expect(links(page)).toHaveCount(2);
  await expect(removeLink(page, "Filter → Delay")).toBeVisible();

  // The filter goes later in its chain: it is the one that is not fed, wherever it is, and the delay is fed by the instrument now.
  await page.getByRole("button", { name: "Move Filter later in its chain" }).click();
  // (The instrument to the delay, and the delay to the filter, which goes to the master: no link into the filter, whose input is cut.)
  await expect(links(page)).toHaveCount(2);
  await expect(removeLink(page, "Oscillator → Delay")).toBeVisible();
  await expect(removeLink(page, "Delay → Filter")).toHaveCount(0);
  await expect(removeLink(page, "Filter → Master")).toBeVisible();

  // The filter that is not fed is taken out: the delay is fed by the instrument still, and the chain closes up.
  await node(page, "Filter").getByRole("button", { name: "Remove Filter" }).click();
  await expect(node(page, "Filter")).toHaveCount(0);
  await expect(links(page)).toHaveCount(2);
  await expect(removeLink(page, "Delay → Master")).toBeVisible();
  // Undo puts it back, with its input still cut.
  await page.getByRole("button", { name: /^Undo remove Filter/ }).click();
  await expect(node(page, "Filter")).toBeVisible();
  await expect(removeLink(page, "Delay → Filter")).toHaveCount(0);
  await expect(removeLink(page, "Filter → Master")).toBeVisible();
});

mock("the history has the input in the words of the controller's: cut when its link is taken out, and given back when a cable draws the link again", async ({ page }) => {
  await open(page);
  const [undo, redo] = [page.getByRole("button", { name: /^Undo/ }), page.getByRole("button", { name: /^Redo/ })];
  await removeLink(page, "Oscillator → Filter").click();
  await expect(undo).toHaveAccessibleName("Undo cut the input of Filter");
  await send(page, "Oscillator").click();
  await page.getByRole("menuitem", { name: "Filter" }).click();
  await expect(undo).toHaveAccessibleName("Undo give Filter its input back");
  await expect(links(page)).toHaveCount(2);
  await undo.click();
  await expect(links(page)).toHaveCount(1);
  await expect(redo).toHaveAccessibleName("Redo give Filter its input back");
  await undo.click();
  await expect(links(page)).toHaveCount(2);
  await redo.click();
  await expect(links(page)).toHaveCount(1);
});

test("the link into an effect is taken out in the real program: the lane is silent at the master, a cable drawn between the two ends gives the link back and makes no cable, another lane's instrument feeds the filter by a cable on its own, the file keeps the cut and the cable, and undo gives the link back", async ({ page, own }) => {
  // (A good many waits for silence, each the better part of a second.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const saved = async (): Promise<string> => {
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };

  // The lane's instrument goes to its filter, which goes to the master, and a key is heard.
  await expect(links(page)).toHaveCount(2);
  await heardAt(page, "the lane");

  // The link into the filter is taken out: it is gone from the canvas, and the filter is given nothing, so the same key is silent.
  await removeLink(page, "Oscillator → Filter").click();
  await expect(links(page)).toHaveCount(1);
  await silentAt(page, "a filter that is fed nothing");

  // A cable drawn from the instrument to the filter is the link that was taken out (step 88: every link is a cable): it gives the link back and makes no cable of its own, the key is heard again, and the file has
  // no word of a feed.
  await showAll(page);
  await drag(page, audioOut(node(page, "Oscillator")), audioIn(node(page, "Filter")));
  await expect(links(page)).toHaveCount(2);
  await expect(sends(page)).toHaveCount(0);
  await heardAt(page, "the lane, given its input back by a cable");
  const given = await saved();
  expect(given).not.toContain("feed");
  expect(given).not.toContain("[[cable]]");
  // It is a step of the history in the words of the link, and undo cuts the input again.
  await page.getByRole("button", { name: "Undo give Filter its input back", exact: true }).click();
  await expect(links(page)).toHaveCount(1);
  await silentAt(page, "the filter, with the input that a cable gave back cut by undo");

  // A second lane's oscillator, with no way out of its own (its link to the master is taken out), is drawn to the filter: the filter is fed by that cable, and the key is heard through the filter and only through it.
  await page.getByRole("button", { name: "Add an instrument" }).click();
  await page.getByRole("menuitem", { name: "Oscillator" }).click();
  await expect(node(page, "Oscillator 2")).toBeVisible();
  await removeLink(page, "Oscillator 2 → Master").click();
  await expect(removeLink(page, "Oscillator 2 → Master")).toHaveCount(0);
  await showAll(page);
  await drag(page, audioOut(node(page, "Oscillator 2")), audioIn(node(page, "Filter")));
  await expect(sends(page)).toHaveCount(1);
  await heardAt(page, "the second lane through its cable");
  const cut = await saved();
  expect(cut).toMatch(/\[\[instrument\.effect\]\]\nplugin = "builtin:filter"\nfeed = "none"\n/);
  expect(cut).toMatch(/\[\[cable\]\]\nfrom = "instrument\[1\]"\nto = "instrument\[0\]\.effect\[0\]"\n/);

  // The cable is taken out, and the filter is fed by nothing again; a cable drawn from the first lane's instrument gives the link back, the key is heard, and the file has no word of a feed.
  await page.getByRole("button", { name: "Remove cable Oscillator 2 → Filter" }).click();
  await expect(sends(page)).toHaveCount(0);
  await silentAt(page, "a filter that is fed nothing, with no cable");
  await drag(page, audioOut(node(page, "Oscillator 1")), audioIn(node(page, "Filter")));
  await expect(links(page)).toHaveCount(2);
  await expect(sends(page)).toHaveCount(0);
  await heardAt(page, "the lane, given its input back");
  expect(await saved()).not.toContain("feed");

  // The history has it: undo cuts the input that was given back, the key is silent, and redo gives it back.
  await page.getByRole("button", { name: "Undo give Filter its input back", exact: true }).click();
  await expect(links(page)).toHaveCount(1);
  await silentAt(page, "the filter, with its input cut by undo");
  await page.getByRole("button", { name: "Redo give Filter its input back", exact: true }).click();
  await expect(links(page)).toHaveCount(2);
  await heardAt(page, "the lane, with its input given back by redo");

  // A page that is opened again draws what the program has: the link, taken out.
  await removeLink(page, "Oscillator 1 → Filter").click();
  await expect(links(page)).toHaveCount(1);
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(links(page)).toHaveCount(1);
  await expect(removeLink(page, "Oscillator 1 → Filter")).toHaveCount(0);
  await expect(removeLink(page, "Filter → Master")).toBeVisible();
});
