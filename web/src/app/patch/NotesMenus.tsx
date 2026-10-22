// The buttons on the nodes that move notes (docs/patch-graph.md §11.6, P4c): a source's, which sends its notes to a lane, and a lane's, which takes the notes of a source, for a keyboard, which cannot drag a
// cable, and for whoever would rather choose than aim. What takes a cable is worked out when the structure of the session changes (a cable that is drawn, a node that comes or goes), not on every
// value: the canvas hands the structure to them (`StructureContext`), as it does to the menu that sends audio, so that they do not each look for a change of it at every change of a value.
import { useContext, useMemo } from "react";

import { noteFromItems, noteToItems } from "../../client/noteItems";
import type { NodeId } from "../../protocol";
import { MenuButton } from "../../ui/MenuButton";
import { useEditor } from "../ClientProvider";
import { StructureContext } from "./structure";

/** The `id` of the button that sends the notes of the source `node`, which what takes a cable of notes out of it gives the focus to. */
export const notesToId = (node: NodeId): string => `notes-to-${String(node)}`;

/** The `id` of the button that takes notes into the lane `node`. */
export const notesFromId = (node: NodeId): string => `notes-from-${String(node)}`;

/**
 * The menu that draws a cable of notes from the source `node` to the lane that is chosen. A lane that has a cable from the source already is not listed; with none left the button is shut. A lane that has
 * no cable of notes hears every source by itself, and hears only those it has a cable from once it has one.
 */
export function NotesToMenu({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const graph = useContext(StructureContext);
  const items = useMemo(() => (graph ? noteToItems(graph, node) : []), [graph, node]);
  return (
    <MenuButton
      id={notesToId(node)}
      label={`Send notes from ${name}`}
      className="tang-node__notes"
      items={items}
      onChoose={(lane) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addNotesCable(node, Number(lane));
      }}
    >
      ♫
    </MenuButton>
  );
}

/** The menu that draws a cable of notes into the lane `node` from the source that is chosen. A source that has a cable to the lane already is not listed; with none left the button is shut. */
export function NotesFromMenu({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const graph = useContext(StructureContext);
  const items = useMemo(() => (graph ? noteFromItems(graph, node) : []), [graph, node]);
  return (
    <MenuButton
      id={notesFromId(node)}
      label={`Take notes into ${name}`}
      className="tang-node__notes"
      items={items}
      onChoose={(source) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addNotesCable(Number(source), node);
      }}
    >
      ♫
    </MenuButton>
  );
}
