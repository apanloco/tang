// Undo and Redo, in the bar at the top of the page: two buttons that say what they would put back or do again (the words
// of the server's history, `ClientState.history`), and the keys that do it (Control+Z or Command+Z to undo; Control+Shift+Z,
// Command+Shift+Z or Control+Y to redo). A key typed in a field that takes text is that field's own undo and is left to the
// browser, and so is a key when there is nothing to do. One is asked at a time: the history a press finds is the one the
// last answer left.
import { useCallback, useEffect, useRef, useState } from "react";

import { takesText } from "../client/keyboard";
import { useClient, useClientState, useEditor } from "./ClientProvider";
import "./UndoRedo.css";

/** Which of the two a key is the chord of, if it is one: Control or Command with Z; with Shift or Control with Y it is Redo. */
function chordOf(event: KeyboardEvent): "undo" | "redo" | null {
  if (event.altKey) return null;
  const key = event.key.toLowerCase();
  if (key === "z" && (event.ctrlKey || event.metaKey)) return event.shiftKey ? "redo" : "undo";
  // (Command+Y is the browser's own History on a Mac.)
  if (key === "y" && event.ctrlKey && !event.metaKey && !event.shiftKey) return "redo";
  return null;
}

export function UndoRedo() {
  const client = useClient();
  const editor = useEditor();
  const history = useClientState((state) => state.history);
  const [asking, setAsking] = useState(false);
  const busy = useRef(false);

  // Asks for one at a time. (A button that has nothing to do is disabled, and a key finds out before it takes the key from the browser.)
  const ask = useCallback(
    (which: "undo" | "redo"): void => {
      if (busy.current) return;
      busy.current = true;
      setAsking(true);
      void editor[which]().finally(() => {
        busy.current = false;
        setAsking(false);
      });
    },
    [editor],
  );

  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      const which = chordOf(event);
      if (which === null || takesText(event.target) || client.state.history[which] === null) return;
      // (The browser's own would undo the last thing typed in a field, or nothing.)
      event.preventDefault();
      ask(which);
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
    };
  }, [client, ask]);

  return (
    <span className="tang-undo-redo">
      <button
        type="button"
        className="tang-undo-redo__button"
        disabled={asking || history.undo === null}
        aria-label={history.undo === null ? "Undo" : `Undo ${history.undo}`}
        title={history.undo === null ? "Nothing to undo" : `Undo ${history.undo} (Control+Z, or Command+Z)`}
        onClick={() => {
          ask("undo");
        }}
      >
        Undo
      </button>
      <button
        type="button"
        className="tang-undo-redo__button"
        disabled={asking || history.redo === null}
        aria-label={history.redo === null ? "Redo" : `Redo ${history.redo}`}
        title={history.redo === null ? "Nothing to redo" : `Redo ${history.redo} (Control+Shift+Z, Command+Shift+Z, or Control+Y)`}
        onClick={() => {
          ask("redo");
        }}
      >
        Redo
      </button>
    </span>
  );
}
