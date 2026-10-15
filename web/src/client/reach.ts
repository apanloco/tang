// Where a cable from a modulator may end (docs/web-ui.md §9.3, D21). The controller decides
// (`SessionModel::param_targets` for the parameters of plugins, `SessionModel::field_targets` for the
// settings of other modulators, and `AddModEdge` refuses the rest), and a client that lets
// the user draw cables has to know before it asks, to show which parameters and settings take the cable
// that is being dragged. It reads the answer from the graph alone, by the `parent` of each node; fixtures
// that the controller writes (what it accepted, for every modulator of a session with groups and lanes in
// and out of them: `fixtures/reach.json`, and for the settings of modulators `fixtures/fields.json`) pin
// the two together.
import type { Edge, Graph, Node, NodeId, Port } from "../protocol";
import { endLabel } from "./names";

/** Whether a node has a plugin, whose parameters a cable can end at. */
const hasPlugin = (node: Node): boolean => (node.kind === "instrument" && node.plugin !== null) || node.kind === "effect";

/**
 * The nodes with a plugin that the modulator `modulator` can draw a cable to. A lane's modulator reaches the
 * plugins in its lane: the instrument (the lane's own node) and the effects whose parent the lane is. A
 * group's reaches the plugins in the lanes that are its members, and its own bus effects. Nothing else does:
 * not another lane, not another group's bus. A node that is not a modulator reaches nothing.
 */
export function reachFrom(graph: Graph, modulator: NodeId): Set<NodeId> {
  const from = graph.nodes.find((node) => node.id === modulator);
  const owner = from?.kind === "modulator" ? graph.nodes.find((node) => node.id === from.parent) : undefined;
  if (!owner) return new Set();
  const children = (parent: Node): Node[] => graph.nodes.filter((node) => node.parent === parent.id);
  const chainOf = (lane: Node): Node[] => [lane, ...children(lane).filter((node) => node.kind === "effect")];
  let reached: Node[] = [];
  if (owner.kind === "instrument") {
    reached = chainOf(owner);
  } else if (owner.kind === "group") {
    const members = children(owner).filter((node) => node.kind === "instrument");
    reached = [...members.flatMap(chainOf), ...children(owner).filter((node) => node.kind === "effect")];
  }
  return new Set(reached.filter(hasPlugin).map((node) => node.id));
}

/** A setting of a modulator, or the depth of one of its cables: the node of the modulator and the port on it that a cable ends at. */
export interface FieldRef {
  node: NodeId;
  port: Port;
}

/** The modulators of a lane or a group's rack, in the order of the graph. */
const rackOf = (graph: Graph, parent: Node): Node[] => graph.nodes.filter((node) => node.parent === parent.id && node.kind === "modulator");

/**
 * The modulators whose settings the modulator `modulator` can be drawn to, in the order of the controller's list
 * (`SessionModel::modulators_in_reach`): a lane's modulator reaches the others of its lane; a group's reaches
 * its members' modulators, member by member, and then the others of its own rack. Never itself. A node that is
 * not a modulator, or whose lane or group is not there, reaches none.
 */
export function modulatorsInReach(graph: Graph, modulator: NodeId): Node[] {
  const from = graph.nodes.find((node) => node.id === modulator);
  const owner = from?.kind === "modulator" ? graph.nodes.find((node) => node.id === from.parent) : undefined;
  if (!owner) return [];
  const others = (parent: Node): Node[] => rackOf(graph, parent).filter((node) => node.id !== modulator);
  if (owner.kind === "instrument") return others(owner);
  if (owner.kind !== "group") return [];
  const members = graph.nodes.filter((node) => node.parent === owner.id && node.kind === "instrument");
  return [...members.flatMap((member) => rackOf(graph, member)), ...others(owner)];
}

/** Whether two ports are the same place on a node: of the same kind, and for a parameter the same one, for a depth the cable's. */
export function samePort(a: Port, b: Port): boolean {
  if (a.type === "param") return b.type === "param" && a.index === b.index;
  if (a.type === "depth") return b.type === "depth" && a.edge === b.edge;
  return a.type === b.type;
}

/** The ports of the settings a modulator has: an LFO's rate, an envelope's four stages, nothing for a MIDI control. */
function settingPorts(node: Node): Port[] {
  if (node.kind !== "modulator") return [];
  switch (node.source.type) {
    case "lfo":
      return [{ type: "rate" }];
    case "envelope":
      return [{ type: "attack" }, { type: "decay" }, { type: "sustain" }, { type: "release" }];
    case "midiLearn":
      return [];
  }
}

