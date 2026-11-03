// The button on a modulator that is in no lane and no group, which chooses whose notes it hears (docs/patch-graph.md §11.6, step 93): every note, or the notes that a lane's instrument is given (after its
// range, its pattern and its transpose), or those of a group's members. Like the menus that move notes (`NotesMenus`), it is on the node for a keyboard and for whoever would rather choose than aim.
import { useContext, useMemo } from "react";

import { EVERY_NOTE, earItems, earWords } from "../../client/hears";
import type { NodeId } from "../../protocol";
import { MenuButton } from "../../ui/MenuButton";
import { useEditor } from "../ClientProvider";
import { StructureContext } from "./structure";

/** The `id` of the button that chooses whose notes the modulator `node` hears. */
export const hearsId = (node: NodeId): string => `hears-${String(node)}`;

/**
 * The menu that makes the modulator `node` hear a lane, a group or every note. The button is lit while it hears a lane or a group, and says whom in its tooltip. It is nothing for a modulator that is in a lane or a
 * group (it hears its own), which has no items.
 */
export function HearsMenu({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const graph = useContext(StructureContext);
  const items = useMemo(() => (graph ? earItems(graph, node) : []), [graph, node]);
  if (items.length === 0) return null;
  const set = !items[0]?.current;
  return (
    <MenuButton
      id={hearsId(node)}
      label={`Whose notes ${name} hears: ${graph ? earWords(graph, node) : "every note"}`}
      className={`tang-node__notes${set ? " tang-node__notes--set" : ""}`}
      items={items}
      onChoose={(id) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.setModulatorEar(node, id === EVERY_NOTE ? null : Number(id));
      }}
    >
      ♫
    </MenuButton>
  );
}
