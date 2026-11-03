// A card that stays in hand after the button went up (the user: "sometimes the mouse pointer gets "attached" to a knob or window and wont release even when I release my mouse"). React Flow moves a node by
// listening, from the press, for the mouse up in the window; a release that the window never sees (the button goes up over another window, the page loses the focus in the middle of the drag) leaves the node in hand, and it
// follows a mouse that is not pressed until the next click. What shows it is a mouse that moves with no button down while a node is being dragged, or the page losing the focus; the mouse up that was lost is sent then, where React
// Flow listens for it. (A cable that is being drawn is not let go of this way: ending it where the pointer is could draw it to whatever the pointer is over.)
import { useEffect } from "react";

/** Whether React Flow has a node in hand: it marks the card that is being dragged. */
function nodeInHand(): boolean {
  return document.querySelector(".react-flow__node.dragging") !== null;
}

/**
 * The mouse up that React Flow waits for, sent to the window it waits in, where the pointer is. It has its `view`, the window: d3-drag, which moves the cards, takes the window whose listeners it takes off from the
 * `view` of the mouse up (`select(event.view)`), and an event that a script makes has none unless it is given one (the drag would end, and the listener for its moves would stay and fail at every move after).
 */
function letGo(at?: { clientX: number; clientY: number }): void {
  const up = new MouseEvent("mouseup", { bubbles: true, cancelable: true, clientX: at?.clientX ?? 0, clientY: at?.clientY ?? 0 });
  Object.defineProperty(up, "view", { value: window });
  window.dispatchEvent(up);
}

/** Lets go of a node that is in hand when its release was lost (see above). Used once, by the canvas. */
export function useStuckDragGuard(): void {
  useEffect(() => {
    const moved = (event: MouseEvent): void => {
      if (event.buttons === 0 && nodeInHand()) letGo(event);
    };
    const lost = (): void => {
      if (nodeInHand()) letGo();
    };
    const hidden = (): void => {
      if (document.hidden) lost();
    };
    window.addEventListener("mousemove", moved);
    window.addEventListener("blur", lost);
    document.addEventListener("visibilitychange", hidden);
    return () => {
      window.removeEventListener("mousemove", moved);
      window.removeEventListener("blur", lost);
      document.removeEventListener("visibilitychange", hidden);
    };
  }, []);
}
