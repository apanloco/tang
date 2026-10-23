import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { audioIn, audioOut, centre, drag, node, nodeBoxes, open, sends, showAll } from "./canvas";
import { test } from "./own";
import { heardAt, silentAt } from "./sound";

// The link into an effect from the stage before it in its chain, from the page (docs/web-ui.md D86): every link of the audio path is a cable that can be cut. A button on the badge's line of an
// effect says where it is fed from and, pressed, cuts the link, so that it is fed only by the cables that end at it; the link has a × at its middle that does the same. The first tests are against the
// mock engine in the page (what the canvas draws and says), the last against the real program (what is heard, what the file keeps and what the history does).

/** The button of the input of the effect `name`. */
const input = (page: Page, name: string) => page.getByRole("button", { name: `Cut the input of ${name}`, exact: true });

/** The links of the audio path that the canvas draws, grey (the amber ones are the cables that a person drew). */
const links = (page: Page) => page.locator(".tang-cable--audio");

/** Whether the text of a button is all there: a name that is too long for the line is cut. */
const fits = (button: Locator) => button.evaluate((element) => element.scrollWidth <= element.clientWidth);

mock("the input of an effect is cut from its node: the link into it is gone, the button says that there is none, nothing on the canvas moves, and a second press gives it back", async ({ page }) => {
  await open(page);
  const button = input(page, "Filter");
  await expect(button).toHaveAttribute("aria-pressed", "false");
  await expect(button).toContainText("←");
  // (The tracer's lane has its instrument and a filter: a link from the one to the other, and one from the filter to the master.)
  await expect(links(page)).toHaveCount(2);
  const before = await nodeBoxes(page);

  await button.click();
  await expect(button).toHaveText("no in");
  await expect(button).toHaveAttribute("aria-pressed", "true");
  expect(await fits(button)).toBe(true);
  // The link out of the filter is there still: the chain goes on from it.
  await expect(links(page)).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Disconnect Filter → Master" })).toBeVisible();
  expect(await nodeBoxes(page)).toEqual(before);

  await button.click();
  await expect(button).toHaveAttribute("aria-pressed", "false");
  await expect(button).toContainText("←");
  await expect(links(page)).toHaveCount(2);
  expect(await nodeBoxes(page)).toEqual(before);
});

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

mock("the link from the instrument to the filter has a button on it that disconnects it: the filter's input is cut as the button in its header cuts it, the focus goes to that button, and undo gives the link back", async ({ page }) => {
  await open(page);
  const disconnect = page.getByRole("button", { name: "Disconnect Oscillator → Filter" });
  await expect(disconnect).toBeVisible();
  // Every link of the audio path has one: the end of the chain is the way out of the lane.
  await expect(page.getByRole("button", { name: /^Disconnect / })).toHaveCount(2);
  // It is on the link, in the middle of it, as the buttons of the cables that a person draws are.
  expect(await distanceFromLink(disconnect, links(page).first())).toBeLessThan(4);
  const before = await nodeBoxes(page);

  await disconnect.click();
  await expect(input(page, "Filter")).toHaveText("no in");
  await expect(input(page, "Filter")).toBeFocused();
  await expect(links(page)).toHaveCount(1);
  await expect(page.getByRole("button", { name: /^Disconnect / })).toHaveCount(1);
  expect(await nodeBoxes(page)).toEqual(before);

  await page.getByRole("button", { name: /^Undo/ }).click();
  await expect(input(page, "Filter")).toHaveAttribute("aria-pressed", "false");
  await expect(links(page)).toHaveCount(2);
  await expect(disconnect).toBeVisible();
});

