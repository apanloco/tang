import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { CABLE_ROW_HEIGHT, CABLES_GAP, MODULATOR_SETTINGS_HEIGHT, NODE_WIDTH } from "../src/app/patch/toFlow";
import { dragged } from "../src/ui/knob-geometry";
import { lfoAccordingToTheServer, resetTracer, sampleCutoff, spyOnTheWire } from "./support";

// What a person does to a modulator on the page, against the real program (the tracer scene:
// a saw oscillator through a low-pass filter, an LFO of 0.5 Hz and a depth of 0.4 on the filter's
// cutoff) on the null audio backend, so that nothing is heard. What the server says is read by
// another client of it; what the engine does is read off the page's own meters and dots.

test.afterEach(async ({ page }) => {
  await resetTracer(page);
});

const DEPTH = "Depth of LFO → Filter: cutoff";

async function open(page: Page): Promise<void> {
  await spyOnTheWire(page);
  await page.goto("/");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await resetTracer(page); // (in case another spec left it turned)
  await expect(page.getByRole("slider", { name: "Rate", exact: true })).toHaveAttribute("aria-valuenow", "0.5");
  await expect(page.getByRole("slider", { name: DEPTH })).toHaveValue("0.4");
  await expect(page.locator(".tang-knob", { has: page.getByRole("slider", { name: "cutoff", exact: true }) }).locator(".tang-knob__dot")).toHaveAttribute("visibility", "visible");
}

async function dragUp(page: Page, name: string, pixels: number): Promise<void> {
  const box = await page.getByRole("slider", { name, exact: true }).boundingBox();
  if (!box) throw new Error(`no slider ${name}`);
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x, y - pixels, { steps: 10 });
  await page.mouse.up();
}

test("turning the LFO's rate knob changes the rate in the server, through the decades", async ({ page }) => {
  await open(page);
  await dragUp(page, "Rate", 40);
  const expected = dragged(0.5, -40, 0.01, 50, 1, "log"); // a fifth of the sweep on from 0.5 Hz: about 2.3 Hz, not the 10 of a fifth of the range
  expect(expected).toBeGreaterThan(2);
  expect(expected).toBeLessThan(3);
  await expect.poll(async () => (await lfoAccordingToTheServer(page)).rate).toBeCloseTo(expected, 3);
  // The knob shows what the server has: the engine's value, once it has answered.
  await expect.poll(async () => Number(await page.getByRole("slider", { name: "Rate", exact: true }).getAttribute("aria-valuenow"))).toBeCloseTo(expected, 3);
});

test("choosing a waveform changes it in the server, and the engine runs it", async ({ page }) => {
  await open(page);
  await page.getByRole("combobox", { name: "Wave", exact: true }).selectOption("square");
  await expect.poll(async () => (await lfoAccordingToTheServer(page)).waveform).toBe("square");
  // A square wave puts out only the two ends, so the LFO's bar says nothing in between.
  const looks = (await sampleCutoff(page, 1500)).flatMap((sample) => (sample.lfo === null ? [] : [Math.abs(sample.lfo)]));
  expect(looks.length).toBeGreaterThan(50);
  expect(looks.filter((look) => look > 0.99).length / looks.length).toBeGreaterThan(0.9);
});

test("choosing the random waveform makes the engine's LFO hold a number for each cycle, and a new one when the next begins", async ({ page }) => {
  await open(page);
  await page.getByRole("combobox", { name: "Wave", exact: true }).selectOption("random");
  await expect.poll(async () => (await lfoAccordingToTheServer(page)).waveform).toBe("random");
  await page.waitForTimeout(300); // (until the bar has stopped showing what the sine put out, which the telemetry was still bringing)
  // At half a hertz a cycle is two seconds, so that 4.5 seconds are in three or four of them. The LFO's bar says one number for each and none in between (a sine would say a
  // hundred), and the numbers are not all one.
  const looks = (await sampleCutoff(page, 4500)).flatMap((sample) => (sample.lfo === null ? [] : [sample.lfo]));
  expect(looks.length).toBeGreaterThan(100);
  expect(looks.every((look) => look >= -1 && look < 1)).toBe(true);
  const numbers = new Set(looks.map((look) => look.toFixed(3)));
  expect(numbers.size).toBeGreaterThanOrEqual(2);
  expect(numbers.size).toBeLessThanOrEqual(4);
});

test("choosing the smooth waveform makes the engine's LFO glide between the numbers of its cycles, never jumping", async ({ page }) => {
  await open(page);
  await page.getByRole("combobox", { name: "Wave", exact: true }).selectOption("smooth");
  await expect.poll(async () => (await lfoAccordingToTheServer(page)).waveform).toBe("smooth");
  await page.waitForTimeout(300); // (until the bar has stopped showing what the sine put out: a step of more than a whole unit, the telemetry being a few frames behind the server)
  const looks = (await sampleCutoff(page, 4500)).flatMap((sample) => (sample.lfo === null ? [] : [sample.lfo]));
  expect(looks.length).toBeGreaterThan(100);
  expect(new Set(looks.map((look) => look.toFixed(3))).size, "it is on the way between two numbers most of the time").toBeGreaterThan(30);
  // The steepest it goes is a glide from one end of the range to the other in a cycle (1.5 a second at the middle of it, about 0.05 between two of the 30 frames a second), where
  // the random one jumps by the difference of two numbers from one frame to the next.
  const steps = looks.slice(1).map((look, i) => Math.abs(look - (looks[i] ?? look)));
  expect(Math.max(...steps)).toBeLessThan(0.4);
});

