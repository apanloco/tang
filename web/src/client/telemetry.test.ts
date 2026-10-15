import { describe, expect, test } from "vitest";

import { SCOPE_FRAME_LENGTH } from "../protocol";
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

  test("knows the meters from the modulation, from the held notes and from the scope", () => {
    expect(decodeFrame(frame(0, 7, []))?.channel).toBe("meters");
    expect(decodeFrame(frame(1, 7, []))?.channel).toBe("modulation");
    expect(decodeFrame(frame(2, 0, []))?.channel).toBe("held");
    expect(decodeFrame(frame(3, 0, []))?.channel).toBe("scope");
  });

  test("reads the scope as the Rust side writes it: the values of the left side and then the right, no layout", () => {
    // The bytes of `telemetry_frame(Scope, 0, &[0.0, 1.0])`, which the Rust test `a_telemetry_frame_is_a_small_header_and_little_endian_floats` writes out.
    const decoded = decodeFrame(new Uint8Array([3, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0x3f]));
    expect(decoded?.channel).toBe("scope");
    expect(decoded?.layoutRev).toBe(0);
    expect(Array.from(decoded?.values ?? [])).toEqual([0, 1]);
  });

  test("reads the held notes as the Rust side writes them: a value for each note, no layout", () => {
    // The bytes of `telemetry_frame(Held, 0, &[0.0, 1.0])`.
    const decoded = decodeFrame(new Uint8Array([2, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x80, 0x3f]));
    expect(decoded?.channel).toBe("held");
    expect(decoded?.layoutRev).toBe(0);
    expect(Array.from(decoded?.values ?? [])).toEqual([0, 1]);
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
    expect(decodeFrame(frame(4, 1, [1]))).toBeNull(); // a channel this client does not know
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

describe("the held notes in the buffers", () => {
  const noteFrame = (...held: number[]) => {
    const values = new Array<number>(128).fill(0);
    for (const note of held) values[note] = 1;
    return decodeFrame(frame(2, 0, values)) ?? never();
  };

  test("are known before there is a layout and follow none: a frame is taken, and the notes are by their numbers", () => {
    const buffers = new TelemetryBuffers();
    expect(buffers.has("held")).toBe(false);
    expect(buffers.isHeld(60)).toBe(false);
    expect(buffers.accept(noteFrame(60, 64))).toBe(true);
    expect(buffers.has("held")).toBe(true);
    expect([buffers.isHeld(59), buffers.isHeld(60), buffers.isHeld(61), buffers.isHeld(64)]).toEqual([false, true, false, true]);
    expect(buffers.frames).toBe(1);
    expect(buffers.values("held")?.length).toBe(128);
  });

  test("are taken into the array a reader already holds, and the next frame says what is held now", () => {
    const buffers = new TelemetryBuffers();
    buffers.accept(noteFrame(60));
    const mine = buffers.values("held");
    buffers.accept(noteFrame(62));
    expect(buffers.values("held")).toBe(mine);
    expect([buffers.isHeld(60), buffers.isHeld(62)]).toEqual([false, true]);
  });

  test("refuse a frame that does not have a value for each of the 128 notes, and keep what they have", () => {
    const buffers = new TelemetryBuffers();
    buffers.accept(noteFrame(60));
    const short = decodeFrame(frame(2, 0, new Array<number>(127).fill(1))) ?? never();
    const long = decodeFrame(frame(2, 0, new Array<number>(129).fill(1))) ?? never();
    expect(buffers.accept(short)).toBe(false);
    expect(buffers.accept(long)).toBe(false);
    expect(buffers.isHeld(60)).toBe(true);
    expect(buffers.frames).toBe(1);
  });

  test("are not forgotten when the layout changes, which they do not follow, and do not need one to be heard", () => {
    const buffers = new TelemetryBuffers();
    buffers.accept(noteFrame(60));
    buffers.setLayout(layout);
    expect(buffers.has("held")).toBe(true);
    expect(buffers.isHeld(60)).toBe(true);
    buffers.setLayout({ ...layout, rev: layout.rev + 1 });
    expect(buffers.has("held")).toBe(true);
    expect(buffers.isHeld(60)).toBe(true);
    expect(buffers.layoutRev).toBe(layout.rev + 1);
  });

  test("say that a note that is not on the keyboard is not held, whatever its number", () => {
    const buffers = new TelemetryBuffers();
    buffers.accept(noteFrame(0, 127));
    expect([buffers.isHeld(0), buffers.isHeld(127), buffers.isHeld(128), buffers.isHeld(-1)]).toEqual([true, true, false, false]);
  });

  test("can be forgotten, in the array a reader already holds, and are known again by the next frame", () => {
    const buffers = new TelemetryBuffers();
    buffers.accept(noteFrame(60, 64));
    const mine = buffers.values("held");
    buffers.forgetHeld();
    expect(buffers.values("held")).toBe(mine);
    expect(Array.from(mine ?? []).every((value) => value === 0)).toBe(true);
    expect([buffers.isHeld(60), buffers.isHeld(64)]).toEqual([false, false]);
    expect(buffers.has("held")).toBe(true);
    buffers.accept(noteFrame(67));
    expect([buffers.isHeld(60), buffers.isHeld(67)]).toEqual([false, true]);
  });

  test("being forgotten leaves the other channels as they are", () => {
    const buffers = new TelemetryBuffers();
    buffers.setLayout(layout);
    const values = layout.meters.map((_, i) => i + 1);
    expect(buffers.accept(decodeFrame(frame(0, layout.rev, values)) ?? never())).toBe(true);
    buffers.accept(noteFrame(60));
    buffers.forgetHeld();
    expect(Array.from(buffers.values("meters") ?? [])).toEqual(values);
  });
});

describe("the scope in the buffers", () => {
  /** A frame of the scope: `SCOPE_FRAME_LENGTH` values of the left side, with the one given last, and then as many of the right. */
  const scopeFrame = (lastLeft = 0, lastRight = 0) => {
    const values = new Array<number>(2 * SCOPE_FRAME_LENGTH).fill(0);
    values[SCOPE_FRAME_LENGTH - 1] = lastLeft;
    values[2 * SCOPE_FRAME_LENGTH - 1] = lastRight;
    return decodeFrame(frame(3, 0, values)) ?? never();
  };

  test("are known before there is a layout and follow none: a frame is taken, the left side and then the right", () => {
    const buffers = new TelemetryBuffers();
    expect(buffers.has("scope")).toBe(false);
    expect(buffers.accept(scopeFrame(0.5, -0.25))).toBe(true);
    expect(buffers.has("scope")).toBe(true);
    const values = buffers.values("scope");
    expect(values?.length).toBe(2 * SCOPE_FRAME_LENGTH);
    expect([values?.[SCOPE_FRAME_LENGTH - 1], values?.[2 * SCOPE_FRAME_LENGTH - 1]]).toEqual([0.5, -0.25]);
    expect(buffers.frames).toBe(1);
  });

  test("are taken into the array a reader already holds", () => {
    const buffers = new TelemetryBuffers();
    buffers.accept(scopeFrame(0.5));
    const mine = buffers.values("scope");
    buffers.accept(scopeFrame(0.75));
    expect(buffers.values("scope")).toBe(mine);
    expect(mine?.[SCOPE_FRAME_LENGTH - 1]).toBe(0.75);
  });

  test("refuse a frame that is not a scope's length, and keep what they have", () => {
    const buffers = new TelemetryBuffers();
    buffers.accept(scopeFrame(0.5));
    const short = decodeFrame(frame(3, 0, new Array<number>(2 * SCOPE_FRAME_LENGTH - 1).fill(1))) ?? never();
    const long = decodeFrame(frame(3, 0, new Array<number>(2 * SCOPE_FRAME_LENGTH + 1).fill(1))) ?? never();
    expect([buffers.accept(short), buffers.accept(long)]).toEqual([false, false]);
    expect(buffers.values("scope")?.[SCOPE_FRAME_LENGTH - 1]).toBe(0.5);
    expect(buffers.frames).toBe(1);
  });

  test("are not forgotten when the layout changes, which they do not follow", () => {
    const buffers = new TelemetryBuffers();
    buffers.accept(scopeFrame(0.5));
    buffers.setLayout(layout);
    buffers.setLayout({ ...layout, rev: layout.rev + 1 });
    expect(buffers.has("scope")).toBe(true);
    expect(buffers.values("scope")?.[SCOPE_FRAME_LENGTH - 1]).toBe(0.5);
  });

  test("are counted by the frames of their own, so that a drawing can tell that there is a new one", () => {
    const buffers = new TelemetryBuffers();
    buffers.setLayout(layout);
    expect([buffers.framesOf("scope"), buffers.framesOf("meters")]).toEqual([0, 0]);
    buffers.accept(scopeFrame());
    buffers.accept(scopeFrame());
    buffers.accept(decodeFrame(frame(0, layout.rev, layout.meters.map(() => 0))) ?? never());
    expect([buffers.framesOf("scope"), buffers.framesOf("meters"), buffers.framesOf("held"), buffers.frames]).toEqual([2, 1, 0, 3]);
    buffers.accept(decodeFrame(frame(3, 0, [1, 2, 3])) ?? never());
    expect(buffers.framesOf("scope"), "a frame that was refused is not counted").toBe(2);
  });

  test("can be forgotten, in the array a reader already holds, and are known again by the next frame", () => {
    const buffers = new TelemetryBuffers();
    buffers.accept(scopeFrame(0.5, 0.25));
    const mine = buffers.values("scope");
    buffers.forgetScope();
    expect(buffers.values("scope")).toBe(mine);
    expect(Array.from(mine ?? []).every((value) => value === 0)).toBe(true);
    expect(buffers.has("scope")).toBe(true);
    buffers.accept(scopeFrame(0.75));
    expect(mine?.[SCOPE_FRAME_LENGTH - 1]).toBe(0.75);
  });

  test("being forgotten leaves the held notes and the other channels as they are", () => {
    const buffers = new TelemetryBuffers();
    buffers.setLayout(layout);
    buffers.accept(decodeFrame(frame(2, 0, new Array<number>(128).fill(1))) ?? never());
    const values = layout.meters.map((_, i) => i + 1);
    buffers.accept(decodeFrame(frame(0, layout.rev, values)) ?? never());
    buffers.accept(scopeFrame(0.5));
    buffers.forgetScope();
    expect(buffers.isHeld(60)).toBe(true);
    expect(Array.from(buffers.values("meters") ?? [])).toEqual(values);
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

  test("say which layout they follow", () => {
    const buffers = new TelemetryBuffers();
    expect(buffers.layoutRev).toBeUndefined();
    buffers.setLayout(layout);
    expect(buffers.layoutRev).toBe(layout.rev);
    buffers.setLayout({ ...layout, rev: layout.rev + 7, meters: [] });
    expect(buffers.layoutRev).toBe(layout.rev + 7);
  });

  test("say for each channel whether a frame of the layout has come, which a zero cannot", () => {
    const buffers = new TelemetryBuffers();
    expect(buffers.has("meters")).toBe(false);
    buffers.setLayout(layout);
    expect(buffers.has("meters")).toBe(false);
    expect(buffers.has("modulation")).toBe(false);
    buffers.accept(decodeFrame(frame(0, layout.rev, new Array<number>(meters).fill(0))) ?? never());
    expect(buffers.has("meters")).toBe(true);
    expect(buffers.has("modulation")).toBe(false);
    // A frame that was refused is not one that came.
    buffers.accept(decodeFrame(frame(1, layout.rev + 1, new Array<number>(modulation).fill(0))) ?? never());
    expect(buffers.has("modulation")).toBe(false);
    buffers.accept(decodeFrame(frame(1, layout.rev, new Array<number>(modulation).fill(0))) ?? never());
    expect(buffers.has("modulation")).toBe(true);
  });

  test("forget what has come when the layout changes, and not when it is told again", () => {
    const buffers = ready();
    buffers.accept(decodeFrame(frame(0, layout.rev, new Array<number>(meters).fill(0.5))) ?? never());
    buffers.setLayout({ ...layout });
    expect(buffers.has("meters")).toBe(true);
    buffers.setLayout({ ...layout, rev: layout.rev + 1 });
    expect(buffers.has("meters")).toBe(false);
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
