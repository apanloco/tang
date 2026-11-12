// The buttons on the nodes that move notes (docs/patch-graph.md §11.6, P4c): a source's, which sends its notes to a lane (a note splitter has one for each of its two outputs), and a lane's, which takes the notes of a source, for a keyboard,
// which cannot drag a cable, and for whoever would rather choose than aim. What takes a cable is worked out when the structure of the session changes (a cable that is drawn, a node that comes or goes), not on every
// value: the canvas hands the structure to them (`StructureContext`), as it does to the menu that sends audio, so that they do not each look for a change of it at every change of a value.
import { useContext, useMemo } from "react";

import { noteFromChosen, noteFromItems, noteToItems } from "../../client/noteItems";
import type { NotesOutput } from "../../client/noteWiring";
import type { NodeId } from "../../protocol";
import { MenuButton } from "../../ui/MenuButton";
import { useEditor } from "../ClientProvider";
import { StructureContext } from "./structure";

/** The `id` of the button that sends the notes of the source `node` (the second output of a note splitter, when `output` is "rest"), which what takes a cable of notes out of it gives the focus to. */
export const notesToId = (node: NodeId, output: NotesOutput = "pick"): string => (output === "rest" ? `notes-to-rest-${String(node)}` : `notes-to-${String(node)}`);

/** The `id` of the button that takes notes into the lane `node`. */
export const notesFromId = (node: NodeId): string => `notes-from-${String(node)}`;

/**
 * The menu that draws a cable of notes from the source `node` (or the pattern box, or an output of a note splitter) to the lane that is chosen. A lane that has a cable from the output already is not listed; with none left the button is shut.
 * A lane that has no cable of notes hears every source by itself, and hears only those it has a cable from once it has one. `glyph` is what the button shows: a box has a menu of each direction and tells them apart by it. `output` is the
 * output of a splitter that it sends from (the first of every other node), and `what` says what it sends in its label: a splitter's two buttons tell the outputs apart by it.
 */
export function NotesToMenu({ node, name, glyph = "♫", output = "pick", what = "notes" }: { node: NodeId; name: string; glyph?: string; output?: NotesOutput; what?: string }) {
  const editor = useEditor();
  const graph = useContext(StructureContext);
  const items = useMemo(() => (graph ? noteToItems(graph, node, output) : []), [graph, node, output]);
  return (
    <MenuButton
      id={notesToId(node, output)}
      label={`Send ${what} from ${name}`}
      className="tang-node__notes"
      items={items}
      onChoose={(lane) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addNotesCable(node, Number(lane), output);
      }}
    >
      {glyph}
    </MenuButton>
  );
}

/** The menu that draws a cable of notes into the lane `node` (or the pattern box, or the note splitter) from the source, box or output of a splitter that is chosen. One that has a cable to the lane already is not listed; with none left the button is shut. */
export function NotesFromMenu({ node, name, glyph = "♫" }: { node: NodeId; name: string; glyph?: string }) {
  const editor = useEditor();
  const graph = useContext(StructureContext);
  const items = useMemo(() => (graph ? noteFromItems(graph, node) : []), [graph, node]);
  return (
    <MenuButton
      id={notesFromId(node)}
      label={`Take notes into ${name}`}
      className="tang-node__notes"
      items={items}
      onChoose={(chosen) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        const from = noteFromChosen(chosen);
        void editor.addNotesCable(from.node, node, from.output);
      }}
    >
      {glyph}
    </MenuButton>
  );
}
