import { expect, test } from "@playwright/test";
import type { Page } from "@playwright/test";

import { cutoffAccordingToTheServer, knob, resetTracer, sampleCutoff, spyOnTheWire, wireOf } from "./support";

// The tracer scene (src/client/fixtures/tracer.toml): a saw oscillator through a low-pass filter, with a
// LFO of 0.5 Hz and a depth of 0.4 on the filter's cutoff, which is set to 0.5. The real
// program, on the null audio backend, so that nothing is heard.

// What a test turns is put back after it, for the next test and for the other specs.
test.afterEach(async ({ page }) => {
  await resetTracer(page);
});

/** Open the page, and wait until the cutoff knob and the LFO are on it, and the telemetry is coming. */
async function open(page: Page): Promise<void> {
  await spyOnTheWire(page);
  await page.goto("/");
  await expect(page.getByRole("status")).toHaveText("Connected");
  await resetTracer(page); // (in case another spec left it turned)
  await expect(page.getByRole("slider", { name: "cutoff", exact: true })).toBeVisible();
  await expect(page.getByRole("slider", { name: "cutoff", exact: true })).toHaveAttribute("aria-valuenow", "0.5");
  // The dot is not there until the first frame of telemetry has been drawn.
  await expect(knob(page, "cutoff").locator(".tang-knob__dot")).toHaveAttribute("visibility", "visible");
}

/** The slider of a knob: where to press, and what it says. */
async function dragUp(page: Page, name: string, pixels: number, steps = 10): Promise<void> {
  const box = await page.getByRole("slider", { name, exact: true }).boundingBox();
  if (!box) throw new Error(`no slider ${name}`);
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x, y - pixels, { steps });
  await page.mouse.up();
}

test("the LFO drives the cutoff knob's dot, as far as the engine says", async ({ page }) => {
  await open(page);
  const samples = await sampleCutoff(page, 3000); // a period of the LFO and a half
  const seen = samples.filter((sample) => sample.dot !== null && sample.lfo !== null);
  expect(seen.length, "frames with both the dot and the bar").toBeGreaterThan(100);

  const lfo = seen.map((sample) => sample.lfo ?? 0);
  const dot = seen.map((sample) => sample.dot ?? 0);
  // The LFO goes through its whole range, and the dot through 0.5 ± 0.4 with it.
  expect(Math.min(...lfo)).toBeLessThan(-0.9);
  expect(Math.max(...lfo)).toBeGreaterThan(0.9);
  expect(Math.min(...dot)).toBeLessThan(0.15);
  expect(Math.max(...dot)).toBeGreaterThan(0.85);
  // And at every frame the dot is where the bar says it should be: base + output × depth × range.
  for (const sample of seen) {
    expect(Math.abs((sample.dot ?? 0) - (0.5 + 0.4 * (sample.lfo ?? 0)))).toBeLessThan(0.05);
  }
});

test("dragging the cutoff knob changes the value in the server, and the engine moves the dot with it", async ({ page }) => {
  await open(page);
  const slider = page.getByRole("slider", { name: "cutoff", exact: true });
  await expect(slider).toHaveAttribute("aria-valuenow", "0.5");

  await dragUp(page, "cutoff", 40); // a fifth of the range
  // The knob settles on what the server has.
  await expect.poll(async () => Number(await slider.getAttribute("aria-valuenow")), { message: "the knob after the drag" }).toBeCloseTo(0.7, 2);
  // Another client sees the same value: it is the server's.
  expect(await cutoffAccordingToTheServer(page)).toBeCloseTo(0.7, 3);

  // The engine's own value (the dot) now swings around 0.7: from 0.3 up to the top, where it stops.
  await page.waitForTimeout(300);
  const dot = (await sampleCutoff(page, 2500)).flatMap((sample) => (sample.dot === null ? [] : [sample.dot]));
  expect(Math.min(...dot)).toBeGreaterThan(0.25);
  expect(Math.min(...dot)).toBeLessThan(0.4);
  expect(Math.max(...dot)).toBeGreaterThan(0.95);

  // And the ring is around the new value: it is drawn from where the LFO can take it.
  const ring = await knob(page, "cutoff").locator(".tang-knob__ring").getAttribute("d");
  expect(ring).not.toBeNull();
});

test("the keyboard turns a knob and a double click puts it back", async ({ page }) => {
  await open(page);
  const slider = page.getByRole("slider", { name: "resonance", exact: true });
  await expect(slider).toHaveAttribute("aria-valuenow", "2");
  await slider.focus();
  await page.keyboard.press("ArrowUp"); // a hundredth of the range of 9.5
  await expect.poll(async () => Number(await slider.getAttribute("aria-valuenow"))).toBeCloseTo(2.095, 2);
  await slider.dblclick();
  await expect.poll(async () => Number(await slider.getAttribute("aria-valuenow"))).toBeCloseTo(0.707, 3);
});

test("the choice of the filter's type is sent and kept", async ({ page }) => {
  await open(page);
  const type = page.getByRole("combobox", { name: "type" });
  await expect(type).toHaveValue("0");
  await type.selectOption({ label: "ladder" });
  await expect(type).toHaveValue("4");
  // The same answer from a second look at the server's own copy.
  await page.reload();
  await expect(page.getByRole("combobox", { name: "type" })).toHaveValue("4");
});

