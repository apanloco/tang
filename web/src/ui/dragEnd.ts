// What ends a drag that no pointer up has ended (the user: "sometimes the mouse pointer gets "attached" to a knob or window and wont release even when I release my mouse"). A control that is dragged learns of the
// release from the pointer up that reaches it, and there are releases that never do: the button goes up over another window (alt-tab in the middle of a turn), a context menu that Control and a click open on a Mac
// takes the release, the page loses the focus or is hidden. The knob then goes on following a mouse that is not pressed, and a stroke on the step bars goes on painting. The piano has had this net since step 24
// (`PianoKeys`); the other controls that are dragged have it from here.
import { useEffect, useRef } from "react";

/**
 * Whether the event is a mouse that moves with no button down. A drag that it reaches has lost its release, and is over: it has the value it had. Touch and pen are left alone (a pen that hovers moves
 * with no button down before it touches, and a touch that is lost is a `pointercancel`).
 */
export function releaseLost(event: { pointerType: string; buttons: number }): boolean {
  return event.pointerType === "mouse" && event.buttons === 0;
}

/**
 * Whether a press starts a drag: the main button, and not with Control held. On a Mac Control and a click is the secondary button (the browser says `button` 0 and `ctrlKey`), which opens a context menu
 * that takes the release, so that a drag that it began would not end.
 */
export function startsDrag(event: { button: number; ctrlKey: boolean }): boolean {
  return event.button === 0 && !event.ctrlKey;
}

/**
 * While `active`, calls `end` when the page lets go of the drag by any way but the pointer up that reaches the control: a pointer up or a cancel anywhere in the window, the window losing the focus, the page being
 * hidden and a context menu opening. `end` is whatever the control does when its drag is over, and may be called more than once (the pointer up that reaches the control calls it as well): it must be a thing
 * that does nothing when no drag is on. The latest `end` is the one that is called.
 */
export function useDragEnd(active: boolean, end: () => void): void {
  const latest = useRef(end);
  useEffect(() => {
    latest.current = end;
  });
  useEffect(() => {
    if (!active) return undefined;
    const stop = (): void => {
      latest.current();
    };
    const hide = (): void => {
      if (document.hidden) stop();
    };
    window.addEventListener("pointerup", stop);
    window.addEventListener("pointercancel", stop);
    window.addEventListener("blur", stop);
    window.addEventListener("contextmenu", stop);
    document.addEventListener("visibilitychange", hide);
    return () => {
      window.removeEventListener("pointerup", stop);
      window.removeEventListener("pointercancel", stop);
      window.removeEventListener("blur", stop);
      window.removeEventListener("contextmenu", stop);
      document.removeEventListener("visibilitychange", hide);
    };
  }, [active]);
}
