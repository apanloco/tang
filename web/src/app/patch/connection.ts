// A cable dragged on the canvas, as the session sees it: React Flow reports a connection between two
// handles of two nodes (ids as text, handles by the names `handleOf` gave them), and the cable it stands for is
// a modulator, a plugin node and one of its parameters (docs/web-ui.md §9.3). Whether it can be drawn is not
// decided here: the controller decides (`AddModEdge`); the list it is looked up in (`drivablesOf`, in the data
// of each modulator's node) is the client's reading of the same rule, so that the canvas can show the ends that take
// the cable before it is dropped, and not drop it on one that does not.
import { createContext } from "react";

import type { NodeId } from "../../protocol";
import type { PatchNode } from "./toFlow";

/** What React Flow says of a connection. */
interface Connection {
  source: string;
  target: string;
  sourceHandle?: string | null;
  targetHandle?: string | null;
}

/** The ends of a cable: from a modulator to a parameter of the plugin in a node, by the plugin's own index. */
export interface CableEnds {
  modulator: NodeId;
  node: NodeId;
  param: number;
}

const WHOLE = /^\d+$/;

/** The cable a connection asks for; null if it does not go from a modulator's output to a parameter's port. */
export function cableOf(connection: Connection): CableEnds | null {
  const { source, target, sourceHandle, targetHandle } = connection;
  if (sourceHandle !== "modOut" || !targetHandle?.startsWith("param-")) return null;
  const param = targetHandle.slice("param-".length);
  if (![source, target, param].every((text) => WHOLE.test(text))) return null;
  return { modulator: Number(source), node: Number(target), param: Number(param) };
}

/** The key of a parameter of a node, which is how the ends that take a cable are listed. */
export const keyOf = (node: NodeId, param: number): string => `${String(node)}:${String(param)}`;

/** The parameters that the modulator `modulator` can still be drawn to, as `keyOf` says: none if it is not a modulator of the canvas. */
export function drawableFrom(nodes: readonly PatchNode[], modulator: NodeId): Set<string> {
  const found = nodes.find((node) => node.data.node === modulator);
  return new Set((found?.data.targets ?? []).flatMap((group) => group.params.map((param) => keyOf(group.node, param.index))));
}

/** Whether the cable can be drawn: the modulator can still drive that parameter. */
export function canDraw(nodes: readonly PatchNode[], ends: CableEnds): boolean {
  return drawableFrom(nodes, ends.modulator).has(keyOf(ends.node, ends.param));
}

/**
 * The parameters that take the cable that is being dragged (`keyOf`), for the ports to say so; null while no cable is
 * being dragged. (A port is a handle in a node of its own, which cannot be told by the canvas.)
 */
export const ConnectingContext = createContext<ReadonlySet<string> | null>(null);
