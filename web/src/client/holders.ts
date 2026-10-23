// The lanes that the page draws as the effects they hold and no more (step 87, docs/web-ui.md D87): an effect that stands by itself is, to the controller, a lane that has no instrument (the
// intent `AddFreeEffect`), and a person who takes the instrument out of a lane that has effects and nothing else leaves one too. The page has no card for it: the lane is the effects' holder.
import type { Graph, Node, NodeId } from "../protocol";

/**
 * The lanes that are drawn as the effects they hold and no more, each by its node, with its effects in the order of its chain. A holder has no instrument, has effects, and has nothing else of its own
 * that its card would show: no key range, no pattern, no group, and no cable that starts or ends at it (a cable of notes into the lane, an audio cable from its instrument: its card is where such a
 * cable is drawn). Its card is not on the canvas, the effects are drawn as modules that stand by themselves, and what the card said of the lane as a whole (its mute, its solo, its way out, and the "+"
 * that adds a modulator to it) is on the first of them. This is a rule of the picture and no more: the controller has the lane, as it has any, and a person who gives it a key range or an instrument, or
 * a cable, from another front-end, gets the card back with the next graph.
 */
export function holdersOf(graph: Graph): Map<NodeId, readonly Node[]> {
  const effects = new Map<NodeId, Node[]>();
  for (const node of graph.nodes) {
    if (node.kind === "effect" && node.parent !== null) effects.set(node.parent, [...(effects.get(node.parent) ?? []), node]);
  }
  // (The lanes that a cable of notes or an audio cable is at: the "hears" links, which the graph derives for a lane that has no cable of notes, are no cables.)
  const cabled = new Set<NodeId>();
  for (const edge of graph.edges) {
    if (edge.kind === "cable" || edge.kind === "events") {
      cabled.add(edge.from.node);
      cabled.add(edge.to.node);
    }
  }
  const holders = new Map<NodeId, readonly Node[]>();
  for (const node of graph.nodes) {
    if (node.kind !== "instrument" || node.plugin !== null || node.range !== null || node.pattern !== null || node.parent !== null || cabled.has(node.id)) continue;
    const held = effects.get(node.id);
    if (held !== undefined) holders.set(node.id, held);
  }
  return holders;
}
