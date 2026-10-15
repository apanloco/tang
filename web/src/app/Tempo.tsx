// The tempo of the session, in the bar at the top of the page: the beats a minute that patterns are recorded at (`SetBpm`; the terminal's `b`), as a field and its unit
// ("120 BPM": the bar has no room for a name as well, and the field's title says what it is). Enter, or leaving the field, asks for the number that was typed; Escape gives
// it up; and the arrow keys turn the tempo by one beat a minute (ten with Shift), asking for each step as it is made. What is typed is not asked for until then (a "1" on the
// way to "120" is not a tempo), and a tempo that another front-end sets does not take it away. The server holds a tempo to the tempos there can be and the field shows what
// it was held to; the steps stay within `BPM_RANGE`, since they are the page's own.
import { useRef, useState } from "react";

import { BPM_RANGE } from "../protocol";
import { useClientState, useEditor } from "./ClientProvider";

/** How much an arrow key turns the tempo by, and how much with Shift. */
const STEP = 1;
const BIG_STEP = 10;

/** The number that `text` is, if it is one: a decimal number and nothing else (what a person types is not read as an exponent, a fraction or a list). */
function numberIn(text: string): number | null {
  const trimmed = text.trim();
  if (!/^[+-]?(\d+\.?\d*|\.\d+)$/.test(trimmed)) return null;
  const value = Number(trimmed);
  return Number.isFinite(value) ? value : null;
}

export function Tempo() {
  const editor = useEditor();
  const bpm = useClientState((state) => state.snapshot?.meta.bpm);
  // What is being typed, and the tempo that was asked for and has not been answered (shown until it is, so that the field does not go back to the old one for a moment).
  const [typed, setTyped] = useState<string | null>(null);
  const [asking, setAsking] = useState<number | null>(null);
  const latest = useRef(0);
  if (bpm === undefined) return null;
  const [low, high] = BPM_RANGE;
  const shown = asking ?? bpm;

  const ask = (value: number): void => {
    setTyped(null);
    setAsking(value);
    const mine = ++latest.current;
    // (If it is refused the page says so: `Editor` puts the reason among the notices.)
    void editor.setBpm(value).finally(() => {
      if (latest.current === mine) setAsking(null);
    });
  };

  const leave = (): void => {
    if (typed === null) return;
    const value = numberIn(typed);
    if (value === null || value === shown) setTyped(null);
    else ask(value);
  };

  const step = (by: number): void => {
    const from = typed === null ? shown : (numberIn(typed) ?? shown);
    // (Rounded, so that steps from a tempo with a fraction do not gather the noise of the 64-bit numbers.)
    const next = Math.round(Math.min(high, Math.max(low, from + by)) * 1000) / 1000;
    if (next === from) return;
    ask(next);
  };

  return (
    <label className="tang-tempo" title="The tempo that patterns are recorded at, in beats a minute (the arrow keys turn it by one, by ten with Shift)">
      <input
        className="tang-tempo__field"
        type="text"
        inputMode="decimal"
        role="spinbutton"
        aria-label="Tempo"
        aria-valuemin={low}
        aria-valuemax={high}
        aria-valuenow={shown}
        spellCheck={false}
        autoComplete="off"
        value={typed ?? String(shown)}
        onFocus={(event) => {
          event.target.select();
        }}
        onChange={(event) => {
          setTyped(event.target.value);
        }}
        onBlur={leave}
        onKeyDown={(event) => {
          // (An Enter that confirms what an input method is composing is not for the field.)
          if (event.nativeEvent.isComposing) return;
          if (event.key === "Enter") {
            event.preventDefault();
            leave();
          } else if (event.key === "Escape") {
            event.preventDefault();
            setTyped(null);
          } else if (event.key === "ArrowUp" || event.key === "ArrowDown") {
            event.preventDefault();
            step((event.key === "ArrowUp" ? 1 : -1) * (event.shiftKey ? BIG_STEP : STEP));
          }
        }}
      />
      <span className="tang-tempo__unit">BPM</span>
    </label>
  );
}
