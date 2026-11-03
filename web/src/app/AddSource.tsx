// The button in the bar of the page that adds a source of notes (docs/patch-graph.md §11.6, P4c): a menu of the keys, if the session has none, and of the MIDI inputs that are open and that no device of the
// session stands for already. A source is a node of the canvas at the end of the sources; every lane that no cable of notes ends at hears it, and the canvas shows it by a link. What is offered follows the
// session and the inputs the server says are open (`ClientState.inputs`); with nothing to add the button is shut.
import { sourceChosen, sourceItems } from "../client/noteItems";
import type { SourceItemId } from "../client/noteItems";
import { MenuButton } from "../ui/MenuButton";
import type { MenuItem } from "../ui/MenuButton";
import { useClientSelection, useEditor } from "./ClientProvider";

/** Whether two lists of items are the same: what the menu draws, and not the objects that were made of it (an item's id is the keys, or the name of the input, which is its label: the ids tell the lists apart). */
const sameItems = (a: readonly MenuItem<SourceItemId>[], b: readonly MenuItem<SourceItemId>[]): boolean => a.length === b.length && a.every((item, i) => item.id === b[i]?.id);

export function AddSourceMenu() {
  const editor = useEditor();
  // (Looked up at every change of the client's state, which is cheap, and drawn again only when the list is another: a knob that is turned changes nothing here.)
  const items = useClientSelection((state) => (state.snapshot ? sourceItems(state.snapshot.graph, state.inputs) : []), sameItems);
  return (
    <MenuButton
      id="add-source"
      label="Add a source of notes"
      className="tang-app__add-instrument"
      items={items}
      onChoose={(id) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addSource(sourceChosen(id));
      }}
    >
      + Source
    </MenuButton>
  );
}
