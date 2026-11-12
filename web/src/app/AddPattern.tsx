// The button in the bar of the page that adds a pattern box (docs/patch-graph.md §11.6, P4h-C part C): an empty box, `PATTERN_BOX_DEFAULT_LENGTH` beats long, that notes are recorded into and sent from. There is nothing to
// choose, so the button adds it at once (a session has at most `PATTERN_BOX_LIMIT` of them: the page says so when it is refused); the canvas shows the box that was made.
import { useEditor } from "./ClientProvider";
import { ADD_PATTERN_ID } from "./patch/PatternBox";

export function AddPatternButton() {
  const editor = useEditor();
  return (
    <button
      type="button"
      id={ADD_PATTERN_ID}
      className="tang-menu-button tang-app__add-instrument"
      aria-label="Add a pattern box"
      title="Add a pattern box: a place to record notes into, and to send them to lanes from"
      onClick={() => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addPattern();
      }}
    >
      + Pattern
    </button>
  );
}
