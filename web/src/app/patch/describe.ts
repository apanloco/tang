// How a modulator is put in words and on a scale, for its node: what kind it is, which
// settings it has, and the range its output is drawn over.
import type { ModulatorField, ModulatorSource } from "../../protocol";
import { formatValue } from "../../ui/Knob";

/** A time in seconds as a person writes it: milliseconds below a second. */
export function seconds(value: number): string {
  return value < 1 ? `${formatValue(value * 1000)} ms` : `${formatValue(value)} s`;
}

/** What kind of modulator a source is, in a word. */
export function kindOf(source: ModulatorSource): string {
  switch (source.type) {
    case "lfo":
      return "LFO";
    case "envelope":
      return "Envelope";
    case "midiLearn":
      return "MIDI";
    case "macro":
      return "Macro";
    case "steps":
      return "Steps";
  }
}

/**
 * A setting that is a cell of its node's grid of controls. The note length that an LFO or a step sequencer follows the tempo by is not: it is a choice in the node's header
 * (`SyncControl`), since a fifth control would make the grid a second row taller than the layout assumes, and its names (`1/16 triplet`) are longer than a cell has room for.
 */
export type SettingField = Exclude<ModulatorField, "sync">;

/** The settings a source has, in the order its node shows them as controls. */
export function settingsOf(source: ModulatorSource): readonly SettingField[] {
  switch (source.type) {
    case "lfo":
      return ["rate", "waveform", "retrigger", "fade"];
    case "envelope":
      return ["attack", "decay", "sustain", "release"];
    case "midiLearn":
      return [];
    case "macro":
      return ["value"];
    case "steps":
      return ["rate", "glide", "length", "retrigger"];
  }
}

/** What a MIDI control is bound to, in words (it has no settings to show as controls). */
export function controlDetail(source: Extract<ModulatorSource, { type: "midiLearn" }>): string {
  if (source.learning) return "waiting for a control…";
  if (source.control === null) return "not bound";
  switch (source.control.type) {
    case "cc":
      return `CC ${String(source.control.number)}`;
    case "pitchBend":
      return "pitch bend";
    case "aftertouch":
      return "aftertouch";
    case "velocity":
      return "velocity of the last note";
    case "key":
      return "pitch of the last note";
  }
}

/** The range the output of a modulator of a kind goes over: an LFO's and a step sequencer's go both ways, the rest from nothing up. */
export function outputRange(type: ModulatorSource["type"]): { min: number; max: number } {
  return type === "lfo" || type === "steps" ? { min: -1, max: 1 } : { min: 0, max: 1 };
}

/** The range a source's output is drawn over: a LFO, a step sequencer and the pitch bend go both ways (the bend's rest is 0.5), the rest from nothing up. */
export function outputScale(source: ModulatorSource): { min: number; max: number; centred: boolean } {
  const centred = source.type === "lfo" || source.type === "steps" || (source.type === "midiLearn" && source.control?.type === "pitchBend");
  return { ...outputRange(source.type), centred };
}

/** How thick to draw a modulation cable of this depth (-1 to 1): a deeper one is thicker, in either direction. */
export function cableWidth(amount: number): number {
  return 1.5 + 2 * Math.min(Math.abs(amount), 1);
}
