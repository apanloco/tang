import { readFileSync } from "node:fs";

import { expect, test as mock } from "@playwright/test";
import type { Page } from "@playwright/test";

import { audioIn, audioOut, drag, node, nodeBoxes, open, sends } from "./canvas";
import { test } from "./own";

// The way out of a lane and of a group from the page (docs/patch-graph.md §11.6, P3c-2a): a button under the mute and the solo that says where the audio goes and, pressed, cuts it, so that the
// node is heard only through the cables that start at it. The first tests are against the mock engine in the page (what the canvas draws and says), the last against the real program (what is
// heard, what the file keeps and what the history does).

/** The button of the way out of the node `name`. */
const wayOut = (page: Page, name: string) => page.getByRole("button", { name: `Cut the way out of ${name}`, exact: true });

/** The links of the audio path that the canvas draws, grey (the amber ones are the cables that a person drew). */
const links = (page: Page) => page.locator(".tang-cable--audio");

/** Whether the text of a button is all there: a name that is too long for the cell is cut. */
const fits = (button: ReturnType<typeof wayOut>) => button.evaluate((element) => element.scrollWidth <= element.clientWidth);

mock("the way out of a lane is cut from its node: its link to the master is gone, the button says that there is none, nothing on the canvas moves, and a second press gives it back", async ({ page }) => {
  await open(page);
  const button = wayOut(page, "Oscillator");
  await expect(button).toHaveText("→ Master");
  await expect(button).toHaveAttribute("aria-pressed", "false");
  expect(await fits(button)).toBe(true);
  // (The tracer's lane has its instrument and a filter: a link from the one to the other, and one from the filter to the master.)
  await expect(links(page)).toHaveCount(2);
  const before = await nodeBoxes(page);

  await button.click();
  await expect(button).toHaveText("no out");
  await expect(button).toHaveAttribute("aria-pressed", "true");
  expect(await fits(button)).toBe(true);
  await expect(links(page)).toHaveCount(1);
  expect(await nodeBoxes(page)).toEqual(before);

  await button.click();
  await expect(button).toHaveText("→ Master");
  await expect(button).toHaveAttribute("aria-pressed", "false");
  await expect(links(page)).toHaveCount(2);
  expect(await nodeBoxes(page)).toEqual(before);
});

mock("a lane in a group says that it goes to its group and a group that it goes to the master, and the group's way out is cut apart from the lane's", async ({ page }) => {
  await open(page);
  await page.getByRole("combobox", { name: "Group of Oscillator" }).selectOption({ label: "new group" });
  await expect(node(page, "Group 1")).toBeVisible();
  await expect(wayOut(page, "Oscillator")).toHaveText("→ Group 1");
  await expect(wayOut(page, "Group 1")).toHaveText("→ Master");
  expect(await fits(wayOut(page, "Oscillator"))).toBe(true);
  // The instrument to the filter, the filter to the group, the group to the master.
  await expect(links(page)).toHaveCount(3);

  // The group's is cut: its link to the master goes, and the lane is as it was (it feeds the group, which has nowhere to put it).
  await wayOut(page, "Group 1").click();
  await expect(wayOut(page, "Group 1")).toHaveText("no out");
  await expect(wayOut(page, "Oscillator")).toHaveText("→ Group 1");
  await expect(links(page)).toHaveCount(2);

  // The lane's is cut as well: the link from the filter to the group goes too, and the group's button is as it was.
  await wayOut(page, "Oscillator").click();
  await expect(wayOut(page, "Oscillator")).toHaveText("no out");
  await expect(wayOut(page, "Group 1")).toHaveText("no out");
  await expect(links(page)).toHaveCount(1);
});

mock("the history has the way out in the words of the controller's: cut, and given back", async ({ page }) => {
  await open(page);
  const [undo, redo] = [page.getByRole("button", { name: /^Undo/ }), page.getByRole("button", { name: /^Redo/ })];
  await wayOut(page, "Oscillator").click();
  await expect(undo).toHaveAccessibleName("Undo cut the way out of the lane of Oscillator");
  await wayOut(page, "Oscillator").click();
  await expect(undo).toHaveAccessibleName("Undo give the lane of Oscillator its way out");
  await undo.click();
  await expect(wayOut(page, "Oscillator")).toHaveText("no out");
  await expect(redo).toHaveAccessibleName("Redo give the lane of Oscillator its way out");
  await undo.click();
  await expect(wayOut(page, "Oscillator")).toHaveText("→ Master");
  await expect(links(page)).toHaveCount(2);
  await redo.click();
  await expect(wayOut(page, "Oscillator")).toHaveText("no out");
  await expect(links(page)).toHaveCount(1);
});

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

test("a lane whose way out is cut is silent at the master, and is heard through a cable that is drawn from it and only through that; the file keeps the way out, and undo gives it back", async ({ page, own }) => {
  // (A good many waits for silence, each the better part of a second.)
  test.slow();
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(own.url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const master = async (): Promise<number> => Number(await page.getByRole("meter", { name: "Master level" }).getAttribute("aria-valuenow"));
  /** A key that is held, heard at the master, and let go of: the master is quiet again before the next thing is looked at. */
  const heard = async (what: string): Promise<void> => {
    await page.keyboard.down("z");
    await expect.poll(master, { message: `${what} is heard` }).toBeGreaterThan(-50);
    await page.keyboard.up("z");
    await expect.poll(master, { message: "the master is quiet again" }).toBeLessThan(-50);
  };
  /** A key that is held and not heard, for the better part of a second. */
  const silent = async (what: string): Promise<void> => {
    await page.keyboard.down("z");
    expect(await loudest(page, 800), `${what} is silent`).toBeLessThan(-50);
    await page.keyboard.up("z");
    await expect.poll(master, { message: "the master is quiet again" }).toBeLessThan(-50);
  };
  const button = wayOut(page, "Oscillator");
  const saved = async (): Promise<string> => {
    await page.getByRole("button", { name: "Save", exact: true }).click();
    await expect(page.getByText("Saved", { exact: true })).toBeVisible();
    return readFileSync(own.file, "utf8");
  };

  // The lane goes to the master, and a key is heard.
  await expect(button).toHaveText("→ Master");
  await heard("the lane");

  // Its way out is cut: the link to the master is gone from the canvas, and the same key is silent.
  await button.click();
  await expect(button).toHaveText("no out");
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

  // Given back it is heard again, and a file that has it has no word of a way out.
  await button.click();
  await expect(button).toHaveText("→ Master");
  await expect(links(page)).toHaveCount(2);
  await heard("the lane, given its way out");
  expect(await saved()).not.toContain("route");

  // The history has it: undo cuts the way out that was given back, the key is silent, and redo gives it back.
  await page.getByRole("button", { name: "Undo give the lane of Oscillator its way out", exact: true }).click();
  await expect(button).toHaveText("no out");
  await silent("the lane, with its way out cut by undo");
  await page.getByRole("button", { name: "Redo give the lane of Oscillator its way out", exact: true }).click();
  await expect(button).toHaveText("→ Master");
  await heard("the lane, given its way out by redo");

  // A page that is opened again draws what the program has: the way out, cut.
  await button.click();
  await expect(button).toHaveText("no out");
  await page.reload();
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(wayOut(page, "Oscillator")).toHaveText("no out");
  await expect(links(page)).toHaveCount(1);
});
