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
import { MAX_STEPS } from "../protocol";
import type { EdgeId, Gain, Intent, ModulatorField, NodeId } from "../protocol";
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

export class Control<T = number> {
  #pending: T | null = null;
  /** There is a value that has been set and not sent. */
  #dirty = false;
  /** Intents sent and not yet answered. */
  #inFlight = 0;
  #cancelFrame: (() => void) | null = null;
  /** The frame (as the loop counts them) in which an intent was last sent: there is one in each. */
  #sentInFrame = -1;
  readonly #listeners = new Set<() => void>();
  readonly #make: (value: T) => Intent;
  readonly #accepts: (value: T) => boolean;
  readonly #options: DriveOptions;

  /** `make` is the intent that sets the control to a value; `accepts` says which values it can be set to (a number that is a number, for the numbers). */
  constructor(make: (value: T) => Intent, options: DriveOptions, accepts: (value: T) => boolean) {
    this.#make = make;
    this.#accepts = accepts;
    this.#options = options;
  }

  /**
   * What the control was last set to, as long as the server has not answered everything it
   * has sent; null when the session's value is the one to show. (A property that returns
   * the same function every time, for `useSyncExternalStore`.)
   */
  readonly getPending = (): T | null => this.#pending;

  /** Tell `listener` whenever `getPending` changes. */
  readonly subscribe = (listener: () => void): (() => void) => {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  };

  /** Set the control to `value`: it shows it now and sends it on the next frame. */
  set(value: T): void {
    if (!this.#accepts(value)) return;
    this.#dirty = true;
    this.#show(value);
    this.#cancelFrame ??= this.#options.frames.next(() => {
      this.#cancelFrame = null;
      this.#flush();
    });
  }

  /**
   * Send what has been set, now and not on the next frame, because a gesture is over; unless
   * an intent has gone in this frame already, and then on the next, which is the one that is
   * allowed.
   */
  commit(): void {
    this.#cancelFrame?.();
    this.#cancelFrame = null;
    this.#flush();
  }

  #flush(): void {
    if (!this.#dirty || this.#pending === null) return;
    const { frames } = this.#options;
    if (frames.frames === this.#sentInFrame) {
      // One intent a frame: what has been set since waits for the next one.
      // (Nothing is waiting for a frame here: the one that called this has just left, or `commit` took it back.)
      this.#cancelFrame = frames.next(() => {
        this.#cancelFrame = null;
        this.#flush();
      });
      return;
    }
    this.#sentInFrame = frames.frames;
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

  #show(value: T | null): void {
    if (this.#pending === value) return;
    this.#pending = value;
    for (const listener of this.#listeners) listener();
  }
}

/** The controls of a session, one for each parameter, gain, setting of a modulator or cable that has been asked for. */
export class Drive {
  /** Each control by what it is for, of the kind of value that it takes (what a key asks for says which). */
  readonly #controls = new Map<string, unknown>();
  readonly #options: DriveOptions;

  constructor(options: DriveOptions) {
    this.#options = options;
  }

  /** The control of parameter `index` of the plugin in `node`. */
  param(node: NodeId, index: number): Control {
    return this.#numeric(`param:${String(node)}:${String(index)}`, (value) => ({ type: "setParam", node, param: index, value }));
  }

  /** The control of a host-side gain of `node`. */
  gain(node: NodeId, gain: Gain): Control {
    return this.#numeric(`gain:${String(node)}:${gain}`, (value) => ({ type: "setGain", node, gain, value }));
  }

  /** The control of a setting of the modulator in `node`: an LFO's rate or waveform, a stage of an envelope. */
  modulatorField(node: NodeId, field: ModulatorField): Control {
    return this.#numeric(`field:${String(node)}:${field}`, (value) => ({ type: "setModulatorField", node, field, value }));
  }

  /** The control of how deep a modulation cable pulls. */
  cableDepth(edge: EdgeId): Control {
    return this.#numeric(`depth:${String(edge)}`, (depth) => ({ type: "setEdgeDepth", edge, depth }));
  }

  /** The control of how loud an audio cable is: its level, 0 to 2, which the server holds it to. */
  cableGain(edge: EdgeId): Control {
    return this.#numeric(`level:${String(edge)}`, (gain) => ({ type: "setCableGain", edge, gain }));
  }

  /** The control of all the steps of the step sequencer in `node`: a stroke across its bars sets them all at once, a frame at a time. */
  steps(node: NodeId): Control<readonly number[]> {
    return this.#control(
      `steps:${String(node)}`,
      (values) => ({ type: "setSteps", node, values: [...values] }),
      (values) => values.length === MAX_STEPS && values.every((value) => Number.isFinite(value)),
    );
  }

  /** A control of a number: one that is not a number is not sent. */
  #numeric(key: string, make: (value: number) => Intent): Control {
    return this.#control(key, make, Number.isFinite);
  }

  #control<T>(key: string, make: (value: T) => Intent, accepts: (value: T) => boolean): Control<T> {
    let control = this.#controls.get(key) as Control<T> | undefined;
    if (!control) {
      control = new Control<T>(make, this.#options, accepts);
      this.#controls.set(key, control);
    }
    return control;
  }
}
