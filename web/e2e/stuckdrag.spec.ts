import { expect, test } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { centre, node, open, showAll } from "./canvas";

// A drag whose release the page never sees (docs/web-ui.md step 101; the user: "sometimes the mouse pointer gets "attached" to a knob or window and wont release even when I release my mouse"). A browser always sends the
// release of a mouse that a test presses, so the release that is lost is made here by what the page takes for a sign of it: the window losing the focus, and a mouse that moves with no button down. The knob (and the bars of a
// step sequencer) and a card of the canvas must be let go of by either, and must not follow the mouse after.

const losses: [string, (page: Page, target: Locator) => Promise<void>][] = [
  [
    "the window loses the focus",
    async (page) => {
      await page.evaluate(() => window.dispatchEvent(new Event("blur")));
    },
  ],
  [
    "a mouse moves with no button down",
    async (_page, target) => {
      // (To the card or the dial itself for the knob, which hears its own pointer, and to the window for the card: the mouse that moves there is the same event.)
      await target.dispatchEvent("pointermove", { buttons: 0, pointerType: "mouse", pointerId: 1, bubbles: true });
      await target.page().evaluate(() => {
        window.dispatchEvent(new MouseEvent("mousemove", { buttons: 0, clientX: 5, clientY: 5 }));
      });
    },
  ],
];

const placeOf = async (card: Locator): Promise<{ x: number; y: number }> => {
  const box = await card.boundingBox();
  if (!box) throw new Error("the card is not on the page");
  return { x: Math.round(box.x), y: Math.round(box.y) };
};

for (const [what, lose] of losses) {
  test(`a knob that is turned when ${what} is let go of, and does not follow the mouse after`, async ({ page }) => {
    await open(page);
    await showAll(page);
    const dial = node(page, "Filter").getByRole("slider", { name: "cutoff", exact: true });
    const start = await dial.getAttribute("aria-valuenow");
    const at = await centre(dial);
    await page.mouse.move(at.x, at.y);
    await page.mouse.down();
    await page.mouse.move(at.x, at.y - 20, { steps: 4 });
    const turned = await dial.getAttribute("aria-valuenow");
    expect(turned, "the drag turned the knob").not.toBe(start);
    await lose(page, dial);
    // (As far as the browser knows the button is still down: the knob is let go of all the same.)
    await page.mouse.move(at.x, at.y - 70, { steps: 4 });
    expect(await dial.getAttribute("aria-valuenow"), "the knob does not follow the mouse").toBe(turned);
    await page.mouse.up();
  });

  test(`a card that is dragged when ${what} is let go of, and does not follow the mouse after`, async ({ page }) => {
    await open(page);
    await showAll(page);
    const card = node(page, "Filter");
    const grab = await centre(card.locator(".tang-node__name"));
    await page.mouse.move(grab.x, grab.y);
    await page.mouse.down();
    await page.mouse.move(grab.x + 60, grab.y + 40, { steps: 6 });
    await expect(page.locator(".react-flow__node.dragging"), "the card is in hand").toHaveCount(1);
    await lose(page, card);
    await expect(page.locator(".react-flow__node.dragging"), "and let go of").toHaveCount(0);
    const dropped = await placeOf(card);
    await page.mouse.move(grab.x + 220, grab.y + 120, { steps: 6 });
    expect(await placeOf(card), "the card does not follow the mouse").toEqual(dropped);
    await page.mouse.up();
  });
}
