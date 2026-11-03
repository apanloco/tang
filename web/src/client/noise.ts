// The random numbers of an LFO, as the engine makes them (`plugin::chain::noise`): the mock engine's random LFOs draw the same numbers from the same hash. The two are pinned to one
// another by a table that the engine writes (`fixtures/noise.json`, read by `noise.test.ts`).

/** Chris Wellons's "lowbias32": a number of 32 bits scrambled (by its low 32 bits). */
function lowbias32(value: number): number {
  let x = value >>> 0;
  x ^= x >>> 16;
  x = Math.imul(x, 0x7feb352d);
  x ^= x >>> 15;
  x = Math.imul(x, 0x846ca68b);
  x ^= x >>> 16;
  return x >>> 0;
}

/**
 * The number of cycle `cycle` of a random LFO that has the seed `seed`, from -1 up to but not including 1: the top 24 bits of a hash of the two, which a 32-bit float holds exactly, so that the
 * number is the engine's to the last bit. The seed and the cycle go by their low 32 bits, as the engine's counters do. (The seed is offset first, since the hash of 0 is 0.)
 */
export function noise(seed: number, cycle: number): number {
  const hash = lowbias32(lowbias32(seed + 0x9e3779b9) + cycle);
  return (hash >>> 8) / 8_388_608 - 1;
}
