// The session as a canvas: which node goes where, and which cable joins which two ports
// (docs/web-ui.md §9.3). A pure function of the graph the server sends: the layout is a
// lane strip for each instrument (the instrument, then its effects, left to right), its
// modulators above the strip, over the plugin each one drives (and a modulator that drives
// the settings of another above that one), the groups to the right of the lanes that feed
// them, and the master at the end. That is where a node is unless the
// session says otherwise: a node that has been moved has a place (`Node.ui`, kept in the
// session file, §4.4), and is drawn there; the layout of the others does not leave room for it.
import type { Edge as FlowEdge, Node as FlowNode } from "@xyflow/react";

import { cableWords, displayNames, endLabel } from "../../client/names";
import { knobsOf } from "../../client/pinning";
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

// What the layout assumes about the size of a node: the components' CSS is built to these.
const HEADER_HEIGHT = 44;
const CONTROL_ROW_HEIGHT = 108;
const CONTROLS_PER_ROW = 4;
const NODE_PADDING = 16;
/** The list at the foot of a modulator, with which it draws a cable (measured in a browser: 29). */
const DRIVES_HEIGHT = 30;
/** The trace of what a modulator puts out is this much taller than the bar it was (44 against 8: the bar's nodes were 119 and 213). */
const TRACE_EXTRA = 36;
/** The shape of an envelope's settings (a drawing a little under 60 px tall, and the gap above it) makes its node this much taller than an LFO's (measured in a browser: 315 against 249). */
const ENVELOPE_EXTRA = 66;
/** A modulator that has no settings (a MIDI control) is this tall (measured in a browser: 155)... */
export const MODULATOR_HEIGHT = 128 + TRACE_EXTRA;
/** ...and one that has them has a row of controls more (measured in a browser: 249). */
export const MODULATOR_SETTINGS_HEIGHT = HEADER_HEIGHT + CONTROL_ROW_HEIGHT + NODE_PADDING + 20 + DRIVES_HEIGHT + TRACE_EXTRA;
/** ...and an envelope's, which draws its shape, is taller by that. */
export const ENVELOPE_NODE_HEIGHT = MODULATOR_SETTINGS_HEIGHT + ENVELOPE_EXTRA;
export const MASTER_HEIGHT = 164;
/** A cable that a modulator's node lists is a row this tall (the style says so: `.tang-cable-row`)... */
export const CABLE_ROW_HEIGHT = 20;
/** ...and the list has this much room before it, as much as the rest of what is in the body of the node (`.tang-node__body`'s gap). */
export const CABLES_GAP = 8;

export type PatchNodeKind = "instrument" | "effect" | "modulator" | "group" | "master";

export interface PatchNodeData extends Record<string, unknown> {
  /** The node of the session this one is the picture of. */
  node: NodeId;
  /** What to call it: the server's name, with a number after it when another node has the same (`displayNames`). */
  name: string;
  /** What kind of plugin it has (`Built-in`, `LV2`, `CLAP`, `VST3`), if it has one. */
  format: string | null;
  /** The parameters of its plugin it shows as knobs, by the plugin's own index: the ones pinned to it or the first few (`knobsOf`), and every one a cable ends at. */
  knobs: readonly number[];
  /** Those of them that a modulator drives (a cable ends at them). */
  modulated: readonly number[];
  /** For a modulator: what it can still be drawn to, plugin by plugin and modulator by modulator (`drivablesOf`); nothing for the other nodes. */
  targets: readonly Drivable[];
  /** For a modulator: the settings of it that a cable ends at, as the names of their handles, in the order a modulator has them; nothing for the other nodes. */
  drivenSettings: readonly SettingHandle[];
  /** For a modulator: the cables that start at it, in the order of the graph, as its node lists them (`CableRow`); nothing for the other nodes. */
  cables: readonly CableRow[];
}

/**
 * A cable that starts at a modulator, as the modulator's node lists it: a row that says what the cable goes to, with a place on its
 * edge where another modulator's cable can end, to drive how deep this one goes.
 */
