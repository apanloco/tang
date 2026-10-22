import type { Graph, Node, NodeId } from "../protocol";

/** What the law of mute and solo makes of each lane and group: heard for what it is, and heard at all. */
interface Law {
  /** A lane is heard for what it is when it is not muted and nothing is soloed, or it is, or its group is: its instrument, then. (A group's is the same, with a lane of it soloed.) */
  own: ReadonlyMap<NodeId, boolean>;
  /** A lane's chain, or a group's bus, is heard: for what it is, or for what is sent to it by a cable from something that is heard. */
  out: ReadonlyMap<NodeId, boolean>;
  /** The lanes and groups that a cable from something that is heard puts sound into (or, for a group, a lane of it that does, and goes to it), whether or not they are heard for what they are as well. */
  fed: ReadonlySet<NodeId>;
}

/** What `lawOf` has made of each graph: every node of the page asks, at every change, and the answer is the graph's. */
const laws = new WeakMap<Graph, Law>();

function lawOf(graph: Graph): Law {
  const known = laws.get(graph);
  if (known !== undefined) return known;
  const made = lawNow(graph);
  laws.set(graph, made);
  return made;
}

/**
 * The engine's law of mute and solo, written again (`Schedule::set_gates`; pinned to a run of it by `fixtures/audible.json`, for sessions with no cables, and `fixtures/audibleCables.json`):
 * a lane or a group that is muted is never heard (a mute wins over a solo), and while anything is soloed only what is soloed is. A lane that is not soloed is also heard when its group is
 * soloed, and a group that is not soloed is also heard for a lane in it that is soloed (its bus carries that lane): that is `own`, what a lane or a group is heard for by itself.
 *
 * Audio that cables carry is heard where it comes from something that is heard: a lane or a group that is not muted is also heard *for what is sent to it* when a cable (at a level above
 * nothing) ends at an effect of its chain or its bus, or at the bus of a group, and starts at the instrument of a lane that is heard for what it is, at an effect of a lane or a group that is
 * heard either way, or at the output of a group that is; a group is also heard for a lane in it that goes to its bus and is heard for what is sent to it. That, `own` or sent to, is `out`: what the
 * lane's chain and the group's bus are heard at. (A lane that is heard only for what is sent to it puts none of its own sound into its chain: see `ownAudibleOf`.) It is worked out by going
 * through the cables until nothing changes, as the engine does: cables go one way, so that is at most as many times as there are.
 */
function lawNow(graph: Graph): Law {
  const flagged = graph.nodes.flatMap((node) => (node.kind === "instrument" || node.kind === "group" ? [node] : []));
  const anySolo = flagged.some((node) => node.solo);
  const groups = new Map<NodeId, Node & { kind: "group" }>();
  // (The groups that have a lane in them that is soloed.)
  const carrying = new Set<NodeId>();
  for (const node of flagged) {
    if (node.kind === "group") groups.set(node.id, node);
    else if (node.solo && node.parent !== null) carrying.add(node.parent);
  }
  const letIn = (node: { solo: boolean }, related: boolean): boolean => !anySolo || node.solo || related;
  const own = new Map<NodeId, boolean>();
  const soloHeard = new Map<NodeId, boolean>();
  for (const node of flagged) {
    // (A lane whose group is not in the graph goes to the master like one that is in none.)
    const related = node.kind === "group" ? carrying.has(node.id) : node.parent === null ? false : (groups.get(node.parent)?.solo ?? false);
    soloHeard.set(node.id, letIn(node, related));
    own.set(node.id, !node.muted && soloHeard.get(node.id) === true);
  }
  // The lane or group that the end of a cable belongs to: an effect is its parent's and a group is its own (a cable ends at no lane: a lane has no input; the start at an instrument is read by itself).
  const ownerOf = (id: NodeId): NodeId | undefined => {
    const node = graph.nodes.find((n) => n.id === id);
    if (node?.kind === "effect") return node.parent ?? undefined;
    return node?.kind === "group" ? node.id : undefined;
  };
  const cables = graph.edges.flatMap((edge) => {
    if (edge.kind !== "cable" || edge.gain <= 0) return [];
    const [from, to] = [graph.nodes.find((n) => n.id === edge.from.node), ownerOf(edge.to.node)];
    const start = from?.kind === "instrument" ? { owner: from.id, instrument: true } : from?.kind === "effect" || from?.kind === "group" ? { owner: ownerOf(from.id), instrument: false } : undefined;
    return start?.owner === undefined || to === undefined ? [] : [{ from: start.owner, instrument: start.instrument, to }];
  });
  const flags = new Map(flagged.map((node) => [node.id, node]));
  const muted = (id: NodeId): boolean => flags.get(id)?.muted === true;
  const live = new Set<NodeId>();
  const heard = (id: NodeId): boolean => !muted(id) && (soloHeard.get(id) === true || live.has(id));
  for (let changed = true; changed; ) {
    changed = false;
    for (const cable of cables) {
      const sent = cable.instrument ? own.get(cable.from) === true : heard(cable.from);
      if (sent && !muted(cable.to) && !live.has(cable.to)) {
        live.add(cable.to);
        changed = true;
      }
    }
    // A lane that is heard for what is sent to it puts it into the bus of its group, which is heard for that.
    for (const node of flagged) {
      if (node.kind !== "instrument" || !live.has(node.id) || node.route !== "out" || node.parent === null) continue;
      const bus = groups.get(node.parent);
      if (bus !== undefined && !bus.muted && !live.has(bus.id)) {
        live.add(bus.id);
        changed = true;
      }
    }
  }
  return { own, out: new Map(flagged.map((node) => [node.id, heard(node.id)])), fed: live };
}

