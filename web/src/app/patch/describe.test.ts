import { describe, expect, test } from "vitest";

import type { ModulatorSource } from "../../protocol";
import { cableWidth, detailOf, kindOf, outputScale } from "./describe";

const lfo: ModulatorSource = { type: "lfo", waveform: "triangle", rate: 0.5 };
const envelope: ModulatorSource = { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 2 };
const midi = (control: Extract<ModulatorSource, { type: "midiLearn" }>["control"], learning = false): ModulatorSource => ({ type: "midiLearn", control, learning });

describe("a modulator in words", () => {
  test("is of a kind", () => {
    expect(kindOf(lfo)).toBe("LFO");
    expect(kindOf(envelope)).toBe("Envelope");
    expect(kindOf(midi(null))).toBe("MIDI");
  });

  test("has its settings on one line", () => {
    expect(detailOf(lfo)).toBe("triangle, 0.5 Hz");
    expect(detailOf({ type: "lfo", waveform: "sine", rate: 12.345 })).toBe("sine, 12.3 Hz");
    expect(detailOf(envelope)).toBe("A 10 ms · D 300 ms · S 0.7 · R 2 s");
    expect(detailOf({ type: "envelope", attack: 1, decay: 0.999, sustain: 1, release: 10 })).toBe("A 1 s · D 999 ms · S 1 · R 10 s");
  });

  test("says what a MIDI control is bound to, or that it is not", () => {
    expect(detailOf(midi({ type: "cc", number: 74 }))).toBe("CC 74");
    expect(detailOf(midi({ type: "pitchBend" }))).toBe("pitch bend");
    expect(detailOf(midi({ type: "aftertouch" }))).toBe("aftertouch");
    expect(detailOf(midi(null))).toBe("not bound");
    expect(detailOf(midi(null, true))).toBe("waiting for a control…");
    // While it is learning it is waiting, whatever it was bound to.
    expect(detailOf(midi({ type: "cc", number: 1 }, true))).toBe("waiting for a control…");
  });
});

describe("the scale of a modulator's output", () => {
  test("goes both ways, from the middle, for a LFO", () => {
    expect(outputScale(lfo)).toEqual({ min: -1, max: 1, centred: true });
  });

  test("goes from nothing up for an envelope, a CC and pressure, and a control that is not bound", () => {
    expect(outputScale(envelope)).toEqual({ min: 0, max: 1, centred: false });
    expect(outputScale(midi({ type: "cc", number: 74 }))).toEqual({ min: 0, max: 1, centred: false });
    expect(outputScale(midi({ type: "aftertouch" }))).toEqual({ min: 0, max: 1, centred: false });
    expect(outputScale(midi(null))).toEqual({ min: 0, max: 1, centred: false });
  });

  test("goes both ways from the middle of 0 to 1 for the pitch bend, which rests there", () => {
    expect(outputScale(midi({ type: "pitchBend" }))).toEqual({ min: 0, max: 1, centred: true });
  });
});

describe("how thick a cable is", () => {
  test("is a thin line for no depth, and thicker for more, in either direction", () => {
    expect(cableWidth(0)).toBe(1.5);
    expect(cableWidth(0.5)).toBe(2.5);
    expect(cableWidth(1)).toBe(3.5);
    expect(cableWidth(-0.5)).toBe(2.5);
    expect(cableWidth(-1)).toBe(3.5);
  });

  test("is no thicker for more than there can be", () => {
    expect(cableWidth(5)).toBe(3.5);
    expect(cableWidth(-5)).toBe(3.5);
  });
});
