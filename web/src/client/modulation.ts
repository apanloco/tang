// What modulation does to a parameter, as far as the interface needs to know it to draw the
// ring of a knob (docs/web-ui.md §6, "Visual vocabulary"): which modulators drive it, and
// between which values they can swing it. The laws are the engine's, read from
// `Modulator::target_offset` in `src/plugin/chain.rs`: every modulator adds an offset to the
// parameter's base value, the offsets add up, and the sum is clamped to the parameter's range.
// A setting of a modulator (an LFO's rate, a stage of an envelope, the depth of a cable) is
// different: its cables each write it from the value the user set, and the last stands
// (`apply_cross_mod`), so `settingExtent` is the extent of the last cable alone.
import type { Graph, ModulatorSource, NodeId, Port } from "../protocol";
import { samePort } from "./reach";

/** One modulator driving a parameter: what it is, and its depth (-1 to 1) on that parameter. */
export interface Modulation {
  source: ModulatorSource;
  amount: number;
}

/**
 * The modulators that drive the place `port` of `node`: a parameter of its plugin, a setting of a modulator, the depth of one of its
 * cables. In the order of the graph's edges, which is the order the engine writes them in (a group's cables, then a lane's, each
 * modulator's in the order of the rack).
 */
export function modulationsAt(graph: Graph, node: NodeId, port: Port): Modulation[] {
  const found: Modulation[] = [];
  for (const edge of graph.edges) {
    if (edge.kind !== "modulation" || edge.to.node !== node || !samePort(edge.to.port, port)) continue;
    const modulator = graph.nodes.find((candidate) => candidate.id === edge.from.node);
    if (modulator?.kind === "modulator") found.push({ source: modulator.source, amount: edge.amount });
  }
  return found;
}

/** The modulators that drive parameter `index` of the plugin in `node`, in the order of the graph's edges. */
export function modulationsOn(graph: Graph, node: NodeId, index: number): Modulation[] {
  return modulationsAt(graph, node, { type: "param", index });
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
 */
export function settingExtent(
  base: number,
  min: number,
  max: number,
  modulations: readonly Modulation[],
): { low: number; high: number } | undefined {
  return modulationExtent(base, min, max, modulations.slice(-1));
}
