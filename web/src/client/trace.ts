// The recent past of a value that changes all the time (docs/web-ui.md §9.4), for a drawing that shows it as it
// goes by: a ring of the last samples of it, each with the time it was taken at, and what a drawing asks of the
// ring, where each sample falls on a plot that shows the last few seconds. Nothing here touches the DOM or
// makes anything that is kept: a drawing that runs on every frame has to keep its work small and its garbage none.

/** A ring of the last `capacity` samples of a value, each with the time it was taken (in milliseconds, never earlier than the one before). */
export class TraceBuffer {
  readonly #times: Float64Array;
  readonly #values: Float32Array;
  /** Where the oldest sample is. */
  #first = 0;
  #length = 0;
  #changedAt: number | undefined;

  constructor(capacity: number) {
    if (!Number.isInteger(capacity) || capacity < 2) throw new RangeError(`a trace holds at least two samples, not ${String(capacity)}`);
    this.#times = new Float64Array(capacity);
    this.#values = new Float32Array(capacity);
  }

  /** How many samples it holds now. */
  get length(): number {
    return this.#length;
  }

  /**
   * The time at which the value became what the newest sample says: the time of the first of the samples that
   * have had that value since it last was another. Undefined when there is no sample. A plot of a value that
   * has not changed for as long as the plot spans is the same whenever it is drawn.
   */
  get changedAt(): number | undefined {
    return this.#changedAt;
  }

  /** Add a sample. Whatever is not a finite number, the time or the value, is not a sample. */
  push(time: number, value: number): void {
    if (!Number.isFinite(time) || !Number.isFinite(value)) return;
    const capacity = this.#times.length;
    const stored = Math.fround(value);
    if (this.#length === 0 || stored !== this.valueAt(this.#length - 1)) this.#changedAt = time;
    if (this.#length < capacity) {
      const at = (this.#first + this.#length) % capacity;
      this.#times[at] = time;
      this.#values[at] = stored;
      this.#length += 1;
    } else {
      // Full: the oldest sample gives its place to the new one, which is the newest.
      this.#times[this.#first] = time;
      this.#values[this.#first] = stored;
      this.#first = (this.#first + 1) % capacity;
    }
  }

  /** Forget every sample. */
  clear(): void {
    this.#first = 0;
    this.#length = 0;
    this.#changedAt = undefined;
  }

  /** The time of the `index`th sample, the oldest being the 0th. */
  timeAt(index: number): number {
    return this.#times[(this.#first + index) % this.#times.length] ?? Number.NaN;
  }

  /** The value of the `index`th sample, the oldest being the 0th. */
  valueAt(index: number): number {
    return this.#values[(this.#first + index) % this.#values.length] ?? Number.NaN;
  }

  /** Call `visit` with the time and the value of each sample, the oldest first. */
  forEach(visit: (time: number, value: number) => void): void {
    for (let index = 0; index < this.#length; index += 1) visit(this.timeAt(index), this.valueAt(index));
  }
}

/** The points of a trace: `count` pairs, x then y, at the start of `points` (which is as long as the largest plot has needed, or longer). */
export interface Plot {
  points: Float32Array;
  count: number;
}

/** The plot's place: its size in pixels, the values at its bottom and at its top, and the room kept above and below the line. */
export interface PlotBox {
  width: number;
  height: number;
  min: number;
  max: number;
  pad: number;
}

/**
 * Where the samples of `buffer` fall on a plot of the last `span` milliseconds up to `now`: the newest at the right
 * edge if it was taken now, and every other as far left of it as it is older, the whole span being the plot's width.
 * The line begins at the last sample from before the span, so that it comes in from the edge, and the newest value is
 * held to the right edge when it is older than `now`. The values are spread over the height inside the padding, the least
 * at the bottom, one beyond the range at its edge. The points are written into `out`, which grows if it is too small.
 */
export function plotTrace(buffer: TraceBuffer, now: number, span: number, box: PlotBox, out: Plot): void {
  const length = buffer.length;
  out.count = 0;
  if (length === 0 || !(span > 0)) return;
  const since = now - span;
  let start = 0;
  while (start + 1 < length && buffer.timeAt(start + 1) < since) start += 1;
  const needed = 2 * (length - start + 1);
  if (out.points.length < needed) out.points = new Float32Array(Math.max(needed, 2 * out.points.length));

  const top = box.pad;
  const bottom = box.height - box.pad;
  const range = box.max - box.min;
  const scale = box.width / span;
  const yOf = (value: number): number => {
    if (!(range > 0)) return (top + bottom) / 2;
    return bottom - Math.min(Math.max((value - box.min) / range, 0), 1) * (bottom - top);
  };
  let count = 0;
  for (let index = start; index < length; index += 1) {
    out.points[2 * count] = box.width + (buffer.timeAt(index) - now) * scale;
    out.points[2 * count + 1] = yOf(buffer.valueAt(index));
    count += 1;
  }
  if (buffer.timeAt(length - 1) < now) {
    out.points[2 * count] = box.width;
    out.points[2 * count + 1] = yOf(buffer.valueAt(length - 1));
    count += 1;
  }
  out.count = count;
}
