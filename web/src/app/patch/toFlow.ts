// The session as a canvas: which node goes where, and which cable joins which two ports
// (docs/web-ui.md §9.3). A pure function of the graph the server sends: the layout is a
// lane strip for each instrument (the instrument, then its effects, left to right), its
// modulators above the strip, over the plugin each one drives, the groups to the right of
// the lanes that feed them, and the master at the end. That is where a node is unless the
// session says otherwise: a node that has been moved has a place (`Node.ui`, kept in the
// session file, §4.4), and is drawn there; the layout of the others does not leave room for it.
import type { Edge as FlowEdge, Node as FlowNode } from "@xyflow/react";

import { cableLabel, displayNames, portName } from "../../client/names";
import { drivablesOf } from "../../client/reach";
import type { Drivable } from "../../client/reach";
import type { Edge, EdgeId, Graph, Node, NodeId, NodeUi, Plugin, Port } from "../../protocol";

/** The width of every node, so that the columns of the canvas are known without measuring anything. */
export const NODE_WIDTH = 304;
/** The distance from one column of the canvas to the next. */
export const COLUMN = NODE_WIDTH + 72;
/** Room for a cable to arrive, between the modulators above a strip and the strip. */
export const CABLE_GAP = 72;
export const ROW_GAP = 56;

/** How many of a plugin's parameters a node shows as controls, besides those that are modulated. */
export const MAX_PINNED = 8;

// What the layout assumes about the size of a node: the components' CSS is built to these.
const HEADER_HEIGHT = 44;
const CONTROL_ROW_HEIGHT = 108;
const CONTROLS_PER_ROW = 4;
const NODE_PADDING = 16;
/** The list at the foot of a modulator, with which it draws a cable (measured in a browser: 29). */
const DRIVES_HEIGHT = 30;
/** A modulator that has no settings (a MIDI control) is this tall (measured in a browser: 119)... */
export const MODULATOR_HEIGHT = 128;
/** ...and one that has them has a row of controls more (measured in a browser: 213). */
export const MODULATOR_SETTINGS_HEIGHT = HEADER_HEIGHT + CONTROL_ROW_HEIGHT + NODE_PADDING + 20 + DRIVES_HEIGHT;
export const MASTER_HEIGHT = 164;

export type PatchNodeKind = "instrument" | "effect" | "modulator" | "group" | "master";

export interface PatchNodeData extends Record<string, unknown> {
  /** The node of the session this one is the picture of. */
  node: NodeId;
  /** What to call it: the server's name, with a number after it when another node has the same (`displayNames`). */
  name: string;
  /** What kind of plugin it has (`Built-in`, `LV2`, `CLAP`, `VST3`), if it has one. */
  format: string | null;
  /** The parameters of its plugin it shows as controls, by the plugin's own index. */
  pinned: readonly number[];
  /** Those of them that a modulator drives (a cable ends at them). */
  modulated: readonly number[];
  /** For a modulator: what it can still be drawn to, plugin by plugin (`drivablesOf`); nothing for the other nodes. */
  targets: readonly Drivable[];
}

export type PatchNode = FlowNode<PatchNodeData, PatchNodeKind>;

export type PatchEdgeKind = "audio" | "modulation";

export interface PatchEdgeData extends Record<string, unknown> {
  /**
   * The id of a modulation cable, by which the cable reads how deep it goes (a value, which is
   * turned at the pace of the frames, and so not part of what the canvas is laid out by).
   */
  cable?: EdgeId;
  /** The modulator that drives the cable, whose list the focus goes to when the cable is taken out. */
  modulator?: NodeId;
  /** What the cable is, in words: `LFO → Filter: cutoff`. */
  label?: string;
}

export type PatchEdge = FlowEdge<PatchEdgeData, PatchEdgeKind>;

export interface Flow {
  nodes: PatchNode[];
  edges: PatchEdge[];
}

