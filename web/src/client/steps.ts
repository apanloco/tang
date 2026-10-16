// What a step sequencer puts out, as the engine works it out (`plugin::chain::steps_output`): the mock engine's step sequencers put out the same numbers. The two are pinned to one another by a
// table that the engine writes (`fixtures/steps.json`, read by `steps.test.ts`).

/**
 * What a step sequencer puts out at `position`: the step it is at as the whole part, how far into it as the fraction, from 0 to `length`, which it goes round at. The value of the step, held for the
 * part of it before its last `glide` (0 to 1), which is spent going to the value of the next step (the first, after the last) in a straight line. A length of none is one, and one of more steps than there
 * are is all of them; a place that is not a number is the beginning.
 */
export function stepsOutput(values: readonly number[], length: number, position: number, glide: number): number {
  const steps = Math.min(Math.max(Math.trunc(length), 1), values.length);
  const rest = position % steps;
  const place = Number.isFinite(position) ? (rest < 0 ? rest + steps : rest) : 0;
  const step = Math.min(Math.floor(place), steps - 1);
  const within = place - step;
  const here = values[step] ?? 0;
  const mix = Math.min(Math.max(glide, 0), 1);
  if (mix <= 0 || within <= 1 - mix) return here;
  const next = values[(step + 1) % steps] ?? 0;
  return here + (next - here) * ((within - (1 - mix)) / mix);
}
