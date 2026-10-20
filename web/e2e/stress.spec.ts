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

test("every bar and every dot is alive: each of the 32 LFOs' bars has a value, and moves", async ({ page }) => {
  await open(page);
  const bars = page.locator('[role="meter"][aria-label$=" output"]');
  await expect(page.locator('[role="meter"][aria-label$=" output"][aria-valuenow]')).toHaveCount(32);
  const read = () => bars.evaluateAll((all) => all.map((bar) => Number(bar.getAttribute("aria-valuenow"))));
  const first = await read();
  await page.waitForTimeout(130);
  const second = await read();
  // Every LFO has a rate of its own, and an eighth of a second moves each of them.
  expect(second.filter((value, i) => Math.abs(value - (first[i] ?? value)) > 0.001).length).toBeGreaterThanOrEqual(30);
  expect(Math.max(...second)).toBeLessThanOrEqual(1);
  expect(Math.min(...second)).toBeGreaterThanOrEqual(-1);
  // And so do the dots: they are written, so they are not where they started. Not in one short
  // interval (a sine at its peak hardly moves in an eighth of a second, so how many of the 64 had moved
  // was chance: 54 where it wanted 55 now and then): a dot is alive if it is ever somewhere else than
  // where it started, in two seconds. Except one that sits at an end of its knob, where half of
  // what an LFO does is clamped away (the stress scene has a volume at 1): it may stay for as long as
  // the LFO is in that half. Nearly all of them move, and the ones that do not are at an end.
  // A knob that is near an end is at it as long, and the look cannot tell: the attack (0.004 s), the decay
  // (0.1 s) and the release (0.012 s) are at the bottom of 0 to 10 s, and their LFOs, which go by the page's
  // clock from the start of the page, are under it for half of every cycle (1.25 s to 1.29 s at the slowest, 0.4 Hz;
  // the release's, 0.55 Hz, goes under at 0.91 s). The first look is at 0.71 to 0.85 s in eight runs and a
  // machine that is busy is later, so half a second that began there saw the release stand still (f146: dot 5,
  // the release of the first lane). Two seconds is longer than any of those halves.
  const dots = page.locator('.tang-knob__dot[visibility="visible"]');
  const look = () =>
    dots.evaluateAll((all) =>
      all.map((dot) => {
        const slider = dot.closest(".tang-knob")?.querySelector('[role="slider"]');
        const now = Number(slider?.getAttribute("aria-valuenow"));
        const atAnEnd = now === Number(slider?.getAttribute("aria-valuemin")) || now === Number(slider?.getAttribute("aria-valuemax"));
        return { place: `${dot.getAttribute("cx") ?? ""},${dot.getAttribute("cy") ?? ""}`, atAnEnd };
      }),
    );
  const started = await look();
  const moved = new Set<number>();
  for (let sample = 0; sample < 20; sample++) {
    await page.waitForTimeout(100);
    (await look()).forEach((dot, i) => {
      if (dot.place !== started[i]?.place) moved.add(i);
    });
  }
  expect(started).toHaveLength(64);
  expect(moved.size).toBeGreaterThanOrEqual(60);
  expect(started.flatMap((dot, i) => (dot.atAnEnd || moved.has(i) ? [] : [i]))).toEqual([]);
});

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
