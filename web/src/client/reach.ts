// Where a cable from a modulator may end (docs/web-ui.md §9.3, D21). The controller decides
// (`SessionModel::param_targets`, and `AddModEdge` refuses the rest), and a client that lets
// the user draw cables has to know before it asks, to show which parameters take the cable that
// is being dragged. It reads the answer from the graph alone, by the `parent` of each node; a
// fixture that the controller writes (what it accepted, for every modulator of a session with
// groups and lanes in and out of them: `fixtures/reach.json`) pins the two together.
import type { Graph, Node, NodeId } from "../protocol";

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
