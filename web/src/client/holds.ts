// The notes the page holds, whoever holds them (docs/web-ui.md §9.5): the keys of the computer and the pointer on the keys of
// the page both play, and they can come to the same note. It starts when the first of them takes it and ends when the last lets
// go, so that letting go of the mouse does not end a note a key is still holding.
import type { NoteSink } from "./keyboard";

/** Where the notes go, and the panic that lets go of all of them: the client is one. */
export interface PanicSink extends NoteSink {
  panic(): void;
}

/** A sink of notes that counts how many players hold each note, and passes on only the first note-on and the last note-off. */
export class NoteHolds implements NoteSink {
  readonly #sink: PanicSink;
  readonly #holds = new Map<number, number>();

  constructor(sink: PanicSink) {
    this.#sink = sink;
  }

  /**
   * Let go of everything: the sink is told to (the server ends every note it holds, this page's and the others'), and no player holds anything any more. A player that lets go after it ends
   * nothing, and the next to take a note is the first, as it would be if the note had never been held.
   */
  panic(): void {
    this.#holds.clear();
    this.#sink.panic();
  }

  noteOn(note: number, velocity?: number): void {
    const holds = (this.#holds.get(note) ?? 0) + 1;
    this.#holds.set(note, holds);
    if (holds === 1) this.#sink.noteOn(note, velocity);
  }

  noteOff(note: number): void {
    const holds = this.#holds.get(note);
    // (A note that nobody holds has nothing to end.)
    if (holds === undefined) return;
    if (holds > 1) {
      this.#holds.set(note, holds - 1);
      return;
    }
    this.#holds.delete(note);
    this.#sink.noteOff(note);
  }
}
