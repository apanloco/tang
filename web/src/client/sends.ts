// Where a person sends the audio of a node to, who cannot drag a cable (a keyboard) or would rather choose: the places that take a cable from the output of a node now, as the items of a menu.
import type { Graph, NodeId } from "../protocol";
import type { MenuItem } from "../ui/MenuButton";
import { displayNames } from "./names";
import { audioTakers } from "./wiring";

/** What a place that takes audio is, as the menu heads it, in the order the menu goes in: the effects (a lane's and a bus's), the buses of the groups, the mix. */
const KINDS = [
  { kind: "effect", heading: "Effects" },
  { kind: "group", heading: "Group buses" },
  { kind: "master", heading: "Mix" },
] as const;

/** The end of an item of the menu: the node, and whether it is the second input of an effect that has one (`sendTarget`). */
export interface SendTarget {
  node: NodeId;
  side: boolean;
}

/** The id of the item that sends to `target`: the node's id as text, and `<id>:side` for the second input of an effect (an effect that has two inputs has an item for each). */
const itemId = ({ node, side }: SendTarget): string => (side ? `${String(node)}:side` : String(node));

/** The end that the item `id` of `sendItems` stands for. */
export function sendTarget(id: string): SendTarget {
  const side = id.endsWith(":side");
  return { node: Number(side ? id.slice(0, -":side".length) : id), side };
}

/**
 * The places that a cable from the output of `from` can end at now (`audioTakers`: no cable between the two already, and the audio does not go round), each by the id of its node, as
 * text (`sendTarget` reads it back), and the name the canvas gives it, under what it is: an effect that has a second input is named by each of its two (`Math (A)`, `Math (B)`). The canvas
 * order is kept within each kind. None for a node that has no output of audio.
 */
export function sendItems(graph: Graph, from: NodeId): MenuItem[] {
  const names = displayNames(graph);
  const takers = audioTakers(graph, from);
  return KINDS.flatMap(({ kind, heading }) =>
    graph.nodes
      .filter((node) => node.kind === kind)
      .flatMap((node) => {
        const name = names.get(node.id) ?? node.name;
        const inputs = node.kind === "effect" && node.side !== undefined ? [{ side: false, label: `${name} (${node.side.main})` }, { side: true, label: `${name} (${node.side.side})` }] : [{ side: false, label: name }];
        return inputs
          .filter(({ side }) => (side ? takers.side : takers.main).includes(node.id))
          .map(({ side, label }) => ({ id: itemId({ node: node.id, side }), label, group: heading }));
      }),
  );
}
