// What a module can hold of the nodes that are chosen on the canvas (steps 105 and 106): the plugins, which are the instruments of the lanes that have one and the effects that are in a lane, and the modulators that go with them.
// The server takes nothing else (`module::to_text_of`: a group's bus, a source of notes, an empty lane and the master are none of a module's), so that what is chosen with them in the box is left out here and the
// page says how many, and a module can still be made of the rest.
import type { Graph, NodeId } from "../../protocol";

/** What a module holds of what is chosen. */
export interface Held {
  /** The nodes that a module can hold, in the order they were chosen in. */
  readonly nodes: readonly NodeId[];
  /** How many of them are plugins (an instrument or an effect): a module needs one at least, which the modulators that are chosen go with. */
  readonly plugins: number;
  /** How many of the chosen it cannot hold. */
  readonly left: number;
}

/** What a module holds of the nodes in `chosen`: see the top of this file. A node that the session does not have is one that cannot be held. */
export function modulePluginsOf(graph: Graph | null, chosen: readonly NodeId[]): Held {
  const nodes: NodeId[] = [];
  let plugins = 0;
  for (const id of chosen) {
    const node = graph?.nodes.find((candidate) => candidate.id === id);
    const holdsAPlugin =
      node?.kind === "instrument"
        ? node.plugin !== null
        : node?.kind === "effect" && graph?.nodes.some((owner) => owner.id === node.parent && owner.kind === "instrument") === true;
    if (holdsAPlugin || node?.kind === "modulator") nodes.push(id);
    if (holdsAPlugin) plugins += 1;
  }
  return { nodes, plugins, left: chosen.length - nodes.length };
}
