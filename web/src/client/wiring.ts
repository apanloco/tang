// The audio of a session as the engine's plan has it (`build_patch` in `src/plugin/chain/nodes.rs`, docs/patch-graph.md §11.6), read from the graph a client is given, so that the page can
// say what an audio cable may join and the mock engine can refuse what the controller refuses: which two places a cable goes between (`cableEnds`, `SessionModel::cable_ends`), and whether
// the audio would go round with it (`cableLoops`), or when a lane is put in a group (`laneLoops`) or an effect is moved along its chain (`effectLoops`), with the cables there are.
//
// A lane is a row of three kinds of stage: its head (the instrument and its volume), an effect each and its tail (the fade, the meter); a group is the same, with its head taking the sum of its
// members. The chain runs head, effects, tail; a lane's tail runs into its group's head, or into the mix, and a group's into the mix. A cable leaves the head of a lane (what its instrument puts
// out), an effect or the tail of a group (its output), and ends at an effect, the head of a group (its bus) or the mix. The audio goes round if those stages have a cycle: the plan does not compile.
// The graph has one node for a lane's instrument and one for a group, which stand for their head and their tail as the cable at them says (a cable from a group leaves its tail, a cable to a group
// ends at its head), so the stages are made from the nodes and the kind of end. A lane or a group whose way out is cut (`route` is `none`) has no link out of its tail.
import type { Edge, Endpoint, Graph, Node, NodeId, Route, WireError } from "../protocol";
import { isAudioCable } from "./cables";

/** The master's node id, as the server numbers it (`NodeId::MASTER`). */
const MASTER_ID: NodeId = 4294967295;

/** Where a cable starts: what a lane's instrument puts out, an effect (a lane's or a bus's) or the output of a group. */
export interface CableStart {
  kind: "instrument" | "effect" | "group";
  node: NodeId;
}

/** Where a cable ends: an effect, the bus of a group or the mix. */
export interface CableEnd {
  kind: "effect" | "groupBus" | "master";
  node: NodeId;
}

/** The two ends of an audio cable. */
export interface Ends {
  from: CableStart;
  to: CableEnd;
}

const invalid = (message: string): { error: WireError } => ({ error: { code: "invalid", message } });

/**
 * The ends that a cable from the port `from` to the port `to` has: an instrument's, an effect's or a group's `audioOut` to an effect's, a group's or the master's `audioIn`; or why it has
 * none, in the controller's words and its order: a port that is not that, the mix as a start, a node that is not there (not found), a modulator, a lane as an end (`SessionModel::cable_ends`).
 */
export function cableEnds(graph: Graph, from: Endpoint, to: Endpoint): Ends | { error: WireError } {
  if (from.port.type !== "audioOut") return invalid("an audio cable starts at an output of audio");
  if (to.port.type !== "audioIn") return invalid("an audio cable ends at an input of audio");
  if (from.node === MASTER_ID) return invalid("the mix has no output of audio");
  const start = graph.nodes.find((n) => n.id === from.node && n.kind !== "master");
  if (!start) return { error: { code: "notFound", message: `there is no node ${String(from.node)}` } };
  if (start.kind === "modulator") return invalid("a modulator has no output of audio");
  if (start.kind === "source") return invalid("a source of notes has no output of audio: it has an output of events");
  const first: CableStart = { kind: start.kind === "instrument" ? "instrument" : start.kind === "group" ? "group" : "effect", node: start.id };
  if (to.node === MASTER_ID) return { from: first, to: { kind: "master", node: MASTER_ID } };
  const end = graph.nodes.find((n) => n.id === to.node && n.kind !== "master");
  if (!end) return { error: { code: "notFound", message: `there is no node ${String(to.node)}` } };
  if (end.kind === "instrument") return invalid("a lane has no input of audio: a cable ends at an effect, a group or the mix");
  if (end.kind === "modulator") return invalid("a modulator has no input of audio");
  if (end.kind === "source") return invalid("a source of notes has no input of audio");
  return { from: first, to: { kind: end.kind === "group" ? "groupBus" : "effect", node: end.id } };
}

/** The wiring of a lane: its instrument's node, the effects of its chain in order, the group it is in (none for the mix) and its way out (`none`: its tail feeds nothing). */
export interface LaneWiring {
  node: NodeId;
  effects: NodeId[];
  group: NodeId | null;
  route: Route;
}

/** The wiring of a group: its node, the effects of its bus in order and its way out (`none`: its tail feeds nothing). */
export interface GroupWiring {
  node: NodeId;
  effects: NodeId[];
  route: Route;
}

/** The lanes and the groups of a session as the plan wires them, in the order of the graph (`SessionModel::wiring`). */
export interface Wiring {
  lanes: LaneWiring[];
  groups: GroupWiring[];
}

