// Note splitters as the mock engine and the page read them (docs/patch-graph.md §11.6, P4i-C): a node that takes notes in and sends the lowest (or the highest) of the notes that are held to its first output and the others to its
// second. What a splitter is when it is new and what it is called (`newSplitter`, `nextSplitterName`, as `NoteSplitter::new` and `SessionModel::next_splitter_name`), the name that a person gives it (`splitterTitle`, as
// `splitter_title`), and the words of the steps that its settings make in the history (`splitterFieldBefore`, as `splitter_field_before`). What a splitter does with the keys that are pressed is `splitterRoute`'s, in
// `splitterPlayer.ts` (the engine's `notesplit::Splitter`).
import type { Node, NodeId, SplitterField } from "../protocol";
import { tidyName } from "./modules";

/** A note splitter, as the graph has it. */
export type SplitterNode = Extract<Node, { kind: "splitter" }>;

/** What a splitter is called when it has no name of its own (`DEFAULT_SPLITTER_NAME`): what a name of nothing comes to. */
const DEFAULT_NAME = "Splitter";

/** What a splitter is called, from the text that was given: the text without the spaces round it, by a group's rule, and `Splitter` for a name of nothing (`splitter_title`); else what is wrong with it, in the controller's words. */
export function splitterTitle(name: string): { name: string } | { error: string } {
  const tidied = tidyName(name);
  if ("error" in tidied) return tidied;
  return { name: tidied.name === "" ? DEFAULT_NAME : tidied.name };
}

/** What a splitter that is added is called: `Splitter N` for the lowest N, from 1, that no splitter is called (`SessionModel::next_splitter_name`). */
export function nextSplitterName(nodes: readonly Node[]): string {
  const taken = new Set(nodes.flatMap((n) => (n.kind === "splitter" ? [n.name] : [])));
  for (let n = 1; ; n += 1) {
    if (!taken.has(`${DEFAULT_NAME} ${String(n)}`)) return `${DEFAULT_NAME} ${String(n)}`;
  }
}

/** A splitter that is new, as `AddSplitter` makes it: it picks the lowest note (`NoteSplitter::new`). */
export function newSplitter(id: NodeId, name: string): SplitterNode {
  return { id, parent: null, name, kind: "splitter", pick: "lowest" };
}

/** What a setting of a splitter is about to be set to: the words for the step, the setting as the splitter has it now (what undo sets) and whether the splitter has what is asked for already (no step is kept for it), as `splitter_field_before` says. */
export function splitterFieldBefore(splitter: SplitterNode, field: SplitterField): { label: string; undo: SplitterField; same: boolean } {
  switch (field.type) {
    case "name": {
      const title = splitterTitle(field.name);
      return { label: `rename ${splitter.name}`, undo: { type: "name", name: splitter.name }, same: "name" in title && title.name === splitter.name };
    }
    case "pick":
      return { label: `let ${splitter.name} pick ${field.pick === "highest" ? "the highest note" : "the lowest note"}`, undo: { type: "pick", pick: splitter.pick }, same: field.pick === splitter.pick };
  }
}
