// Chains of effects saved as modules (step 104), for the page's mock engine: what the controller does with `Intent::SaveModule` and with an `AddFreeEffect` of `module:<name>`
// (`src/app/module.rs`, `apply.rs`), with a library that is kept in memory in place of a directory of files. A module is the chain of one lane: its effects in order, each as a session file
// says an effect (the plugin, the preset by its name, the values that are not the plugin's own, the mix, the knobs pinned to its node, and whether the stage before it feeds it) and the audio
// cables that are between those effects, and nothing else. `fixtures/modules.json`, which the controller writes, pins what is made of one to the controller's.
import type { CatalogEntry, Node, Snapshot } from "../protocol";
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

/** An audio cable of a module: from the output of one effect of the chain to the input of another, or to its second input, at a level. */
export interface ModuleCable {
  readonly from: number;
  readonly to: number;
  readonly side: boolean;
  readonly gain: number;
}

/** A module as the library keeps it. */
export interface Module {
  readonly effects: readonly ModuleEffect[];
  readonly cables: readonly ModuleCable[];
}

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

/** The entry that the catalog has for the module called `name` (`Catalog::entries`): no instrument, as many effects as parameters, and no presets. */
export const moduleEntry = (name: string, module: Module): CatalogEntry => ({ id: moduleId(name), name, format: "Module", isInstrument: false, params: module.effects.length, presets: 0 });

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

/**
 * The module that the chain of the lane of `node` is (`module::to_text`): `node` is a lane, or an effect of its chain. The reason, in the controller's words, if it is a group or the bus of one, some other
 * node, or a lane that has no effect.
 */
export function moduleOf(snapshot: Snapshot, node: Node): Module | string {
  const { nodes, edges } = snapshot.graph;
  let lane: Node | undefined;
  if (node.kind === "instrument") lane = node;
  else if (node.kind === "effect") lane = nodes.find((n) => n.id === node.parent);
  else if (node.kind === "group") return "a module is the chain of a lane: a group's bus is not saved";
  if (lane?.kind !== "instrument") return lane?.kind === "group" ? "a module is the chain of a lane: a group's bus is not saved" : "a module is made of the chain of a lane";
  const chain = nodes.filter((n): n is Extract<Node, { kind: "effect" }> => n.kind === "effect" && n.parent === lane.id);
  if (chain.length === 0) return "a lane that has no effect has no chain to save";
  const effects = chain.map((effect): ModuleEffect => {
    const plugin = effect.plugin;
    return {
      plugin: plugin.id,
      preset: plugin.currentPreset,
      params: Object.fromEntries(plugin.params.filter((p) => Math.abs(p.value - p.default) > SAME).map((p) => [p.name, p.value])),
      mix: effect.mix,
      pinned: plugin.pinned.flatMap((index) => plugin.params.filter((p) => p.index === index).map((p) => p.name)),
      fed: effect.feed === "chain",
    };
  });
  const place = (id: number): number => chain.findIndex((effect) => effect.id === id);
  // (The cables between two effects of the chain, in the order of their ids; one that starts at the lane's instrument, or has an end outside the chain, is the session's.)
  const cables = edges.flatMap((edge): ModuleCable[] => {
    if (edge.kind !== "cable") return [];
    const [from, to] = [place(edge.from.node), place(edge.to.node)];
    return from < 0 || to < 0 ? [] : [{ from, to, side: edge.to.port.type === "audioInSide", gain: edge.gain }];
  });
  return { effects, cables };
}
