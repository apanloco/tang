// What the nodes of a session are called, wherever a person reads it: on the canvas, in what a screen
// reader says of a control, in what the page says of a cable or of what it could not do.
import type { EdgeId, Endpoint, Graph, NodeId, Port } from "../protocol";

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

/**
 * What a cable ends at, in words: the node and the port of it (`Filter: cutoff`, `LFO: rate`), or, for the depth of a cable, that cable's
 * words (`depth of LFO → Filter: cutoff`: a modulator can have many cables, and the depth of one is told from the depth of another by where
 * the cable goes). A cable that is being read (`reading`: the ids of the cables whose words are being told) is not read again, since cables can
 * drive each other's depths: its depth is called after the node it is the port of, as the depth of a cable that is not there is. `names` is
 * what each node is called (`displayNames`).
 */
export function endLabel(graph: Graph, names: ReadonlyMap<NodeId, string>, to: Endpoint, reading: ReadonlySet<EdgeId> = new Set()): string {
  if (to.port.type === "depth") {
    const { edge } = to.port;
    const cable = reading.has(edge) ? undefined : graph.edges.find((found) => found.kind === "modulation" && found.id === edge);
    if (cable) return `depth of ${cableWords(graph, names, cable, reading)}`;
  }
  const name = names.get(to.node) ?? "?";
  const port = portName(graph, to.node, to.port);
  return port ? `${name}: ${port}` : name;
}

/**
 * A cable, in words: `LFO → Filter: cutoff`, `ADSR → depth of LFO → Filter: cutoff` (`endLabel` says how its end is told). A cable
 * that is not made yet has no id, and is the same.
 */
export function cableWords(graph: Graph, names: ReadonlyMap<NodeId, string>, cable: { from: Endpoint; to: Endpoint; id?: EdgeId }, reading: ReadonlySet<EdgeId> = new Set()): string {
  const now = cable.id === undefined ? reading : new Set([...reading, cable.id]);
  return `${names.get(cable.from.node) ?? "?"} → ${endLabel(graph, names, cable.to, now)}`;
}

/**
 * What the port of a node that a cable ends at is called, for the cable's words: a parameter is what its plugin calls it (by the
 * plugin's own index), a setting of a modulator is `rate`, `attack`, `decay`, `sustain` or `release`, the depth of a cable is
 * `depth`. A parameter the node has not, and the ports that no cable ends at (an output, an audio port), have no name.
 */
export function portName(graph: Graph, node: NodeId, port: Port): string | null {
  switch (port.type) {
    case "param": {
      const found = graph.nodes.find((n) => n.id === node);
      const plugin = found?.kind === "instrument" || found?.kind === "effect" ? found.plugin : null;
      return plugin?.params.find((param) => param.index === port.index)?.name ?? null;
    }
    case "rate":
    case "attack":
    case "decay":
    case "sustain":
    case "release":
    case "depth":
      return port.type;
    case "modOut":
    case "audioIn":
    case "audioOut":
      return null;
  }
}