/** The name of the handle of a node where a port is, if the canvas draws one. */
export function handleOf(port: Port): string | null {
  switch (port.type) {
    case "audioIn":
    case "audioOut":
    case "modOut":
      return port.type;
    case "param":
      return `param-${String(port.index)}`;
    default:
      // What a modulator can drive in another modulator (its rate, its stages, its depths)
      // has no handle yet.
      return null;
  }
}

/** The parameters of `plugin` a node shows: the first few, and every one a modulator drives. */
export function pinnedParams(plugin: Plugin | null, modulated: ReadonlySet<number>): number[] {
  if (!plugin) return [];
  return plugin.params.filter((param, position) => position < MAX_PINNED || modulated.has(param.index)).map((param) => param.index);
}

/** The parameters that a modulation cable ends at, for each node. */
function modulatedParams(graph: Graph): Map<NodeId, Set<number>> {
  const modulated = new Map<NodeId, Set<number>>();
  for (const edge of graph.edges) {
    if (edge.to.port.type !== "param") continue;
    const params = modulated.get(edge.to.node) ?? new Set<number>();
    params.add(edge.to.port.index);
    modulated.set(edge.to.node, params);
  }
  return modulated;
}

/** How many controls a node shows: its parameters, and its own gain (a volume or a mix; a lane's keys too) or a group's two. */
export function controlCount(node: Node, pinned: number): number {
  switch (node.kind) {
    case "instrument":
      return pinned + 2;
    case "effect":
      return pinned + 1;
    case "group":
      return 2;
    default:
      return 0;
  }
}

/** How tall a node is, as the layout assumes it (the components' styles keep to it). */
export function nodeHeight(node: Node, pinned: number): number {
  switch (node.kind) {
    case "modulator":
      return node.source.type === "midiLearn" ? MODULATOR_HEIGHT : MODULATOR_SETTINGS_HEIGHT;
    case "master":
      return MASTER_HEIGHT;
    default: {
      const rows = Math.max(1, Math.ceil(controlCount(node, pinned) / CONTROLS_PER_ROW));
      return HEADER_HEIGHT + rows * CONTROL_ROW_HEIGHT + NODE_PADDING;
    }
  }
}

/** A strip: nodes in a row, left to right, from a column, with the modulators that sit above them. */
interface Strip {
  /** The node the strip is of: an instrument or a group. */
  head: Node;
  /** The nodes in the row, the head first. */
  row: Node[];
  modulators: Node[];
  /** The column of each modulator, from the strip's first. */
  columns: Map<Node, number>;
}

/** A strip of the instrument or group `head`: its nodes, and the modulators that belong to it. */
function stripOf(graph: Graph, head: Node): Strip {
  const children = graph.nodes.filter((node) => node.parent === head.id);
  const row = [head, ...children.filter((node) => node.kind === "effect")];
  const modulators = children.filter((node) => node.kind === "modulator");
  return { head, row, modulators, columns: modulatorColumns(graph, row, modulators) };
}

/**
 * How many columns a strip takes: its row, or as far as its modulators go if they go past the row (a
 * lane with more modulators than nodes). What comes after it, groups and the master, starts after that.
 */
function widthOf(strip: Strip): number {
  return Math.max(strip.row.length, ...[...strip.columns.values()].map((column) => column + 1));
}

/** Where the modulators of a strip go, as columns from its first: above the node each drives, or the first column that is free. */
function modulatorColumns(graph: Graph, row: readonly Node[], modulators: readonly Node[]): Map<Node, number> {
  const columnOf = new Map(row.map((node, column) => [node.id, column]));
  const taken = new Set<number>();
  const placed = new Map<Node, number>();
  for (const modulator of modulators) {
    const aimed = graph.edges
      .filter((edge) => edge.from.node === modulator.id)
      .map((edge) => columnOf.get(edge.to.node))
      .filter((column): column is number => column !== undefined);
    let column = aimed.length > 0 ? Math.min(...aimed) : 0;
    while (taken.has(column)) column += 1;
    taken.add(column);
    placed.set(modulator, column);
  }
  return placed;
}

