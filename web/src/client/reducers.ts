// How a client keeps its copy of the session in step with the server's: apply the
// events that come after a snapshot. Pure: a snapshot in, a snapshot out, with only
// what changed replaced (so an unchanged node is the same object, which is what a
// store's subscribers compare). The rules are the controller's (`src/app/apply.rs`),
// read from the other side.
import { WAVEFORMS } from "../protocol";
import type { Gain, Graph, ModulatorField, Node, NodeUi, ParamValue, PianoMeta, Plugin, Snapshot, StampedEvent } from "../protocol";

/** The snapshot after an event, or why the event does not fit the one it was applied to. */
export type Applied = { ok: true; snapshot: Snapshot } | { ok: false; reason: string };

/**
 * Apply `event` to `snapshot`. An event is the next revision, or one this snapshot
 * already has (nothing to do: a client that asked for a second snapshot may see
 * events from before it). Anything else means events were missed, or the session is
 * not the one the event is about, and the client has to start again from a snapshot.
 */
export function applyEvent(snapshot: Snapshot, event: StampedEvent): Applied {
  if (event.rev <= snapshot.rev) return { ok: true, snapshot };
  if (event.rev !== snapshot.rev + 1) {
    return { ok: false, reason: `the session is at revision ${snapshot.rev} and the next event is ${event.rev}` };
  }
  // A save changes no node: the session is as it was, and nothing in it is unsaved now.
  if (event.type === "saved") return { ok: true, snapshot: { ...snapshot, rev: event.rev, meta: { ...snapshot.meta, dirty: false, file: event.file } } };
  // The piano's scale and lock are the session's, not a node's: no node changes.
  if (event.type === "scaleSet" || event.type === "lockedSet") {
    return { ok: true, snapshot: { ...snapshot, rev: event.rev, meta: { ...snapshot.meta, dirty: true, piano: pianoAfter(snapshot.meta.piano, event) } } };
  }
  const graph = changeGraph(snapshot.graph, event);
  if (typeof graph === "string") return { ok: false, reason: graph };
  // An intent, from whichever front-end, is a change the session has not saved (`Controller::apply` marks it so).
  const meta = snapshot.meta.dirty ? snapshot.meta : { ...snapshot.meta, dirty: true };
  return { ok: true, snapshot: { ...snapshot, rev: event.rev, meta, graph } };
}

/** The piano's scale and lock after an event about them. */
function pianoAfter(piano: PianoMeta, event: Extract<StampedEvent, { type: "scaleSet" | "lockedSet" }>): PianoMeta {
  if (event.type === "lockedSet") return { ...piano, locked: event.locked };
  return { ...piano, scale: event.scale, root: event.root, kind: event.kind, notes: event.notes };
}

/** The graph after the event, with only what it changed replaced; or the reason it cannot be. */
function changeGraph(graph: Graph, event: Exclude<StampedEvent, { type: "saved" | "scaleSet" | "lockedSet" }>): Graph | string {
  if (event.type === "edgeDepthSet") return withDepth(graph, event.edge, event.depth);
  const node = graph.nodes.find((n) => n.id === event.node);
  if (!node) return `there is no node ${event.node}`;
  const changed = changeNode(node, event);
  if (typeof changed === "string") return changed;
  return { ...graph, nodes: graph.nodes.map((n) => (n === node ? changed : n)) };
}

