// What the page has to tell the person that nobody asked it for: that something they did was not
// done, and why. A short list, newest last, that views draw and a person can dismiss (or that goes
// by itself: the view says when). The same words said again and again (a knob dragged into a
// refusal asks sixty times a second) are one notice with a count.

export interface Notice {
  readonly id: number;
  readonly text: string;
  /** How many times it was said, in a row. */
  readonly count: number;
}

/** How many notices are kept: the older ones go first. */
export const MAX_NOTICES = 3;

export class Notices {
  #items: readonly Notice[] = [];
  #next = 1;
  readonly #listeners = new Set<() => void>();

  /** The notices now, oldest first: the same array until they change (for `useSyncExternalStore`). */
  readonly getItems = (): readonly Notice[] => this.#items;

  /** Tell `listener` whenever the notices change. */
  readonly subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  /** Say `text`; if it is what was said last, that notice counts one more. The id of the notice. */
  say(text: string): number {
    const last = this.#items.at(-1);
    if (last?.text === text) {
      this.#set([...this.#items.slice(0, -1), { ...last, count: last.count + 1 }]);
      return last.id;
    }
    const notice: Notice = { id: this.#next, text, count: 1 };
    this.#next += 1;
    this.#set([...this.#items, notice].slice(-MAX_NOTICES));
    return notice.id;
  }

  /** Take a notice away (nothing happens if it is gone already). */
  dismiss(id: number): void {
    if (this.#items.some((notice) => notice.id === id)) this.#set(this.#items.filter((notice) => notice.id !== id));
  }

  #set(items: readonly Notice[]): void {
    this.#items = items;
    for (const listener of this.#listeners) listener();
  }
}