/** Where each node of the graph goes on the canvas. */
function layout(graph: Graph, pinnedCounts: ReadonlyMap<NodeId, number>): Map<NodeId, { x: number; y: number }> {
  const placed = new Map<NodeId, { x: number; y: number }>();
  const heightOfNode = (node: Node): number => nodeHeight(node, pinnedCounts.get(node.id) ?? 0);
  const place = (node: Node, column: number, y: number): void => {
    placed.set(node.id, { x: column * COLUMN, y });
  };

  /** The room a strip's modulators need above its row: the tallest of them, and the room for a cable. */
  const aboveOf = (strip: Strip): number => (strip.modulators.length > 0 ? Math.max(...strip.modulators.map(heightOfNode)) + CABLE_GAP : 0);

  /** Put a strip's row from `column` on, its top at `rowTop`, its modulators above it; the y at the bottom of it. */
  const placeStrip = (strip: Strip, column: number, rowTop: number): number => {
    strip.row.forEach((node, offset) => {
      place(node, column + offset, rowTop);
    });
    for (const [modulator, offset] of strip.columns) place(modulator, column + offset, rowTop - aboveOf(strip));
    return rowTop + Math.max(...strip.row.map(heightOfNode)) + ROW_GAP;
  };

  // One strip for each instrument, from the top: modulators above, the row below them.
  const lanes = graph.nodes.filter((node) => node.kind === "instrument").map((lane) => stripOf(graph, lane));
  const rowTops = new Map<NodeId, number>();
  let y = 0;
  for (const lane of lanes) {
    const rowTop = y + aboveOf(lane);
    rowTops.set(lane.head.id, rowTop);
    y = placeStrip(lane, 0, rowTop);
  }

  // A strip for each group, to the right of the longest lane, its row level with the middle of its members'.
  const groups = graph.nodes.filter((node) => node.kind === "group").map((group) => stripOf(graph, group));
  const lanesWidth = Math.max(0, ...lanes.map(widthOf));
  const wanted = groups.map((group) => {
    const tops = lanes.filter((lane) => lane.head.parent === group.head.id).map((lane) => rowTops.get(lane.head.id) ?? 0);
    return { group, rowTop: tops.length > 0 ? tops.reduce((sum, top) => sum + top, 0) / tops.length : y + aboveOf(group) };
  });
  wanted.sort((a, b) => a.rowTop - b.rowTop);
  let groupsBottom = 0;
  for (const { group, rowTop } of wanted) {
    groupsBottom = placeStrip(group, lanesWidth, Math.max(rowTop, groupsBottom + aboveOf(group)));
  }

  // The master at the end, in the middle of everything.
  const groupsWidth = Math.max(0, ...groups.map(widthOf));
  const middle = (Math.max(y, groupsBottom) - ROW_GAP) / 2;
  for (const node of graph.nodes) {
    if (node.kind === "master") place(node, lanesWidth + groupsWidth, Math.max(0, middle - heightOfNode(node) / 2));
  }

  // Whatever the layout did not reach (a node whose parent is not there) goes in a row below.
  let stray = 0;
  for (const node of graph.nodes) {
    if (placed.has(node.id)) continue;
    place(node, stray, Math.max(y, groupsBottom));
    stray += 1;
  }
  return placed;
}

