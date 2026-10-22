// A cable dragged on the canvas, as the session sees it: React Flow reports a connection between two
// handles of two nodes (ids as text, handles by the names `handleOf` gave them), and the cable it stands for is
// a modulator, a node and a port of it: a parameter of the plugin in it, or a setting of it if it is a modulator,
// or the depth of one of the modulator's cables (docs/web-ui.md §9.3); or an audio cable, from the output of one node to the input of another
// (docs/patch-graph.md §11.6); or a cable of notes, from a source to a lane. Whether it can be drawn is not decided here: the controller decides (`AddModEdge`,
// `AddCable`); the list it is looked up in (`drivablesOf`, in the data of each modulator's node, `cableTakers` for the audio and `noteTakers` for the notes) is the
// client's reading of the same rule, so that the canvas can show the ends that take the cable before it is dropped, and not drop it on one that does not.
import { createContext } from "react";

import { noteTakers } from "../../client/noteWiring";
import type { NotesEnds } from "../../client/noteWiring";
import { cableTakers } from "../../client/wiring";
import type { Graph, NodeId, Port } from "../../protocol";
import { handleOf, paramHandle, SETTING_HANDLES } from "./toFlow";
import type { PatchNode } from "./toFlow";

/** What React Flow says of a connection. */
interface Connection {
  source: string;
  target: string;
  sourceHandle?: string | null;
  targetHandle?: string | null;
}

/**
 * The ends of a cable: from a modulator to a port of a node, which is a parameter of its plugin (by the plugin's own index), a setting of it, or the depth of
 * one of its cables (by the id of the cable, which starts at the node).
 */
export interface CableEnds {
  modulator: NodeId;
  node: NodeId;
  port: Port;
}

const WHOLE = /^\d+$/;
const PARAM_HANDLE = /^param-(\d+)$/;
const DEPTH_HANDLE = /^depth-(\d+)$/;

/** The port that the handle `handle` of a node is, if it is one that a cable ends at: a parameter's, or a modulator's rate or stage, or the depth of a cable of it. */
function portAt(handle: string): Port | null {
  const param = PARAM_HANDLE.exec(handle)?.[1];
  if (param !== undefined) return { type: "param", index: Number(param) };
  const depth = DEPTH_HANDLE.exec(handle)?.[1];
  if (depth !== undefined) return { type: "depth", edge: Number(depth) };
  const setting = SETTING_HANDLES.find((name) => name === handle);
  return setting === undefined ? null : { type: setting };
}

/** The cable a connection asks for; null if it does not go from a modulator's output to a parameter's port, a modulator's setting or the depth of a cable. */
export function cableOf(connection: Connection): CableEnds | null {
  const { source, target, sourceHandle, targetHandle } = connection;
  if (sourceHandle !== "modOut" || !targetHandle || ![source, target].every((text) => WHOLE.test(text))) return null;
  const port = portAt(targetHandle);
  return port === null ? null : { modulator: Number(source), node: Number(target), port };
}

/** The ends of an audio cable: from the node whose output it starts at to the node whose input it ends at (an effect's, a group's bus or the mix). */
export interface AudioEnds {
  from: NodeId;
  to: NodeId;
}

/** The audio cable a connection asks for; null if it does not go from an output of audio to an input of audio, or the nodes are not named by whole numbers. */
export function audioCableOf(connection: Connection): AudioEnds | null {
  const { source, target, sourceHandle, targetHandle } = connection;
  if (sourceHandle !== "audioOut" || targetHandle !== "audioIn" || ![source, target].every((text) => WHOLE.test(text))) return null;
  return { from: Number(source), to: Number(target) };
}

/** The cable of notes a connection asks for: from the source whose output of events it starts at to the lane whose input of events it ends at; null if it does not go from one to the other, or the nodes are not named by whole numbers. */
export function notesCableOf(connection: Connection): NotesEnds | null {
  const { source, target, sourceHandle, targetHandle } = connection;
  if (sourceHandle !== "eventsOut" || targetHandle !== "eventsIn" || ![source, target].every((text) => WHOLE.test(text))) return null;
  return { source: Number(source), lane: Number(target) };
}

