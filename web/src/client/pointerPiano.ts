// The piano that a pointer makes on the keys of the page (docs/web-ui.md §9.5): a mouse, a finger, a pen. Each pointer that is down
// plays the key it is on, and the note follows it from key to key; where it goes off the keys the note ends, and it plays again where
// it comes back to one. Nothing here knows about the document: the page says which key a pointer is on (or none), and this says
// which notes to play.
import { VELOCITY } from "./keyboard";
import type { NoteSink } from "./keyboard";

export class PointerPiano {
  readonly #sink: NoteSink;
  /** The pointers that are down. */
  readonly #down = new Set<number>();
  /** The note each pointer that is down is playing; a pointer that is on no key is not here. */
  readonly #playing = new Map<number, number>();

  constructor(sink: NoteSink) {
    this.#sink = sink;
  }

  /** Whether `pointer` is down: it went down, on a key or not, and has not gone up. */
  isDown(pointer: number): boolean {
    return this.#down.has(pointer);
  }

  /** `pointer` went down on the key `note` (or on none: null). */
  press(pointer: number, note: number | null): void {
    // (A pointer that goes down while it is down already is one whose release was lost.)
    this.release(pointer);
    this.#down.add(pointer);
    this.#start(pointer, note);
  }

  /** `pointer` is now on the key `note` (or on none: null). Nothing, if it is not down. */
  move(pointer: number, note: number | null): void {
    if (!this.#down.has(pointer) || this.#playing.get(pointer) === (note ?? undefined)) return;
    this.#stop(pointer);
    this.#start(pointer, note);
  }

  /** `pointer` went up, or the page lost it: its note ends. */
  release(pointer: number): void {
    this.#down.delete(pointer);
    this.#stop(pointer);
  }

  /** Every pointer is let go of: the page has lost them (it lost the focus, or was hidden), and their releases will not come. */
  releaseAll(): void {
    for (const pointer of [...this.#down]) this.release(pointer);
  }

  #start(pointer: number, note: number | null): void {
    if (note === null) return;
    this.#playing.set(pointer, note);
    this.#sink.noteOn(note, VELOCITY);
  }

  #stop(pointer: number): void {
    const note = this.#playing.get(pointer);
    if (note === undefined) return;
    this.#playing.delete(pointer);
    this.#sink.noteOff(note);
  }
}
