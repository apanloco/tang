// @vitest-environment jsdom
import { describe, expect, test } from "vitest";

import { DEFAULT_OCTAVE, HIGHEST_OCTAVE, KeyboardPiano, LOWEST_OCTAVE, VELOCITY, noteOf } from "./keyboard";
import type { KeyLike } from "./keyboard";

/** MIDI number of a note name as the project writes them: C4 is middle C, 60. */
function midi(name: string): number {
  const match = /^([A-G])(#?)(-?\d+)$/.exec(name);
  if (!match) throw new Error(`not a note: ${name}`);
  const [, letter, sharp, octave] = match;
  const semitones: Record<string, number> = { C: 0, D: 2, E: 4, F: 5, G: 7, A: 9, B: 11 };
  return (Number(octave) + 1) * 12 + (semitones[letter ?? ""] ?? 0) + (sharp ? 1 : 0);
}

/** The layout CLAUDE.md gives the virtual piano, at the default octave: the lower row from C3, the upper from C4. */
const LAYOUT: [code: string, note: string][] = [
  ["KeyZ", "C3"], ["KeyS", "C#3"], ["KeyX", "D3"], ["KeyD", "D#3"], ["KeyC", "E3"], ["KeyV", "F3"], ["KeyG", "F#3"], ["KeyB", "G3"],
  ["KeyH", "G#3"], ["KeyN", "A3"], ["KeyJ", "A#3"], ["KeyM", "B3"], ["Comma", "C4"], ["KeyL", "C#4"], ["Period", "D4"], ["Semicolon", "D#4"],
  ["Slash", "E4"],
  ["KeyQ", "C4"], ["Digit2", "C#4"], ["KeyW", "D4"], ["Digit3", "D#4"], ["KeyE", "E4"], ["KeyR", "F4"], ["Digit5", "F#4"], ["KeyT", "G4"],
  ["Digit6", "G#4"], ["KeyY", "A4"], ["Digit7", "A#4"], ["KeyU", "B4"], ["KeyI", "C5"], ["Digit9", "C#5"], ["KeyO", "D5"], ["Digit0", "D#5"],
  ["KeyP", "E5"],
];

describe("noteOf", () => {
  test("is the tracker layout of the TUI's virtual piano, key by key", () => {
    expect(DEFAULT_OCTAVE).toBe(4);
    for (const [code, note] of LAYOUT) {
      expect(noteOf(code, DEFAULT_OCTAVE), `${code} is ${note}`).toBe(midi(note));
    }
    expect(LAYOUT).toHaveLength(34);
  });

  test("moves an octave with the base octave, both rows together", () => {
    expect(noteOf("KeyZ", 3)).toBe(36);
    expect(noteOf("KeyZ", 5)).toBe(60);
    expect(noteOf("KeyQ", 3)).toBe(48);
    expect(noteOf("KeyQ", 5)).toBe(72);
  });

  test("keeps within what a MIDI note can be", () => {
    expect(noteOf("KeyZ", LOWEST_OCTAVE)).toBe(0);
    expect(noteOf("KeyP", HIGHEST_OCTAVE)).toBe(124);
    // The top of the upper row at a ninth octave would be 136, and nothing below 0 is reachable.
    expect(noteOf("KeyP", 9)).toBeNull();
    expect(noteOf("KeyZ", -1)).toBeNull();
    expect(noteOf("KeyS", -1)).toBeNull();
  });

  test("says nothing for a key that is not a piano key", () => {
    for (const code of ["KeyA", "KeyK", "KeyF", "Digit1", "Digit4", "Digit8", "Space", "Enter", "Escape", "ArrowUp", "BracketLeft", "Backspace", ""]) {
      expect(noteOf(code, DEFAULT_OCTAVE), code).toBeNull();
    }
  });
});

/** A piano over a sink that writes down what it is asked, and the octaves it says. */
function rig() {
  const sent: string[] = [];
  const octaves: number[] = [];
  const piano = new KeyboardPiano(
    {
      noteOn: (note, velocity) => sent.push(`on ${note} ${velocity}`),
      noteOff: (note) => sent.push(`off ${note}`),
    },
    (octave) => octaves.push(octave),
  );
  return { piano, sent, octaves };
}

const key = (code: string, extra: Partial<KeyLike> = {}): KeyLike => ({ code, repeat: false, ctrlKey: false, metaKey: false, altKey: false, target: null, ...extra });

describe("KeyboardPiano", () => {
  test("a key plays its note while it is down, at the velocity of the TUI's piano, and says that it was a piano key", () => {
    const { piano, sent } = rig();
    expect(VELOCITY).toBe(100);
    expect(piano.keyDown(key("KeyZ"))).toBe(true);
    expect(sent).toEqual(["on 48 100"]);
    expect(piano.keyUp(key("KeyZ"))).toBe(true);
    expect(sent).toEqual(["on 48 100", "off 48"]);
  });

  test("keys that are not piano keys are left alone: not played, not taken", () => {
    const { piano, sent } = rig();
    for (const code of ["KeyA", "Digit1", "Space", "Enter", "ArrowLeft", "Tab"]) {
      expect(piano.keyDown(key(code)), code).toBe(false);
      expect(piano.keyUp(key(code)), code).toBe(false);
    }
    expect(sent).toEqual([]);
  });

  test("a chord is each key on its own, and ends one key at a time", () => {
    const { piano, sent } = rig();
    for (const code of ["KeyZ", "KeyC", "KeyB"]) piano.keyDown(key(code));
    piano.keyUp(key("KeyC"));
    piano.keyUp(key("KeyZ"));
    piano.keyUp(key("KeyB"));
    expect(sent).toEqual(["on 48 100", "on 52 100", "on 55 100", "off 52", "off 48", "off 55"]);
  });

  test("a key that repeats while it is held plays once", () => {
    const { piano, sent } = rig();
    piano.keyDown(key("KeyZ"));
    expect(piano.keyDown(key("KeyZ", { repeat: true }))).toBe(true);
    expect(piano.keyDown(key("KeyZ"))).toBe(true);
    expect(sent).toEqual(["on 48 100"]);
    // And one key up ends it: a repeat is not a second key that holds the note.
    piano.keyUp(key("KeyZ"));
    expect(sent).toEqual(["on 48 100", "off 48"]);
  });

  test("a key that goes up that never went down sends nothing", () => {
    const { piano, sent } = rig();
    expect(piano.keyUp(key("KeyZ"))).toBe(false);
    expect(sent).toEqual([]);
  });

  test("a key is released as the note it started, whatever the octave has become (the TUI's piano gets this wrong)", () => {
    const { piano, sent, octaves } = rig();
    piano.keyDown(key("KeyZ"));
    piano.keyDown(key("BracketRight"));
    piano.keyDown(key("BracketRight"));
    expect(octaves).toEqual([5, 6]);
    piano.keyUp(key("KeyZ"));
    expect(sent).toEqual(["on 48 100", "off 48"]);
    // And a key pressed now is at the new octave.
    piano.keyDown(key("KeyZ"));
    expect(sent.at(-1)).toBe("on 72 100");
  });

  test("two keys that come to the same note keep it on until both are up", () => {
    const { piano, sent } = rig();
    // Q at octave 4 is C4, 60. Raise the octave, and Z is C4 as well.
    piano.keyDown(key("KeyQ"));
    piano.keyDown(key("BracketRight"));
    piano.keyDown(key("KeyZ"));
    expect(sent).toEqual(["on 60 100"]);
    piano.keyUp(key("KeyQ"));
    expect(sent).toEqual(["on 60 100"]);
    piano.keyUp(key("KeyZ"));
    expect(sent).toEqual(["on 60 100", "off 60"]);
  });

  test("the bracket keys change the octave, within 0 to 8, and say so; a repeat does not change it again", () => {
    const { piano, octaves } = rig();
    expect(piano.octave).toBe(4);
    expect(piano.keyDown(key("BracketRight"))).toBe(true);
    expect(piano.keyDown(key("BracketRight", { repeat: true }))).toBe(true);
    expect(piano.octave).toBe(5);
    for (let i = 0; i < 10; i++) piano.keyDown(key("BracketRight"));
    expect(piano.octave).toBe(HIGHEST_OCTAVE);
    for (let i = 0; i < 10; i++) piano.keyDown(key("BracketLeft"));
    expect(piano.octave).toBe(LOWEST_OCTAVE);
    // It says each change, and not a press that changed nothing.
    expect(octaves).toEqual([5, 6, 7, 8, 7, 6, 5, 4, 3, 2, 1, 0]);
  });

  test("a key with Control, Command or Alt is the browser's: not played, not taken, and its release sends nothing", () => {
    const { piano, sent } = rig();
    for (const extra of [{ ctrlKey: true }, { metaKey: true }, { altKey: true }]) {
      expect(piano.keyDown(key("KeyZ", extra))).toBe(false);
      expect(piano.keyDown(key("BracketRight", extra))).toBe(false);
      expect(piano.keyUp(key("KeyZ", extra))).toBe(false);
    }
    expect(sent).toEqual([]);
    expect(piano.octave).toBe(4);
  });

  test("Shift does not matter, as in the TUI, where a capital is the same key", () => {
    const { piano, sent } = rig();
    const shifted = { ...key("KeyZ"), shiftKey: true };
    expect(piano.keyDown(shifted)).toBe(true);
    expect(sent).toEqual(["on 48 100"]);
  });

  test("a key held when Control goes down is still released", () => {
    const { piano, sent } = rig();
    piano.keyDown(key("KeyZ"));
    expect(piano.keyUp(key("KeyZ", { ctrlKey: true }))).toBe(true);
    expect(sent).toEqual(["on 48 100", "off 48"]);
  });

  test("a place that takes text keeps its keys: fields, text areas, lists and what can be edited", () => {
    const { piano, sent } = rig();
    const make = (tag: string, attributes: Record<string, string> = {}): HTMLElement => {
      const element = document.createElement(tag);
      for (const [name, value] of Object.entries(attributes)) element.setAttribute(name, value);
      document.body.append(element);
      return element;
    };
    const typing = [make("input"), make("input", { type: "text" }), make("input", { type: "number" }), make("textarea"), make("select"), make("div", { contenteditable: "true" }), make("div", { contenteditable: "" })];
    const inside = make("div", { contenteditable: "true" });
    inside.append(document.createElement("span"));
    typing.push(inside.firstElementChild as HTMLElement);
    for (const target of typing) {
      expect(piano.keyDown(key("KeyZ", { target })), target.outerHTML).toBe(false);
      expect(piano.keyDown(key("BracketRight", { target })), target.outerHTML).toBe(false);
    }
    expect(sent).toEqual([]);
    expect(piano.octave).toBe(4);
    // What takes no text does not keep them: a button, a slider (a knob is one), a checkbox, what is marked as not to be edited, the page.
    const frozen = [make("button"), make("div", { role: "slider", tabindex: "0" }), make("input", { type: "checkbox" }), make("input", { type: "range" }), make("input", { type: "button" }), make("div", { contenteditable: "false" })];
    for (const target of [...frozen, document.body, null]) {
      expect(piano.keyDown(key("KeyZ", { target })), target?.nodeName ?? "nothing").toBe(true);
      piano.keyUp(key("KeyZ", { target }));
    }
    document.body.replaceChildren();
  });

  test("releaseAll ends every note that is held, once, and keys that go up after it send nothing", () => {
    const { piano, sent } = rig();
    piano.keyDown(key("KeyZ"));
    piano.keyDown(key("KeyC"));
    piano.releaseAll();
    expect([...sent].sort()).toEqual(["off 48", "off 52", "on 48 100", "on 52 100"]);
    sent.length = 0;
    piano.releaseAll();
    expect(piano.keyUp(key("KeyZ"))).toBe(false);
    expect(sent).toEqual([]);
    // And the keys can be played again.
    piano.keyDown(key("KeyZ"));
    expect(sent).toEqual(["on 48 100"]);
  });

  test("every piano key has a note at every octave the piano can be at", () => {
    const { piano, sent } = rig();
    for (let octave = LOWEST_OCTAVE; octave <= HIGHEST_OCTAVE; octave++) {
      while (piano.octave < octave) piano.keyDown(key("BracketRight"));
      for (const [code] of LAYOUT) {
        const note = noteOf(code, octave);
        expect(note, `${code} at ${octave}`).not.toBeNull();
        expect(note, `${code} at ${octave}`).toBeGreaterThanOrEqual(0);
        expect(note, `${code} at ${octave}`).toBeLessThanOrEqual(127);
      }
    }
    // The top key of the top octave is 124: it plays, and it is let go as that.
    expect(piano.octave).toBe(HIGHEST_OCTAVE);
    piano.keyDown(key("KeyP"));
    piano.keyUp(key("KeyP"));
    expect(sent).toEqual(["on 124 100", "off 124"]);
  });
});