/** The lanes and groups of the graph: a chain is the effects whose parent is the lane or the group, in the order of the graph. */
export function wiringOf(graph: Graph): Wiring {
  const chain = (owner: NodeId): NodeId[] => graph.nodes.filter((n) => n.kind === "effect" && n.parent === owner).map((n) => n.id);
  const groupIds = new Set(graph.nodes.filter((n) => n.kind === "group").map((n) => n.id));
  return {
    lanes: graph.nodes.flatMap((n) => (n.kind === "instrument" ? [{ node: n.id, effects: chain(n.id), group: n.parent !== null && groupIds.has(n.parent) ? n.parent : null, route: n.route }] : [])),
    groups: graph.nodes.flatMap((n) => (n.kind === "group" ? [{ node: n.id, effects: chain(n.id), route: n.route }] : [])),
  };
}

/**
 * The links of the audio path that the nodes of a graph have, in the order that `SessionModel::graph` lists them: each group's chain (the group, its bus effects, and then the master unless
 * its way out is cut), then each lane's (the instrument, its effects, and then its group by a link of kind `member`, or the master by one of kind `audio`, unless its way out is cut).
 * A chain whose way out is cut ends at its last node. There is no master among the nodes of a session that has none: its links end nowhere, and are left out.
 */
export function linksOf(nodes: readonly Node[]): Edge[] {
  const master = nodes.find((n) => n.kind === "master");
  const groupIds = new Set(nodes.filter((n) => n.kind === "group").map((n) => n.id));
  const link = (from: NodeId, to: NodeId, kind: "audio" | "member"): Edge => ({ from: { node: from, port: { type: "audioOut" } }, to: { node: to, port: { type: "audioIn" } }, kind });
  const chain = (owner: Node, sink: { node: NodeId; kind: "audio" | "member" } | null): Edge[] => {
    const through = nodes.filter((n) => n.kind === "effect" && n.parent === owner.id).map((n) => n.id);
    const edges: Edge[] = [];
    let from = owner.id;
    for (const node of through) {
      edges.push(link(from, node, "audio"));
      from = node;
    }
    if (sink) edges.push(link(from, sink.node, sink.kind));
    return edges;
  };
  const toMaster = master ? { node: master.id, kind: "audio" as const } : null;
  return [
    ...nodes.flatMap((n) => (n.kind === "group" ? chain(n, n.route === "out" ? toMaster : null) : [])),
    ...nodes.flatMap((n) => {
      if (n.kind !== "instrument") return [];
      const sink = n.parent !== null && groupIds.has(n.parent) ? { node: n.parent, kind: "member" as const } : toMaster;
      return chain(n, n.route === "out" ? sink : null);
    }),
  ];
}

// The stages, as the names of the nodes of the plan: a lane's or a group's head and tail, an effect, the mix.
const head = (node: NodeId): string => `head:${String(node)}`;
const tail = (node: NodeId): string => `tail:${String(node)}`;
const effect = (node: NodeId): string => `fx:${String(node)}`;
const MIX = "mix";

/** The stage that a cable starts at: the head of a lane (after its volume), an effect, the tail of a group. */
const startStage = (from: CableStart): string => (from.kind === "instrument" ? head(from.node) : from.kind === "effect" ? effect(from.node) : tail(from.node));

/** The stage that a cable ends at: an effect, the head of a group, the mix. */
const endStage = (to: CableEnd): string => (to.kind === "effect" ? effect(to.node) : to.kind === "groupBus" ? head(to.node) : MIX);

/** What `cable` (an edge of kind `cable`) joins, by the kinds of the nodes it is at; none if a node of it is not in the graph. */
function endsOfEdge(nodes: ReadonlyMap<NodeId, Node>, cable: Edge): Ends | null {
  const start = nodes.get(cable.from.node);
  if (!start || (start.kind !== "instrument" && start.kind !== "effect" && start.kind !== "group")) return null;
  const from: CableStart = { kind: start.kind, node: start.id };
  if (cable.to.node === MASTER_ID) return { from, to: { kind: "master", node: MASTER_ID } };
  const end = nodes.get(cable.to.node);
  if (!end || (end.kind !== "effect" && end.kind !== "group")) return null;
  return { from, to: { kind: end.kind === "group" ? "groupBus" : "effect", node: end.id } };
}

/** The ends of every audio cable of the graph that has them (one with a node that is not there is left out, as the plan leaves it out), in the order of the graph. */
export function cablesOf(graph: Graph): Ends[] {
  const nodes = new Map(graph.nodes.map((n) => [n.id, n]));
  return graph.edges.flatMap((e) => (isAudioCable(e) ? (endsOfEdge(nodes, e) ?? []) : []));
}

