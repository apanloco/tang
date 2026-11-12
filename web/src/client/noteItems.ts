// Where a person gets notes from and sends them to, who cannot drag a cable (a keyboard) or would rather choose: the sources that can be added, and the places that take a cable of notes now, as the items
// of a menu (docs/patch-graph.md §11.6, P4c).
import type { Graph, Node, NodeId, SourceInfo } from "../protocol";
import type { MenuItem } from "../ui/MenuButton";
import { holdersOf } from "./holders";
import { inputsStoodFor } from "./inputs";
import { displayNames } from "./names";
import { noteTakers } from "./noteWiring";
import type { NotesOutput } from "./noteWiring";

/** The id of the item that adds the keys. */
const KEYS = "keys";
/** What an item that adds a device starts with: the rest of its id is the device's name. */
const DEVICE = "device:";

/** The id of an item of the menu that adds a source: `keys`, or `device:` and the name of an input. */
export type SourceItemId = typeof KEYS | `${typeof DEVICE}${string}`;

/**
 * The sources that can be added to the session: the keys, if it has none, and each of the MIDI inputs that are open (`inputs`, in the order they were opened, once for a name) that no device of the session
 * stands for already (`inputsStoodFor`: a device that is called as the input is, or by part of its name, is heard from it, and a second device for it would put every note in twice).
 */
export function sourceItems(graph: Graph, inputs: readonly string[]): MenuItem<SourceItemId>[] {
  const standing = new Set(graph.nodes.flatMap((node) => (node.kind === "source" && node.source.type === "device" ? inputsStoodFor(node.source.name, inputs) : [])));
  const hasKeys = graph.nodes.some((node) => node.kind === "source" && node.source.type === "keys");
  const keys: MenuItem<SourceItemId>[] = hasKeys ? [] : [{ id: KEYS, label: "Keys" }];
  const devices = [...new Set(inputs)].filter((input) => !standing.has(input)).map((input): MenuItem<SourceItemId> => ({ id: `${DEVICE}${input}`, label: input, group: "MIDI inputs" }));
  return [...keys, ...devices];
}

/** The source that the item `id` of the menu that adds one stands for: the keys, or the device that is called by the rest of the id. */
export function sourceChosen(id: SourceItemId): SourceInfo {
  return id === KEYS ? { type: "keys" } : { type: "device", name: id.slice(DEVICE.length) };
}

/** The nodes `nodes` of `graph` as the items of a menu: by the id of the node, as text, and the name that the canvas gives it. */
function itemsOf(graph: Graph, nodes: readonly Node[]): MenuItem[] {
  const names = displayNames(graph);
  return nodes.map((node) => ({ id: String(node.id), label: names.get(node.id) ?? node.name }));
}

/** What the id of an item that stands for the second output of a note splitter ends with: the rest of the id is the splitter's node. The first output of every node is its node, as text. */
const REST = ":rest";

/** The node and the output of it that an item of the menu that takes notes into a node (`noteFromItems`) stands for: a node as text is its first output, and a splitter's node with `:rest` after it is its second. */
export function noteFromChosen(id: string): { node: NodeId; output: NotesOutput } {
  return id.endsWith(REST) ? { node: Number(id.slice(0, -REST.length)), output: "rest" } : { node: Number(id), output: "pick" };
}

/**
 * The outputs of events that a cable of notes to the lane, the box or the splitter `lane` can still be drawn from (`noteTakers`: a node takes a cable from an output once), in the order of the graph: each source, box or splitter that has none to it
 * from that output, and a splitter once for each of its two outputs, by what it is picking (`Splitter 1 · lowest`) and the rest (`Splitter 1 · rest`). None for a node that takes no notes. A lane that has no cable of notes hears every source by itself;
 * the first it is given is the one it hears from then on.
 */
export function noteFromItems(graph: Graph, lane: NodeId): MenuItem[] {
  const names = displayNames(graph);
  return graph.nodes.flatMap((node): MenuItem[] => {
    const name = names.get(node.id) ?? node.name;
    const outputs: NotesOutput[] = node.kind === "splitter" ? ["pick", "rest"] : ["pick"];
    return outputs
      .filter((output) => noteTakers(graph, node.id, output).includes(lane))
      .map((output): MenuItem => ({ id: output === "rest" ? `${String(node.id)}${REST}` : String(node.id), label: node.kind === "splitter" ? `${name} · ${output === "rest" ? "rest" : node.pick}` : name }));
  });
}

/**
 * The places that a cable of notes from the output `output` of the source, the box or the splitter `source` can still be drawn to (`noteTakers`), in the order of the graph: lanes, and from a source or a box also the splitters, and from a source also
 * the boxes. None for a node that sends no notes. The lanes that only hold effects (`holdersOf`) are left out: the canvas draws no card for them, and a cable of notes would bring the card back for a lane that was never seen.
 */
export function noteToItems(graph: Graph, source: NodeId, output: NotesOutput = "pick"): MenuItem[] {
  const takers = noteTakers(graph, source, output);
  const holders = holdersOf(graph);
  return itemsOf(
    graph,
    graph.nodes.filter((node) => takers.includes(node.id) && !holders.has(node.id)),
  );
}
