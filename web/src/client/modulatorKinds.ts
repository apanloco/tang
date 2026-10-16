// The kinds of modulator a button adds, and which kind a modulator is: the words of both, as the controller has them (`ModulatorKind::label` and `ModulatorKind::of`).
import type { ModulatorKind, ModulatorSource } from "../protocol";

/** What a button that adds a modulator says it adds. */
export const MODULATOR_LABELS: Record<ModulatorKind, string> = {
  lfo: "an LFO",
  envelope: "an envelope",
  midiLearn: "a MIDI control",
  macro: "a macro",
  velocity: "a velocity control",
  key: "a key control",
};

/**
 * The kind that a modulator with `source` is. A control that follows the notes of a lane is a MIDI control to its node (`ModulatorSource` has no kind for it), and a kind of its own to the
 * button that adds one; the others that a controller moves, or that wait for one, are a MIDI control.
 */
export function kindOf(source: ModulatorSource): ModulatorKind {
  if (source.type !== "midiLearn") return source.type;
  const control = source.control?.type;
  return control === "velocity" || control === "key" ? control : "midiLearn";
}
