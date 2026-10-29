// What a person chooses a plugin from: the catalog the server told (`ClientState.catalog`), as the
// items of a menu.
import type { CatalogEntry } from "../protocol";
import type { MenuItem } from "../ui/MenuButton";

/** The formats of plugins in the order a list of them goes in: the built-ins, which are always there, first. */
const FORMATS = ["Built-in", "CLAP", "LV2", "VST3"];

/** What the catalog calls the format of a module: a chain of effects that a person saved, which is listed with the plugins and is none of them (it goes in a lane of its own, whole). */
export const MODULE_FORMAT = "Module";

/** Where a format goes in a list: one this page does not know of after those it does. */
const placeOf = (format: string): number => {
  const place = FORMATS.indexOf(format);
  return place < 0 ? FORMATS.length : place;
};

/** The plugins that are instruments, or are not, as the items of a menu: see `effectItems`. */
function itemsOf(entries: readonly CatalogEntry[], instruments: boolean): MenuItem[] {
  return entries
    .filter((entry) => entry.isInstrument === instruments)
    .sort((a, b) => placeOf(a.format) - placeOf(b.format))
    .map((entry) => ({ id: entry.id, label: entry.name, group: entry.format }));
}

/**
 * The plugins that can be an effect (the instruments cannot), as the items of a menu: each by the `id` the server knows
 * it by, under the format it is of, the formats in order and, within each, the order the server gave (which is by
 * name, the TUI's too). A module is not one: it is a chain of effects, which does not go in the chain of a lane (`freeEffectItems`).
 */
export function effectItems(entries: readonly CatalogEntry[]): MenuItem[] {
  return itemsOf(
    entries.filter((entry) => entry.format !== MODULE_FORMAT),
    false,
  );
}

/**
 * What can be added as an effect that stands by itself: the plugins that can be an effect and the modules that a person saved, which come first (they are the person's own, and a list of plugins that is long
 * would put them last of all), each under the format it is of.
 */
export function freeEffectItems(entries: readonly CatalogEntry[]): MenuItem[] {
  const modules = entries.filter((entry) => entry.format === MODULE_FORMAT && !entry.isInstrument).map((entry) => ({ id: entry.id, label: entry.name, group: entry.format }));
  return [...modules, ...effectItems(entries)];
}

/** The plugins that can be an instrument (the effects cannot), as the items of a menu, in the order of `effectItems`. */
export function instrumentItems(entries: readonly CatalogEntry[]): MenuItem[] {
  return itemsOf(entries, true);
}
