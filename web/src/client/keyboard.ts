// The computer keyboard as a piano: the tracker layout of the TUI's virtual piano (CLAUDE.md,
// "Virtual piano"), so that a key plays the same note in the terminal and in the page. Keys are
// told by where they are (`KeyboardEvent.code`), not by what they print, so a layout that is not
// QWERTY keeps the shape of the piano. Nothing here knows about React or the document: the page
// feeds it the events of the keys, and it says which notes to play and whether a key was its own.

/** The octave the lower row starts at, as the TUI's piano has it (its C is MIDI 48, written C3). */
export const DEFAULT_OCTAVE = 4;
export const LOWEST_OCTAVE = 0;
export const HIGHEST_OCTAVE = 8;
/** How hard every key plays, as in the TUI. */
export const VELOCITY = 100;

/** The keys of the lower row, from the octave's C: the white keys are on the letters and the black ones above them. */
const LOWER_ROW = ["KeyZ", "KeyS", "KeyX", "KeyD", "KeyC", "KeyV", "KeyG", "KeyB", "KeyH", "KeyN", "KeyJ", "KeyM", "Comma", "KeyL", "Period", "Semicolon", "Slash"];
/** The upper row: an octave above the lower, from the same C. */
const UPPER_ROW = ["KeyQ", "Digit2", "KeyW", "Digit3", "KeyE", "KeyR", "Digit5", "KeyT", "Digit6", "KeyY", "Digit7", "KeyU", "KeyI", "Digit9", "KeyO", "Digit0", "KeyP"];

/** Where a key is: the semitone above its row's C, and the row (the upper is an octave above the lower). */
interface Place {
  semitone: number;
  row: 0 | 1;
}

const PLACES: ReadonlyMap<string, Place> = new Map([
  ...LOWER_ROW.map((code, semitone): [string, Place] => [code, { semitone, row: 0 }]),
  ...UPPER_ROW.map((code, semitone): [string, Place] => [code, { semitone, row: 1 }]),
]);

/** The MIDI note a key plays with the lower row at `octave`, or nothing if it is not a piano key (or the note is not one MIDI has). */
export function noteOf(code: string, octave: number): number | null {
  const place = PLACES.get(code);
  if (!place) return null;
  const note = (octave + place.row) * 12 + place.semitone;
  return note >= 0 && note <= 127 ? note : null;
}

/** What the page's keys are told to: where the notes go. (`Client` is one.) */
export interface NoteSink {
  noteOn(note: number, velocity?: number): void;
  noteOff(note: number): void;
}

/** What `KeyboardPiano` reads of a key event: a `KeyboardEvent` has all of it. */
export interface KeyLike {
  /** Which key, by where it is. */
  code: string;
  /** Held down and repeating, which is not a new press. */
  repeat: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  /** Where the key went: a place that takes text keeps its keys. */
  target: EventTarget | null;
}

/** Whether keys typed at `target` are text: a field, a text area, a list that finds an item by its first letters, or what can be edited. */
export function takesText(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (target instanceof HTMLInputElement) return !["button", "checkbox", "radio", "range", "submit", "reset", "image", "file", "color"].includes(target.type);
  return target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement || target.closest('[contenteditable]:not([contenteditable="false"])') !== null;
}

/**
 * The piano that the keys of the computer make. It plays what a key starts for as long as the key
 * is down, and remembers which note each key started: the octave can be changed while a key is held,
 * and the key still ends the note it began. (Two keys can come to one note that way; the note stays
 * on until both are up.)
 */
export class KeyboardPiano {
  readonly #sink: NoteSink;
  readonly #onOctave: (octave: number) => void;
  #octave = DEFAULT_OCTAVE;
  /** The note each key that is down started; a key that is down and had no note is not here. */
  readonly #started = new Map<string, number>();
  /** How many keys keep each note on. */
  readonly #holds = new Map<number, number>();

  constructor(sink: NoteSink, onOctave: (octave: number) => void = () => undefined) {
    this.#sink = sink;
    this.#onOctave = onOctave;
  }

  /** The octave the lower row is at. */
  get octave(): number {
    return this.#octave;
  }

  /**
   * A key went down. True if it was the piano's (a note, or an octave key), which the page keeps from
   * the browser; false if it is for something else: a key that is not on the piano, one with Control,
   * Command or Alt (the browser's own), or one typed at a place that takes text.
   */
  keyDown(event: KeyLike): boolean {
    if (event.ctrlKey || event.metaKey || event.altKey || takesText(event.target)) return false;
    if (event.code === "BracketLeft" || event.code === "BracketRight") {
      if (!event.repeat) this.#shift(event.code === "BracketRight" ? 1 : -1);
      return true;
    }
    if (!PLACES.has(event.code)) return false;
    // (A key that is down already is one that repeats, or one whose release was lost.)
    if (this.#started.has(event.code)) return true;
    const note = noteOf(event.code, this.#octave);
    if (note !== null) {
      this.#started.set(event.code, note);
      const holds = this.#holds.get(note) ?? 0;
      this.#holds.set(note, holds + 1);
      if (holds === 0) this.#sink.noteOn(note, VELOCITY);
    }
    return true;
  }

  /** A key went up. True if it was one that this was playing, which ends its note; however the other keys are, a key that is down is always let go. */
  keyUp(event: KeyLike): boolean {
    const note = this.#started.get(event.code);
    if (note === undefined) return false;
    this.#started.delete(event.code);
    this.#release(note);
    return true;
  }

  /** End every note that is held: the page has lost the keys (it lost the focus, or was hidden), and their releases will not come. */
  releaseAll(): void {
    const notes = [...this.#started.values()];
    this.#started.clear();
    for (const note of notes) this.#release(note);
  }

  #release(note: number): void {
    const holds = (this.#holds.get(note) ?? 0) - 1;
    if (holds > 0) {
      this.#holds.set(note, holds);
      return;
    }
    this.#holds.delete(note);
    this.#sink.noteOff(note);
  }

  #shift(by: number): void {
    const octave = Math.min(HIGHEST_OCTAVE, Math.max(LOWEST_OCTAVE, this.#octave + by));
    if (octave === this.#octave) return;
    this.#octave = octave;
    this.#onOctave(octave);
  }
}
