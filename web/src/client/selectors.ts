// Reading the session in the client's store: the parts of it a view shows, found by the id of
// the node they are of. Pure, on the state, so that a view can hand one to a store hook.
import type { Gain, Node, NodeId, Param } from "../protocol";
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

/** The value of a host-side gain of a node, if the node has that gain (`withGain` in reducers.ts is the other way round). */
export function gainOf(state: ClientState, id: NodeId, gain: Gain): number | undefined {
  const node = nodeById(state, id);
  if (gain === "volume" && node?.kind === "instrument") return node.volume;
  if (gain === "mix" && node?.kind === "effect") return node.mix;
  if (gain === "groupVolume" && node?.kind === "group") return node.volume;
  if (gain === "groupPan" && node?.kind === "group") return node.pan;
  return undefined;
}
