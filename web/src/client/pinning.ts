// Which parameters of a plugin a node shows as knobs, and how pinning changes that (docs/web-ui.md §4.4). The session keeps a list of
// the parameters pinned to a plugin (`Plugin.pinned`, by the plugin's own indices, in the order they were pinned); with none pinned a
// node shows the first few of the plugin's, and the target of a cable is shown as well for as long as the cable is there. Pure: a
// plugin in, indices out, so that the canvas, the panel and the editor agree on what a node shows.
import { PINNED_LIMIT } from "../protocol";
import type { Plugin } from "../protocol";

/** How many of a plugin's parameters a node shows as knobs when none is pinned to it. */
export const DEFAULT_KNOBS = 8;

/**
 * The parameters a node shows as knobs for its plugin: the ones pinned to it, in the order they were pinned, or the first few of the
 * plugin's if none are (a pin that the plugin has no parameter for, as a session of another version of it may have, is left out).
 */
export function shownParams(plugin: Plugin): number[] {
  const pinned = plugin.pinned.filter((index) => plugin.params.some((param) => param.index === index));
  return pinned.length > 0 ? pinned : plugin.params.slice(0, DEFAULT_KNOBS).map((param) => param.index);
}

/** The knobs a node draws: the ones it shows (`shownParams`) and, after them, every parameter a modulator drives that is not among them, in the plugin's order. */
export function knobsOf(plugin: Plugin | null, modulated: ReadonlySet<number>): number[] {
  if (!plugin) return [];
  const shown = shownParams(plugin);
  const driven = plugin.params.map((param) => param.index).filter((index) => modulated.has(index) && !shown.includes(index));
  return [...shown, ...driven];
}

/** What asking for a parameter to be pinned or taken off comes to: the list to ask the server to keep, that it is so already, or why it cannot be. */
export type Pinning = { list: number[] } | { unchanged: true } | { refused: string };

/**
 * The list to pin so that the parameter `index` of `plugin` is shown as a knob (`pinned`) or is not, from what the node shows now: a
 * node with nothing pinned has the first few as its list, so pinning one more keeps them, and taking one off leaves the others.
 * Taking off the last one leaves an empty list, which gives the node back to the first few.
 */
export function pinnedWith(plugin: Plugin, index: number, pinned: boolean): Pinning {
  const shown = shownParams(plugin);
  if (!pinned) return shown.includes(index) ? { list: shown.filter((shownIndex) => shownIndex !== index) } : { unchanged: true };
  if (shown.includes(index)) return { unchanged: true };
  if (plugin.params.every((param) => param.index !== index)) return { refused: `the plugin has no parameter ${String(index)}` };
  if (shown.length >= PINNED_LIMIT) return { refused: `a node can show at most ${String(PINNED_LIMIT)} parameters` };
  return { list: [...shown, index] };
}
