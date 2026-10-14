// What being a member of a group means for the cables (docs/web-ui.md §5): the controller's `SessionModel::group_cables_into_lane`, read from the graph.
import type { EdgeId, Graph, NodeId } from "../protocol";

/**
 * The cables of the group that the lane `lane` is a member of that go into the lane: from the group's own modulators to a plugin of the lane
 * (the instrument and its effects) and to the settings of the lane's modulators and the depths of their cables, which all end at a node that is
 * in the lane. The lane's own modulators' cables are not these (they come from a node of the lane), and nor is a cable that drives the depth of one
 * of these (it ends at the group's modulator). None for what is no lane, and for a lane in no group.
 */
export function cablesIntoLaneFromItsGroup(graph: Graph, lane: NodeId): EdgeId[] {
  const found = graph.nodes.find((n) => n.id === lane);
  if (found?.kind !== "instrument" || found.parent === null) return [];
  const inside = new Set<NodeId>([lane, ...graph.nodes.filter((n) => n.parent === lane).map((n) => n.id)]);
  const parentOf = new Map(graph.nodes.map((n) => [n.id, n.parent]));
  return graph.edges.flatMap((e) => (e.kind === "modulation" && parentOf.get(e.from.node) === found.parent && inside.has(e.to.node) ? [e.id] : []));
}
