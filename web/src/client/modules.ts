// Modules, for the page's mock engine (steps 104 and 105): what the controller does with `Intent::SaveModule`, `Intent::SaveSelectionAsModule` and with an `AddFreeEffect` of `module:<name>`
// (`src/app/module.rs`, `apply.rs`), with a library that is kept in memory in place of a directory of files. A module is lanes of plugins and the audio cables between them: each lane has an instrument or none
// and its effects in order, each plugin as a session file says one (the plugin, the preset by its name, the values that are not the plugin's own, the mix of an effect and the volume of an instrument, the knobs pinned
// to its node, and whether the stage before an effect feeds it), and nothing else. `fixtures/modules.json`, which the controller writes, pins what is made of one to the controller's.
import type { CatalogEntry, Node, Plugin, Snapshot } from "../protocol";
import { sessionFileName } from "./filenames";

/** What the catalog's id of a module begins with: `module:Space` is the module called `Space`. */
export const MODULE_PREFIX = "module:";

/** The id that the catalog gives the module called `name`. */
export const moduleId = (name: string): string => `${MODULE_PREFIX}${name}`;

/** The name of the module that the catalog's `id` says, or null for the id of a plugin. */
export const moduleNameOf = (id: string): string | null => (id.startsWith(MODULE_PREFIX) ? id.slice(MODULE_PREFIX.length) : null);

/** An effect of a module, as its file says it. */
export interface ModuleEffect {
  readonly plugin: string;
  /** The name of the preset it has, if any. */
  readonly preset: string | null;
  /** The values that are not the plugin's own once its preset is loaded, by the name of the parameter. */
  readonly params: Readonly<Record<string, number>>;
  readonly mix: number;
  /** The names of the parameters pinned to its node, in order. */
  readonly pinned: readonly string[];
  /** Whether the stage before it feeds it (`feed = "chain"`). */
  readonly fed: boolean;
}

/** The instrument of a lane of a module, as its file says it: what an effect has but for the mix, and the lane's volume. */
export interface ModuleInstrument {
  readonly plugin: string;
  readonly preset: string | null;
  readonly params: Readonly<Record<string, number>>;
  /** The lane's volume (what the host keeps for an instrument). */
  readonly volume: number;
  readonly pinned: readonly string[];
}

/** A lane of a module: an instrument, if it has one, and the effects of its chain in order. */
export interface ModuleLane {
  readonly instrument: ModuleInstrument | null;
  readonly effects: readonly ModuleEffect[];
}

/** Where an audio cable of a module starts: the instrument of a lane (`effect` is null), or an effect of its chain. */
export interface ModuleOutput {
  readonly lane: number;
  readonly effect: number | null;
}

/** Where an audio cable of a module ends: an effect of a lane, or its second input. */
export interface ModuleInput {
  readonly lane: number;
  readonly effect: number;
  readonly side: boolean;
}

/** An audio cable of a module, from the output of one plugin to the input of an effect, at a level. */
export interface ModuleCable {
  readonly from: ModuleOutput;
  readonly to: ModuleInput;
  readonly gain: number;
}

/** A module as the library keeps it. */
export interface Module {
  readonly lanes: readonly ModuleLane[];
  readonly cables: readonly ModuleCable[];
}

/** How many plugins a module is made of, the instruments and the effects (`Module::plugin_count`). */
export const pluginCount = (module: Module): number => module.lanes.reduce((count, lane) => count + (lane.instrument ? 1 : 0) + lane.effects.length, 0);

/**
 * The name a module is saved under, as the controller makes it (`module::clean_name`): a plain file name (`sessionFileName`, which `filenames.json` pins to the controller's) without the `.toml`
 * that its file has, and not one that ends in it twice, which the library would list under another name; or the words for what is wrong with it.
 */
export function cleanModuleName(given: string): { name: string } | { error: string } {
  const made = sessionFileName(given);
  if ("error" in made) return made;
  const stem = made.file.endsWith(".toml") ? made.file.slice(0, -".toml".length) : made.file;
  if (stem.endsWith(".toml")) return { error: "the name of a module does not end in .toml: that is the extension of its file" };
  return { name: stem };
}

