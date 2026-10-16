import type { Graph, Node, NodeId } from "../protocol";

/**
 * What the engine's mute and solo make of each lane and group (`AudioGraph::process`; pinned to a run of it by `fixtures/audible.json`), by node: a lane or a group
 * that is muted is never heard (a mute wins over a solo), and while anything is soloed only what is soloed is. A lane that is not soloed is also heard when its group
 * is soloed, and a group that is not soloed is also heard for a lane in it that is soloed (its bus carries that lane).
 *
 * This is what the engine measures a lane at and a group's bus: a lane in a muted group is heard by its own flags (its meter reads) and its group is not (the bus is
 * cut). `audibleOf` says what reaches the master.
 */
export function heardOf(graph: Graph): ReadonlyMap<NodeId, boolean> {
  const flagged = graph.nodes.flatMap((node) => (node.kind === "instrument" || node.kind === "group" ? [node] : []));
  const anySolo = flagged.some((node) => node.solo);
  const groups = new Map<NodeId, Node & { kind: "group" }>();
  // (The groups that have a lane in them that is soloed.)
  const carrying = new Set<NodeId>();
  for (const node of flagged) {
    if (node.kind === "group") groups.set(node.id, node);
    else if (node.solo && node.parent !== null) carrying.add(node.parent);
  }
  const heard = (node: { muted: boolean; solo: boolean }, related: boolean): boolean => !node.muted && (!anySolo || node.solo || related);
  const result = new Map<NodeId, boolean>();
  for (const node of flagged) {
    if (node.kind === "group") {
      result.set(node.id, heard(node, carrying.has(node.id)));
    } else {
      // (A lane whose group is not in the graph goes to the master like one that is in none.)
      const group = node.parent === null ? undefined : groups.get(node.parent);
      result.set(node.id, heard(node, group?.solo ?? false));
    }
  }
  return result;
}

/** What `audibleOf` has made of each graph: every node of the page asks, at every change, and the answer is the graph's. */
const worked = new WeakMap<Graph, ReadonlyMap<NodeId, boolean>>();

/**
 * Which lanes and groups can be heard at the master: a group that is heard, and a lane that is heard and is in no group or in one that is (a muted group cuts
 * the lanes in it). The map has an entry for every lane and group of the graph and for nothing else. It is worked out once for a graph (a graph that is
 * changed is another object), since each node of the canvas asks at every change of the session.
 */
export function audibleOf(graph: Graph): ReadonlyMap<NodeId, boolean> {
  const known = worked.get(graph);
  if (known !== undefined) return known;
  const made = audibleNow(graph);
  worked.set(graph, made);
  return made;
}

function audibleNow(graph: Graph): ReadonlyMap<NodeId, boolean> {
  const heard = heardOf(graph);
  const groups = new Set(graph.nodes.flatMap((node) => (node.kind === "group" ? [node.id] : [])));
  const result = new Map<NodeId, boolean>();
  for (const node of graph.nodes) {
    if (node.kind !== "instrument" && node.kind !== "group") continue;
    // (A lane whose group is not in the graph is in none.)
    const bus = node.kind === "instrument" && node.parent !== null && groups.has(node.parent) ? node.parent : null;
    result.set(node.id, heard.get(node.id) === true && (bus === null || heard.get(bus) === true));
  }
  return result;
}
