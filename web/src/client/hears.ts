// Whose notes a modulator hears (docs/patch-graph.md §11.6, step 93; every modulator is the session's own since step 96): every note, or the notes that a lane's instrument is given, or those of a group's members. The items of
// the menu on its node, and the words for it.
import type { Graph, NodeId } from "../protocol";
import type { MenuItem } from "../ui/MenuButton";
import { holdersOf } from "./holders";
import { displayNames } from "./names";

/** The id of the item that makes a modulator hear every note. */
export const EVERY_NOTE = "every-note";

/**
 * The lane or the group that the modulator `node` hears the notes of, if it has one that is there: none for every note and for a node that is no modulator. The graph says none for an ear that is gone,
 * as the audio thread hears every note then.
 */
export function hearsOf(graph: Graph, node: NodeId): NodeId | undefined {
  const modulator = graph.nodes.find((n) => n.id === node);
  if (modulator?.kind !== "modulator") return undefined;
  const heard = graph.nodes.find((n) => n.id === modulator.hears);
  return heard?.kind === "instrument" || heard?.kind === "group" ? heard.id : undefined;
}

/**
 * What the modulator `node` can hear, as the items of a menu: every note, then the lanes and the groups of the session by the names that the canvas gives them, the one it hears marked. A lane that only holds
 * effects (`holdersOf`) is left out: the canvas has no card for it, and its notes are no notes that anything plays. Nothing for a node that is not a modulator.
 */
export function earItems(graph: Graph, node: NodeId): MenuItem[] {
  const modulator = graph.nodes.find((n) => n.id === node);
  if (modulator?.kind !== "modulator") return [];
  const heard = hearsOf(graph, node);
  const names = displayNames(graph);
  const holders = holdersOf(graph);
  const item = (id: NodeId, group: string): MenuItem => ({ id: String(id), label: names.get(id) ?? "", group, current: id === heard });
  return [
    { id: EVERY_NOTE, label: "Every note", current: heard === undefined },
    ...graph.nodes.filter((n) => n.kind === "instrument" && !holders.has(n.id)).map((n) => item(n.id, "Lanes")),
    ...graph.nodes.filter((n) => n.kind === "group").map((n) => item(n.id, "Groups")),
  ];
}

/** Whom the modulator `node` hears, in words: "every note", or the name that the canvas gives the lane or the group. */
export function earWords(graph: Graph, node: NodeId): string {
  const heard = hearsOf(graph, node);
  return heard === undefined ? "every note" : (displayNames(graph).get(heard) ?? "every note");
}