/** The entry that the catalog has for the module called `name` (`Catalog::entries`): no instrument, as many plugins as parameters, and no presets. */
export const moduleEntry = (name: string, module: Module): CatalogEntry => ({ id: moduleId(name), name, format: "Module", isInstrument: false, params: pluginCount(module), presets: 0 });

/**
 * The plugins and the modules of a library as the catalog lists them to a client (`Catalog::entries`): by name without regard to case, a plugin before a module that has the same name. The plugins
 * are as they came.
 */
export function withModules(plugins: readonly CatalogEntry[], library: ReadonlyMap<string, Module>): CatalogEntry[] {
  const lower = (entry: CatalogEntry): string => entry.name.toLowerCase();
  return [...plugins, ...[...library].map(([name, module]) => moduleEntry(name, module))].sort((a, b) => (lower(a) < lower(b) ? -1 : lower(a) > lower(b) ? 1 : 0));
}

/** How far a value may be from the plugin's own for the file to keep it (`f32::EPSILON`, in `overrides` of `src/app/save.rs`). */
const SAME = 1.1920929e-7;

/** The nodes of the chain of effects of the lane `node` is, or is an effect in (`module::chain_of`), or why it is none, in the controller's words: a group's bus, some other node, a lane that has no effect. */
export function chainOf(snapshot: Snapshot, node: Node): Node[] | string {
  const { nodes } = snapshot.graph;
  let lane: Node | undefined;
  if (node.kind === "instrument") lane = node;
  else if (node.kind === "effect") lane = nodes.find((n) => n.id === node.parent);
  else if (node.kind === "group") return "a module is the chain of a lane: a group's bus is not saved";
  if (lane?.kind !== "instrument") return lane?.kind === "group" ? "a module is the chain of a lane: a group's bus is not saved" : "a module is made of the chain of a lane";
  const chain = nodes.filter((n) => n.kind === "effect" && n.parent === lane.id);
  return chain.length === 0 ? "a lane that has no effect has no chain to save" : chain;
}

/** The module that the chain of the lane of `node` is (`module::to_text`): `chainOf` and then `moduleOfNodes`. The reason, in the controller's words, if there is none. */
export function moduleOf(snapshot: Snapshot, node: Node): Module | string {
  const chain = chainOf(snapshot, node);
  return typeof chain === "string" ? chain : moduleOfNodes(snapshot, chain);
}

/**
 * The module that the plugins in `chosen` are (`module::to_text_of`, step 105): the instrument of a lane is the lane's node, an effect is its own. Each lane that has one of them is a lane of the module, with the instrument
 * if it was chosen and the effects that were, in the order of their chain, each as it is now; and the audio cables that have both ends among them, in the order of their ids. What joins them to what is not chosen is the session's: the cables that
 * go in and out, the way out of a lane, the link from a stage that was not chosen (the effect after it is not fed by the chain: `fed` is false, but for the first of a lane that has no instrument, which has nothing before it
 * either way). The reason, in the controller's words, for a group, the bus of one, a modulator, a source, a lane that has no instrument, or nothing at all.
 */
