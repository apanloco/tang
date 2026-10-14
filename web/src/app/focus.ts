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

/** Which request of `keepFocus` is the latest: an earlier one is let go. */
let latestKeeping = 0;

/**
 * Keep the focus on the element with this `id` through a change of the page that moves the element in the page: a browser lets the focus of an
 * element go when the element is moved (its node is put in another place among its siblings, as the canvas does when it lays its nodes out in
 * another order), and nobody has it then, the page itself does. For `frames` frames, whenever the focus is found on the page itself (or on
 * nothing), it is given back to the element. What a person has moved the focus to is left alone, and so is what an earlier request was for:
 * the latest is the one that counts (the element before was left for this one).
 */
export function keepFocus(id: string, frames = 20): void {
  latestKeeping += 1;
  keep(id, frames, latestKeeping);
}

function keep(id: string, frames: number, request: number): void {
  if (request !== latestKeeping) return;
  const active = document.activeElement;
  if (active === null || active === document.body) document.getElementById(id)?.focus();
  if (frames > 0) {
    requestAnimationFrame(() => {
      keep(id, frames - 1, request);
    });
  }
}
