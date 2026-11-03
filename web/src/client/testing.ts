// A frame clock the test turns by hand, for the tests of whatever draws on the frame loop.
import type { Scheduler } from "./frameloop";

export class Clock implements Scheduler {
  #next = 1;
  readonly #waiting = new Map<number, (time: number) => void>();
  requested = 0;
  cancelled = 0;

  request(callback: (time: number) => void): number {
    this.requested += 1;
    const handle = this.#next++;
    this.#waiting.set(handle, callback);
    return handle;
  }

  cancel(handle: number): void {
    this.cancelled += 1;
    this.#waiting.delete(handle);
  }

  /** Frames that are waiting to be drawn. */
  get pending(): number {
    return this.#waiting.size;
  }

  /** Draw the waiting frame(s) at `time`. */
  tick(time: number): void {
    const callbacks = [...this.#waiting.values()];
    this.#waiting.clear();
    for (const callback of callbacks) callback(time);
  }
}