export function moduleOfNodes(snapshot: Snapshot, chosen: readonly Node[]): Module | string {
  const { nodes, edges } = snapshot.graph;
  const picked = new Map<number, { instrument: boolean; effects: Set<number> }>();
  const lanePicked = (lane: number): { instrument: boolean; effects: Set<number> } => {
    let found = picked.get(lane);
    if (!found) {
      found = { instrument: false, effects: new Set() };
      picked.set(lane, found);
    }
    return found;
  };
  for (const node of chosen) {
    if (node.kind === "instrument") {
      if (node.plugin === null) return "a lane that has no instrument has none to save: choose its effects";
      lanePicked(node.id).instrument = true;
    } else if (node.kind === "effect") {
      const owner = nodes.find((n) => n.id === node.parent);
      if (owner?.kind === "group") return "a module is made of effects and instruments: a group's bus is not saved";
      if (owner?.kind !== "instrument") return "a module is made of effects and instruments: that effect is in no lane";
      lanePicked(owner.id).effects.add(node.id);
    } else if (node.kind === "group") {
      return "a module is made of effects and instruments: a group's bus is not saved";
    } else if (node.kind === "modulator") {
      return "a modulator is not part of a module yet: a module is made of effects and instruments";
    } else if (node.kind === "source") {
      return "a source of notes is not part of a module: a module is made of effects and instruments";
    } else {
      return `there is no node ${String(node.id)}`;
    }
  }
  if (picked.size === 0) return "nothing is chosen: a module is made of the effects and instruments that are";
  // (The lanes of the module are the session's, in its order; the effects of each, in the order of its chain.)
  const lanes: ModuleLane[] = [];
  const laneAt = new Map<number, number>();
  const effectAt = new Map<number, { lane: number; effect: number }>();
  for (const lane of nodes) {
    if (lane.kind !== "instrument") continue;
    const here = picked.get(lane.id);
    if (!here) continue;
    const chain = nodes.filter((n): n is Extract<Node, { kind: "effect" }> => n.kind === "effect" && n.parent === lane.id);
    const effects: ModuleEffect[] = [];
    let before: number | null = null;
    chain.forEach((effect, index) => {
      if (!here.effects.has(effect.id)) return;
      const plugin = effect.plugin;
      // (The stage before it in the module's chain is the one before it in the session's only if that was chosen too; the first of a lane that has no instrument has nothing before it either way, which the cut below leaves alone.)
      const sameStage = before === null ? index === 0 : before + 1 === index;
      const cut = !sameStage && (before !== null || here.instrument);
      effectAt.set(effect.id, { lane: lanes.length, effect: effects.length });
      effects.push({
        plugin: plugin.id,
        preset: plugin.currentPreset,
        params: valuesOver(plugin),
        mix: effect.mix,
        pinned: pinnedNames(plugin),
        fed: !cut && effect.feed === "chain",
      });
      before = index;
    });
    laneAt.set(lane.id, lanes.length);
    const plugin = lane.plugin;
    lanes.push({ instrument: here.instrument && plugin ? { plugin: plugin.id, preset: plugin.currentPreset, params: valuesOver(plugin), volume: lane.volume, pinned: pinnedNames(plugin) } : null, effects });
  }
  // (The audio cables that have both ends among what is chosen, in the order of their ids; one that starts at an instrument that was not chosen, at a group, or ends at a group's bus or the mix is the session's.)
  const cables = edges.flatMap((edge): ModuleCable[] => {
    if (edge.kind !== "cable") return [];
    const start = effectAt.get(edge.from.node);
    let from: ModuleOutput | null = start ? { lane: start.lane, effect: start.effect } : null;
    const lane = laneAt.get(edge.from.node);
    if (!from && lane !== undefined && lanes[lane]?.instrument) from = { lane, effect: null };
    const end = effectAt.get(edge.to.node);
    if (!from || !end) return [];
    return [{ from, to: { lane: end.lane, effect: end.effect, side: edge.to.port.type === "audioInSide" }, gain: edge.gain }];
  });
  return { lanes, cables };
}

/** The values of a plugin that are not its own once its preset is loaded, by the name of the parameter (`overrides` of `src/app/save.rs`). */
function valuesOver(plugin: Plugin): Record<string, number> {
  return Object.fromEntries(plugin.params.filter((p) => Math.abs(p.value - p.default) > SAME).map((p) => [p.name, p.value]));
}

/** The names of the parameters pinned to a plugin's node, in order (`pinned_names`). */
function pinnedNames(plugin: Plugin): string[] {
  return plugin.pinned.flatMap((index) => plugin.params.filter((p) => p.index === index).map((p) => p.name));
}
