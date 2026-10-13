// How a modulator is put in words and on a scale, for its node: what kind it is, its
// settings, and the range its output is drawn over.
import type { ModulatorSource } from "../../protocol";
import { formatValue } from "../../ui/Knob";

/** A time in seconds as a person writes it: milliseconds below a second. */
function seconds(value: number): string {
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
  }
}

/** The settings of a source, on one line. */
export function detailOf(source: ModulatorSource): string {
  switch (source.type) {
    case "lfo":
      return `${source.waveform}, ${formatValue(source.rate)} Hz`;
    case "envelope":
      return `A ${seconds(source.attack)} · D ${seconds(source.decay)} · S ${formatValue(source.sustain)} · R ${seconds(source.release)}`;
    case "midiLearn":
      if (source.learning) return "waiting for a control…";
      if (source.control === null) return "not bound";
      switch (source.control.type) {
        case "cc":
          return `CC ${String(source.control.number)}`;
        case "pitchBend":
          return "pitch bend";
        case "aftertouch":
          return "aftertouch";
      }
  }
}

/** The range a source's output is drawn over: a LFO and the pitch bend go both ways (the bend's rest is 0.5), the rest from nothing up. */
export function outputScale(source: ModulatorSource): { min: number; max: number; centred: boolean } {
  if (source.type === "lfo") return { min: -1, max: 1, centred: true };
  if (source.type === "midiLearn" && source.control?.type === "pitchBend") return { min: 0, max: 1, centred: true };
  return { min: 0, max: 1, centred: false };
}

/** How thick to draw a modulation cable of this depth (-1 to 1): a deeper one is thicker, in either direction. */
export function cableWidth(amount: number): number {
  return 1.5 + 2 * Math.min(Math.abs(amount), 1);
}
