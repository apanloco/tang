// Giving the focus back to something after the node it was in has gone. The canvas lays its nodes out
// again when the session changes, and a node is not ready for the focus (it is not visible) until it
// has been measured again, a few frames later: asking at once does nothing. So the focus is asked for
// a frame at a time until it is had, or for a while.

/** Focus the element with this `id`, now or as soon as it can be: a frame at a time, `tries` more times at most. */
export function focusWhenPossible(id: string, tries = 30): void {
  const target = document.getElementById(id);
  target?.focus();
  if (target && document.activeElement === target) return;
  if (tries > 0) {
    requestAnimationFrame(() => {
      focusWhenPossible(id, tries - 1);
    });
  }
}