test("turning a cable's depth changes it in the server, in the cable, and in what the engine does with the cutoff", async ({ page }) => {
  await open(page);
  const depth = page.getByRole("slider", { name: DEPTH });
  await depth.fill("0.9");
  await expect.poll(async () => (await lfoAccordingToTheServer(page)).depth).toBeCloseTo(0.9, 6);
  // (A cable is 1.5px and 2 more for the whole of its depth: `cableWidth`, which the page's own CSS cannot be asked to import here.)
  await expect(page.locator(".tang-cable--modulation")).toHaveCSS("stroke-width", `${String(1.5 + 2 * 0.9)}px`);
  await expect(depth).toHaveAttribute("aria-valuetext", "90%");
  // 0.5 and a swing of 0.9 of the range: it reaches both ends (at 0.4 it did not).
  const dots = (await sampleCutoff(page, 3000)).flatMap((sample) => (sample.dot === null ? [] : [sample.dot]));
  expect(Math.min(...dots)).toBeLessThan(0.05);
  expect(Math.max(...dots)).toBeGreaterThan(0.95);
});

test("a negative depth swings the other way, as the engine says", async ({ page }) => {
  await open(page);
  await page.getByRole("slider", { name: DEPTH }).fill("-0.4");
  await expect.poll(async () => (await lfoAccordingToTheServer(page)).depth).toBeCloseTo(-0.4, 6);
  await page.waitForTimeout(300);
  const seen = (await sampleCutoff(page, 2000)).filter((sample) => sample.dot !== null && sample.lfo !== null);
  expect(seen.length).toBeGreaterThan(50);
  // base - depth × output: where the LFO is up the cutoff is down.
  for (const sample of seen) expect(Math.abs((sample.dot ?? 0) - (0.5 - 0.4 * (sample.lfo ?? 0)))).toBeLessThan(0.05);
});

test("the depth is a hundredth at a time from the keys, and a value on the hundredths goes through whole", async ({ page }) => {
  await open(page);
  const depth = page.getByRole("slider", { name: DEPTH });
  await depth.focus();
  await depth.press("ArrowRight");
  await expect.poll(async () => (await lfoAccordingToTheServer(page)).depth).toBeCloseTo(0.41, 6);
  await depth.fill("0.37");
  await expect.poll(async () => (await lfoAccordingToTheServer(page)).depth).toBeCloseTo(0.37, 6);
});

test("the nodes keep to the sizes the layout assumes, and none lies over another", async ({ page }) => {
  await open(page);
  const nodes = await page.evaluate(() =>
    [...document.querySelectorAll(".react-flow__node")].map((element) => {
      const box = element.getBoundingClientRect();
      return { name: element.querySelector(".tang-node__name")?.textContent ?? "?", left: box.left, top: box.top, width: box.width, height: box.height };
    }),
  );
  expect(nodes.map((node) => node.name).sort()).toEqual(["Filter", "LFO", "Master", "Oscillator"]);
  // (The canvas is zoomed to fit the window: the scale is how much a node of the layout's width is drawn.)
  const scale = (nodes[0]?.width ?? NODE_WIDTH) / NODE_WIDTH;
  const lfo = nodes.find((node) => node.name === "LFO");
  // (The LFO lists its one cable: a row, and the room before it.)
  const assumed = MODULATOR_SETTINGS_HEIGHT + CABLES_GAP + CABLE_ROW_HEIGHT;
  expect((lfo?.height ?? 0) / scale).toBeLessThanOrEqual(assumed);
  expect((lfo?.height ?? 0) / scale).toBeGreaterThan(assumed - 12); // and the allowance is not much more than it needs
  for (const a of nodes) {
    for (const b of nodes) {
      if (a === b) continue;
      const apart = a.left + a.width <= b.left + 1 || b.left + b.width <= a.left + 1 || a.top + a.height <= b.top + 1 || b.top + b.height <= a.top + 1;
      expect(apart, `${a.name} and ${b.name}`).toBe(true);
    }
  }
});

test("the trace of the LFO is drawn on its node, and the cable is as bright as the LFO is high", async ({ page }) => {
  await open(page);
  const lfo = page.locator(".react-flow__node").filter({ has: page.locator(".tang-node__name", { hasText: /^LFO$/ }) });
  // The trace is a canvas, and what is drawn on it is a line that scrolls: more than a handful of pixels are lit, and they move.
  const canvas = lfo.locator("canvas");
  const lit = async (): Promise<number> =>
    canvas.evaluate((element: HTMLCanvasElement) => {
      const context = element.getContext("2d");
      if (!context) return -1;
      const { data } = context.getImageData(0, 0, element.width, element.height);
      let pixels = 0;
      for (let i = 3; i < data.length; i += 4) if ((data[i] ?? 0) > 0) pixels += 1;
      return pixels;
    });
  await expect.poll(lit).toBeGreaterThan(100);
  // The cable's opacity follows the output over its 2 s: dim at the bottom of the sine, bright at the top.
  const cable = page.locator(".tang-cable--modulation").first();
  const seen: number[] = [];
  for (let i = 0; i < 30; i += 1) {
    seen.push(Number(await cable.evaluate((path) => getComputedStyle(path).strokeOpacity)));
    await page.waitForTimeout(100);
  }
  expect(Math.min(...seen)).toBeLessThan(0.45);
  expect(Math.max(...seen)).toBeGreaterThan(0.85);
  for (const opacity of seen) {
    expect(opacity).toBeGreaterThanOrEqual(0.29);
    expect(opacity).toBeLessThanOrEqual(1);
  }
});
