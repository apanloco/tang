// A session much bigger than the tracer, for the performance test of the interface
// (docs/web-ui.md §9.4: 60 fps with 32 modulators and 64 live knobs): eight lanes, each an
// oscillator through a filter, with four LFOs above it that between them drive eight of the
// lane's parameters. It has the shape the server makes (ids, cables, a telemetry layout with
// a slot for every output and every modulated parameter), so that the mock engine serves it
// and makes the telemetry for it like it does for the tracer.
import type { Edge, LayoutEntry, Node, NodeId, Param, Plugin, Snapshot } from "../protocol";
import { MASTER, tracerSnapshot } from "./fixtures";

const LANES = 8;
const LFOS_PER_LANE = 4;
const TARGETS_PER_LFO = 2;

/** A plugin of the tracer, as a template for the plugins of the lanes. */
function pluginOf(snapshot: Snapshot, kind: "instrument" | "effect"): Plugin {
  for (const node of snapshot.graph.nodes) {
    if (node.kind === kind && node.plugin) return node.plugin;
  }
  throw new Error(`the tracer has no ${kind}`);
}

/** The parameters of a plugin that are numbers (not names), which is what a LFO can drive. */
const floats = (plugin: Plugin): Param[] => plugin.params.filter((param) => param.labels === null);

export interface StressSize {
  lanes: number;
  /** How many LFOs there are above each lane. */
  lfosPerLane: number;
  /** How many parameters each LFO drives. */
  targetsPerLfo: number;
}

/** What the session has: so many lanes, each with so many LFOs of so many cables. */
export function stressSnapshot(size: StressSize = { lanes: LANES, lfosPerLane: LFOS_PER_LANE, targetsPerLfo: TARGETS_PER_LFO }): Snapshot {
  const oscillator = pluginOf(tracerSnapshot, "instrument");
  const filter = pluginOf(tracerSnapshot, "effect");
  const nodes: Node[] = [];
  const edges: Edge[] = [];
  const meters: LayoutEntry[] = [];
  const outputs: LayoutEntry[] = [];
  const effective: LayoutEntry[] = [];
  let id = 0;
  let edgeId = 0;
  let slot = 0;
  const meter = (node: NodeId, port: "audioIn" | "audioOut"): void => {
    meters.push({ slot: slot++, node, port: { type: port }, kind: "peak" });
    meters.push({ slot: slot++, node, port: { type: port }, kind: "clips" });
  };
  meter(MASTER, "audioIn");

  for (let lane = 0; lane < size.lanes; lane += 1) {
    const instrument: NodeId = id++;
    const effect: NodeId = id++;
    nodes.push({
      id: instrument,
      parent: null,
      name: `Oscillator ${String(lane + 1)}`,
      kind: "instrument",
      plugin: structuredClone(oscillator),
      range: null,
      transpose: 0,
      volume: 0.1,
      muted: false,
      solo: false,
      route: "out",
      pattern: null,
    });
    nodes.push({ id: effect, parent: instrument, name: `Filter ${String(lane + 1)}`, kind: "effect", plugin: structuredClone(filter), mix: 1, feed: "chain" });
    edges.push({ kind: "audio", from: { node: instrument, port: { type: "audioOut" } }, to: { node: effect, port: { type: "audioIn" } } });
    edges.push({ kind: "audio", from: { node: effect, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } } });
    meter(instrument, "audioOut");

    // The parameters of the lane that can be driven, in order, taken in turn by the LFOs.
    const driveable = [
      ...floats(oscillator).map((param) => ({ node: instrument, index: param.index })),
      ...floats(filter).map((param) => ({ node: effect, index: param.index })),
    ];
    let next = 0;
    const driven = new Set<string>();
    for (let k = 0; k < size.lfosPerLane; k += 1) {
      const lfo: NodeId = id++;
      const rate = 0.25 + 0.15 * (lane * size.lfosPerLane + k);
      nodes.push({ id: lfo, parent: instrument, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate, retrigger: false, fade: 0, sync: 0 } });
      outputs.push({ slot: slot++, node: lfo, port: { type: "modOut" }, kind: "modOutput" });
      for (let t = 0; t < size.targetsPerLfo; t += 1) {
        const target = driveable[next % driveable.length];
        next += 1;
        if (!target) continue;
        edges.push({
          kind: "modulation",
          id: edgeId++,
          amount: 0.1 + 0.05 * ((k + t) % 5),
          from: { node: lfo, port: { type: "modOut" } },
          to: { node: target.node, port: { type: "param", index: target.index } },
        });
        const key = `${String(target.node)}:${String(target.index)}`;
        if (!driven.has(key)) {
          driven.add(key);
          effective.push({ slot: slot++, node: target.node, port: { type: "param", index: target.index }, kind: "effective" });
        }
      }
    }
  }
  nodes.push({ id: MASTER, parent: null, name: "Master", kind: "master" });

  return {
    rev: 0,
    meta: { ...tracerSnapshot.meta, file: "stress.toml" },
    graph: { nodes, edges },
    telemetry: { rev: 1, meters, modulation: [...outputs, ...effective] },
  };
}