/** The node the event is about, as it is after it; or the reason it cannot be. */
function changeNode(node: Node, event: Exclude<StampedEvent, { type: "edgeDepthSet" | "saved" | "scaleSet" | "lockedSet" }>): Node | string {
  switch (event.type) {
    case "paramSet": {
      const plugin = pluginOf(node);
      if (!plugin) return `node ${node.id} has no plugin`;
      const changed = withValues(plugin, [{ param: event.param, value: event.value }]);
      return changed ? withPlugin(node, changed) : `node ${node.id} has no parameter ${event.param}`;
    }
    case "gainSet":
      return withGain(node, event.gain, event.value) ?? `${event.gain} does not apply to node ${node.id}`;
    case "presetLoaded": {
      const plugin = pluginOf(node);
      if (!plugin) return `node ${node.id} has no plugin`;
      // The controller keeps a preset's values as the parameters' baseline too, and a snapshot shows the baseline as the default.
      const changed = withValues({ ...plugin, currentPreset: event.preset }, event.values, true);
      return changed ? withPlugin(node, changed) : `the preset names a parameter node ${node.id} does not have`;
    }
    case "rangeSet":
      return node.kind === "instrument" ? { ...node, range: event.range } : `node ${node.id} is not a lane`;
    case "nodeUiSet":
      return withPlace(node, event.ui);
    case "pinnedSet": {
      const plugin = pluginOf(node);
      if (!plugin) return `node ${node.id} has no plugin`;
      const missing = event.params.find((index) => plugin.params.every((param) => param.index !== index));
      if (missing !== undefined) return `node ${node.id} has no parameter ${missing}`;
      return withPlugin(node, { ...plugin, pinned: event.params });
    }
    case "modulatorFieldSet":
      return withSetting(node, event.field, event.value);
  }
}

/** The node at a place, or, for none, as a node the server has not placed is: with no `ui` at all. */
function withPlace(node: Node, ui: NodeUi | null): Node {
  if (ui !== null) return { ...node, ui };
  const unplaced = { ...node };
  delete unplaced.ui;
  return unplaced;
}

function pluginOf(node: Node): Plugin | null {
  return node.kind === "instrument" || node.kind === "effect" ? node.plugin : null;
}

/** The node with its plugin replaced (only nodes that have one get here). */
function withPlugin(node: Node, plugin: Plugin): Node {
  return node.kind === "instrument" || node.kind === "effect" ? { ...node, plugin } : node;
}

/** The plugin with these parameters at these values (and, if `baseline`, with them as their defaults); null if it lacks one of them. */
function withValues(plugin: Plugin, values: readonly ParamValue[], baseline = false): Plugin | null {
  const wanted = new Map(values.map((v) => [v.param, v.value]));
  let found = 0;
  const params = plugin.params.map((param) => {
    const value = wanted.get(param.index);
    if (value === undefined) return param;
    found += 1;
    return baseline ? { ...param, value, default: value } : { ...param, value };
  });
  return found === wanted.size ? { ...plugin, params } : null;
}

/** The node with its host-side gain set; null if that gain is not one this node has. */
function withGain(node: Node, gain: Gain, value: number): Node | null {
  if (gain === "volume" && node.kind === "instrument") return { ...node, volume: value };
  if (gain === "mix" && node.kind === "effect") return { ...node, mix: value };
  if (gain === "groupVolume" && node.kind === "group") return { ...node, volume: value };
  if (gain === "groupPan" && node.kind === "group") return { ...node, pan: value };
  return null;
}

/** The node with a setting of its modulator set; a reason if it is not a modulator, or not one that has that setting. */
function withSetting(node: Node, field: ModulatorField, value: number): Node | string {
  if (node.kind !== "modulator") return `node ${node.id} is not a modulator`;
  const { source } = node;
  if (source.type === "lfo") {
    if (field === "rate") return { ...node, source: { ...source, rate: value } };
    if (field === "waveform") {
      // (Only a whole number, 0 to 3, is an index: the rest, a half or a minus one or NaN, finds nothing.)
      const waveform = WAVEFORMS[value];
      return waveform === undefined ? `${value} is not a waveform` : { ...node, source: { ...source, waveform } };
    }
  }
  if (source.type === "envelope" && field !== "rate" && field !== "waveform") {
    return { ...node, source: { ...source, [field]: value } };
  }
  return `${field} does not apply to node ${node.id}`;
}

/** The graph with the cable `edge` pulling `depth`; a reason if there is no such cable. */
function withDepth(graph: Graph, edge: number, depth: number): Graph | string {
  if (!graph.edges.some((e) => e.kind === "modulation" && e.id === edge)) return `there is no cable ${edge}`;
  return { ...graph, edges: graph.edges.map((e) => (e.kind === "modulation" && e.id === edge ? { ...e, amount: depth } : e)) };
}