/** Whether the audio goes round in the wiring `wiring` with the cables `cables`: the stages have a cycle, which is what keeps the plan from compiling. */
export function goesRound(wiring: Wiring, cables: readonly Ends[]): boolean {
  const out = new Map<string, string[]>();
  const link = (from: string, to: string): void => {
    out.set(from, [...(out.get(from) ?? []), to]);
  };
  const chain = (first: string, effects: readonly NodeId[], last: string): void => {
    let from = first;
    for (const fx of effects) {
      link(from, effect(fx));
      from = effect(fx);
    }
    link(from, last);
  };
  const groups = new Set(wiring.groups.map((g) => g.node));
  for (const lane of wiring.lanes) {
    chain(head(lane.node), lane.effects, tail(lane.node));
    if (lane.route === "out") link(tail(lane.node), lane.group !== null && groups.has(lane.group) ? head(lane.group) : MIX);
  }
  for (const group of wiring.groups) {
    chain(head(group.node), group.effects, tail(group.node));
    if (group.route === "out") link(tail(group.node), MIX);
  }
  for (const cable of cables) link(startStage(cable.from), endStage(cable.to));
  // A cycle is found by walking each stage once, with the stages on the way marked.
  const done = new Set<string>();
  const onWay = new Set<string>();
  const visit = (stage: string): boolean => {
    if (onWay.has(stage)) return true;
    if (done.has(stage)) return false;
    onWay.add(stage);
    const round = (out.get(stage) ?? []).some(visit);
    onWay.delete(stage);
    done.add(stage);
    return round;
  };
  return [...out.keys()].some(visit);
}

/** Whether the audio would go round if a cable with these ends were drawn, with the cables there are (`SessionModel::cable_problem`). */
export function cableLoops(graph: Graph, ends: Ends): boolean {
  return goesRound(wiringOf(graph), [...cablesOf(graph), ends]);
}

/** Whether the audio would go round if the lane `lane` were in the group `group` (none for no group), with the cables there are (`SessionModel::loop_if_lane_goes_to`). */
export function laneLoops(graph: Graph, lane: NodeId, group: NodeId | null): boolean {
  const wiring = wiringOf(graph);
  // (A group that is not one of the wiring's is no group to `goesRound`: the lane goes into the mix.)
  const moved = wiring.lanes.map((l) => (l.node === lane ? { ...l, group } : l));
  return goesRound({ ...wiring, lanes: moved }, cablesOf(graph));
}

/**
 * Whether the audio would go round if the lane or the group `node` had the way out `route`, with the cables there are (`SessionModel::loop_if_route_is`). Only a way out that is `out` can close a loop
 * (a link that is not there is on no path), and what is no lane or group of the graph makes none.
 */
export function routeLoops(graph: Graph, node: NodeId, route: Route): boolean {
  const wiring = wiringOf(graph);
  return goesRound(
    { lanes: wiring.lanes.map((l) => (l.node === node ? { ...l, route } : l)), groups: wiring.groups.map((g) => (g.node === node ? { ...g, route } : g)) },
    cablesOf(graph),
  );
}

/**
 * Whether the audio would go round if the effect `node` of a lane's chain or of a group's bus were at place `index` of its chain, the effects between moving one place the other way, with
 * the cables there are (`SessionModel::loop_if_effect_goes_to`). False for what is no effect of the graph or a place that the chain has not.
 */
export function effectLoops(graph: Graph, node: NodeId, index: number): boolean {
  const wiring = wiringOf(graph);
  const reordered = (effects: NodeId[]): NodeId[] | null => {
    const from = effects.indexOf(node);
    if (from < 0 || index < 0 || index >= effects.length) return null;
    const moved = [...effects];
    moved.splice(from, 1);
    moved.splice(index, 0, node);
    return moved;
  };
  let moved: Wiring | null = null;
  for (const lane of wiring.lanes) {
    const effects = reordered(lane.effects);
    if (effects) moved = { ...wiring, lanes: wiring.lanes.map((l) => (l === lane ? { ...l, effects } : l)) };
  }
  for (const group of wiring.groups) {
    const effects = reordered(group.effects);
    if (effects) moved = { ...wiring, groups: wiring.groups.map((g) => (g === group ? { ...g, effects } : g)) };
  }
  return moved !== null && goesRound(moved, cablesOf(graph));
}

/**
 * The nodes that a cable from the output of `from` can be drawn to now: every effect, every group (its bus) and the mix that the controller would take a cable to from there, that is
 * an end it has, with no cable between the two already and no loop. For the canvas to say which inputs take the cable that is being dragged. A node that has no output of audio has none.
 */
export function cableTakers(graph: Graph, from: NodeId): NodeId[] {
  // (The wiring and the cables are the same for every end that is tried: a menu on every node asks this for each of them.)
  const wiring = wiringOf(graph);
  const cables = cablesOf(graph);
  return graph.nodes.flatMap((node) => {
    if (node.kind !== "effect" && node.kind !== "group" && node.kind !== "master") return [];
    const ends = cableEnds(graph, { node: from, port: { type: "audioOut" } }, { node: node.id, port: { type: "audioIn" } });
    if ("error" in ends) return [];
    const drawn = graph.edges.some((e) => isAudioCable(e) && e.from.node === from && e.to.node === node.id);
    return drawn || goesRound(wiring, [...cables, ends]) ? [] : [node.id];
  });
}
