// Moving nodes on the canvas (docs/web-ui.md §9.3): while a person has a node in hand the session
// can be laid out again (a modulator added by another browser, say), and the node must not jump
// back to where the session has it until it is put down and the server has said where it is.
import type { PatchNode } from "./toFlow";

/**
 * The nodes of the canvas after the session was laid out again (`incoming`): as the layout says, except that a node a person has
 * in hand, or has put down and the server has not answered for yet (`held`, by the node's id on the canvas), stays where it is
 * on the canvas (`current`), and goes on being dragged if it is: where it is is not the session's to say.
 */
export function keepHeld(incoming: readonly PatchNode[], current: readonly PatchNode[], held: ReadonlySet<string>): PatchNode[] {
  if (held.size === 0) return [...incoming];
  const before = new Map(current.map((node) => [node.id, node]));
  return incoming.map((node) => {
    const now = held.has(node.id) ? before.get(node.id) : undefined;
    if (!now) return node;
    // (A node that is not being dragged has no `dragging` at all, as React Flow has it.)
    return { ...node, position: now.position, ...(now.dragging === undefined ? {} : { dragging: now.dragging }) };
  });
}
