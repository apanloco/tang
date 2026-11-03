// "Save as…", in the bar of the session: a field for the name of a file, with the name of the file that the session has to begin with, which asks the server
// to save the session as it (`Editor.saveAs`: the server says what a name may be, puts the file beside the one the session has and does not write over another that
// is there), and the keys that open it: Control+Shift+S, or Command+Shift+S, as in the terminal. Enter asks; Escape and Cancel leave. A name that the server refuses
// is said among the notices and the field stays, with the name selected, to be tried again.
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { useClient, useEditor } from "./ClientProvider";

/** Whether `event` is the chord of Save as: Control or Command with Shift and S, and not Alt. (Without Shift it is the chord of Save.) */
const isSaveAsChord = (event: KeyboardEvent): boolean => event.key.toLowerCase() === "s" && (event.ctrlKey || event.metaKey) && !event.altKey && event.shiftKey;

export function SaveAs() {
  const client = useClient();
  const editor = useEditor();
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const [asking, setAsking] = useState(false);
  const busy = useRef(false);
  const field = useRef<HTMLInputElement>(null);
  const opener = useRef<HTMLButtonElement>(null);
  // Whether the focus goes to the button when the field has gone: it does when the person left the field or it was answered, as they are still there.
  const focusOpener = useRef(false);

  const focusField = (): void => {
    field.current?.focus();
    field.current?.select();
  };

  // Opens the field with the name of the file the session has now (read when it is asked for, not when it was drawn), or finds the one that is open.
  const show = useCallback((): void => {
    if (field.current) {
      field.current.focus();
      field.current.select();
      return;
    }
    setText(client.state.snapshot?.meta.file ?? "");
    setOpen(true);
  }, [client]);

  const close = (): void => {
    focusOpener.current = true;
    setOpen(false);
  };

  useEffect(() => {
    const onKey = (event: KeyboardEvent): void => {
      if (!isSaveAsChord(event)) return;
      // (The browser's own would write the page to a file.)
      event.preventDefault();
      show();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
    };
  }, [show]);

  // (Before the page is painted or anything else runs, so that the focus is never on the page between the field going and the button having it.)
  useLayoutEffect(() => {
    if (open) {
      field.current?.focus();
      field.current?.select();
    } else if (focusOpener.current) {
      focusOpener.current = false;
      opener.current?.focus();
    }
  }, [open]);

  // Asks for one at a time, and for a name that is something. (The server decides what a name may be; the page does not have its rule.)
  const ask = (): void => {
    if (busy.current || text.trim() === "") return;
    busy.current = true;
    setAsking(true);
    // (If it is refused the page says so: `Editor` puts the reason among the notices.)
    void editor.saveAs(text).then((done) => {
      busy.current = false;
      setAsking(false);
      if (done) close();
      else focusField();
    });
  };

  if (!open) {
    return (
      <button type="button" ref={opener} className="tang-session-bar__button" title="Save the session under another name (Control+Shift+S, or Command+Shift+S)" onClick={show}>
        Save as…
      </button>
    );
  }
  return (
    <span className="tang-save-as" role="group" aria-label="Save the session as">
      <input
        ref={field}
        className="tang-save-as__field"
        type="text"
        aria-label="File name"
        title="The name of the file, which goes beside the one the session has (.toml is added)"
        spellCheck={false}
        autoComplete="off"
        value={text}
        onChange={(event) => {
          setText(event.target.value);
        }}
        onKeyDown={(event) => {
          // (An Enter that confirms what an input method is composing is not for the field.)
          if (event.nativeEvent.isComposing) return;
          if (event.key === "Enter") {
            event.preventDefault();
            ask();
          } else if (event.key === "Escape") {
            event.preventDefault();
            close();
          }
        }}
      />
      <button type="button" className="tang-session-bar__button" disabled={asking || text.trim() === ""} onClick={ask}>
        OK
      </button>
      <button type="button" className="tang-session-bar__button" onClick={close}>
        Cancel
      </button>
    </span>
  );
}
