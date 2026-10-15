import { expect } from "@playwright/test";
import type { Locator, Page } from "@playwright/test";

import { node } from "./canvas";
import { test } from "./own";

// The scope on the master node, on the real program (a program of its own, on the null audio backend): what comes out of the engine is drawn as a wave and as a
// spectrum. A sine is played, so that where the spectrum has its peak says what the engine put out: the pitch of the note.

/**
 * What is drawn on the canvas of the scope, as the line of it: for each column of the canvas, the row of the highest pixel of the colour of the line (`--tang-scope-line`,
 * #7aa2f7: the grid is grey, and the area under a spectrum is faint: an alpha of 0.18) and of the lowest one, or none where the column has no pixel of it.
 */
interface Line {
  tops: (number | undefined)[];
  bottoms: (number | undefined)[];
}

async function drawn(scope: Locator): Promise<Line> {
  return scope.locator("canvas").evaluate((canvas: HTMLCanvasElement) => {
    const context = canvas.getContext("2d");
    if (!context) throw new Error("no context");
    const { width, height } = canvas;
    const pixels = context.getImageData(0, 0, width, height).data;
    const tops: (number | undefined)[] = [];
    const bottoms: (number | undefined)[] = [];
    for (let x = 0; x < width; x += 1) {
      let top: number | undefined;
      let bottom: number | undefined;
      for (let y = 0; y < height; y += 1) {
        const at = 4 * (y * width + x);
        const [red, green, blue, alpha] = [pixels[at] ?? 0, pixels[at + 1] ?? 0, pixels[at + 2] ?? 0, pixels[at + 3] ?? 0];
        if (alpha > 100 && blue > 200 && green > 120 && red < 160) {
          top ??= y;
          bottom = y;
        }
      }
      tops.push(top);
      bottoms.push(bottom);
    }
    return { tops, bottoms };
  });
}

/** How many columns have a line in them. */
const columns = (line: Line): number => line.tops.filter((top) => top !== undefined).length;

/** How far apart the highest pixel of the line and the lowest one are. */
function extent(line: Line): number {
  const tops = line.tops.filter((top): top is number => top !== undefined);
  const bottoms = line.bottoms.filter((bottom): bottom is number => bottom !== undefined);
  return tops.length === 0 ? 0 : Math.max(...bottoms) - Math.min(...tops);
}

/**
 * Where a spectrum has its peak: the column of its highest point, as a frequency (the scope runs from 20 Hz to 20 kHz, a decade of the columns for each decade of the
 * frequencies), how far above the middle of the line, the median of its rows, that point is (a spectrum with a peak is high in one place only), and how much of the width
 * the line is in at all.
 */
function peakOf(line: Line): { hertz: number; rise: number; covered: number } {
  const lit = line.tops.flatMap((top, column) => (top === undefined ? [] : [{ top, column }]));
  const highest = lit.reduce((best, now) => (now.top < best.top ? now : best), lit[0] ?? { top: 0, column: 0 });
  const median = lit.map((point) => point.top).sort((a, b) => a - b)[Math.floor(lit.length / 2)] ?? 0;
  return { hertz: 20 * (20_000 / 20) ** (highest.column / line.tops.length), rise: median - highest.top, covered: lit.length / line.tops.length };
}

async function open(page: Page, url: string): Promise<{ scope: Locator; peak: () => Promise<number> }> {
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.goto(url);
  await expect(page.getByRole("status")).toHaveText("Connected");
  const scope = page.getByRole("meter", { name: "Output scope" });
  await expect(scope).toBeVisible();
  return { scope, peak: async () => Number(await scope.getAttribute("aria-valuenow")) };
}

test("the scope of the master is silent until a note is played, draws a wave that goes up and down with the note and is flat again after it", async ({ page, own }) => {
  const { scope, peak } = await open(page, own.url);
  await expect(scope).toHaveAttribute("data-view", "wave");
  await expect.poll(peak, { message: "silence to begin with" }).toBe(0);
  const quiet = await drawn(scope);
  expect(columns(quiet), "a line across the middle").toBeGreaterThan(50);
  expect(extent(quiet), "and a flat one").toBeLessThan(4);

  // The tracer is quiet on purpose (the oscillator's volume is a tenth): its wave is a line two pixels high on the scope. Its volume is put back to 1, ten times as much, so
  // that the wave is one.
  const volume = page.getByRole("slider", { name: "Volume", exact: true });
  await volume.dblclick();
  await expect.poll(async () => Number(await volume.getAttribute("aria-valuenow")), { message: "the volume is 1" }).toBeCloseTo(1, 3);

  await page.keyboard.down("z");
  await expect.poll(peak, { message: "the note is on the scope" }).toBeGreaterThan(0.1);
  await expect.poll(async () => extent(await drawn(scope)), { message: "the wave is more than a line" }).toBeGreaterThan(8);

  await page.keyboard.up("z");
  await expect.poll(peak, { message: "silence again" }).toBe(0);
  await expect.poll(async () => extent(await drawn(scope)), { message: "and the line is flat again" }).toBeLessThan(4);
});

test("the scope of the master draws the spectrum of a sine that is played with its peak at the pitch of the note, and the choices of the view are the master's", async ({ page, own }) => {
  const { scope, peak } = await open(page, own.url);
  const master = node(page, "Master");
  // A sine, so that there is one peak: the oscillator's waveform is a choice on its node.
  await page.locator(".tang-choice", { has: page.getByText("waveform", { exact: true }) }).getByRole("combobox").selectOption({ label: "sine" });
  await master.getByRole("combobox", { name: "View" }).selectOption({ label: "Spectrum" });
  await expect(scope).toHaveAttribute("data-view", "spectrum");
  await expect(master.getByRole("combobox", { name: "Time" })).toHaveCount(0);
  await expect(scope.getByText("1k", { exact: true })).toBeVisible();

  // Z is the C below the keys of the computer's middle: 130.8 Hz. The spectrum of a sine has a peak, well above the rest of the line, which is down at the floor where the
  // sine has nothing, and the peak is over the frequencies of the note: a pixel is a thirtieth of an octave wide here, and a bin of the block is 23 Hz, so that the peak
  // is within a bin or two of it.
  await page.keyboard.down("z");
  await expect.poll(peak, { message: "the note is on the scope" }).toBeGreaterThan(0.02);
  await expect.poll(async () => peakOf(await drawn(scope)).rise, { message: "the spectrum has a peak", timeout: 8000 }).toBeGreaterThan(30);
  const { hertz, covered } = peakOf(await drawn(scope));
  expect(covered, "a line all the way across").toBeGreaterThan(0.9);
  expect(hertz).toBeGreaterThan(100);
  expect(hertz).toBeLessThan(165);
  await page.keyboard.up("z");
});
