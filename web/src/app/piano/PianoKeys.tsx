// The keys of the piano: the white and the black ones of as many octaves as the width has room for, each coloured by what it is
// in the scale; lit while the engine holds its note (written to the DOM on the frame: a note is not a reason to render again);
// played by whatever pointer is down on them.
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, PointerEvent, RefObject } from "react";

import { firstBlock, isWhite, keyboardLayout, keyLabel, octavesFor, roleOf } from "../../client/pianoLayout";
import { noteName } from "../../client/notes";
import { PointerPiano } from "../../client/pointerPiano";
import type { PianoMeta } from "../../protocol";
import { useClient, useFrames, useNotes } from "../ClientProvider";

/** How wide the element is, as far as it has been measured (0 until it is). */
function useWidth(element: RefObject<HTMLElement | null>): number {
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const target = element.current;
    if (!target) return;
    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry) setWidth(entry.contentRect.width);
    });
    observer.observe(target);
    return () => {
      observer.disconnect();
    };
  }, [element]);
  return width;
}

/** The note of the key under a point of the page, or none if what is there is not a key. */
function noteAt(x: number, y: number): number | null {
  return noteOfElement(document.elementFromPoint(x, y));
}

function noteOfElement(element: EventTarget | null): number | null {
  const key = element instanceof Element ? element.closest<HTMLElement>("[data-note]") : null;
  return key?.dataset.note === undefined ? null : Number(key.dataset.note);
}

export function PianoKeys({ piano, octave }: { piano: PianoMeta; octave: number }) {
  const client = useClient();
  const frames = useFrames();
  const notes = useNotes();
  const keys = useRef<HTMLDivElement>(null);
  const octaves = octavesFor(useWidth(keys));
  const layout = useMemo(() => keyboardLayout(firstBlock(octave, octaves), octaves), [octave, octaves]);
  const pointers = useMemo(() => new PointerPiano(notes), [notes]);

  // The keys that are held are lit on the frame, from the telemetry. (Where the keys are drawn again, an element that React kept
  // may be lit already: what is lit is read off the elements, so that a key that was let go meanwhile is not left lit.)
  useEffect(() => {
    const root = keys.current;
    if (!root) return;
    const drawn = Array.from(root.querySelectorAll<HTMLElement>("[data-note]"), (element) => ({
      element,
      note: Number(element.dataset.note),
      held: element.hasAttribute("data-held"),
    }));
    return frames.subscribe(() => {
      for (const key of drawn) {
        const held = client.telemetry.isHeld(key.note);
        if (held === key.held) continue;
        key.held = held;
        key.element.toggleAttribute("data-held", held);
        key.element.setAttribute("aria-pressed", String(held));
      }
    });
  }, [client, frames, layout]);

  // A pointer is let go of wherever it goes up (the event reaches the window from wherever it is: on a key, over the edge of the
  // keys where it is captured, or off them), and when the page loses the pointers (it loses the focus, or is hidden).
  useEffect(() => {
    const up = (event: globalThis.PointerEvent): void => {
      pointers.release(event.pointerId);
    };
    const releaseAll = (): void => {
      pointers.releaseAll();
    };
    const hide = (): void => {
      if (document.hidden) pointers.releaseAll();
    };
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
    window.addEventListener("blur", releaseAll);
    document.addEventListener("visibilitychange", hide);
    return () => {
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
      window.removeEventListener("blur", releaseAll);
      document.removeEventListener("visibilitychange", hide);
      pointers.releaseAll();
    };
  }, [pointers]);

  const onPointerDown = (event: PointerEvent<HTMLDivElement>): void => {
    if (event.button !== 0) return;
    // (Neither the focus nor the selection moves for a key.)
    event.preventDefault();
    // (A pointer that is gone before it is captured is no harm, and jsdom has no capture.)
    try {
      event.currentTarget.setPointerCapture(event.pointerId);
    } catch {
      // nothing to capture
    }
    pointers.press(event.pointerId, noteOfElement(event.target));
  };
  const onPointerMove = (event: PointerEvent<HTMLDivElement>): void => {
    if (pointers.isDown(event.pointerId)) pointers.move(event.pointerId, noteAt(event.clientX, event.clientY));
  };

  const key = (note: number, style?: CSSProperties) => (
    <button
      key={note}
      type="button"
      tabIndex={-1}
      className={isWhite(note) ? "tang-key tang-key--white" : "tang-key tang-key--black"}
      style={style}
      data-note={note}
      data-role={roleOf(note, piano.root, piano.notes)}
      aria-label={noteName(note)}
      aria-pressed="false"
    >
      <span className="tang-key__name" aria-hidden="true">
        {keyLabel(note)}
      </span>
    </button>
  );
  return (
    <div
      ref={keys}
      className="tang-piano__keys"
      role="group"
      aria-label="Piano keys"
      data-locked={piano.locked ? "" : undefined}
      style={{ "--whites": layout.whites.length } as CSSProperties}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onContextMenu={(event) => {
        event.preventDefault();
      }}
    >
      {layout.whites.map((note) => key(note))}
      {layout.blacks.map(({ note, after }) => key(note, { "--after": after } as CSSProperties))}
    </div>
  );
}
