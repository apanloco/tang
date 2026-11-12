// The notes of a session as the page's engine and the canvas read them from the graph (docs/patch-graph.md §11.6, P4c and P4h): which two places a cable of notes may join (`eventCableEnds`, as
// `SessionModel::event_cable_ends`: from a source or a pattern box to a lane, and from a source to a box), which places can still be given a cable from a source or a box (`noteTakers`), the links that the graph derives
// for the lanes and the boxes that no cable of notes ends at (`withHears`, as `SessionModel::hears_edges`), and the rule for a device's name (`deviceName`, as `session::device_name`).
import { DEVICE_NAME_LIMIT } from "../protocol";
import type { Edge, Endpoint, Graph, Node, NodeId, WireError } from "../protocol";
import { isHears, isLink, isNotesCable } from "./cables";
import { trimmed } from "./inputs";

/** The master's node id, as the server numbers it (`NodeId::MASTER`). */
const MASTER_ID: NodeId = 4294967295;

/** The two ends of a cable of notes: the node that it starts at, a source or a pattern box (`source`), and the node that it ends at, a lane or a box (`lane`). */
export interface NotesEnds {
  source: NodeId;
  lane: NodeId;
}

const invalid = (message: string): { error: WireError } => ({ error: { code: "invalid", message } });

/**
 * Whether the node is a lane that takes notes (`SessionModel::lane_takes_notes`, step 99c): its instrument says so (`Plugin::takes_notes`: the wire leaves the word out for a plugin that does, and says `false`
 * for an audio input), and a lane that has no instrument, which may be given one that plays notes, does. A node that is no lane does not. A lane that takes none has no key range, no pattern and no input of events on
 * its card, hears no source and is given no cable of notes.
 */
export function laneTakesNotes(node: Node): boolean {
  return node.kind === "instrument" && node.plugin?.takesNotes !== false;
}

const notFound = (node: NodeId): { error: WireError } => ({ error: { code: "notFound", message: `there is no node ${String(node)}` } });

/**
 * The ends that a cable from the port `from` to the port `to` has, if it is a cable of notes: the source or the pattern box it starts at (`eventsOut`) and the lane or the box it ends at (`eventsIn`; a lane's instrument is the
 * lane's own node); a box takes the notes of a source and no box's: a box sends its own to a lane. Or why it has none, in the controller's words and its order: a port that is not that, the mix, a node that is not there
 * (not found), a node that is not a source or a box to start at, a box to end at that starts at a box, a node that is not a lane or a box to end at.
 */
export function eventCableEnds(graph: Graph, from: Endpoint, to: Endpoint): NotesEnds | { error: WireError } {
  if (from.port.type !== "eventsOut") return invalid("a cable of notes starts at an output of events");
  if (to.port.type !== "eventsIn") return invalid("a cable of notes ends at an input of events");
  if (from.node === MASTER_ID) return invalid("the mix has no output of events");
  const start = graph.nodes.find((n) => n.id === from.node);
  if (!start) return notFound(from.node);
  if (start.kind !== "source" && start.kind !== "pattern") return invalid("only a source of notes has an output of events");
  if (to.node === MASTER_ID) return invalid("the mix has no input of events");
  const end = graph.nodes.find((n) => n.id === to.node);
  if (!end) return notFound(to.node);
  if (end.kind === "pattern") return start.kind === "pattern" ? invalid("a pattern box cannot send its notes to another box yet: it sends them to a lane") : { source: start.id, lane: end.id };
  if (end.kind !== "instrument") return invalid("a cable of notes ends at a lane: it is the lanes that have an input of events");
  if (!laneTakesNotes(end)) return invalid("that instrument takes no notes: an audio input has no input of events");
  return { source: start.id, lane: end.id };
}

/**
 * The places that a cable of notes from the source or the pattern box `from` can still be drawn to: every lane that takes notes and has none from it already (a lane takes a cable from a source or a box once), and, from a
 * source, every box that has none from it already (a box takes a cable from a source once, and from no box). None for a node that is no source and no box. The controller's rule, `event_cable_ends` and the check for a cable that is there
 * already, read from the graph, so that the canvas can show the ends that take the cable before it is dropped.
 */
export function noteTakers(graph: Graph, from: NodeId): NodeId[] {
  const start = graph.nodes.find((n) => n.id === from);
  if (start?.kind !== "source" && start?.kind !== "pattern") return [];
  const takes = (n: Node): boolean => laneTakesNotes(n) || (n.kind === "pattern" && start.kind === "source");
  return graph.nodes.filter((n) => takes(n) && !graph.edges.some((e) => isNotesCable(e) && e.from.node === from && e.to.node === n.id)).map((n) => n.id);
}

/**
 * The graph with the links that the controller derives from each source to each lane that takes notes and each pattern box, that no cable of notes ends at (a lane or a box that has none hears every source): the sources in
 * order and for each the lanes in order and then the boxes in order, after the links of the audio path and before the cables that have been drawn (`SessionModel::graph`). Those that the graph had are left out first: a link is a
 * fact of the cables. (A lane hears no box until a cable from it says so: there is no link from a box.)
 */
export function withHears(graph: Graph): Graph {
  const kept = graph.edges.filter((e) => !isHears(e));
  const fed = new Set(kept.filter(isNotesCable).map((e) => e.to.node));
  const hearers = [...graph.nodes.filter((n) => laneTakesNotes(n) && !fed.has(n.id)), ...graph.nodes.filter((n) => n.kind === "pattern" && !fed.has(n.id))];
  const links = graph.nodes.flatMap((source): Edge[] =>
    source.kind === "source" ? hearers.map((to) => ({ from: { node: source.id, port: { type: "eventsOut" } }, to: { node: to.id, port: { type: "eventsIn" } }, kind: "hears" })) : [],
  );
  const at = kept.findIndex((e) => !isLink(e));
  const place = at < 0 ? kept.length : at;
  return { nodes: graph.nodes, edges: [...kept.slice(0, place), ...links, ...kept.slice(place)] };
}

/**
 * The name that a device is called by, from the text that was given: the text without the spaces round it, which has to be something, at most `DEVICE_NAME_LIMIT` characters (the characters that a
 * person sees: a pair of units that a string counts as two is one) and with no control character in it; else what is wrong, as the end of a sentence about the name (`session::device_name`). The case is
 * the name's: two names that differ by it are two devices.
 */
export function deviceName(text: string): { name: string } | { why: string } {
  const name = trimmed(text);
  if (name === "") return { why: "is blank: a device is called by its name, or as much of it as tells it" };
  if (Array.from(name).length > DEVICE_NAME_LIMIT) return { why: `is longer than ${String(DEVICE_NAME_LIMIT)} characters` };
  if (/\p{Cc}/u.test(name)) return { why: "has a control character in it" };
  return { name };
}
