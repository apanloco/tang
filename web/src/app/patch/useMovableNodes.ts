// The nodes of the canvas as a person moves them (docs/web-ui.md §9.3): the layout's, except that a node in hand is where the
// pointer has taken it, and one put down is where the server says it is once it has said.
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

  const onNodesChange = useCallback((changes: NodeChange<PatchNode>[]) => {
    setNodes((current) => applyNodeChanges(changes, current));
  }, []);
  const onNodeDragStart = useCallback<OnNodeDrag<PatchNode>>((_event, node) => {
    held.current.set(node.id, (held.current.get(node.id) ?? 0) + 1);
  }, []);
  const onNodeDragStop = useCallback<OnNodeDrag<PatchNode>>(
    (_event, node) => {
      const { x, y } = node.position;
      const before = latest.current.find((candidate) => candidate.id === node.id)?.position;
      // Put down is where it was picked up, say: nothing to tell (and a node that was not placed stays that).
      const moved = before?.x !== x || before.y !== y;
      void (moved ? moveNode(node.data.node, { x, y }) : Promise.resolve(true)).finally(() => {
        const left = (held.current.get(node.id) ?? 1) - 1;
        if (left > 0) held.current.set(node.id, left);
        else held.current.delete(node.id);
        // The server has said where the node is by now (or refused: the page says why, and the node goes back).
        setNodes((current) => keepHeld(latest.current, current, new Set(held.current.keys())));
      });
    },
    [moveNode],
  );
  return { nodes, onNodesChange, onNodeDragStart, onNodeDragStop };
}
