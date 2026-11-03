import { describe, expect, test, vi } from "vitest";

import type { Port, TelemetryLayout } from "../protocol";
import { MASTER, nodeNamed, tracerSnapshot } from "./fixtures";
import { slotReader } from "./readers";
import { decodeFrame, TelemetryBuffers } from "./telemetry";

function frame(channel: number, layoutRev: number, values: readonly number[]): ArrayBuffer {
  const buffer = new ArrayBuffer(8 + 4 * values.length);
  const view = new DataView(buffer);
  view.setUint8(0, channel);
  view.setUint16(2, values.length, true);
  view.setUint32(4, layoutRev, true);
  values.forEach((value, i) => {
    view.setFloat32(8 + 4 * i, value, true);
  });
  return buffer;
}

const layout = tracerSnapshot.telemetry;
const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");
const cutoff: Port = { type: "param", index: 0 };
const modOut: Port = { type: "modOut" };

/** Buffers with the layout set, and a way to feed them modulation frames. */
function setup() {
  const buffers = new TelemetryBuffers();
  buffers.setLayout(layout);
  const feed = (values: readonly number[], rev = layout.rev) => {
    const decoded = decodeFrame(frame(1, rev, values));
    if (!decoded) throw new Error("not a frame");
    return buffers.accept(decoded);
  };
  return { buffers, feed };
}

describe("a reader of one slot", () => {
  test("gives what the latest frame says about its slot", () => {
    const { buffers, feed } = setup();
    const effective = slotReader(buffers, "modulation", filter.id, cutoff, "effective");
    const output = slotReader(buffers, "modulation", lfo.id, modOut, "modOutput");
    // The layout lists the LFO's output and then the cutoff's effective value.
    feed([-0.5, 0.25]);
    expect(output()).toBeCloseTo(-0.5, 6);
    expect(effective()).toBeCloseTo(0.25, 6);
    feed([0.75, 0.5]);
    expect(output()).toBeCloseTo(0.75, 6);
    expect(effective()).toBeCloseTo(0.5, 6);
  });

  test("has nothing before there is a layout, and nothing before a frame of it has come", () => {
    const buffers = new TelemetryBuffers();
    const read = slotReader(buffers, "modulation", filter.id, cutoff, "effective");
    expect(read()).toBeUndefined();
    buffers.setLayout(layout);
    expect(read()).toBeUndefined(); // the buffer has a zero, which is not a reading
    const decoded = decodeFrame(frame(1, layout.rev, [0.5, 0.25]));
    if (decoded) buffers.accept(decoded);
    expect(read()).toBeCloseTo(0.25, 6);
  });

  test("has nothing for a slot the layout does not have", () => {
    const { buffers, feed } = setup();
    feed([0.5, 0.25]);
    expect(slotReader(buffers, "modulation", filter.id, { type: "param", index: 3 }, "effective")()).toBeUndefined();
    expect(slotReader(buffers, "modulation", 999, cutoff, "effective")()).toBeUndefined();
    expect(slotReader(buffers, "modulation", filter.id, cutoff, "modOutput")()).toBeUndefined();
    expect(slotReader(buffers, "meters", filter.id, cutoff, "effective")()).toBeUndefined();
  });

  test("reads the channel it was made for, which has had a frame when the other has not", () => {
    const { buffers, feed } = setup();
    const peak = slotReader(buffers, "meters", MASTER, { type: "audioIn" }, "peak");
    feed([0.5, 0.25]);
    expect(peak()).toBeUndefined(); // no meters frame yet
    const decoded = decodeFrame(frame(0, layout.rev, layout.meters.map((entry) => (entry.kind === "peak" ? 0.9 : 0))));
    if (decoded) buffers.accept(decoded);
    expect(peak()).toBeCloseTo(0.9, 6);
  });

  test("finds its slot once for each layout, not once for each frame", () => {
    const { buffers, feed } = setup();
    const find = vi.spyOn(buffers, "find");
    const read = slotReader(buffers, "modulation", filter.id, cutoff, "effective");
    feed([0.1, 0.2]);
    for (let frames = 0; frames < 100; frames += 1) {
      feed([0.1, 0.2 + frames / 1000]);
      read();
    }
    expect(find).toHaveBeenCalledTimes(1);
  });

  test("finds it again when the layout changes, which may have moved it", () => {
    const { buffers, feed } = setup();
    const read = slotReader(buffers, "modulation", filter.id, cutoff, "effective");
    feed([0.5, 0.25]);
    expect(read()).toBeCloseTo(0.25, 6);
    // A layout in which the cutoff's value comes first, and the LFO's output second.
    const swapped: TelemetryLayout = {
      ...layout,
      rev: layout.rev + 1,
      modulation: [...layout.modulation].reverse().map((entry, slot) => ({ ...entry, slot: 100 + slot })),
    };
    buffers.setLayout(swapped);
    expect(read()).toBeUndefined(); // nothing of the new layout has come
    feed([0.125, 0.5], swapped.rev);
    expect(read()).toBeCloseTo(0.125, 6);
  });

  test("has nothing when the layout has changed to one without its slot", () => {
    const { buffers, feed } = setup();
    const read = slotReader(buffers, "modulation", filter.id, cutoff, "effective");
    feed([0.5, 0.25]);
    expect(read()).toBeCloseTo(0.25, 6);
    const without: TelemetryLayout = { ...layout, rev: layout.rev + 1, modulation: layout.modulation.filter((entry) => entry.node !== filter.id) };
    buffers.setLayout(without);
    feed([0.5], without.rev);
    expect(read()).toBeUndefined();
  });
});
