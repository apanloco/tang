// What the nodes of a session are called, wherever a person reads it: on the canvas, in what a screen
// reader says of a control, in what the page says of a cable or of what it could not do.
import type { Graph, NodeId } from "../protocol";

/**
 * What each node is called on the canvas. The server names a node by what it is (a plugin's name, a
 * modulator's kind: its settings are not in the name, they change under a client), so two LFOs, or two
 * filters, have the same name, and a person, or a screen reader, cannot tell which is which, nor which
 * `Rate` is which. A name that more than one node has is numbered, from 1, in the order of the graph;
 * a node that is alone with its name keeps it.
 */
export function displayNames(graph: Graph): Map<NodeId, string> {
  const shared = new Map<string, number>();
  for (const node of graph.nodes) shared.set(node.name, (shared.get(node.name) ?? 0) + 1);
  const next = new Map<string, number>();
  return new Map(
    graph.nodes.map((node) => {
      if ((shared.get(node.name) ?? 0) < 2) return [node.id, node.name];
      const number = (next.get(node.name) ?? 0) + 1;
      next.set(node.name, number);
      return [node.id, `${node.name} ${String(number)}`];
    }),
  );
}
