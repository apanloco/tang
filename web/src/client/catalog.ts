// What a person chooses a plugin from: the catalog the server told (`ClientState.catalog`), as the
// items of a menu.
import type { CatalogEntry } from "../protocol";
import type { MenuItem } from "../ui/MenuButton";

/** The formats of plugins in the order a list of them goes in: the built-ins, which are always there, first. */
const FORMATS = ["Built-in", "CLAP", "LV2", "VST3"];

/** Where a format goes in a list: one this page does not know of after those it does. */
const placeOf = (format: string): number => {
  const place = FORMATS.indexOf(format);
  return place < 0 ? FORMATS.length : place;
};

/**
 * The plugins that can be an effect (the instruments cannot), as the items of a menu: each by the `id` the server knows
 * it by, under the format it is of, the formats in order and, within each, the order the server gave (which is by
 * name, the TUI's too).
 */
export function effectItems(entries: readonly CatalogEntry[]): MenuItem[] {
  return entries
    .filter((entry) => !entry.isInstrument)
    .sort((a, b) => placeOf(a.format) - placeOf(b.format))
    .map((entry) => ({ id: entry.id, label: entry.name, group: entry.format }));
}
