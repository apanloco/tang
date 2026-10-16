// @vitest-environment jsdom
import { afterEach, describe, expect, test, vi } from "vitest";

import { cancelPendingFocus, focusWhenPossible, keepFocus } from "./focus";

afterEach(() => {
  document.body.innerHTML = "";
  vi.useRealTimers();
});

/** Let `frames` animation frames of the page go by. */
async function frames(count: number): Promise<void> {
  for (let i = 0; i < count; i += 1) await new Promise((resolve) => requestAnimationFrame(() => { resolve(undefined); }));
}

describe("giving the focus to something that may not be ready for it", () => {
  test("focuses what is there at once", () => {
    document.body.innerHTML = '<button id="b">b</button>';
    focusWhenPossible("b");
    expect(document.activeElement?.id).toBe("b");
  });

  test("goes on trying, a frame at a time, for what is not there yet, and stops when it has it", async () => {
    focusWhenPossible("late");
    await frames(3);
    expect(document.activeElement).toBe(document.body);
    document.body.innerHTML = '<button id="late">late</button>';
    await frames(2);
    expect(document.activeElement?.id).toBe("late");
  });

  test("goes on trying for what is there but cannot take the focus yet (hidden, or disabled), and has it when it can", async () => {
    document.body.innerHTML = '<button id="b" disabled>b</button>';
    focusWhenPossible("b");
    await frames(3);
    expect(document.activeElement).toBe(document.body);
    (document.getElementById("b") as HTMLButtonElement).disabled = false;
    await frames(2);
    expect(document.activeElement?.id).toBe("b");
  });

  test("gives up after a while, so that what never comes is not waited for for ever", async () => {
    const asked = vi.spyOn(document, "getElementById");
    focusWhenPossible("never", 5);
    await frames(12);
    const calls = asked.mock.calls.filter(([id]) => id === "never").length;
    expect(calls).toBe(6); // the first try, and five more
    asked.mockRestore();
  });

  test("stops once it has the focus, and does not take it back from what the person moves it to after", async () => {
    document.body.innerHTML = '<button id="other">o</button>';
    focusWhenPossible("late");
    document.body.insertAdjacentHTML("beforeend", '<button id="late">l</button>');
    await frames(2);
    expect(document.activeElement?.id).toBe("late");
    document.getElementById("other")?.focus();
    await frames(5);
    expect(document.activeElement?.id).toBe("other");
  });
});

describe("keeping the focus on something that the page moves", () => {
  test("gives the focus back when the page has let it go to nothing, whenever that is in the frames it is kept for", async () => {
    document.body.innerHTML = '<button id="kept1">b</button>';
    const b = document.getElementById("kept1") as HTMLButtonElement;
    b.focus();
    keepFocus("kept1");
    // (What a browser does with the focus of an element that is moved in the page: it is on the page itself, the body.)
    b.blur();
    expect(document.activeElement).toBe(document.body);
    await frames(2);
    expect(document.activeElement).toBe(b);
    b.blur();
    await frames(2);
    expect(document.activeElement).toBe(b);
  });

  test("leaves the focus where a person has put it", async () => {
    document.body.innerHTML = '<button id="kept2">b</button><button id="other2">o</button>';
    document.getElementById("kept2")?.focus();
    keepFocus("kept2");
    document.getElementById("other2")?.focus();
    await frames(5);
    expect(document.activeElement?.id).toBe("other2");
  });

  test("does not take the focus when it is on what it is kept for, and has nothing to do for what is not there", async () => {
    document.body.innerHTML = '<button id="kept3">b</button>';
    const b = document.getElementById("kept3") as HTMLButtonElement;
    b.focus();
    const focused = vi.spyOn(b, "focus");
    keepFocus("kept3");
    keepFocus("nothing3");
    await frames(3);
    expect(focused).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(b);
  });

  test("is kept for the latest element only: what was asked for before it is let go, and does not take the focus from it", async () => {
    document.body.innerHTML = '<button id="kept5">a</button><button id="kept6">b</button>';
    keepFocus("kept5");
    // (A person goes on to the other button, which the page then moves as well: the focus is let go.)
    const second = document.getElementById("kept6") as HTMLButtonElement;
    second.focus();
    keepFocus("kept6");
    second.blur();
    await frames(2);
    expect(document.activeElement?.id).toBe("kept6");
  });

  test("stops after the frames it was asked for", async () => {
    document.body.innerHTML = '<button id="kept4">b</button>';
    const b = document.getElementById("kept4") as HTMLButtonElement;
    b.focus();
    keepFocus("kept4", 3);
    await frames(8);
    b.blur();
    await frames(3);
    expect(document.activeElement).toBe(document.body);
  });
});

describe("letting go of what is waiting for a frame", () => {
  test("a request that is cancelled gives the focus to nothing, whatever it had left to try", async () => {
    document.body.innerHTML = '<button id="b">b</button>';
    keepFocus("b", 20);
    expect(document.activeElement?.id).toBe("b");
    (document.activeElement as HTMLElement).blur();
    cancelPendingFocus();
    await frames(3);
    expect(document.activeElement).toBe(document.body);
    focusWhenPossible("late", 30);
    cancelPendingFocus();
    document.body.insertAdjacentHTML("beforeend", '<button id="late">l</button>');
    await frames(3);
    expect(document.activeElement).toBe(document.body);
  });

  // The two tests that follow are one: the first leaves a request that has frames left to try, and the second is a page that has an element of the same name (as the
  // next test of a page that is built again has) and that nothing is keeping the focus for. (A request that outlived its test took the focus for a button of the next one:
  // `PatchView.order.test.tsx` failed once in a whole run, when the frames were slow enough for the twenty of one to go on past the test's own wait.)
  test("(first) leaves a request that has frames to try", () => {
    document.body.innerHTML = '<button id="stale">s</button>';
    keepFocus("stale", 20);
    expect(document.activeElement?.id).toBe("stale");
  });

  test("(second) a request of the test before does not take the focus for the page that this one has", async () => {
    document.body.innerHTML = '<button id="stale">s</button>';
    await frames(3);
    expect(document.activeElement).toBe(document.body);
  });
});
