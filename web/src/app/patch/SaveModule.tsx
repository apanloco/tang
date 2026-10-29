// "Save as a module" (steps 104 and 105): the button that saves plugins as a module of the library: all of what is asked for, as it is (the plugins in order, each with its preset, its values, its mix or its volume, its pinned
// knobs and whether the stage before it feeds it, and the cables between them), to be added to this session or to another from the bar's "+ Effect". There are two of them: the one in the cell of a lane, beside its mute and its solo,
// which saves the chain of effects of the lane (step 104), and the one on the canvas's bar of choosing, which saves the nodes that were chosen (step 105). Either opens a small field for the name under the button; Enter or Save asks
// the server (`Editor.saveModule`, `saveSelectionAsModule`: the server says what a name may be, a module that has the name is written over), Escape and Cancel leave. A name that the server refuses is said among the notices and the
// field stays, with the name selected, to be tried again; when it is saved the notices say so. The field is drawn in the page and not in the node (the canvas moves and the node is in front of the next), so it closes when the page is
// pressed anywhere else, scrolled or sized.
import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { createPortal } from "react-dom";

import type { NodeId } from "../../protocol";
import { useEditor } from "../ClientProvider";
import "./SaveModule.css";

/** About how wide the field is, to keep it in the window when the button is near its right edge. */
const PANEL_WIDTH = 300;

interface ModuleNameButtonProps {
  /** What the button is called, and the field that it opens (a dialog of that name). */
  label: string;
  /** The class of the button. */
  className: string;
  /** What is on the button. */
  children: ReactNode;
  /** Whether it can be pressed: nothing is asked of the server for a button that cannot. */
  disabled?: boolean;
  /** Asks the server to save under the name as it was typed (the page says why if it was refused); true if it was saved. */
  save: (name: string) => Promise<boolean>;
}

/** The button and the field for the name that opens under it, whatever it is that is saved. */
function ModuleNameButton({ label, className, children, disabled = false, save }: ModuleNameButtonProps) {
  const [place, setPlace] = useState<{ left: number; top: number } | null>(null);
  const [text, setText] = useState("");
  const [asking, setAsking] = useState(false);
  const busy = useRef(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLFormElement>(null);
  const field = useRef<HTMLInputElement>(null);
  const open = place !== null;

  const close = (refocus: boolean): void => {
    setPlace(null);
    if (refocus) trigger.current?.focus();
  };

  // The field has the focus when it opens (before the page is painted, so that a letter that is typed is never the page's: its keys play notes).
  useLayoutEffect(() => {
    if (open) field.current?.focus();
  }, [open]);

  // A press anywhere else, the page being scrolled or sized: the field goes (and the focus stays where the person put it).
  useEffect(() => {
    if (!open) return;
    const away = (event: Event): void => {
      const target = event.target;
      if (target instanceof Node && (panel.current?.contains(target) || trigger.current?.contains(target))) return;
      setPlace(null);
    };
    const leave = (): void => {
      setPlace(null);
    };
    document.addEventListener("pointerdown", away, true);
    window.addEventListener("wheel", leave, true);
    window.addEventListener("resize", leave);
    return () => {
      document.removeEventListener("pointerdown", away, true);
      window.removeEventListener("wheel", leave, true);
      window.removeEventListener("resize", leave);
    };
  }, [open]);

  const show = (): void => {
    const box = trigger.current?.getBoundingClientRect();
    if (!box) return;
    setText("");
    setPlace({ left: Math.max(8, Math.min(box.left, window.innerWidth - PANEL_WIDTH - 8)), top: box.bottom + 4 });
  };

  // Asks for one at a time, and for a name that is something. (The server decides what a name may be; the page does not have its rule.)
  const ask = (): void => {
    if (busy.current || text.trim() === "") return;
    busy.current = true;
    setAsking(true);
    // (If it is refused the page says so: `Editor` puts the reason among the notices.)
    void save(text).then((done) => {
      busy.current = false;
      setAsking(false);
      if (done) {
        close(true);
      } else {
        field.current?.focus();
        field.current?.select();
      }
    });
  };

  return (
    <>
      <button
        ref={trigger}
        type="button"
        className={className}
        disabled={disabled}
        aria-label={label}
        aria-haspopup="dialog"
        aria-expanded={open}
        title={`${label}…`}
        onClick={() => {
          if (open) close(false);
          else show();
        }}
      >
        {children}
      </button>
      {place
        ? createPortal(
            <form
              ref={panel}
              role="dialog"
              aria-label={label}
              className="tang-module-name"
              style={{ position: "fixed", left: `${String(place.left)}px`, top: `${String(place.top)}px`, width: `${String(PANEL_WIDTH)}px` }}
              onSubmit={(event) => {
                event.preventDefault();
                ask();
              }}
              onKeyDown={(event) => {
                if (event.key !== "Escape") return;
                // (Not the page's: Escape closes what is open there.)
                event.preventDefault();
                event.stopPropagation();
                close(true);
              }}
            >
              <input
                ref={field}
                className="tang-module-name__field"
                type="text"
                aria-label="Module name"
                placeholder="Module name"
                title="The name of the module: a file name, to which .toml is added; a module that has the name is written over"
                spellCheck={false}
                autoComplete="off"
                value={text}
                onChange={(event) => {
                  setText(event.target.value);
                }}
              />
              <button type="submit" className="tang-module-name__button" disabled={asking || text.trim() === ""}>
                Save
              </button>
              <button
                type="button"
                className="tang-module-name__button"
                onClick={() => {
                  close(true);
                }}
              >
                Cancel
              </button>
            </form>,
            document.body,
          )
        : null}
    </>
  );
}

/** The button in the cell of a lane that saves the chain of effects of the lane (`node` is the lane: the server finds the chain), whose name the nodes call it by is `name`. */
export const SaveModuleButton = memo(function SaveModuleButton({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const save = useCallback((text: string) => editor.saveModule(node, text), [editor, node]);
  return (
    <ModuleNameButton label={`Save the chain of ${name} as a module`} className="tang-flag tang-flag--module nodrag nopan" save={save}>
      ⤓
    </ModuleNameButton>
  );
});

/** The button on the canvas's bar of choosing that saves the nodes that were chosen (`nodes`: the plugins that a module can hold, which `modulePluginsOf` picks out of what is chosen). It cannot be pressed with none. */
export function SaveSelectionButton({ nodes }: { nodes: readonly NodeId[] }) {
  const editor = useEditor();
  const save = useCallback((text: string) => editor.saveSelectionAsModule(nodes, text), [editor, nodes]);
  return (
    <ModuleNameButton label="Save the chosen nodes as a module" className="tang-patch__arrange" disabled={nodes.length === 0} save={save}>
      Save as module…
    </ModuleNameButton>
  );
}
