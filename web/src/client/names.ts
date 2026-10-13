// What the nodes of a session are called, wherever a person reads it: on the canvas, in what a screen
// reader says of a control, in what the page says of a cable or of what it could not do.
import type { Graph, NodeId } from "../protocol";

/**
 * What each node is called on the canvas. The server names a node by what it is (a plugin's name, a
 * modulator's kind: its settings are not in the name, they change under a client), so two LFOs, or two
 * filters, have the same name, and a person, or a screen reader, cannot tell which is which, nor which
 * `Rate` is which. A name that more than one node has is numbered, from 1, in the order of the graph;
 * a node that is alone with its name keeps it, and a number that would make a name that such a node has
 * (two LFOs, and a plugin called "LFO 1") is skipped, so that no two nodes are called the same.
 */
export function displayNames(graph: Graph): Map<NodeId, string> {
  const shared = new Map<string, number>();
  for (const node of graph.nodes) shared.set(node.name, (shared.get(node.name) ?? 0) + 1);
  const kept = new Set(graph.nodes.filter((node) => shared.get(node.name) === 1).map((node) => node.name));
  const last = new Map<string, number>();
  return new Map(
    graph.nodes.map((node) => {
      if ((shared.get(node.name) ?? 0) < 2) return [node.id, node.name];
      let number = last.get(node.name) ?? 0;
      let name: string;
      do {
        number += 1;
        name = `${node.name} ${String(number)}`;
      } while (kept.has(name));
      last.set(node.name, number);
      return [node.id, name];
    }),
  );
}

/** What a cable joins, in words: `LFO → Filter: cutoff` (the parameter is left out when it is not one). */
export const cableLabel = (from: string, to: string, param?: string | null): string => `${from} → ${to}${param ? `: ${param}` : ""}`;