/** The nodes of the session on the canvas, and the cables between them. */
export function toFlow(graph: Graph): Flow {
  const names = displayNames(graph);
  const modulated = modulatedParams(graph);
  const pinned = new Map<NodeId, number[]>();
  for (const node of graph.nodes) {
    const plugin = node.kind === "instrument" || node.kind === "effect" ? node.plugin : null;
    pinned.set(node.id, pinnedParams(plugin, modulated.get(node.id) ?? new Set()));
  }
  const positions = layout(graph, new Map([...pinned].map(([id, params]) => [id, params.length])));

  const nodes: PatchNode[] = graph.nodes.map((node) => ({
    id: String(node.id),
    type: node.kind,
    position: node.ui ? { x: node.ui.x, y: node.ui.y } : (positions.get(node.id) ?? { x: 0, y: 0 }),
    width: NODE_WIDTH,
    // (React Flow makes a node a group of this name for a screen reader.)
    ariaLabel: names.get(node.id) ?? node.name,
    data: {
      node: node.id,
      name: names.get(node.id) ?? node.name,
      format: node.kind === "instrument" || node.kind === "effect" ? (node.plugin?.format ?? null) : null,
      pinned: pinned.get(node.id) ?? [],
      modulated: [...(modulated.get(node.id) ?? [])].sort((a, b) => a - b),
      targets: node.kind === "modulator" ? drivablesOf(graph, node.id, names) : [],
    },
    draggable: true,
    deletable: false,
  }));

  const edges: PatchEdge[] = [];
  for (const edge of graph.edges) {
    const sourceHandle = handleOf(edge.from.port);
    const targetHandle = handleOf(edge.to.port);
    if (sourceHandle === null || targetHandle === null) continue;
    const common = { source: String(edge.from.node), target: String(edge.to.node), sourceHandle, targetHandle, deletable: false };
    if (edge.kind === "modulation") {
      edges.push({ ...common, id: `mod:${String(edge.id)}`, type: "modulation", data: { cable: edge.id, modulator: edge.from.node, label: describeCable(graph, names, edge) }, zIndex: 10 });
    } else {
      edges.push({ ...common, id: `audio:${String(edge.from.node)}>${String(edge.to.node)}`, type: "audio", data: {} });
    }
  }
  return { nodes, edges };
}

/** What a modulation cable joins, in words: the modulator, then the node it drives and the parameter or setting of it. */
function describeCable(graph: Graph, names: ReadonlyMap<NodeId, string>, edge: Edge): string {
  return cableLabel(names.get(edge.from.node) ?? "?", names.get(edge.to.node) ?? "?", portName(graph, edge.to.node, edge.to.port));
}

/** A cable as the canvas depends on it: where it goes, and not how deep (which is a value). */
const cableStructure = (edge: Edge): string => JSON.stringify(edge.kind === "modulation" ? { ...edge, amount: 0 } : edge);

/**
 * Whether two graphs are laid out the same: the same nodes, in the same places in the
 * tree, with the same names, the same plugins' parameters, and the same cables. What is
 * not part of it is what the values in them are, which change all the time: a parameter's,
 * a gain, a modulator's rate or stages, the depth of a cable.
 */
export function sameStructure(a: Graph, b: Graph): boolean {
  if (a === b) return true;
  if (a.nodes.length !== b.nodes.length || a.edges.length !== b.edges.length) return false;
  for (let i = 0; i < a.nodes.length; i += 1) {
    const x = a.nodes[i];
    const y = b.nodes[i];
    if (!x || !y || !sameNodeStructure(x, y)) return false;
  }
  if (a.edges === b.edges) return true;
  for (let i = 0; i < a.edges.length; i += 1) {
    const x = a.edges[i];
    const y = b.edges[i];
    if (!x || !y || cableStructure(x) !== cableStructure(y)) return false;
  }
  return true;
}

/** Whether two nodes are placed alike: both left to the layout, or at the same place. */
function samePlace(x: NodeUi | undefined, y: NodeUi | undefined): boolean {
  // (A place that is missing is neither coordinate: it equals no place that there is.)
  return x === y || (x?.x === y?.x && x?.y === y?.y);
}

function sameNodeStructure(x: Node, y: Node): boolean {
  if (x === y) return true;
  if (x.id !== y.id || x.kind !== y.kind || x.parent !== y.parent || x.name !== y.name || !samePlace(x.ui, y.ui)) return false;
  if ((x.kind === "instrument" || x.kind === "effect") && (y.kind === "instrument" || y.kind === "effect")) {
    return samePluginStructure(x.plugin, y.plugin);
  }
  if (x.kind === "modulator" && y.kind === "modulator") return x.source.type === y.source.type;
  return true;
}

function samePluginStructure(x: Plugin | null, y: Plugin | null): boolean {
  if (x === y) return true;
  if (x === null || y === null) return false;
  return x.id === y.id && x.params.length === y.params.length && x.params.every((param, i) => param.index === y.params[i]?.index);
}
