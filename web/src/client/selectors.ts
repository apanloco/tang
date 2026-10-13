// Reading the session in the client's store: the parts of it a view shows, found by the id of
// the node they are of. Pure, on the state, so that a view can hand one to a store hook.
import { WAVEFORMS } from "../protocol";
import type { EdgeId, Gain, ModulatorField, Node, NodeId, Param, Preset } from "../protocol";
import type { ClientState } from "./connection";

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

/** How deep the modulation cable with this id pulls, if there is one. */
export function cableDepthOf(state: ClientState, id: EdgeId): number | undefined {
  const cable = state.snapshot?.graph.edges.find((edge) => edge.kind === "modulation" && edge.id === id);
  return cable?.kind === "modulation" ? cable.amount : undefined;
}