/** The key of a port of a node, by the name of its handle, which is how the ends that take a cable are listed. */
export const keyOf = (node: NodeId, handle: string): string => `${String(node)}:${handle}`;

/** The ports that the modulator `modulator` can still be drawn to, as `keyOf` says: none if it is not a modulator of the canvas. */
export function drawableFrom(nodes: readonly PatchNode[], modulator: NodeId): Set<string> {
  const found = nodes.find((node) => node.data.node === modulator);
  return new Set(
    (found?.data.targets ?? []).flatMap((group) => [
      ...group.params.map((param) => keyOf(group.node, paramHandle(param.index))),
      ...group.fields.map((field) => keyOf(group.node, handleOf(field.port))),
    ]),
  );
}

/** Whether the cable can be drawn: the modulator can still drive that port. */
export function canDraw(nodes: readonly PatchNode[], ends: CableEnds): boolean {
  return drawableFrom(nodes, ends.modulator).has(keyOf(ends.node, handleOf(ends.port)));
}

/** The inputs of audio that take a cable from the output of `from` now, as `keyOf` says them: where a cable could be drawn to, and not where one is drawn already or the audio would go round. */
export function audioTakersOf(graph: Graph, from: NodeId): Set<string> {
  return new Set(cableTakers(graph, from).map((node) => keyOf(node, handleOf({ type: "audioIn" }))));
}

/** Whether the audio cable can be drawn: the controller would take it, which `cableTakers` reads from the graph. */
export function canDrawAudio(graph: Graph, ends: AudioEnds): boolean {
  return cableTakers(graph, ends.from).includes(ends.to);
}

/** The inputs of events that take a cable of notes from the source `from` now, as `keyOf` says them: the lanes that it has none to yet. */
export function notesTakersOf(graph: Graph, from: NodeId): Set<string> {
  return new Set(noteTakers(graph, from).map((lane) => keyOf(lane, handleOf({ type: "eventsIn" }))));
}

/** Whether the cable of notes can be drawn: the controller would take it, which `noteTakers` reads from the graph. */
export function canDrawNotes(graph: Graph, ends: NotesEnds): boolean {
  return noteTakers(graph, ends.source).includes(ends.lane);
}

/** The handle that a drag starts at, as React Flow says it. */
interface Start {
  nodeId: string | null;
  handleType: "source" | "target" | null;
  handleId: string | null;
}

/**
 * The ports that take the cable that is dragged from `start` (`keyOf`): the ports that a modulator can still drive for a drag from its output, the inputs of audio that take audio
 * from the output of a node for a drag from that, the inputs of events of the lanes that take notes from a source for a drag from its output; null where no cable starts (an input,
 * a port of a control, no node, no session to say what is where).
 */
export function takersOf(graph: Graph | null, nodes: readonly PatchNode[], { nodeId, handleType, handleId }: Start): Set<string> | null {
  if (handleType !== "source" || nodeId === null || !WHOLE.test(nodeId)) return null;
  if (handleId === "modOut") return drawableFrom(nodes, Number(nodeId));
  if (handleId === "audioOut") return graph ? audioTakersOf(graph, Number(nodeId)) : null;
  if (handleId === "eventsOut") return graph ? notesTakersOf(graph, Number(nodeId)) : null;
  return null;
}

/** Whether the connection that is dragged is a cable that can be drawn: an audio cable or a cable of notes that the controller would take, or a cable of modulation that its modulator can still drive. */
export function canConnect(graph: Graph | null, nodes: readonly PatchNode[], connection: Connection): boolean {
  const audio = audioCableOf(connection);
  if (audio !== null) return graph !== null && canDrawAudio(graph, audio);
  const notes = notesCableOf(connection);
  if (notes !== null) return graph !== null && canDrawNotes(graph, notes);
  const ends = cableOf(connection);
  return ends !== null && canDraw(nodes, ends);
}

/**
 * The ports that take the cable that is being dragged (`keyOf`), for the ports to say so; null while no cable is
 * being dragged. (A port is a handle in a node of its own, which cannot be told by the canvas.)
 */
export const ConnectingContext = createContext<ReadonlySet<string> | null>(null);
