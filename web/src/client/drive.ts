// What a control shows, and what it sends, while it is being turned (docs/web-ui.md §9.4).
//
// A knob dragged by hand makes sixty changes a second, and the server answers each one that
// it is sent. Two rules keep that sane: at most one intent per frame per parameter (the
// latest value; the ones in between were never wanted), and what the control shows while
// the server catches up is the user's value, not the server's echoes of the earlier ones,
// which would pull the knob back under the pointer. A `Control` is one such parameter (or
// gain): `set` it as often as you like, and it shows what it was set to until what it sent
// has been answered, at which point the session's own value (which is by then the
// server's last word) is what shows.
import type { Gain, Intent, NodeId } from "../protocol";
import type { FrameLoop } from "./frameloop";

/** How intents get to the server: `Client.request`, or a stand-in. */
export type Send = (intent: Intent) => Promise<unknown>;

export interface DriveOptions {
  send: Send;
  /** The frames the intents wait for. */
  frames: FrameLoop;
  /** What to do about an intent that did not get through: the server refused it, or the connection went. */
  onError?: (error: unknown, intent: Intent) => void;
}

export class Control {
  #pending: number | null = null;
  /** There is a value that has been set and not sent. */
  #dirty = false;
  /** Intents sent and not yet answered. */
  #inFlight = 0;
  #cancelFrame: (() => void) | null = null;
  readonly #listeners = new Set<() => void>();
  readonly #make: (value: number) => Intent;
  readonly #options: DriveOptions;

  constructor(make: (value: number) => Intent, options: DriveOptions) {
    this.#make = make;
    this.#options = options;
  }

  /**
   * What the control was last set to, as long as the server has not answered everything it
   * has sent; null when the session's value is the one to show. (A property that returns
   * the same function every time, for `useSyncExternalStore`.)
   */
  readonly getPending = (): number | null => this.#pending;

  /** Tell `listener` whenever `getPending` changes. */
  readonly subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  /** Set the control to `value`: it shows it now and sends it on the next frame. */
  set(value: number): void {
    if (!Number.isFinite(value)) return;
    this.#dirty = true;
    this.#show(value);
    this.#cancelFrame ??= this.#options.frames.next(() => {
      this.#cancelFrame = null;
      this.#flush();
    });
  }

  /** Send what has been set, now, and not on the next frame: a gesture is over. */
  commit(): void {
    this.#cancelFrame?.();
    this.#cancelFrame = null;
    this.#flush();
  }

  #flush(): void {
    if (!this.#dirty || this.#pending === null) return;
    this.#dirty = false;
    const intent = this.#make(this.#pending);
    this.#inFlight += 1;
    let sent: Promise<unknown>;
    try {
      sent = this.#options.send(intent);
    } catch (error) {
      sent = Promise.reject(error instanceof Error ? error : new Error(String(error)));
    }
    sent.then(
      () => {
        this.#answered();
      },
      (error: unknown) => {
        this.#options.onError?.(error, intent);
        this.#answered();
      },
    );
  }

  #answered(): void {
    this.#inFlight -= 1;
    if (this.#inFlight === 0 && !this.#dirty) this.#show(null);
  }

  #show(value: number | null): void {
    if (this.#pending === value) return;
    this.#pending = value;
    for (const listener of this.#listeners) listener();
  }
}

/** The controls of a session, one for each parameter or gain that has been asked for. */
export class Drive {
  readonly #controls = new Map<string, Control>();
  readonly #options: DriveOptions;

  constructor(options: DriveOptions) {
    this.#options = options;
  }

  /** The control of parameter `index` of the plugin in `node`. */
  param(node: NodeId, index: number): Control {
    return this.#control(`param:${String(node)}:${String(index)}`, (value) => ({ type: "setParam", node, param: index, value }));
  }

  /** The control of a host-side gain of `node`. */
  gain(node: NodeId, gain: Gain): Control {
    return this.#control(`gain:${String(node)}:${gain}`, (value) => ({ type: "setGain", node, gain, value }));
  }

  #control(key: string, make: (value: number) => Intent): Control {
    let control = this.#controls.get(key);
    if (!control) {
      control = new Control(make, this.#options);
      this.#controls.set(key, control);
    }
    return control;
  }
}
