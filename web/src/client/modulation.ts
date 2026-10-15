// What modulation does to a parameter, as far as the interface needs to know it to draw the
// ring of a knob (docs/web-ui.md §6, "Visual vocabulary"): which modulators drive it, and
// between which values they can swing it. The laws are the engine's, read from
// `Modulator::target_offset` in `src/plugin/chain.rs`: every modulator adds an offset to the
// parameter's base value, the offsets add up, and the sum is clamped to the parameter's range.
// A lane in a group can have cables to one parameter from its own modulators and from its group's: the
// group's are applied before the lane renders and the lane's later, each from the value the user set, so
// those of the lane stand alone (`modulationsOn`).
// A setting of a modulator (an LFO's rate, a stage of an envelope, the depth of a cable) is
// different: its cables each write it from the value the user set, and the last stands
// (`apply_cross_mod`), so `settingExtent` is the extent of the last cable alone. And one that is turned on a
// log scale (a rate, an attack, a decay, a release) is moved a fraction of the knob's travel, in ratios, as the
// knob shows it (`Modulator::modulated_value`).
import type { Graph, ModulatorSource, NodeId, Port } from "../protocol";
import { samePort } from "./reach";
import { fractionOf, isLog, valueAt } from "./travel";
import type { Scale } from "./travel";

/** One modulator driving a parameter: what it is, and its depth (-1 to 1) on that parameter. */
export interface Modulation {
  source: ModulatorSource;
  amount: number;
}

/** A modulator that drives a place, with whose it is: a group's or a lane's. */
interface Driver {
  modulation: Modulation;
  ofGroup: boolean;
}

/** The modulators that drive the place `port` of `node`, in the order of the graph's edges, each with whose it is. */
function driversAt(graph: Graph, node: NodeId, port: Port): Driver[] {
  const found: Driver[] = [];
  for (const edge of graph.edges) {
    if (edge.kind !== "modulation" || edge.to.node !== node || !samePort(edge.to.port, port)) continue;
    const modulator = graph.nodes.find((candidate) => candidate.id === edge.from.node);
    if (modulator?.kind !== "modulator") continue;
    const owner = graph.nodes.find((candidate) => candidate.id === modulator.parent);
    found.push({ modulation: { source: modulator.source, amount: edge.amount }, ofGroup: owner?.kind === "group" });
  }
  return found;
}

/**
 * The modulators that drive the place `port` of `node`: a parameter of its plugin, a setting of a modulator, the depth of one of its
 * cables. In the order of the graph's edges, which is the order the engine writes them in (a group's cables, then a lane's, each
 * modulator's in the order of the rack). All of them, as the graph has them: for a parameter, `modulationsOn` says which of them count.
 */
export function modulationsAt(graph: Graph, node: NodeId, port: Port): Modulation[] {
  return driversAt(graph, node, port).map((driver) => driver.modulation);
}

/**
 * The modulators that drive parameter `index` of the plugin in `node`, in the order of the graph's edges, and whose offsets add up. A lane
 * that is in a group can be driven by its own modulators and by its group's. The engine applies the group's before the lane renders and the
 * lane's later, each from the value the user set (`apply_group_mod_writes`, then `apply_modulators_to_chain`), so that a parameter that any
 * modulator of the lane drives, one whose control is not bound yet too, is the lane's alone: the group's offsets are not added to its. A bus
 * effect has the group's only.
 */
export function modulationsOn(graph: Graph, node: NodeId, index: number): Modulation[] {
  const drivers = driversAt(graph, node, { type: "param", index });
  const lanes = drivers.filter((driver) => !driver.ofGroup);
  return (lanes.length > 0 ? lanes : drivers).map((driver) => driver.modulation);
}

/** The lowest and highest offset one modulator can add to `base` on a parameter of range `min`…`max`; null if it adds none. */
function offsetRange(modulation: Modulation, base: number, min: number, max: number): { low: number; high: number } | null {
  const { source, amount } = modulation;
  const range = max - min;
  const between = (a: number, b: number) => ({ low: Math.min(a, b), high: Math.max(a, b) });
  switch (source.type) {
    // A LFO is bipolar, -1 to 1: it wobbles ±depth×range around the base.
    case "lfo":
      return between(-amount * range, amount * range);
    // An envelope is unipolar and the base is its peak: from `depth × (base - min)` below it, at rest, up to it.
    case "envelope":
      return between(-amount * (base - min), 0);
    case "midiLearn":
      if (source.control === null) return null; // not bound to anything yet: the base shows through
      // Pitch bend is bipolar around its centre; a CC and pressure start from nothing at rest.
      return source.control.type === "pitchBend" ? between(-amount * range, amount * range) : between(0, amount * range);
    // A macro is unipolar, as a CC is: it starts from nothing at rest and adds up to depth × range at full.
    case "macro":
      return between(0, amount * range);
  }
}

/**
 * The lowest and highest value modulation can give a parameter whose set value is `base`,
 * or undefined when it cannot move it (nothing drives it, or what does is not bound).
 */
export function modulationExtent(
  base: number,
  min: number,
  max: number,
  modulations: readonly Modulation[],
): { low: number; high: number } | undefined {
  let low = 0;
  let high = 0;
  for (const modulation of modulations) {
    const offsets = offsetRange(modulation, base, min, max);
    if (!offsets) continue;
    low += offsets.low;
    high += offsets.high;
  }
  const clamp = (value: number) => Math.min(Math.max(value, min), max);
  const extent = { low: clamp(base + low), high: clamp(base + high) };
  return extent.low === extent.high ? undefined : extent;
}

/**
 * The lowest and highest value modulation can give a setting of a modulator whose set value is `base`, over the range `min`…`max` of the
 * setting, or undefined when it cannot move it. Not a sum, as for a parameter: the engine writes the setting once for each cable that
 * drives it, each from the set value (`base + offset`, clamped), and the one written last stands, so this is the extent of the last
 * cable alone (an unbound MIDI control, the last of them, writes the set value itself).
 *
 * A setting that is turned on a log `scale` (a rate, an attack, a decay, a release: its knob runs over decades) is moved a fraction of the
 * knob's travel and not of its range in seconds or hertz, so that the law is the same over the knob, from 0 at the lowest value to 1 at the
 * highest, where equal steps are equal ratios: the extent is as far up the knob as down it. A range that starts at nothing has no decades,
 * and goes by the range, as the knob does.
 */
export function settingExtent(
  base: number,
  min: number,
  max: number,
  modulations: readonly Modulation[],
  scale: Scale = "linear",
): { low: number; high: number } | undefined {
  const last = modulations.slice(-1);
  if (!isLog(scale, min) || !(max > min)) return modulationExtent(base, min, max, last);
  const along = modulationExtent(fractionOf(base, min, max, "log"), 0, 1, last);
  return along && { low: valueAt(along.low, min, max, "log"), high: valueAt(along.high, min, max, "log") };
}
