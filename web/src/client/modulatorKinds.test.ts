import { describe, expect, test } from "vitest";

import type { MidiControl, ModulatorKind, ModulatorSource } from "../protocol";
import { MODULATOR_LABELS, kindOf } from "./modulatorKinds";

const midi = (control: MidiControl | null, learning = false): ModulatorSource => ({ type: "midiLearn", control, learning });

describe("the kind of a modulator", () => {
  test("is the one that a button that adds it says: an LFO, an envelope, a macro", () => {
    expect(kindOf({ type: "lfo", waveform: "sine", rate: 1 })).toBe("lfo");
    expect(kindOf({ type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 })).toBe("envelope");
    expect(kindOf({ type: "macro", value: 0 })).toBe("macro");
  });

  test("is a MIDI control for what a controller sends or has not sent yet, and a velocity or a key for the notes the lane plays (`ModulatorKind::of`)", () => {
    for (const control of [null, { type: "cc", number: 74 }, { type: "pitchBend" }, { type: "aftertouch" }] as const) {
      expect(kindOf(midi(control)), JSON.stringify(control)).toBe("midiLearn");
    }
    expect(kindOf(midi(null, true))).toBe("midiLearn");
    expect(kindOf(midi({ type: "velocity" }))).toBe("velocity");
    expect(kindOf(midi({ type: "key" }))).toBe("key");
  });

  test("is told in the words of the button for each kind (`ModulatorKind::label`)", () => {
    const kinds: Record<ModulatorKind, string> = {
      lfo: "an LFO",
      envelope: "an envelope",
      midiLearn: "a MIDI control",
      macro: "a macro",
      velocity: "a velocity control",
      key: "a key control",
    };
    expect(MODULATOR_LABELS).toEqual(kinds);
  });
});
