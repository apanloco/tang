import { describe, expect, test } from "vitest";

import type { CatalogEntry } from "../protocol";
import { builtinCatalog } from "./fixtures";
import { effectItems } from "./catalog";

const entry = (id: string, name: string, format: string, isInstrument = false): CatalogEntry => ({ id, name, format, isInstrument, params: 1, presets: 0 });

describe("the plugins a person can choose an effect from", () => {
  test("are the ones that are not instruments, each by the id the server knows it by, under the format it is of", () => {
    expect(effectItems(builtinCatalog)).toEqual([
      { id: "builtin:filter", label: "Filter", group: "Built-in" },
      { id: "builtin:reverb", label: "Reverb", group: "Built-in" },
    ]);
    expect(effectItems([])).toEqual([]);
    expect(effectItems([entry("builtin:osc", "Oscillator", "Built-in", true)])).toEqual([]);
  });

  test("are listed by format, the built-ins first and then CLAP, LV2 and VST3, in the order the server gave within each", () => {
    // (The server lists them by name, whatever the format.)
    const entries = [
      entry("vst3:Acme", "Acme Verb", "VST3"),
      entry("clap:com.acme.chorus", "Chorus", "CLAP"),
      entry("builtin:filter", "Filter", "Built-in"),
      entry("lv2:http://acme.example/delay", "Delay", "LV2"),
      entry("clap:com.acme.flanger", "Flanger", "CLAP"),
      entry("builtin:reverb", "Reverb", "Built-in"),
      entry("vst3:Zeta", "Zeta Comp", "VST3"),
    ];
    expect(effectItems(entries).map((item) => `${item.group ?? "?"}: ${item.label}`)).toEqual([
      "Built-in: Filter",
      "Built-in: Reverb",
      "CLAP: Chorus",
      "CLAP: Flanger",
      "LV2: Delay",
      "VST3: Acme Verb",
      "VST3: Zeta Comp",
    ]);
  });

  test("put a format that this page does not know of after those it does, together", () => {
    const entries = [entry("x:one", "One", "Other"), entry("builtin:filter", "Filter", "Built-in"), entry("x:two", "Two", "Other"), entry("vst3:A", "A", "VST3")];
    expect(effectItems(entries).map((item) => item.label)).toEqual(["Filter", "A", "One", "Two"]);
  });

  test("leave what was given alone", () => {
    const entries = [entry("vst3:A", "A", "VST3"), entry("builtin:filter", "Filter", "Built-in")];
    const copy = [...entries];
    effectItems(entries);
    expect(entries).toEqual(copy);
  });
});
