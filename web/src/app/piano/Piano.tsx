// The piano of the page (docs/web-ui.md §9.5), as the TUI's Piano tab is: the keys of a keyboard drawn in the scale of the session
// (the root in gold, the other notes of the scale in teal), lit while the engine holds a note whoever holds it, and played with
// the pointer; with the choice of the scale, and the lock that drops the notes that are not in it.
import { useState } from "react";

import { useClientState } from "../ClientProvider";
import { PianoBar } from "./PianoBar";
import { PianoKeys } from "./PianoKeys";
import "./piano.css";

/**
 * The piano, below the patch. `octave` is where the keys of the computer start: the window of keys is centred on what they play.
 * The keys are shut away until the bar's button asks for them (step 119: in a host's window there is no room to spare and the host plays the notes): what is left is the bar, with the scale, the lock and
 * the panic, which are not the keys' and so are not hidden with them, and the keys of the computer, which play whether the keys are drawn or not. `defaultOpen` is how it begins: a person who opens the keys has
 * them until the page is made again.
 * Its place is there from the start, empty until the session is (there is no scale to draw the keys in before it), and it is as tall as the bar when the keys are shut: the canvas is
 * fitted to the room it has when it is first drawn, and a piano that came a moment after would take some of it.
 */
export function Piano({ octave, defaultOpen = false }: { octave: number; defaultOpen?: boolean }) {
  const piano = useClientState((state) => state.snapshot?.meta.piano);
  const [open, setOpen] = useState(defaultOpen);
  return (
    <section className="tang-piano" aria-label="Piano" data-open={open ? "" : undefined}>
      {piano ? (
        <>
          <PianoBar
            piano={piano}
            open={open}
            onToggle={() => {
              setOpen(!open);
            }}
          />
          {open ? <PianoKeys piano={piano} octave={octave} /> : null}
        </>
      ) : null}
    </section>
  );
}
