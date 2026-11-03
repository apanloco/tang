// What the page wants shown: a node it has just added, which the canvas may have put out of the
// view (a lane goes below the others, an effect after the last of its row). The views that
// add (the menus) say which node through the editor, and the canvas, if there is one, moves to
// it: neither knows of the other.
import type { NodeId } from "../protocol";

export class Reveals {
  readonly #listeners = new Set<(node: NodeId) => void>();

  /** Tell `listener` which node is to be shown, whenever one is. Returns the function that stops it. */
  readonly subscribe = (listener: (node: NodeId) => void): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  /** The node `node` is to be shown. */
  show(node: NodeId): void {
    // (A copy: a listener that stops, or starts, while it is told does not change who is told now.)
    for (const listener of [...this.#listeners]) listener(node);
  }
}
