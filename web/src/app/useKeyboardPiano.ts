// The keys of the computer play notes for as long as the page is there (client/keyboard.ts): they are
// listened for on the window, so that a key pressed with the focus on a knob, a button or nothing is
// the piano's, and what is held is let go of when the page loses the keys (the window loses the focus,
// the page is hidden, it is taken off).
import { useEffect, useMemo, useState } from "react";

import { DEFAULT_OCTAVE, KeyboardPiano } from "../client/keyboard";
import { useClient } from "./ClientProvider";

/** Make the keys of the computer a piano, played through the page's client; the octave it is at. */
export function useKeyboardPiano(): number {
  const client = useClient();
  const [octave, setOctave] = useState(DEFAULT_OCTAVE);
  const piano = useMemo(() => new KeyboardPiano(client, setOctave), [client]);
  useEffect(() => {
    const down = (event: KeyboardEvent): void => {
      if (piano.keyDown(event)) event.preventDefault();
    };
    const up = (event: KeyboardEvent): void => {
      if (piano.keyUp(event)) event.preventDefault();
    };
    const release = (): void => {
      piano.releaseAll();
    };
    const hide = (): void => {
      if (document.hidden) piano.releaseAll();
    };
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    window.addEventListener("blur", release);
    document.addEventListener("visibilitychange", hide);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
      window.removeEventListener("blur", release);
      document.removeEventListener("visibilitychange", hide);
      piano.releaseAll();
    };
  }, [piano]);
  return octave;
}