test.describe("the performance of the tracer scene (docs/web-ui.md §9.4)", () => {
  test("holds the frame rate while telemetry streams, and React renders nothing for it", async ({ page }) => {
    await open(page);
    await page.waitForTimeout(500);
    const before = await page.evaluate(() => ({ commits: window.tangCommits ?? 0, binary: window.__wire?.binary ?? 0 }));
    const frames = await page.evaluate(
      (duration) =>
        new Promise<number[]>((resolve) => {
          const at: number[] = [];
          const start = performance.now();
          const tick = (now: number): void => {
            at.push(now);
            if (now - start < duration) requestAnimationFrame(tick);
            else resolve(at);
          };
          requestAnimationFrame(tick);
        }),
      4000,
    );
    const after = await page.evaluate(() => ({ commits: window.tangCommits ?? 0, binary: window.__wire?.binary ?? 0 }));

    const intervals = frames.slice(1).map((at, i) => at - (frames[i] ?? at));
    const seconds = ((frames.at(-1) ?? 0) - (frames[0] ?? 0)) / 1000;
    const fps = (frames.length - 1) / seconds;
    const sorted = [...intervals].sort((a, b) => a - b);
    const p95 = sorted[Math.floor(sorted.length * 0.95)] ?? 0;
    test.info().annotations.push({ type: "fps", description: `${fps.toFixed(1)} fps, p95 frame ${p95.toFixed(1)} ms, ${String(after.binary - before.binary)} telemetry frames, ${String(after.commits - before.commits)} commits` });

    // Telemetry did stream (the engine makes about 30 frames a second for each of two channels)...
    expect(after.binary - before.binary).toBeGreaterThan(100);
    // ...the page drew it at (about) the display's rate...
    expect(fps).toBeGreaterThan(55);
    expect(p95).toBeLessThan(34);
    // ...and React did not render once for any of it.
    expect(after.commits - before.commits).toBe(0);
  });

  test("sends one intent for a burst of pointer moves in a frame, and it is the last", async ({ page }) => {
    await open(page);
    await page.waitForTimeout(300);
    const slider = page.getByRole("slider", { name: "cutoff", exact: true });
    const before = (await wireOf(page)).sends.length;
    // Fifty pointer moves in one task, which no hand makes, and which a fast mouse comes near.
    await slider.evaluate((dial) => {
      const box = dial.getBoundingClientRect();
      const x = box.x + box.width / 2;
      const y = box.y + box.height / 2;
      const at = (clientY: number, type: string) => new PointerEvent(type, { bubbles: true, pointerId: 7, button: 0, clientX: x, clientY });
      dial.dispatchEvent(at(y, "pointerdown"));
      for (let step = 1; step <= 50; step += 1) dial.dispatchEvent(at(y - step, "pointermove"));
    });
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    const during = (await wireOf(page)).sends.slice(before);
    expect(during.map((send) => send.intent.type)).toEqual(["setParam"]);
    // 50 pixels of a range of 200: a quarter of 0 to 1, from the 0.5 it was at.
    expect(during[0]?.intent.value).toBeCloseTo(0.75, 3);
    await slider.evaluate((dial) => {
      dial.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, pointerId: 7, button: 0 }));
    });
  });

  test("sends at most one intent a frame while a knob is dragged, soon after the pointer and answered at once", async ({ page }) => {
    await open(page);
    await page.waitForTimeout(300);
    await dragUp(page, "cutoff", 120, 120); // 120 moves, as fast as they come
    await expect.poll(async () => (await wireOf(page)).acks.length, { message: "answers to the drag" }).toBeGreaterThan(0);
    await page.waitForTimeout(300);
    const wire = await wireOf(page);
    const sends = wire.sends.filter((send) => send.intent.type === "setParam");

    // One intent a frame at most: a drag of 120 moves is a lot fewer intents than moves.
    expect(sends.length).toBeGreaterThan(3);
    expect(sends.length).toBeLessThanOrEqual(wire.moves.length);
    // (Counted in frames, not milliseconds: a browser that stalls runs two frames a few milliseconds apart.)
    const frames = sends.map((send) => send.frame);
    expect(new Set(frames).size, "two intents in one frame").toBe(frames.length);

    // Soon after the pointer: within a frame or two of the latest move.
    const delays = sends.map((send) => {
      const move = [...wire.moves].reverse().find((at) => at <= send.at);
      return move === undefined ? 0 : send.at - move;
    });
    expect(Math.max(...delays)).toBeLessThan(50);

    // And answered quickly, on the loopback.
    const answers = sends.flatMap((send) => {
      const ack = wire.acks.find((candidate) => candidate.reqId === send.reqId);
      return ack ? [ack.at - send.at] : [];
    });
    expect(answers.length).toBe(sends.length);
    const slowest = Math.max(...answers);
    test.info().annotations.push({ type: "drag", description: `${String(sends.length)} intents for ${String(wire.moves.length)} moves, latest ${Math.max(...delays).toFixed(1)} ms after a move, slowest answer ${slowest.toFixed(1)} ms` });
    expect(slowest).toBeLessThan(100);
  });
});
