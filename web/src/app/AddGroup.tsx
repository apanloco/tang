// The button in the bar of the page that adds a group, a submix bus: an empty one at the end of the session, with its volume at unity, its pan in the
// middle and no effects. Lanes are put in it from their own choice of group; what it is called, and where it is, is the canvas's business.
import { useEditor } from "./ClientProvider";

export function AddGroupButton() {
  const editor = useEditor();
  return (
    <button
      type="button"
      id="add-group"
      className="tang-menu-button tang-app__add-instrument"
      aria-label="Add a group"
      title="Add a group: a submix that lanes are put in, with effects of its own"
      onClick={() => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addGroup();
      }}
    >
      + Group
    </button>
  );
}
