// Moving nodes on the canvas (docs/web-ui.md §9.3): while a person has a node in hand the session
// can be laid out again (a modulator added by another browser, say), and the node must not jump
// back to where the session has it until it is put down and the server has said where it is.
import type { PatchNode } from "./toFlow";

/**
 * The nodes of the canvas after the session was laid out again (`incoming`): as the layout says, except that a node a person has
 * in hand, or has put down and the server has not answered for yet (`held`, by the node's id on the canvas), stays where it is
 * on the canvas (`current`), and goes on being dragged if it is: where it is is not the session's to say. A node that is chosen
 * (step 107) stays chosen: the layout does not know what a person chose, and nodes that are moved, as chosen nodes are, are
 * laid out again each time. A node keeps the size that React Flow measured it at: one that has none is hidden, with its cables, until it is measured
 * again, which a layout that comes while one is being measured can keep from happening (the node is gone until the window changes size).
 */
export function keepHeld(incoming: readonly PatchNode[], current: readonly PatchNode[], held: ReadonlySet<string>): PatchNode[] {
  const before = new Map(current.map((node) => [node.id, node]));
  return incoming.map((node) => {
    const was = before.get(node.id);
    if (was === undefined || (was.measured === undefined && was.selected !== true && !held.has(node.id))) return node;
    const measured = was.measured === undefined ? {} : { measured: was.measured };
    const chosen = was.selected === true ? { selected: true } : {};
    if (!held.has(node.id)) return { ...node, ...measured, ...chosen };
    // (A node that is not being dragged has no `dragging` at all, as React Flow has it.)
    return { ...node, ...measured, ...chosen, position: was.position, ...(was.dragging === undefined ? {} : { dragging: was.dragging }) };
  });
}
