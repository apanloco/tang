// Where a person sends the audio of a node to, who cannot drag a cable (a keyboard) or would rather choose: the places that take a cable from the output of a node now, as the items of a menu.
import type { Graph, NodeId } from "../protocol";
import type { MenuItem } from "../ui/MenuButton";
import { displayNames } from "./names";
import { cableTakers } from "./wiring";

/** What a place that takes audio is, as the menu heads it, in the order the menu goes in: the effects (a lane's and a bus's), the buses of the groups, the mix. */
const KINDS = [
  { kind: "effect", heading: "Effects" },
  { kind: "group", heading: "Group buses" },
  { kind: "master", heading: "Mix" },
] as const;

/**
 * The places that a cable from the output of `from` can end at now (`cableTakers`: no cable between the two already, and the audio does not go round), each by the id of its node, as
 * text, and the name the canvas gives it, under what it is. The canvas order is kept within each kind. None for a node that has no output of audio.
 */
export function sendItems(graph: Graph, from: NodeId): MenuItem[] {
  const names = displayNames(graph);
  const takers = new Set(cableTakers(graph, from));
  return KINDS.flatMap(({ kind, heading }) =>
    graph.nodes.filter((node) => node.kind === kind && takers.has(node.id)).map((node) => ({ id: String(node.id), label: names.get(node.id) ?? node.name, group: heading })),
  );
}
