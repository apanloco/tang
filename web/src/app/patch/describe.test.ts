import { describe, expect, test } from "vitest";

import type { ModulatorSource } from "../../protocol";
import { cableWidth, controlDetail, kindOf, outputRange, outputScale, seconds, settingsOf } from "./describe";

const lfo: ModulatorSource = { type: "lfo", waveform: "triangle", rate: 0.5 };
const envelope: ModulatorSource = { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 2 };
const macro: ModulatorSource = { type: "macro", value: 0.25 };
type Control = Extract<ModulatorSource, { type: "midiLearn" }>;
const midi = (control: Control["control"], learning = false): Control => ({ type: "midiLearn", control, learning });

describe("a modulator in words", () => {
  test("is of a kind", () => {
    expect(kindOf(lfo)).toBe("LFO");
    expect(kindOf(envelope)).toBe("Envelope");
    expect(kindOf(midi(null))).toBe("MIDI");
    expect(kindOf(macro)).toBe("Macro");
  });

  test("has the settings that its kind has, in the order they are shown", () => {
    expect(settingsOf(lfo)).toEqual(["rate", "waveform"]);
    expect(settingsOf(envelope)).toEqual(["attack", "decay", "sustain", "release"]);
    expect(settingsOf(midi({ type: "cc", number: 74 }))).toEqual([]);
    expect(settingsOf(macro)).toEqual(["value"]);
  });

  test("says what a MIDI control is bound to, or that it is not", () => {
    expect(controlDetail(midi({ type: "cc", number: 74 }))).toBe("CC 74");
    expect(controlDetail(midi({ type: "pitchBend" }))).toBe("pitch bend");
    expect(controlDetail(midi({ type: "aftertouch" }))).toBe("aftertouch");
    expect(controlDetail(midi(null))).toBe("not bound");
    expect(controlDetail(midi(null, true))).toBe("waiting for a control…");
    // While it is learning it is waiting, whatever it was bound to.
    expect(controlDetail(midi({ type: "cc", number: 1 }, true))).toBe("waiting for a control…");
  });
});

describe("a time", () => {
  test("is written in milliseconds below a second and in seconds from it", () => {
    expect(seconds(0.01)).toBe("10 ms");
    expect(seconds(0.3)).toBe("300 ms");
    expect(seconds(0.999)).toBe("999 ms");
    expect(seconds(1)).toBe("1 s");
    expect(seconds(2.5)).toBe("2.5 s");
    expect(seconds(10)).toBe("10 s");
    expect(seconds(0.001)).toBe("1 ms");
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

  test("goes from nothing up for a macro, which is a level", () => {
    expect(outputScale(macro)).toEqual({ min: 0, max: 1, centred: false });
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

describe("the range a modulator's output goes over", () => {
  test("is from -1 to 1 for an LFO, which goes both ways, and from 0 to 1 for the rest", () => {
    expect(outputRange("lfo")).toEqual({ min: -1, max: 1 });
    expect(outputRange("envelope")).toEqual({ min: 0, max: 1 });
    expect(outputRange("midiLearn")).toEqual({ min: 0, max: 1 });
    expect(outputRange("macro")).toEqual({ min: 0, max: 1 });
  });

  test("is the range of the scale the output is drawn over, whatever the source is", () => {
    for (const source of [lfo, envelope, macro, midi(null), midi({ type: "cc", number: 1 }), midi({ type: "pitchBend" }), midi({ type: "aftertouch" })]) {
      const { min, max } = outputScale(source);
      expect(outputRange(source.type), source.type).toEqual({ min, max });
    }
  });
});
