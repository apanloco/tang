// Which of the MIDI inputs that are open a device of a session stands for (docs/patch-graph.md §11.6, P4c): the controller's rule (`midi::matching`), which decides which ports a lane that is wired
// to the device hears, written again for the page, which says of a device node that it is plugged in or not and of an input that a node of the session has it already. Pinned to the rule by
// `fixtures/inputs.json`, which Rust writes.

/** A name without the spaces round it: what Rust's `trim` takes, which is the Unicode white space (and not what the page's own `trim` takes, which has a byte order mark and has not the next line). */
export const trimmed = (name: string): string => name.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");

/**
 * The inputs among `inputs` that the device called `name` in a session stands for: those that are called exactly that, or, if there is none, those that have it in their names, whatever the case
 * (a Launchkey Mini has two ports, "Launchkey Mini MK4 37 MIDI Out" and "Launchkey Mini MK4 37 DAW Out", and "Launchkey Mini" is both). A name that is blank stands for none, and neither does one
 * when no input is open: a device that is not plugged in is heard by nobody.
 */
export function inputsStoodFor(name: string, inputs: readonly string[]): string[] {
  const wanted = trimmed(name);
  if (wanted === "") return [];
  const exact = inputs.filter((input) => input === wanted);
  if (exact.length > 0) return exact;
  const lower = wanted.toLowerCase();
  return inputs.filter((input) => input.toLowerCase().includes(lower));
}
