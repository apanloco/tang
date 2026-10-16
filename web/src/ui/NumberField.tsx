// A number as a field of text, for what the session holds one of that a person types (the tempo, the length of a pattern): it shows the number the session has, and asks for the
// one that is typed. Enter, or leaving the field, asks for it; Escape gives it up; and the arrow keys turn it by a step (a bigger one with Shift), asking for each step as it is
// made. What is typed is not asked for until then (a "1" on the way to "120" is not a tempo), and a number that another front-end sets does not take it away. The server holds a
// number to the ones there can be and the field shows what it was held to; the steps stay within the range, since they are the page's own.
import { useRef, useState } from "react";

export interface NumberFieldProps {
  /** The number the session has. */
  value: number;
  /** The lowest and the highest number there can be: what the arrow keys stop at (a number that is typed beyond them is asked as it was typed, and the server holds it). */
  range: readonly [number, number];
  /** How much an arrow key turns the number by, and how much with Shift. */
  step: number;
  bigStep: number;
  /** What the field is called, for a person who does not see it. */
  label: string;
  className?: string;
  title?: string;
  /** Ask for a number. Resolves when it has been answered, refused or not: the field shows the number it asked for until then (what a refusal says is the caller's to tell). */
  ask: (value: number) => Promise<unknown>;
}

/** The number that `text` is, if it is one: a decimal number and nothing else (what a person types is not read as an exponent, a fraction or a list). */
function numberIn(text: string): number | null {
  const trimmed = text.trim();
  if (!/^[+-]?(\d+\.?\d*|\.\d+)$/.test(trimmed)) return null;
  const value = Number(trimmed);
  return Number.isFinite(value) ? value : null;
}

export function NumberField({ value, range, step, bigStep, label, className, title, ask }: NumberFieldProps) {
  // What is being typed, and the number that was asked for and has not been answered (shown until it is, so that the field does not go back to the old one for a moment).
  const [typed, setTyped] = useState<string | null>(null);
  const [asking, setAsking] = useState<number | null>(null);
  const latest = useRef(0);
  const [low, high] = range;
  const shown = asking ?? value;

  const request = (number: number): void => {
    setTyped(null);
    setAsking(number);
    const mine = ++latest.current;
    void ask(number).finally(() => {
      if (latest.current === mine) setAsking(null);
    });
  };

  const leave = (): void => {
    if (typed === null) return;
    const number = numberIn(typed);
    if (number === null || number === shown) setTyped(null);
    else request(number);
  };

  const turn = (by: number): void => {
    const from = typed === null ? shown : (numberIn(typed) ?? shown);
    // (Rounded, so that steps from a number with a fraction do not gather the noise of the 64-bit numbers.)
    const next = Math.round(Math.min(high, Math.max(low, from + by)) * 1000) / 1000;
    if (next === from) return;
    request(next);
  };

  return (
    <input
      className={className}
      title={title}
      type="text"
      inputMode="decimal"
      role="spinbutton"
      aria-label={label}
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
          turn((event.key === "ArrowUp" ? 1 : -1) * (event.shiftKey ? bigStep : step));
        }
      }}
    />
  );
}
