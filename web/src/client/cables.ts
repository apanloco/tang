// The cables of modulation as the mock engine and its history read them from the graph: which go when one does, and which cannot be put back.
import type { Edge, EdgeId } from "../protocol";

/** A cable of modulation (the graph has other edges: the audio path). */
export const isModulation = (e: Edge): e is Edge & { kind: "modulation"; id: EdgeId } => e.kind === "modulation";

/**
 * The cables that go when those of `start` do: they, and, in turn, the ones that drive the depth of a cable that
 * goes (what is left pointing at a cable that is not there would drive whichever one is put there next).
 */
export function cablesThatGo(cables: readonly Edge[], start: Iterable<EdgeId>): Set<EdgeId> {
  const going = new Set<EdgeId>(start);
  for (let grown = true; grown; ) {
    grown = false;
    for (const e of cables) {
      if (isModulation(e) && !going.has(e.id) && e.to.port.type === "depth" && going.has(e.to.port.edge)) {
        going.add(e.id);
        grown = true;
      }
    }
  }
  return going;
}

/**
 * Whether one of the cables `going` drives the depth of a cable, which drives the depth of another, and so on round to itself. A cable
 * drives one thing, so the cables that go are a tree of drivers unless one of them is in such a cycle, and then one of the two that drive
 * each other goes while the other is still there, driving a place with no cable at it: which cannot be told from the place of the cable
 * after it once that has gone, so the cables of such a removal cannot be put back one at a time (`RemovedCable::still_driven`).
 */
export function drivesRoundToItself(cables: readonly Edge[], going: ReadonlySet<EdgeId>): boolean {
  const drives = new Map<EdgeId, EdgeId>();
  for (const e of cables) {
    if (isModulation(e) && e.to.port.type === "depth") drives.set(e.id, e.to.port.edge);
  }
  for (const start of going) {
    const seen = new Set<EdgeId>();
    for (let at = drives.get(start); at !== undefined && !seen.has(at); at = drives.get(at)) {
      if (at === start) return true;
      seen.add(at);
    }
  }
  return false;
}
