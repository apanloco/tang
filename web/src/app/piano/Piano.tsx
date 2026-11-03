// The piano of the page (docs/web-ui.md §9.5), as the TUI's Piano tab is: the keys of a keyboard drawn in the scale of the session
// (the root in gold, the other notes of the scale in teal), lit while the engine holds a note whoever holds it, and played with
// the pointer; with the choice of the scale, and the lock that drops the notes that are not in it.
import { useClientState } from "../ClientProvider";
import { PianoBar } from "./PianoBar";
import { PianoKeys } from "./PianoKeys";
import "./piano.css";

/**
 * The piano, below the patch. `octave` is where the keys of the computer start: the window of keys is centred on what they play.
 * Its place is there from the start, empty until the session is (there is no scale to draw the keys in before it): the canvas is
 * fitted to the room it has when it is first drawn, and a piano that came a moment after would take some of it.
 */
export function Piano({ octave }: { octave: number }) {
  const piano = useClientState((state) => state.snapshot?.meta.piano);
  return (
    <section className="tang-piano" aria-label="Piano">
      {piano ? (
        <>
          <PianoBar piano={piano} />
          <PianoKeys piano={piano} octave={octave} />
        </>
      ) : null}
    </section>
  );
}
