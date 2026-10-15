// What the scope draws, apart from any drawing (docs/web-ui.md §6): where in a block of the output a picture of the wave starts, so that a wave
// stands still, and the points of a wave and of a spectrum on the pixels of a canvas. Pure functions of numbers; `ui/scope-draw.ts` draws what they plot.

/** The room a picture has: the pixels across and down, and how many of them the line keeps clear of the top and the bottom. */
export interface PlotSize {
  width: number;
  height: number;
  pad: number;
}

/**
 * Where a picture of `length` samples of `samples` starts: at the first place where the wave goes up through `level` after having been
 * under it by `hysteresis` (so that a wobble round the level is not a wave), among the places that leave room for the whole picture. That is
 * what a scope's trigger does, and a wave that goes round again and again is drawn from the same point of its turn every time, and stands
 * still. With no such place (silence, a constant, a wave that stays on one side) it is the latest picture that fits.
 */
export function triggerStart(samples: Float32Array, length: number, level = 0, hysteresis = 0.02): number {
  const last = samples.length - length;
  if (last <= 0) return 0;
  let armed = false;
  for (let at = 0; at <= last; at += 1) {
    const sample = samples[at] ?? 0;
    if (sample < level - hysteresis) armed = true;
    else if (armed && sample >= level) return at;
  }
  return last;
}

/**
 * The points of the wave of `samples[start .. start + length)` on `size`: the first at the left edge and the last at the right one (a picture of
 * one sample is in the middle), +1 at the top of the room that the padding leaves and -1 at the bottom, what is beyond full scale at the edges. Written
 * into `out` as `x, y` pairs; how many there are is returned: no more than `length`, the block has from `start` and `out` has room for.
 */
export function plotWave(samples: Float32Array, start: number, length: number, size: PlotSize, out: Float32Array): number {
  const count = Math.max(0, Math.min(length, samples.length - start, Math.floor(out.length / 2)));
  const room = size.height - 2 * size.pad;
  for (let point = 0; point < count; point += 1) {
    const value = Math.min(1, Math.max(-1, samples[start + point] ?? 0));
    out[2 * point] = count === 1 ? size.width / 2 : (point * size.width) / (count - 1);
    out[2 * point + 1] = size.pad + ((1 - value) / 2) * room;
  }
  return count;
}

/** What a picture of a spectrum covers: the lowest and the highest frequency across it, and the levels at its bottom and its top. */
export interface SpectrumRange {
  minHz: number;
  maxHz: number;
  floorDb: number;
  topDb: number;
}

/**
 * The points of a spectrum (`levels` in dB of the frequencies from nothing to half of `sampleRate`, as `Spectrum.decibels` gives them) on `size`: a
 * point for each pixel across, the frequencies spread over the decades of `range` (equal ratios are equally far apart, as a musician hears them). A
 * pixel that is narrower than the space between two levels has the level between them, drawn as a line; one that holds more of them has the highest of
 * them (each level is in one pixel, the one that its frequency is in), so that a peak is not hidden at the high end, where there are many to a pixel. Written into `out` as `x, y` pairs, how many is returned.
 */
export function plotSpectrum(levels: Float32Array, sampleRate: number, size: PlotSize, range: SpectrumRange, out: Float32Array): number {
  const bins = levels.length - 1;
  const binHz = sampleRate / 2 / bins;
  const count = Math.max(0, Math.min(Math.floor(size.width) + 1, Math.floor(out.length / 2)));
  const decades = Math.log(range.maxHz / range.minHz);
  const room = size.height - 2 * size.pad;
  const at = (column: number): number => range.minHz * Math.exp((decades * column) / size.width);
  const inBins = (hz: number): number => Math.min(bins, Math.max(0, hz / binHz));
  for (let column = 0; column < count; column += 1) {
    const low = inBins(at(column - 0.5));
    const high = inBins(at(column + 0.5));
    let level: number;
    if (high - low >= 1) {
      level = Number.NEGATIVE_INFINITY;
      for (let bin = Math.ceil(low); bin <= Math.floor(high); bin += 1) level = Math.max(level, levels[bin] ?? range.floorDb);
    } else {
      const middle = (low + high) / 2;
      const bin = Math.floor(middle);
      const along = middle - bin;
      level = (levels[bin] ?? range.floorDb) * (1 - along) + (levels[Math.min(bins, bin + 1)] ?? range.floorDb) * along;
    }
    const height = Math.min(1, Math.max(0, (level - range.floorDb) / (range.topDb - range.floorDb)));
    out[2 * column] = column;
    out[2 * column + 1] = size.pad + (1 - height) * room;
  }
  return count;
}

/** The columns, across a picture of `width` pixels, where the frequencies `hzs` are on the decades of `range`: for the lines of a spectrum's graticule and the labels under them. */
export function frequencyColumns(hzs: readonly number[], range: SpectrumRange, width: number): number[] {
  const decades = Math.log(range.maxHz / range.minHz);
  return hzs.map((hz) => (width * Math.log(hz / range.minHz)) / decades);
}

/** The rows, down a picture, where the levels `dbs` are between the floor and the top level of `range`, with the padding that the line keeps clear of the edges. */
export function levelRows(dbs: readonly number[], range: SpectrumRange, size: PlotSize): number[] {
  const room = size.height - 2 * size.pad;
  return dbs.map((db) => size.pad + (1 - (db - range.floorDb) / (range.topDb - range.floorDb)) * room);
}
