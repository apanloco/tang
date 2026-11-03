// The spectrum of a block of samples, for the scope's second picture (docs/web-ui.md §6): a Hann window, a radix-2 FFT and the levels of the
// frequencies in decibels. The page does it and not the server: the server sends the samples that the scope draws, and a spectrum of them
// is a drawing too.

/** The level of a frequency with nothing in it: as low as the picture goes, and not minus infinity. */
export const FLOOR_DB = -120;

/**
 * The analyser of blocks of one length: the tables that it needs (the window, the turns of the FFT, the order of the samples) are made once, and
 * the work arrays are its own, so that a block is analysed without allocating.
 */
export class Spectrum {
  readonly size: number;
  readonly #window: Float64Array;
  readonly #cos: Float64Array;
  readonly #sin: Float64Array;
  readonly #reverse: Uint32Array;
  readonly #re: Float64Array;
  readonly #im: Float64Array;

  /** `size` is the length of the blocks: a power of two, 4 or more. */
  constructor(size: number) {
    if (!Number.isInteger(size) || size < 4 || (size & (size - 1)) !== 0) throw new RangeError(`a block of ${String(size)} samples is not a power of two of 4 or more`);
    this.size = size;
    // (The periodic Hann window: a sine that goes round a whole number of times in the block has only the two frequencies beside its own to leak into.)
    this.#window = Float64Array.from({ length: size }, (_, n) => 0.5 - 0.5 * Math.cos((2 * Math.PI * n) / size));
    this.#cos = Float64Array.from({ length: size / 2 }, (_, t) => Math.cos((2 * Math.PI * t) / size));
    this.#sin = Float64Array.from({ length: size / 2 }, (_, t) => Math.sin((2 * Math.PI * t) / size));
    const bits = Math.log2(size);
    this.#reverse = Uint32Array.from({ length: size }, (_, n) => {
      let reversed = 0;
      for (let bit = 0; bit < bits; bit += 1) reversed |= ((n >> bit) & 1) << (bits - 1 - bit);
      return reversed;
    });
    this.#re = new Float64Array(size);
    this.#im = new Float64Array(size);
  }

  /**
   * The levels of the frequencies of `samples`, from nothing to half the sample rate (`size / 2 + 1` of them, the sample
   * rate over `size` apart), in decibels: 0 dB is a sine of full scale, which a window and a block that go round it a
   * whole number of times show at its own frequency. Written into `out` if it is given.
   */
  decibels(samples: Float32Array, out: Float32Array = new Float32Array(this.size / 2 + 1)): Float32Array {
    const size = this.size;
    if (samples.length !== size) throw new RangeError(`this analyser is of blocks of ${String(size)} samples, and this one has ${String(samples.length)}`);
    const re = this.#re;
    const im = this.#im;
    for (let n = 0; n < size; n += 1) {
      const place = this.#reverse[n] ?? 0;
      re[place] = (samples[n] ?? 0) * (this.#window[n] ?? 0);
      im[place] = 0;
    }
    for (let half = 1; half < size; half *= 2) {
      const stride = size / (2 * half);
      for (let start = 0; start < size; start += 2 * half) {
        for (let k = 0; k < half; k += 1) {
          const turn = k * stride;
          const wr = this.#cos[turn] ?? 1;
          const wi = -(this.#sin[turn] ?? 0);
          const a = start + k;
          const b = a + half;
          const br = re[b] ?? 0;
          const bi = im[b] ?? 0;
          const tr = br * wr - bi * wi;
          const ti = br * wi + bi * wr;
          re[b] = (re[a] ?? 0) - tr;
          im[b] = (im[a] ?? 0) - ti;
          re[a] = (re[a] ?? 0) + tr;
          im[a] = (im[a] ?? 0) + ti;
        }
      }
    }
    // (A sine of full scale, windowed, is a quarter of the block's length in the frequency it is at.)
    const full = size / 4;
    for (let bin = 0; bin <= size / 2; bin += 1) {
      const magnitude = Math.hypot(re[bin] ?? 0, im[bin] ?? 0) / full;
      out[bin] = magnitude > 0 ? Math.max(FLOOR_DB, 20 * Math.log10(magnitude)) : FLOOR_DB;
    }
    return out;
  }
}