/** The ports of the depths of the cables that start at the modulator `node`. */
const depthPorts = (graph: Graph, node: Node): Port[] =>
  graph.edges.flatMap((edge: Edge): Port[] => (edge.kind === "modulation" && edge.from.node === node.id ? [{ type: "depth", edge: edge.id }] : []));

/**
 * Every setting and cable depth of other modulators that `modulator` can be drawn to, as the controller lists them
 * (`SessionModel::field_targets`): for each modulator in reach (`modulatorsInReach`) its settings, then the depths of its
 * cables. Whether the modulator drives one already is not looked at here.
 */
export function fieldsOf(graph: Graph, modulator: NodeId): FieldRef[] {
  return modulatorsInReach(graph, modulator).flatMap((node) => [...settingPorts(node), ...depthPorts(graph, node)].map((port) => ({ node: node.id, port })));
}

/**
 * A node a modulator can still draw a cable to, and what it can still drive in it: the parameters of a plugin (`params`, by the
 * plugin's own index), or the settings of a modulator and the depths of its cables (`fields`, by the port).
 */
export interface Drivable {
  node: NodeId;
  /** What the node is called on the canvas. */
  name: string;
  params: readonly { index: number; name: string }[];
  /** What each is called in a list: a setting by its name (`rate`), the depth of a cable by where the cable goes (`depth → Filter: cutoff`). */
  fields: readonly { port: Port; name: string }[];
}

/** What a setting of a modulator, or the depth of one of its cables, is called in a list: the depth of a cable is told by where the cable goes. */
function fieldName(graph: Graph, names: ReadonlyMap<NodeId, string>, port: Port): string {
  if (port.type !== "depth") return port.type;
  const { edge } = port;
  const cable = graph.edges.find((found) => found.kind === "modulation" && found.id === edge);
  return cable ? `depth → ${endLabel(graph, names, cable.to)}` : "depth";
}

/** The key of a setting of a modulator, or of the depth of one of its cables (a modulator has as many depths as cables), among those that a modulator drives. */
const fieldKey = (node: NodeId, port: Port): string => (port.type === "depth" ? `${String(node)}:depth:${String(port.edge)}` : `${String(node)}:${port.type}`);

/**
 * Everything `modulator` can still be drawn to, plugin by plugin in the order of the graph: the parameters of the
 * plugins it reaches (`reachFrom`) that it does not drive already (another modulator may drive the same one: the
 * two add up); then modulator by modulator, in the order of `fieldsOf`: the settings of the modulators it reaches and
 * the depths of their cables, that it does not drive already. `names` is what each node is called (`displayNames`). A node
 * with nothing left is not listed.
 */
export function drivablesOf(graph: Graph, modulator: NodeId, names: ReadonlyMap<NodeId, string>): Drivable[] {
  const reached = reachFrom(graph, modulator);
  const driven = new Set(
    graph.edges.flatMap((edge) => (edge.kind === "modulation" && edge.from.node === modulator && edge.to.port.type === "param" ? [`${String(edge.to.node)}:${String(edge.to.port.index)}`] : [])),
  );
  const groups: Drivable[] = [];
  for (const node of graph.nodes) {
    if (!reached.has(node.id)) continue;
    const plugin = node.kind === "instrument" || node.kind === "effect" ? node.plugin : null;
    const params = (plugin?.params ?? []).filter((param) => !driven.has(`${String(node.id)}:${String(param.index)}`)).map(({ index, name }) => ({ index, name }));
    if (params.length > 0) groups.push({ node: node.id, name: names.get(node.id) ?? node.name, params, fields: [] });
  }
  const drivenFields = new Set(graph.edges.flatMap((edge) => (edge.kind === "modulation" && edge.from.node === modulator && edge.to.port.type !== "param" ? [fieldKey(edge.to.node, edge.to.port)] : [])));
  const byModulator = new Map<NodeId, Drivable>();
  for (const { node, port } of fieldsOf(graph, modulator)) {
    if (drivenFields.has(fieldKey(node, port))) continue;
    const group = byModulator.get(node) ?? { node, name: names.get(node) ?? graph.nodes.find((n) => n.id === node)?.name ?? "?", params: [], fields: [] };
    byModulator.set(node, { ...group, fields: [...group.fields, { port, name: fieldName(graph, names, port) }] });
  }
  return [...groups, ...byModulator.values()];
}
