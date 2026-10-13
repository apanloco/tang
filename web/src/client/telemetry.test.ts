import { describe, expect, test } from "vitest";

import type { TelemetryLayout } from "../protocol";
import { MASTER, nodeNamed, tracerSnapshot } from "./fixtures";
import { decodeFrame, portKey, TelemetryBuffers, TelemetryIndex } from "./telemetry";

/** A frame as the server writes it: `u8 channel | u8 flags | u16 n | u32 layout_rev | n × f32`, little endian. */
function frame(channel: number, layoutRev: number, values: readonly number[], flags = 0): ArrayBuffer {
  const buffer = new ArrayBuffer(8 + 4 * values.length);
  const view = new DataView(buffer);
  view.setUint8(0, channel);
  view.setUint8(1, flags);
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

describe("decoding a frame", () => {
  test("reads what the Rust side writes", () => {
    // The bytes of `telemetry_frame(Modulation, 0x01020304, &[1.0, -2.5, 0.25])`, which the
    // Rust test `a_telemetry_frame_is_a_small_header_and_little_endian_floats` writes out.
    const bytes = new Uint8Array([1, 0, 3, 0, 4, 3, 2, 1, 0, 0, 0x80, 0x3f, 0, 0, 0x20, 0xc0, 0, 0, 0x80, 0x3e]);
    const decoded = decodeFrame(bytes);
    expect(decoded?.channel).toBe("modulation");
    expect(decoded?.layoutRev).toBe(0x01020304);
    expect(Array.from(decoded?.values ?? [])).toEqual([1, -2.5, 0.25]);
  });

  test("knows the meters from the modulation", () => {
    expect(decodeFrame(frame(0, 7, []))?.channel).toBe("meters");
    expect(decodeFrame(frame(1, 7, []))?.channel).toBe("modulation");
  });

  test("reads a view into a bigger buffer as well as a whole one", () => {
    const inner = new Uint8Array(frame(0, 9, [0.5, 0.75]));
    const padded = new Uint8Array(3 + inner.length + 5);
    padded.set(inner, 3);
    const decoded = decodeFrame(padded.subarray(3, 3 + inner.length));
    expect(decoded?.layoutRev).toBe(9);
    expect(Array.from(decoded?.values ?? [])).toEqual([0.5, 0.75]);
  });

  test("ignores the flags, which are reserved", () => {
    expect(decodeFrame(frame(0, 1, [1], 0xff))?.values[0]).toBe(1);
  });

  test("gives null for what is not a frame", () => {
    expect(decodeFrame(new ArrayBuffer(0))).toBeNull();
    expect(decodeFrame(new ArrayBuffer(7))).toBeNull();
    expect(decodeFrame(frame(2, 1, [1]))).toBeNull(); // a channel this client does not know
    expect(decodeFrame(frame(255, 1, []))).toBeNull();
    const short = new Uint8Array(frame(0, 1, [1, 2])).subarray(0, 12); // says two values, has one
    expect(decodeFrame(short)).toBeNull();
    const long = new Uint8Array(frame(0, 1, [1])).slice();
    const padded = new Uint8Array(long.length + 4);
    padded.set(long);
    expect(decodeFrame(padded)).toBeNull(); // says one value, has two
  });
});

describe("the index", () => {
  const index = new TelemetryIndex(layout);

  test("finds the master's peak where the layout puts it", () => {
    const at = layout.meters.findIndex((e) => e.node === MASTER && e.kind === "peak");
    expect(at).toBeGreaterThanOrEqual(0);
    expect(index.find("meters", MASTER, { type: "audioIn" }, "peak")).toBe(at);
  });

  test("finds what a modulator puts out and what a modulated parameter is set to", () => {
    const out = layout.modulation.findIndex((e) => e.node === lfo.id && e.kind === "modOutput");
    const effective = layout.modulation.findIndex((e) => e.node === filter.id && e.kind === "effective");
    expect(out).toBeGreaterThanOrEqual(0);
    expect(effective).toBeGreaterThanOrEqual(0);
    expect(index.find("modulation", lfo.id, { type: "modOut" }, "modOutput")).toBe(out);
    expect(index.find("modulation", filter.id, { type: "param", index: 0 }, "effective")).toBe(effective);
  });

  test("tells the channels, the kinds and the ports apart", () => {
    expect(index.find("modulation", MASTER, { type: "audioIn" }, "peak")).toBeUndefined();
    expect(index.find("meters", MASTER, { type: "audioIn" }, "clips")).not.toBe(index.find("meters", MASTER, { type: "audioIn" }, "peak"));
    expect(index.find("modulation", filter.id, { type: "param", index: 1 }, "effective")).toBeUndefined();
    expect(index.find("meters", 999, { type: "audioOut" }, "peak")).toBeUndefined();
  });

  test("writes ports as distinct keys", () => {
    expect(portKey({ type: "param", index: 3 })).toBe("param:3");
    expect(portKey({ type: "param", index: 4 })).not.toBe(portKey({ type: "param", index: 3 }));
    expect(portKey({ type: "depth", edge: 3 })).not.toBe(portKey({ type: "param", index: 3 }));
    expect(portKey({ type: "audioIn" })).not.toBe(portKey({ type: "audioOut" }));
  });
});

describe("the buffers", () => {
  const meters = layout.meters.length;
  const modulation = layout.modulation.length;

  function ready(): TelemetryBuffers {
    const buffers = new TelemetryBuffers();
    buffers.setLayout(layout);
    return buffers;
  }

  test("have nothing before there is a layout, and a zero for every entry after", () => {
    const buffers = new TelemetryBuffers();
    expect(buffers.values("meters")).toBeUndefined();
    buffers.setLayout(layout);
    expect(Array.from(buffers.values("meters") ?? [])).toEqual(new Array<number>(meters).fill(0));
    expect(buffers.values("modulation")?.length).toBe(modulation);
  });

  test("take a frame of the layout into the array a reader already holds", () => {
    const buffers = ready();
    const held = buffers.values("modulation");
    const values = Array.from({ length: modulation }, (_, i) => 0.25 * (i + 1));
    const decoded = decodeFrame(frame(1, layout.rev, values));
    expect(decoded && buffers.accept(decoded)).toBe(true);
    expect(buffers.values("modulation")).toBe(held);
    expect(Array.from(held ?? [])).toEqual(values);
    expect(buffers.frames).toBe(1);
  });

  test("keep the channels apart", () => {
    const buffers = ready();
    const decoded = decodeFrame(frame(0, layout.rev, new Array<number>(meters).fill(0.5)));
    expect(decoded && buffers.accept(decoded)).toBe(true);
    expect(Array.from(buffers.values("modulation") ?? [])).toEqual(new Array<number>(modulation).fill(0));
  });

  test("refuse a frame of another layout, or of another length, and keep what they have", () => {
    const buffers = ready();
    buffers.accept(decodeFrame(frame(0, layout.rev, new Array<number>(meters).fill(0.5))) ?? never());
    const other = decodeFrame(frame(0, layout.rev + 1, new Array<number>(meters).fill(0.9)));
    const wrong = decodeFrame(frame(0, layout.rev, [0.9]));
    expect(other && buffers.accept(other)).toBe(false);
    expect(wrong && buffers.accept(wrong)).toBe(false);
    expect(buffers.values("meters")?.[0]).toBe(0.5);
    expect(buffers.frames).toBe(1);
  });

  test("refuse every frame until there is a layout", () => {
    const buffers = new TelemetryBuffers();
    const decoded = decodeFrame(frame(0, layout.rev, new Array<number>(meters).fill(0.5)));
    expect(decoded && buffers.accept(decoded)).toBe(false);
  });

  test("start again, with new arrays and zeros, when the layout changes", () => {
    const buffers = ready();
    buffers.accept(decodeFrame(frame(0, layout.rev, new Array<number>(meters).fill(0.5))) ?? never());
    const held = buffers.values("meters");
    const changed: TelemetryLayout = { ...layout, rev: layout.rev + 1, meters: layout.meters.slice(0, 2) };
    buffers.setLayout(changed);
    expect(buffers.values("meters")).not.toBe(held);
    expect(Array.from(buffers.values("meters") ?? [])).toEqual([0, 0]);
    // and the old layout's frames are of no use now
    const stale = decodeFrame(frame(0, layout.rev, new Array<number>(meters).fill(1)));
    expect(stale && buffers.accept(stale)).toBe(false);
  });

  test("keep their values when they are told the layout they already have", () => {
    const buffers = ready();
    buffers.accept(decodeFrame(frame(0, layout.rev, new Array<number>(meters).fill(0.5))) ?? never());
    const held = buffers.values("meters");
    buffers.setLayout({ ...layout });
    expect(buffers.values("meters")).toBe(held);
    expect(held?.[0]).toBe(0.5);
  });

  test("look a value up by what it is", () => {
    const buffers = ready();
    const values = layout.meters.map((e) => (e.node === MASTER && e.kind === "peak" ? 0.75 : 0.125));
    buffers.accept(decodeFrame(frame(0, layout.rev, values)) ?? never());
    expect(buffers.value("meters", MASTER, { type: "audioIn" }, "peak")).toBe(0.75);
    expect(buffers.value("meters", MASTER, { type: "audioIn" }, "clips")).toBe(0.125);
    expect(buffers.value("meters", 999, { type: "audioIn" }, "peak")).toBeUndefined();
    expect(new TelemetryBuffers().value("meters", MASTER, { type: "audioIn" }, "peak")).toBeUndefined();
  });
});

/** For the places a test has already shown a value is there. */
function never(): never {
  throw new Error("expected a decoded frame");
}
