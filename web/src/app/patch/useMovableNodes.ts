// The nodes of the canvas as a person moves them (docs/web-ui.md §9.3): the layout's, except that a node in hand is where the
// pointer has taken it, and one put down is where the server says it is once it has said. Nodes that are chosen (step 107) are moved together,
// with the one that is taken, or with the arrow keys, and each is told to the server where it was put.
import { applyNodeChanges } from "@xyflow/react";
import type { NodeChange, OnNodeDrag } from "@xyflow/react";
import { useCallback, useEffect, useRef, useState } from "react";

import type { NodeId, NodeUi } from "../../protocol";
import { keepHeld } from "./moving";
import type { PatchNode } from "./toFlow";

export interface MovableNodes {
  /** The nodes to draw: `laidOut`, with the nodes that are in hand where they are. */
  nodes: PatchNode[];
  onNodesChange: (changes: NodeChange<PatchNode>[]) => void;
  onNodeDragStart: OnNodeDrag<PatchNode>;
  onNodeDragStop: OnNodeDrag<PatchNode>;
}

/** How long the keys of the keyboard are let rest before the places that they moved nodes to are told to the server: a held key moves a node a step at a time, and the server is told once. */
export const KEYS_REST_MS = 400;

/** The nodes that a drag moved: the ones that React Flow says (the node that was taken, and every node that is chosen), or the one that was taken if it says none. */
const movedBy = (node: PatchNode, nodes: readonly PatchNode[]): readonly PatchNode[] => (nodes.length > 0 ? nodes : [node]);

/**
 * `laidOut` is what the session says (`toFlow`), which changes whenever its structure does, and `moveNode` tells the server where a
 * node was put down (true if it was kept; if it was not, the node goes back to where the session has it, and the one that asked
 * has said why). A node is not taken out of a hand when the session is laid out again.
 */
export function useMovableNodes(laidOut: PatchNode[], moveNode: (node: NodeId, ui: NodeUi) => Promise<boolean>): MovableNodes {
  // The nodes as they are on the canvas.
  const [nodes, setNodes] = useState<PatchNode[]>(laidOut);
  // The nodes in hand, or put down and not answered for yet, by id on the canvas, and how many drags of each are: the session is
  // laid out again whenever its structure changes, and must not take a node out of a hand.
  const held = useRef(new Map<string, number>());
  const latest = useRef(laidOut);
  useEffect(() => {
    latest.current = laidOut;
    setNodes((current) => keepHeld(laidOut, current, new Set(held.current.keys())));
  }, [laidOut]);

  /** Lets go of one hold on each of the nodes `ids`, once what was asked of the server about them has been answered. */
  const release = useCallback((ids: readonly string[]) => {
    for (const id of ids) {
      const left = (held.current.get(id) ?? 1) - 1;
      if (left > 0) held.current.set(id, left);
      else held.current.delete(id);
    }
    // The server has said where the nodes are by now (or refused: the page says why, and a node goes back).
    setNodes((current) => keepHeld(latest.current, current, new Set(held.current.keys())));
  }, []);

  /** Tells the server where the nodes `ids` were put (those that are where they were are not told), and lets go of them when it has answered. */
  const tell = useCallback(
    (places: ReadonlyMap<string, { x: number; y: number }>) => {
      const asks = [...places].map(([id, { x, y }]) => {
        const there = latest.current.find((candidate) => candidate.id === id);
        // Put down is where it was picked up, say: nothing to tell (and a node that was not placed stays that).
        const moved = there !== undefined && (there.position.x !== x || there.position.y !== y);
        return moved ? moveNode(there.data.node, { x, y }) : Promise.resolve(true);
      });
      void Promise.all(asks).finally(() => {
        release([...places.keys()]);
      });
    },
    [moveNode, release],
  );

  // The nodes that are being dragged, by id on the canvas: what React Flow says of where they are is the drag's, and is told when they are put down.
  const dragging = useRef(new Set<string>());
  // The places that the keys moved nodes to, by id on the canvas, until the keys have rested.
  const keyed = useRef(new Map<string, { x: number; y: number }>());
  const resting = useRef<number | undefined>(undefined);
  useEffect(
    () => () => {
      window.clearTimeout(resting.current);
    },
    [],
  );

  const onNodesChange = useCallback(
    (changes: NodeChange<PatchNode>[]) => {
      setNodes((current) => applyNodeChanges(changes, current));
      // A node that moves with no drag is moved by the keyboard (the arrow keys move what is chosen): it is held where it is put until the server has been told, which is when the keys have rested.
      let keys = false;
      for (const change of changes) {
        if (change.type !== "position" || change.dragging !== false || change.position === undefined || dragging.current.has(change.id)) continue;
        if (!keyed.current.has(change.id)) held.current.set(change.id, (held.current.get(change.id) ?? 0) + 1);
        keyed.current.set(change.id, change.position);
        keys = true;
      }
      if (!keys) return;
      window.clearTimeout(resting.current);
      resting.current = window.setTimeout(() => {
        const places = new Map(keyed.current);
        keyed.current.clear();
        tell(places);
      }, KEYS_REST_MS);
    },
    [tell],
  );
  const onNodeDragStart = useCallback<OnNodeDrag<PatchNode>>((_event, node, dragged) => {
    for (const each of movedBy(node, dragged)) {
      held.current.set(each.id, (held.current.get(each.id) ?? 0) + 1);
      dragging.current.add(each.id);
    }
  }, []);
  const onNodeDragStop = useCallback<OnNodeDrag<PatchNode>>(
    (_event, node, dragged) => {
      const moved = movedBy(node, dragged);
      for (const each of moved) dragging.current.delete(each.id);
      tell(new Map(moved.map((each) => [each.id, each.position])));
    },
    [tell],
  );
  return { nodes, onNodesChange, onNodeDragStart, onNodeDragStop };
}