mock("an effect that is added is fed by the stage before it, and the link into it has a button: cutting the input of the first leaves the second fed, and the cut goes with an effect that is moved", async ({ page }) => {
  await open(page);
  await page.getByRole("button", { name: "Add an effect to Oscillator" }).click();
  await page.getByRole("menuitem", { name: "Delay" }).click();
  await expect(node(page, "Delay")).toBeVisible();
  // The instrument to the filter, the filter to the delay, the delay to the master.
  await expect(links(page)).toHaveCount(3);
  await expect(input(page, "Delay")).toContainText("Filter");
  await expect(page.getByRole("button", { name: "Disconnect Filter → Delay" })).toBeVisible();

  // The filter's input is cut: no link into it, and the delay is still fed by the filter.
  await input(page, "Filter").click();
  await expect(input(page, "Filter")).toHaveText("no in");
  await expect(input(page, "Delay")).toHaveAttribute("aria-pressed", "false");
  await expect(links(page)).toHaveCount(2);

  // The filter goes later in its chain: it is the one that is not fed, wherever it is, and the delay is fed by the instrument now.
  await page.getByRole("button", { name: "Move Filter later in its chain" }).click();
  await expect(input(page, "Filter")).toHaveText("no in");
  await expect(input(page, "Delay")).toContainText("Oscillator");
  // (The instrument to the delay, and the delay to the filter, which goes to the master: no link into the filter... the one from the delay is the filter's input, which is cut.)
  await expect(links(page)).toHaveCount(2);
  await expect(page.getByRole("button", { name: "Disconnect Oscillator → Delay" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Disconnect Delay → Filter" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Disconnect Filter → Master" })).toBeVisible();

  // The filter that is not fed is taken out: the delay is fed by the instrument still, and the chain closes up.
  await node(page, "Filter").getByRole("button", { name: "Remove Filter" }).click();
  await expect(node(page, "Filter")).toHaveCount(0);
  await expect(links(page)).toHaveCount(2);
  await expect(page.getByRole("button", { name: "Disconnect Delay → Master" })).toBeVisible();
  // Undo puts it back, with its input still cut.
  await page.getByRole("button", { name: /^Undo remove Filter/ }).click();
  await expect(input(page, "Filter")).toHaveText("no in");
});

mock("the history has the input in the words of the controller's: cut, and given back", async ({ page }) => {
  await open(page);
  const [undo, redo] = [page.getByRole("button", { name: /^Undo/ }), page.getByRole("button", { name: /^Redo/ })];
  await input(page, "Filter").click();
  await expect(undo).toHaveAccessibleName("Undo cut the input of Filter");
  await input(page, "Filter").click();
  await expect(undo).toHaveAccessibleName("Undo give Filter its input back");
  await undo.click();
  await expect(input(page, "Filter")).toHaveText("no in");
  await expect(redo).toHaveAccessibleName("Redo give Filter its input back");
  await undo.click();
  await expect(input(page, "Filter")).toContainText("←");
  await expect(links(page)).toHaveCount(2);
  await redo.click();
  await expect(input(page, "Filter")).toHaveText("no in");
  await expect(links(page)).toHaveCount(1);
});

test("the input of an effect is cut in the real program: the lane is silent at the master, a cable from its instrument to the effect is heard through it and only through it, the file keeps the cut and the cable, and undo gives the link back", async ({ page, own }) => {
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

  // The filter's input is cut: the link from the instrument is gone from the canvas, and the filter is given nothing, so the same key is silent.
  await page.getByRole("button", { name: "Disconnect Oscillator → Filter" }).click();
  await expect(input(page, "Filter")).toHaveText("no in");
  await expect(links(page)).toHaveCount(1);
  await silentAt(page, "a filter that is fed nothing");

  // A cable drawn from the instrument to the filter is the way that it is fed by now, and the key is heard again, through the filter.
  await showAll(page);
  await drag(page, audioOut(node(page, "Oscillator")), audioIn(node(page, "Filter")));
  await expect(sends(page)).toHaveCount(1);
  await heardAt(page, "the lane through its cable");
  const cut = await saved();
  expect(cut).toMatch(/\[\[instrument\.effect\]\]\nplugin = "builtin:filter"\nfeed = "none"\n/);
  expect(cut).toMatch(/\[\[cable\]\]\nfrom = "instrument\[0\]"\nto = "instrument\[0\]\.effect\[0\]"\n/);

  // The cable is taken out, and the filter is fed by nothing again; given its input back, the key is heard, and the file has no word of a feed.
  await page.getByRole("button", { name: "Remove cable Oscillator → Filter" }).click();
  await expect(sends(page)).toHaveCount(0);
  await silentAt(page, "a filter that is fed nothing, with no cable");
  await input(page, "Filter").click();
  await expect(links(page)).toHaveCount(2);
  await heardAt(page, "the lane, given its input back");
  expect(await saved()).not.toContain("feed");

  // The history has it: undo cuts the input that was given back, the key is silent, and redo gives it back.
  await page.getByRole("button", { name: "Undo give Filter its input back", exact: true }).click();
  await expect(input(page, "Filter")).toHaveText("no in");
  await silentAt(page, "the filter, with its input cut by undo");
  await page.getByRole("button", { name: "Redo give Filter its input back", exact: true }).click();
  await expect(input(page, "Filter")).toContainText("←");
  await heardAt(page, "the lane, with its input given back by redo");

  // A page that is opened again draws what the program has: the input, cut.
  await input(page, "Filter").click();
  await expect(input(page, "Filter")).toHaveText("no in");
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(input(page, "Filter")).toHaveText("no in");
  await expect(links(page)).toHaveCount(1);
});
