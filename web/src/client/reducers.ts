// How a client keeps its copy of the session in step with the server's: apply the
// events that come after a snapshot. Pure: a snapshot in, a snapshot out, with only
// what changed replaced (so an unchanged node is the same object, which is what a
// store's subscribers compare). The rules are the controller's (`src/app/apply.rs`),
// read from the other side.
import type { Gain, Node, ParamValue, Plugin, Snapshot, StampedEvent } from "../protocol";

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
  const changed = changeNode(snapshot, event);
  if (typeof changed === "string") return { ok: false, reason: changed };
  const nodes = snapshot.graph.nodes.map((node) => (node.id === changed.id ? changed : node));
  // An intent, from whichever front-end, is a change the session has not saved (`Controller::apply` marks it so).
  const meta = snapshot.meta.dirty ? snapshot.meta : { ...snapshot.meta, dirty: true };
  return { ok: true, snapshot: { ...snapshot, rev: event.rev, meta, graph: { ...snapshot.graph, nodes } } };
}

/** The node the event is about, as it is after it; or the reason it cannot be. */
function changeNode(snapshot: Snapshot, event: StampedEvent): Node | string {
  const node = snapshot.graph.nodes.find((n) => n.id === event.node);
  if (!node) return `there is no node ${event.node}`;
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
  }
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