export interface CableRow {
  /** The cable. */
  edge: EdgeId;
  /** What the cable ends at, in words (`Filter: cutoff`, `depth of LFO → Filter: cutoff`: `endLabel`). */
  label: string;
  /** Whether a cable ends at its depth: another modulator drives how deep it goes. */
  driven: boolean;
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

/**
 * The settings of a modulator that a cable ends at, by the names of their handles (the names of the ports), in the order a
 * modulator has them: an LFO's rate, the four stages of an envelope.
 */
export const SETTING_HANDLES = ["rate", "attack", "decay", "sustain", "release"] as const;

export type SettingHandle = (typeof SETTING_HANDLES)[number];

/** The name of the handle of a parameter of a plugin, by the plugin's own index. */
export const paramHandle = (index: number): string => `param-${String(index)}`;

/** The name of the handle in a modulator's node where a cable can end to drive the depth of the cable `edge`, which starts at that modulator. */
export const depthHandle = (edge: EdgeId): string => `depth-${String(edge)}`;

/** The name of the handle of a node where a port is. */
export function handleOf(port: Port): string {
  switch (port.type) {
    case "audioIn":
    case "audioOut":
    case "modOut":
    case "rate":
    case "attack":
    case "decay":
    case "sustain":
    case "release":
      return port.type;
    case "param":
      return paramHandle(port.index);
    case "depth":
      return depthHandle(port.edge);
  }
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

/** The settings of each modulator that a cable ends at: the names of their handles, in the order a modulator has them. */
function drivenSettings(graph: Graph): Map<NodeId, SettingHandle[]> {
  const driven = new Map<NodeId, Set<string>>();
  for (const edge of graph.edges) {
    const settings = driven.get(edge.to.node) ?? new Set<string>();
    settings.add(edge.to.port.type);
    driven.set(edge.to.node, settings);
  }
  return new Map([...driven].map(([node, settings]) => [node, SETTING_HANDLES.filter((handle) => settings.has(handle))]));
}

/** How many controls a node shows: its parameters, and its own gain (a volume or a mix; a lane's keys and its group too) or a group's two. */
export function controlCount(node: Node, knobs: number): number {
  switch (node.kind) {
    case "instrument":
      return knobs + 3;
    case "effect":
      return knobs + 1;
    case "group":
      return 2;
    default:
      return 0;
  }
}

/** How tall a node is, as the layout assumes it (the components' styles keep to it). `cables` is how many a modulator lists (`CableRow`); it is nothing to the other nodes. */
export function nodeHeight(node: Node, knobs: number, cables = 0): number {
  switch (node.kind) {
    case "modulator": {
      const listed = cables > 0 ? CABLES_GAP + cables * CABLE_ROW_HEIGHT : 0;
      if (node.source.type === "midiLearn") return MODULATOR_HEIGHT + listed;
      return (node.source.type === "envelope" ? ENVELOPE_NODE_HEIGHT : MODULATOR_SETTINGS_HEIGHT) + listed;
    }
    case "master":
      return MASTER_HEIGHT;
    default: {
      const rows = Math.max(1, Math.ceil(controlCount(node, knobs) / CONTROLS_PER_ROW));
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
  /** The rows of modulators above the strip's row, the nearest first (`tiersOf`). */
  tiers: Node[][];
  /** The column of each modulator, from the strip's first. */
  columns: Map<Node, number>;
}

/** A strip of the instrument or group `head`: its nodes, and the modulators that belong to it. */
function stripOf(graph: Graph, head: Node): Strip {
  const children = graph.nodes.filter((node) => node.parent === head.id);
  const row = [head, ...children.filter((node) => node.kind === "effect")];
  const tiers = tiersOf(graph, children.filter((node) => node.kind === "modulator"));
  return { head, row, tiers, columns: modulatorColumns(graph, row, tiers) };
}

/**
 * The modulators of a rack in rows above its strip, the nearest the strip first. A cable runs from the bottom of a node to the
 * top of what it drives, so a modulator that drives a setting of another (that the canvas draws a cable to) is a row above it:
 * a modulator's row is as many above the strip's as the longest chain of modulators it drives (none for one that drives no
 * other). A rack in which none drives another is one row. A cycle is cut where it is first met, in the order of the graph.
 */
function tiersOf(graph: Graph, modulators: readonly Node[]): Node[][] {
  const drives = new Map<NodeId, NodeId[]>(modulators.map((node) => [node.id, []]));
  for (const edge of graph.edges) {
    // (Only the cables of the rack, from one of its modulators to another.)
    if (drives.has(edge.to.node)) drives.get(edge.from.node)?.push(edge.to.node);
  }
  const heights = new Map<NodeId, number>();
  const visiting = new Set<NodeId>();
  const heightOf = (id: NodeId): number => {
    const known = heights.get(id);
    if (known !== undefined) return known;
    visiting.add(id);
    let height = 0;
    for (const driven of drives.get(id) ?? []) {
      if (!visiting.has(driven)) height = Math.max(height, heightOf(driven) + 1);
    }
    visiting.delete(id);
    heights.set(id, height);
    return height;
  };
  const tiers: Node[][] = [];
  for (const modulator of modulators) {
    const tier = heightOf(modulator.id);
    tiers[tier] = [...(tiers[tier] ?? []), modulator];
  }
  // (A modulator's row is one above the highest of those it drives, so no row between the first and the last is empty.)
  return tiers;
}

/**
 * How many columns a strip takes: its row, or as far as its modulators go if they go past the row (a
 * lane with more modulators than nodes). What comes after it, groups and the master, starts after that.
 */
function widthOf(strip: Strip): number {
  return Math.max(strip.row.length, ...[...strip.columns.values()].map((column) => column + 1));
}

/**
 * Where the modulators of a strip go, as columns from its first: above the node each drives (the first of them, if it drives
 * more than one), or the first column that is free in its row. The rows are placed from the nearest the strip, so that what
 * a modulator drives of the others is placed before it.
 */
function modulatorColumns(graph: Graph, row: readonly Node[], tiers: readonly Node[][]): Map<Node, number> {
  const columnOf = new Map<NodeId, number>(row.map((node, column) => [node.id, column]));
  const placed = new Map<Node, number>();
  for (const tier of tiers) {
    const taken = new Set<number>();
    for (const modulator of tier) {
      const aimed = graph.edges
        .filter((edge) => edge.from.node === modulator.id)
        .map((edge) => columnOf.get(edge.to.node))
        .filter((column): column is number => column !== undefined);
      let column = aimed.length > 0 ? Math.min(...aimed) : 0;
      while (taken.has(column)) column += 1;
      taken.add(column);
      columnOf.set(modulator.id, column);
      placed.set(modulator, column);
    }
  }
  return placed;
}

/** Where each node of the graph goes on the canvas. */
function layout(graph: Graph, knobCounts: ReadonlyMap<NodeId, number>, cableCounts: ReadonlyMap<NodeId, number>): Map<NodeId, { x: number; y: number }> {
  const placed = new Map<NodeId, { x: number; y: number }>();
  const heightOfNode = (node: Node): number => nodeHeight(node, knobCounts.get(node.id) ?? 0, cableCounts.get(node.id) ?? 0);
  const place = (node: Node, column: number, y: number): void => {
    placed.set(node.id, { x: column * COLUMN, y });
  };

  /** The room a row of modulators needs: the tallest of them, and the room for a cable to arrive below it. */
  const roomOf = (tier: readonly Node[]): number => Math.max(...tier.map(heightOfNode)) + CABLE_GAP;

  /** The room a strip's modulators need above its row: a row of them for each step of what drives what. */
  const aboveOf = (strip: Strip): number => strip.tiers.reduce((room, tier) => room + roomOf(tier), 0);

  /** Put a strip's row from `column` on, its top at `rowTop`, its modulators above it, the rows of them from the nearest; the y at the bottom of it. */
  const placeStrip = (strip: Strip, column: number, rowTop: number): number => {
    strip.row.forEach((node, offset) => {
      place(node, column + offset, rowTop);
    });
    let below = rowTop;
    for (const tier of strip.tiers) {
      below -= roomOf(tier);
      for (const modulator of tier) place(modulator, column + (strip.columns.get(modulator) ?? 0), below);
    }
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

/** The cables that start at each modulator, as its node lists them, in the order of the graph. */
function cableRows(graph: Graph, names: ReadonlyMap<NodeId, string>): Map<NodeId, CableRow[]> {
  const drivenDepths = new Set<EdgeId>(graph.edges.flatMap((edge) => (edge.to.port.type === "depth" ? [edge.to.port.edge] : [])));
  const rows = new Map<NodeId, CableRow[]>();
  for (const edge of graph.edges) {
    if (edge.kind !== "modulation") continue;
    rows.set(edge.from.node, [...(rows.get(edge.from.node) ?? []), { edge: edge.id, label: endLabel(graph, names, edge.to), driven: drivenDepths.has(edge.id) }]);
  }
  return rows;
}

/** The nodes of the session on the canvas, and the cables between them. */
export function toFlow(graph: Graph): Flow {
  const names = displayNames(graph);
  const modulated = modulatedParams(graph);
  const settingsDriven = drivenSettings(graph);
  const listed = cableRows(graph, names);
  const knobs = new Map<NodeId, number[]>();
  for (const node of graph.nodes) {
    const plugin = node.kind === "instrument" || node.kind === "effect" ? node.plugin : null;
    knobs.set(node.id, knobsOf(plugin, modulated.get(node.id) ?? new Set()));
  }
  const positions = layout(
    graph,
    new Map([...knobs].map(([id, params]) => [id, params.length])),
    new Map(graph.nodes.map((node) => [node.id, node.kind === "modulator" ? (listed.get(node.id)?.length ?? 0) : 0])),
  );

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
      knobs: knobs.get(node.id) ?? [],
      modulated: [...(modulated.get(node.id) ?? [])].sort((a, b) => a - b),
      targets: node.kind === "modulator" ? drivablesOf(graph, node.id, names) : [],
      drivenSettings: node.kind === "modulator" ? (settingsDriven.get(node.id) ?? []) : [],
      cables: node.kind === "modulator" ? (listed.get(node.id) ?? []) : [],
    },
    draggable: true,
    deletable: false,
  }));

  const edges: PatchEdge[] = [];
  for (const edge of graph.edges) {
    const common = { source: String(edge.from.node), target: String(edge.to.node), sourceHandle: handleOf(edge.from.port), targetHandle: handleOf(edge.to.port), deletable: false };
    if (edge.kind === "modulation") {
      edges.push({ ...common, id: `mod:${String(edge.id)}`, type: "modulation", data: { cable: edge.id, modulator: edge.from.node, label: cableWords(graph, names, edge) }, zIndex: 10 });
    } else {
      edges.push({ ...common, id: `audio:${String(edge.from.node)}>${String(edge.to.node)}`, type: "audio", data: {} });
    }
  }
  return { nodes, edges };
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
  return (
    x.id === y.id &&
    x.params.length === y.params.length &&
    x.params.every((param, i) => param.index === y.params[i]?.index) &&
    x.pinned.length === y.pinned.length &&
    x.pinned.every((index, i) => index === y.pinned[i])
  );
}
