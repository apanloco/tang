// A cable dragged on the canvas, as the session sees it: React Flow reports a connection between two
// handles of two nodes (ids as text, handles by the names `handleOf` gave them), and the cable it stands for is
// a modulator, a node and a port of it: a parameter of the plugin in it, or a setting of it if it is a modulator,
// or the depth of one of the modulator's cables (docs/web-ui.md §9.3). Whether it can be drawn is not decided here: the controller decides (`AddModEdge`); the
// list it is looked up in (`drivablesOf`, in the data of each modulator's node) is the client's reading of the
// same rule, so that the canvas can show the ends that take the cable before it is dropped, and not drop it on
// one that does not.
import { createContext } from "react";

import type { NodeId, Port } from "../../protocol";
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

/**
 * The ports that take the cable that is being dragged (`keyOf`), for the ports to say so; null while no cable is
 * being dragged. (A port is a handle in a node of its own, which cannot be told by the canvas.)
 */
export const ConnectingContext = createContext<ReadonlySet<string> | null>(null);
