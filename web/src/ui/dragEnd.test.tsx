// @vitest-environment jsdom
// What ends a drag that no pointer up has ended (`useDragEnd`, `releaseLost`, `startsDrag`).
import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { releaseLost, startsDrag, useDragEnd } from "./dragEnd";

afterEach(cleanup);

function Held({ active, end }: { active: boolean; end: () => void }) {
  useDragEnd(active, end);
  return null;
}

/** The page is hidden, or shown, as a browser does when its tab is left or come back to (jsdom has no tabs: `document.hidden` says it, and the event follows). */
function pageIs(hidden: boolean): void {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  fireEvent(document, new Event("visibilitychange"));
  Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
}

const endings: [string, () => void][] = [
  ["a pointer goes up", () => fireEvent.pointerUp(document.body, { pointerId: 1 })],
  ["a pointer is cancelled", () => fireEvent.pointerCancel(document.body, { pointerId: 1 })],
  ["the window loses the focus", () => fireEvent.blur(window)],
  ["a context menu opens", () => fireEvent.contextMenu(document.body)],
  [
    "the page is hidden",
    () => {
      pageIs(true);
    },
  ],
];

describe("what ends a drag that no pointer up has ended", () => {
  test("is each of these, while a drag is on", () => {
    for (const [what, lose] of endings) {
      const end = vi.fn();
      const view = render(<Held active end={end} />);
      lose();
      expect(end, what).toHaveBeenCalledTimes(1);
      view.unmount();
    }
  });

  test("is nothing while no drag is on: the window loses the focus a hundred times and nothing is asked", () => {
    for (const [what, lose] of endings) {
      const end = vi.fn();
      render(<Held active={false} end={end} />);
      lose();
      expect(end, what).not.toHaveBeenCalled();
      cleanup();
    }
  });

  test("is not the page being shown: it is the page being hidden that ends a drag", () => {
    const end = vi.fn();
    render(<Held active end={end} />);
    pageIs(false);
    expect(end).not.toHaveBeenCalled();
  });

  test("stops when the drag is over, and when the control goes", () => {
    const end = vi.fn();
    const view = render(<Held active end={end} />);
    view.rerender(<Held active={false} end={end} />);
    for (const [, lose] of endings) lose();
    expect(end).not.toHaveBeenCalled();
    view.rerender(<Held active end={end} />);
    view.unmount();
    for (const [, lose] of endings) lose();
    expect(end).not.toHaveBeenCalled();
  });

  test("calls the latest function that it is given, and not the one it had when the drag began", () => {
    const first = vi.fn();
    const later = vi.fn();
    const view = render(<Held active end={first} />);
    view.rerender(<Held active end={later} />);
    fireEvent.blur(window);
    expect([first.mock.calls.length, later.mock.calls.length]).toEqual([0, 1]);
  });
});

describe("a release that was lost", () => {
  test("is a mouse that moves with no button down, and nothing else: a pen or a touch that moves with none is not", () => {
    expect(releaseLost({ pointerType: "mouse", buttons: 0 })).toBe(true);
    expect(releaseLost({ pointerType: "mouse", buttons: 1 })).toBe(false);
    expect(releaseLost({ pointerType: "mouse", buttons: 2 })).toBe(false);
    expect(releaseLost({ pointerType: "pen", buttons: 0 })).toBe(false);
    expect(releaseLost({ pointerType: "touch", buttons: 0 })).toBe(false);
    expect(releaseLost({ pointerType: "", buttons: 0 })).toBe(false);
  });
});

describe("a press that starts a drag", () => {
  test("is the main button, and not with Control held, which on a Mac is the secondary button", () => {
    expect(startsDrag({ button: 0, ctrlKey: false })).toBe(true);
    expect(startsDrag({ button: 0, ctrlKey: true })).toBe(false);
    expect(startsDrag({ button: 1, ctrlKey: false })).toBe(false);
    expect(startsDrag({ button: 2, ctrlKey: false })).toBe(false);
  });
});
