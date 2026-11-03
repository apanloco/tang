// @vitest-environment jsdom
// The output mix on the canvas (docs/web-ui.md step 102): its volume, the limiter that holds it under a ceiling (a button that puts it in the sound or takes it out, the ceiling, and how far it has
// brought the sound down).
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { MASTER, tracerSnapshot } from "../../client/fixtures";
import { decodeFrame } from "../../client/telemetry";
import type { Clock } from "../../client/testing";
import type { Snapshot, TelemetryLayout } from "../../protocol";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

const master = (): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(MASTER)}`);

/**
 * Turn the frames until `done` holds, and fail if it does not within a second: a knob asks for its value on a frame (`Drive`: one intent a frame), so a request is not made until the test turns one.
 * (Not `waitFor` with the frames turned inside it: a callback that changes the page is run again by the change, and one that keeps failing never gives up.)
 */
async function until(clock: Clock, done: () => boolean, what: string): Promise<void> {
  const deadline = Date.now() + 1000;
  while (!done()) {
    if (Date.now() > deadline) throw new Error(`${what}: not within a second`);
    act(() => {
      clock.tick(performance.now());
    });
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

/** A frame of the meters with the limiter's reduction at `reduction` and every other cell at nothing, as the layout names the cells. */
function reductionFrame(layout: TelemetryLayout, reduction: number): ArrayBuffer {
  const values = layout.meters.map((entry) => (entry.kind === "reduction" ? reduction : 0));
  const buffer = new ArrayBuffer(8 + 4 * values.length);
  const data = new DataView(buffer);
  data.setUint16(2, values.length, true);
  data.setUint32(4, layout.rev, true);
  values.forEach((value, i) => {
    data.setFloat32(8 + 4 * i, value, true);
  });
  return buffer;
}

/** The tracer with the master's volume, its limiter and its ceiling set. */
function mixed(settings: { volume?: number; limiter?: boolean; ceiling?: number }): Snapshot {
  const snapshot = structuredClone(tracerSnapshot);
  const node = snapshot.graph.nodes.find((n) => n.id === MASTER);
  if (node?.kind !== "master") throw new Error("no master");
  Object.assign(node, settings);
  return snapshot;
}

describe("the output mix", () => {
  test("has its volume, the ceiling of its limiter and a button for the limiter in the cells of its grid, and no other node has them", async () => {
    await rig(<PatchView />);
    const card = await master();
    const cells = Array.from(card.querySelectorAll(".tang-node__controls > .tang-cell"));
    expect(cells).toHaveLength(3);
    expect(within(card).getByRole("slider", { name: "Master volume" }).getAttribute("aria-valuenow")).toBe("1");
    expect(within(card).getByRole("slider", { name: "Ceiling" }).getAttribute("aria-valuenow")).toBe("-1");
    // (The volume is written `Volume` under its knob, which is all that fits there: its name is `Master volume`.)
    expect(Array.from(card.querySelectorAll(".tang-node__controls .tang-knob__label")).map((label) => label.textContent)).toEqual(["Volume", "Ceiling"]);
    expect(cells.at(-1)?.textContent).toContain("Limiter");
    expect(within(card).getByRole("button", { name: "Limiter" }).getAttribute("aria-pressed")).toBe("true");
    expect(within(card).getByRole("meter", { name: "Limiter reduction" })).toBeTruthy();
    // (A lane's volume is its own: the master's has another name, so that each of them is one slider of that name.)
    expect(screen.getAllByRole("slider", { name: "Volume" })).toHaveLength(1);
    expect(screen.getAllByRole("slider", { name: "Master volume" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Limiter" })).toHaveLength(1);
  });

  test("says what the session says: the limiter pressed while it is in the sound, and the volume and the ceiling as they are, the ceiling in decibels", async () => {
    await rig(<PatchView />, { snapshot: mixed({ volume: 0.75, limiter: false, ceiling: -6.5 }) });
    const card = await master();
    const button = within(card).getByRole("button", { name: "Limiter" });
    expect(button.getAttribute("aria-pressed")).toBe("false");
    expect(button.textContent).toBe("Off");
    expect(within(card).getByRole("slider", { name: "Master volume" }).getAttribute("aria-valuenow")).toBe("0.75");
    const ceiling = within(card).getByRole("slider", { name: "Ceiling" });
    expect([ceiling.getAttribute("aria-valuenow"), ceiling.getAttribute("aria-valuetext")]).toEqual(["-6.5", "-6.5 dB"]);
    expect(ceiling.getAttribute("aria-valuemin")).toBe("-24");
    expect(ceiling.getAttribute("aria-valuemax")).toBe("0");
    expect(within(card).getByRole("slider", { name: "Master volume" }).getAttribute("aria-valuemax")).toBe("2");
  });

  test("a press on the limiter asks for it to go out of the sound, and the button shows what the server has done: out, and in again", async () => {
    const { requests } = await rig(<PatchView />);
    const card = await master();
    fireEvent.click(within(card).getByRole("button", { name: "Limiter" }));
    await waitFor(() => {
      expect(within(card).getByRole("button", { name: "Limiter" }).getAttribute("aria-pressed")).toBe("false");
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setLimiter", on: false }]);
    expect(within(card).getByRole("button", { name: "Limiter" }).textContent).toBe("Off");
    fireEvent.click(within(card).getByRole("button", { name: "Limiter" }));
    await waitFor(() => {
      expect(within(card).getByRole("button", { name: "Limiter" }).getAttribute("aria-pressed")).toBe("true");
    });
    expect(requests().map((request) => request.intent).at(-1)).toEqual({ type: "setLimiter", on: true });
  });

  test("has a title that says what a press does", async () => {
    await rig(<PatchView />);
    expect((await within(await master()).findByRole("button", { name: "Limiter" })).title).toMatch(/take it out of the sound/);
    cleanup();
    await rig(<PatchView />, { snapshot: mixed({ limiter: false }) });
    expect((await within(await master()).findByRole("button", { name: "Limiter" })).title).toMatch(/press to put it in/);
  });

  test("the knobs ask for the master's gains by the node of the master: a key press turns the volume and the ceiling a step, and a double click puts each back where it starts", async () => {
    const { clock, requests } = await rig(<PatchView />, { snapshot: mixed({ volume: 0.5, ceiling: -6 }) });
    const card = await master();
    const asked = (gain: string, value?: number) => (): boolean => {
      const intent = requests().at(-1)?.intent as { type: string; node: number; gain: string; value: number } | undefined;
      return intent?.type === "setGain" && intent.node === MASTER && intent.gain === gain && (value === undefined || intent.value === value);
    };
    fireEvent.keyDown(within(card).getByRole("slider", { name: "Master volume" }), { key: "ArrowRight" });
    await until(clock, asked("masterVolume"), "a key press asks for the volume");
    expect((requests().at(-1)?.intent as { value: number }).value).toBeGreaterThan(0.5);
    fireEvent.doubleClick(within(card).getByRole("slider", { name: "Master volume" }));
    await until(clock, asked("masterVolume", 1), "a double click asks for a volume of 1");
    fireEvent.doubleClick(within(card).getByRole("slider", { name: "Ceiling" }));
    await until(clock, asked("limiterCeiling", -1), "a double click asks for a ceiling of -1");
    // (And the server's answers are what the knobs show.)
    await waitFor(() => {
      expect([within(card).getByRole("slider", { name: "Master volume" }).getAttribute("aria-valuenow"), within(card).getByRole("slider", { name: "Ceiling" }).getAttribute("aria-valuenow")]).toEqual(["1", "-1"]);
    });
  });

  test("has no place for a cable and no button that modulates it: no modulator drives the master's volume or its ceiling", async () => {
    await rig(<PatchView />);
    const card = await master();
    expect(card.querySelectorAll(".tang-node__controls .tang-port")).toHaveLength(0);
    expect(within(card).queryByRole("button", { name: /^Modulate/ })).toBeNull();
  });

  test("shows how far the limiter has brought the sound down, in decibels, from the telemetry, and nothing while there is none", async () => {
    const { client, clock } = await rig(<PatchView />, { telemetryPeriod: 3_600_000 });
    const card = await master();
    const bar = within(card).getByRole("meter", { name: "Limiter reduction" });
    expect(bar.getAttribute("aria-valuemin")).toBe("0");
    expect(bar.getAttribute("aria-valuemax")).toBe("18");
    const layout = client.state.snapshot?.telemetry;
    if (!layout) throw new Error("no layout");
    expect(layout.meters.filter((entry) => entry.kind === "reduction" && entry.node === MASTER)).toHaveLength(1);
    // Nothing is said while there is no reading: the readout is empty and the bar has no value.
    act(() => {
      clock.tick(0);
    });
    expect(card.querySelector(".tang-reduction__value")?.textContent).toBe("");
    expect(bar.hasAttribute("aria-valuenow")).toBe(false);
    for (const [reduction, text] of [[3.25, "-3.3 dB"], [0, "0 dB"], [0.02, "0 dB"], [0.2, "-0.2 dB"], [12, "-12.0 dB"]] as const) {
      const decoded = decodeFrame(reductionFrame(layout, reduction));
      if (!decoded) throw new Error("the frame does not decode");
      client.telemetry.accept(decoded);
      act(() => {
        clock.tick(0);
      });
      expect(card.querySelector(".tang-reduction__value")?.textContent, `${String(reduction)} dB`).toBe(text);
      expect(Number(bar.getAttribute("aria-valuenow"))).toBeCloseTo(reduction, 5);
    }
  });

  test("writes the reduction's number to the page when it changes and not on every frame: a page that is written to on every frame never settles for a test that waits for it (this is what hung the suite)", async () => {
    const { client, clock } = await rig(<PatchView />, { telemetryPeriod: 3_600_000 });
    const card = await master();
    const readout = card.querySelector(".tang-reduction__value");
    const layout = client.state.snapshot?.telemetry;
    if (!readout || !layout) throw new Error("no readout or no layout");
    const records: MutationRecord[] = [];
    const observer = new MutationObserver((made) => {
      records.push(...made);
    });
    observer.observe(readout, { childList: true, characterData: true, subtree: true });
    let at = 0;
    const show = async (reduction: number, frames: number): Promise<void> => {
      const decoded = decodeFrame(reductionFrame(layout, reduction));
      if (!decoded) throw new Error("the frame does not decode");
      client.telemetry.accept(decoded);
      for (let i = 0; i < frames; i += 1) {
        at += 16;
        act(() => {
          clock.tick(at);
        });
      }
      // (A mutation observer is told after the task.)
      await new Promise((resolve) => setTimeout(resolve, 0));
    };
    await show(3.25, 6);
    expect(readout.textContent).toBe("-3.3 dB");
    expect(records, "one write for the first number, and none for the frames that have the same").toHaveLength(1);
    await show(3.25, 6);
    expect(records, "the same number again is no write").toHaveLength(1);
    await show(12, 6);
    expect(readout.textContent).toBe("-12.0 dB");
    expect(records, "a number that is another is one more").toHaveLength(2);
    observer.disconnect();
  });
});