/**
 * What the engine's mute and solo make of each lane and group (`AudioGraph::process`, `lawOf`), by node: what a lane's chain and a group's bus are heard at, for what the lane or the
 * group is or for what is sent to it by cables. This is what the engine measures a lane at and a group's bus: a lane in a muted group is heard by its own flags (its meter reads) and its
 * group is not (the bus is cut). `audibleOf` says what reaches the master.
 */
export function heardOf(graph: Graph): ReadonlyMap<NodeId, boolean> {
  return lawOf(graph).out;
}

/**
 * The lanes and groups that are fed sound by a cable from something that is heard (at a level above nothing, not muted: the lane's chain, a group's bus or an effect of either), and the groups
 * that a lane that is fed goes to. A meter that has nothing else to read reads for them (`MockServer.frame`): a group that is heard for a cable has no lane in it that is heard.
 */
export function fedOf(graph: Graph): ReadonlySet<NodeId> {
  return lawOf(graph).fed;
}

/** What `audibleOf` has made of each graph. */
const reached = new WeakMap<Graph, ReadonlyMap<NodeId, boolean>>();
/** What `ownAudibleOf` has made of each graph. */
const ownReached = new WeakMap<Graph, ReadonlyMap<NodeId, boolean>>();

/** The bus that a node's sound goes by: a lane's group if it has one in the graph, and nothing for a lane in none and for a group. */
function busOf(node: Node, groups: ReadonlySet<NodeId>): NodeId | null {
  // (A lane whose group is not in the graph is in none.)
  return node.kind === "instrument" && node.parent !== null && groups.has(node.parent) ? node.parent : null;
}

function reachedBy(graph: Graph, heard: ReadonlyMap<NodeId, boolean>): ReadonlyMap<NodeId, boolean> {
  const groups = new Set(graph.nodes.flatMap((node) => (node.kind === "group" ? [node.id] : [])));
  const result = new Map<NodeId, boolean>();
  for (const node of graph.nodes) {
    if (node.kind !== "instrument" && node.kind !== "group") continue;
    const bus = busOf(node, groups);
    result.set(node.id, heard.get(node.id) === true && (bus === null || heardOf(graph).get(bus) === true));
  }
  return result;
}

/**
 * Which lanes and groups can be heard at the master: a group that is heard, and a lane whose chain is heard and that is in no group or in one that is (a muted group cuts the lanes in it).
 * The map has an entry for every lane and group of the graph and for nothing else. It is worked out once for a graph (a graph that is changed is another object), since each node of the
 * canvas asks at every change of the session. It is what the effects of a lane, which are in its chain, are heard at.
 */
export function audibleOf(graph: Graph): ReadonlyMap<NodeId, boolean> {
  const known = reached.get(graph);
  if (known !== undefined) return known;
  const made = reachedBy(graph, heardOf(graph));
  reached.set(graph, made);
  return made;
}

/**
 * Which lanes' own sound can be heard at the master: a lane that is heard for what it is, and is in no group or in one that is heard. A lane that is heard only for what a cable sends it is
 * in `audibleOf` and not here: it puts none of its own sound into its chain while something else is soloed (its head gate). For a group it is what `audibleOf` says.
 */
export function ownAudibleOf(graph: Graph): ReadonlyMap<NodeId, boolean> {
  const known = ownReached.get(graph);
  if (known !== undefined) return known;
  const made = reachedBy(graph, lawOf(graph).own);
  // (A group has no sound of its own to look at: its bus is what is heard.)
  const result = new Map(made);
  for (const node of graph.nodes) if (node.kind === "group") result.set(node.id, audibleOf(graph).get(node.id) === true);
  ownReached.set(graph, result);
  return result;
}
