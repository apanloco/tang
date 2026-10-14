// Reading the session in the client's store: the parts of it a view shows, found by the id of
// the node they are of. Pure, on the state, so that a view can hand one to a store hook.
import { WAVEFORMS } from "../protocol";
import type { EdgeId, Gain, ModulatorField, Node, NodeId, NoteRange, Param, Preset } from "../protocol";
import type { ClientState } from "./connection";
import type { Stages } from "./envelope";
import { displayNames } from "./names";

/** The node of the session with this id, if the client has a session and it has one. */
export function nodeById(state: ClientState, id: NodeId): Node | undefined {
  return state.snapshot?.graph.nodes.find((node) => node.id === id);
}

/** Parameter `index` (the plugin's own number for it) of the plugin in node `id`. */
export function paramOf(state: ClientState, id: NodeId, index: number): Param | undefined {
  const node = nodeById(state, id);
  if (node?.kind !== "instrument" && node?.kind !== "effect") return undefined;
  return node.plugin?.params.find((param) => param.index === index);
}

/**
 * The presets that can be loaded into the plugin of node `id`, and the name of the one that was loaded last: those of a lane's
 * instrument or of an effect in a lane's chain. Null for a node that has no such plugin: a bus effect has presets, but the audio thread
 * loads one by a lane's slot (the controller says "a bus effect has no presets to load"), so it is not offered one. A plugin with no
 * presets has an empty list.
 */
export function presetsOf(state: ClientState, id: NodeId): { presets: readonly Preset[]; current: string | null } | null {
  const node = nodeById(state, id);
  if (node?.kind !== "instrument" && node?.kind !== "effect") return null;
  if (!node.plugin) return null;
  if (node.kind === "effect" && state.snapshot?.graph.nodes.find((n) => n.id === node.parent)?.kind === "group") return null;
  return { presets: node.plugin.presets, current: node.plugin.currentPreset };
}

/** Where an effect is in its chain: its place, from 0 (the first, the nearest the instrument or the group), and how many effects the chain has. */
export interface ChainPlace {
  index: number;
  count: number;
}

/**
 * Where the effect `id` is in its chain, a lane's or a group's bus: the effects that have the same parent, in the order of the graph, which is the
 * order of the audio path. Null for a node that is no effect, a node that is not there, and before there is a session.
 */
export function chainPlaceOf(state: ClientState, id: NodeId): ChainPlace | null {
  const nodes = state.snapshot?.graph.nodes;
  const found = nodes?.find((node) => node.id === id);
  if (!nodes || found?.kind !== "effect") return null;
  const chain = nodes.filter((node) => node.kind === "effect" && node.parent === found.parent);
  return { index: chain.indexOf(found), count: chain.length };
}

/** Whether two answers of `chainPlaceOf` are the same place in a chain of the same length. */
export const sameChainPlace = (a: ChainPlace | null, b: ChainPlace | null): boolean => a === b || (a !== null && b !== null && a.index === b.index && a.count === b.count);

/** The parameters of a plugin to look through: what each is called and its own index, with whose they are. */
export interface ParamList {
  /** What the canvas calls the node. */
  name: string;
  /** What kind of plugin it is (`Built-in`, `CLAP`, ...). */
  format: string;
  params: readonly { index: number; name: string }[];
}

/**
 * Every parameter of the plugin of node `id`, by name and the plugin's own index, for a person to find one in: the plugin
 * of an instrument or of any effect, a group's bus included (a bus effect has parameters to turn, whatever it cannot be
 * given as a preset). Null for a node that has no plugin. It holds no values, so that it is the same list, by `sameParamList`,
 * while they change.
 */
export function paramListOf(state: ClientState, id: NodeId): ParamList | null {
  const graph = state.snapshot?.graph;
  const node = graph?.nodes.find((candidate) => candidate.id === id);
  if (!graph || (node?.kind !== "instrument" && node?.kind !== "effect") || !node.plugin) return null;
  return {
    name: displayNames(graph).get(id) ?? node.name,
    format: node.plugin.format,
    params: node.plugin.params.map((param) => ({ index: param.index, name: param.name })),
  };
}

/** How many parameters the plugin of node `id` has: none for a node that has no plugin. (Cheaper than the list, for what only asks whether there are any.) */
export function paramCountOf(state: ClientState, id: NodeId): number {
  const node = nodeById(state, id);
  return (node?.kind === "instrument" || node?.kind === "effect" ? node.plugin?.params.length : undefined) ?? 0;
}

/** Whether two answers of `paramListOf` are the same list of the same plugin. */
export function sameParamList(a: ParamList | null, b: ParamList | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.name === b.name &&
    a.format === b.format &&
    a.params.length === b.params.length &&
    a.params.every((param, position) => param.index === b.params[position]?.index && param.name === b.params[position].name)
  );
}

/** The keys a lane takes: null if every note, undefined if the node is not a lane (an empty lane is one). */
export function rangeOf(state: ClientState, id: NodeId): NoteRange | null | undefined {
  const node = nodeById(state, id);
  return node?.kind === "instrument" ? node.range : undefined;
}

/** The value of a host-side gain of a node, if the node has that gain (`withGain` in reducers.ts is the other way round). */
export function gainOf(state: ClientState, id: NodeId, gain: Gain): number | undefined {
  const node = nodeById(state, id);
  if (gain === "volume" && node?.kind === "instrument") return node.volume;
  if (gain === "mix" && node?.kind === "effect") return node.mix;
  if (gain === "groupVolume" && node?.kind === "group") return node.volume;
  if (gain === "groupPan" && node?.kind === "group") return node.pan;
  return undefined;
}

/**
 * The value of a setting of the modulator in node `id`, if it has that setting (`withSetting` in
 * reducers.ts is the other way round): an LFO's rate, or its waveform as the index the intent
 * takes, or a stage of an envelope.
 */
export function modulatorFieldOf(state: ClientState, id: NodeId, field: ModulatorField): number | undefined {
  const node = nodeById(state, id);
  if (node?.kind !== "modulator") return undefined;
  const { source } = node;
  if (source.type === "lfo") {
    if (field === "rate") return source.rate;
    if (field === "waveform") return WAVEFORMS.indexOf(source.waveform);
  }
  if (source.type === "envelope") {
    if (field === "attack" || field === "decay" || field === "sustain" || field === "release") return source[field];
  }
  return undefined;
}

/** The four settings of the envelope `id`: the times of its attack, decay and release and the level of its sustain. Null for a node that is no envelope. */
export function envelopeStagesOf(state: ClientState, id: NodeId): Stages | null {
  const node = nodeById(state, id);
  if (node?.kind !== "modulator" || node.source.type !== "envelope") return null;
  const { attack, decay, sustain, release } = node.source;
  return { attack, decay, sustain, release };
}

/** Whether two answers of `envelopeStagesOf` are the same four settings. */
export const sameStages = (a: Stages | null, b: Stages | null): boolean =>
  a === b || (a !== null && b !== null && a.attack === b.attack && a.decay === b.decay && a.sustain === b.sustain && a.release === b.release);

/** How deep the modulation cable with this id pulls, if there is one. */
export function cableDepthOf(state: ClientState, id: EdgeId): number | undefined {
  const cable = state.snapshot?.graph.edges.find((edge) => edge.kind === "modulation" && edge.id === id);
  return cable?.kind === "modulation" ? cable.amount : undefined;
}
