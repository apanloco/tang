import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

// The budget of docs/web-ui.md §9.4: 60 fps with 32 modulators and 64 live knobs, and nothing
// rendered by React for the telemetry. The scene is `stressSnapshot` (eight lanes, each an
// oscillator through a filter with four LFOs over it), served by the mock engine in the page.

/** How the page's frames went over `ms` milliseconds: how many there were, and how long the slow ones took. */
async function frames(page: Page, ms: number): Promise<{ fps: number; p95: number; worst: number }> {
  return page.evaluate(
    (duration) =>
      new Promise<{ fps: number; p95: number; worst: number }>((resolve) => {
        const at: number[] = [];
        const start = performance.now();
        const tick = (now: number): void => {
          at.push(now);
          if (now - start < duration) {
            requestAnimationFrame(tick);
            return;
          }
          const intervals = at.slice(1).map((time, i) => time - (at[i] ?? time));
          const sorted = [...intervals].sort((a, b) => a - b);
          resolve({
            fps: (at.length - 1) / (((at.at(-1) ?? 0) - (at[0] ?? 0)) / 1000),
            p95: sorted[Math.floor(sorted.length * 0.95)] ?? 0,
            worst: sorted.at(-1) ?? 0,
          });
        };
        requestAnimationFrame(tick);
      }),
    ms,
  );
}

async function open(page: Page): Promise<void> {
  await page.goto("/?engine=mock&scene=stress");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await expect(page.locator('[role="meter"][aria-label$=" output"]')).toHaveCount(32);
  // A dot for every parameter a LFO drives, once the first telemetry has been drawn.
  await expect(page.locator('.tang-knob__dot[visibility="visible"]')).toHaveCount(64);
}

test("32 modulators and 64 live knobs run at the display's rate, and React renders nothing for them", async ({ page }) => {
  await open(page);
  await page.waitForTimeout(500);
  const before = await page.evaluate(() => window.tangCommits ?? 0);
  const { fps, p95, worst } = await frames(page, 4000);
  const after = await page.evaluate(() => window.tangCommits ?? 0);
  test.info().annotations.push({ type: "stress", description: `${fps.toFixed(1)} fps, p95 frame ${p95.toFixed(1)} ms, worst ${worst.toFixed(1)} ms, ${String(after - before)} commits` });
  expect(fps).toBeGreaterThan(55);
  expect(p95).toBeLessThan(34);
  expect(after - before).toBe(0);
});

test("the knobs still turn at that rate while a drag is under way", async ({ page }) => {
  await open(page);
  const slider = page.getByRole("slider", { name: "cutoff", exact: true }).first();
  const box = await slider.boundingBox();
  if (!box) throw new Error("no cutoff knob");
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  const moving = frames(page, 2500);
  await page.mouse.move(x, y - 100, { steps: 100 });
  const { fps, p95 } = await moving;
  await page.mouse.up();
  test.info().annotations.push({ type: "stress-drag", description: `${fps.toFixed(1)} fps, p95 frame ${p95.toFixed(1)} ms during a drag` });
  expect(fps).toBeGreaterThan(50);
  expect(p95).toBeLessThan(40);
});
