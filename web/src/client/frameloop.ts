// One animation-frame loop for the whole page (docs/web-ui.md §9.4): whatever draws live
// values (a knob's dot, a meter, a modulator's output) subscribes a callback, and the
// loop calls them all once per frame, with the time of the frame, from a single
// requestAnimationFrame. The callbacks read the telemetry buffers and write what they
// draw straight to the DOM, so no framework renders again for a value that changes thirty
// times a second. The loop runs only while somebody is subscribed.

/** The browser's frame clock, or a stand-in. */
export interface Scheduler {
  request: (callback: (time: number) => void) => number;
  cancel: (handle: number) => void;
}

export const browserScheduler: Scheduler = {
  request: (callback) => requestAnimationFrame(callback),
  cancel: (handle) => {
    cancelAnimationFrame(handle);
  },
};

export type FrameCallback = (time: number) => void;

export class FrameLoop {
  readonly #scheduler: Scheduler;
  /** Entries rather than the callbacks, so that one function subscribed twice is two subscriptions. */
  readonly #entries = new Set<{ callback: FrameCallback }>();
  #handle: number | null = null;
  #frames = 0;

  constructor(scheduler: Scheduler = browserScheduler) {
    this.#scheduler = scheduler;
  }

  /** How many frames the loop has run. */
  get frames(): number {
    return this.#frames;
  }

  /** Whether a frame is waiting to be drawn, which is whether anybody is subscribed. */
  get running(): boolean {
    return this.#handle !== null;
  }

  /** Call `callback` on every frame until the function that is returned is called. */
  subscribe(callback: FrameCallback): () => void {
    const entry = { callback };
    this.#entries.add(entry);
    this.#schedule();
    return () => {
      this.#entries.delete(entry);
      if (this.#entries.size === 0 && this.#handle !== null) {
        this.#scheduler.cancel(this.#handle);
        this.#handle = null;
      }
    };
  }

  #schedule(): void {
    this.#handle ??= this.#scheduler.request(this.#tick);
  }

  readonly #tick = (time: number): void => {
    this.#handle = null;
    this.#frames += 1;
    for (const entry of this.#entries) {
      try {
        entry.callback(time);
      } catch (error) {
        // One drawing that fails must not stop the others.
        console.error(error);
      }
    }
    if (this.#entries.size > 0) this.#schedule();
  };
}

/** The loop of the page. */
export const pageFrames = new FrameLoop();
