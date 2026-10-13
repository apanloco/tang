// @vitest-environment jsdom
import { afterEach, describe, expect, test, vi } from "vitest";

import { focusWhenPossible } from "./focus";

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
