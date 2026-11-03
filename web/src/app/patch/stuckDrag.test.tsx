// @vitest-environment jsdom
// A card that stays in hand when the release of the mouse was lost is let go of (`useStuckDragGuard`).
import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test } from "vitest";

import { rig } from "../testing";
import { PatchView } from "./PatchView";
import { useStuckDragGuard } from "./stuckDrag";

afterEach(cleanup);

function Canvas() {
  useStuckDragGuard();
  return null;
}

/** The mouse ups that the window gets while the test runs: what React Flow listens for to end the drag of a card. */
let ups: MouseEvent[] = [];
const count = (event: MouseEvent): void => {
  ups.push(event);
};

beforeEach(() => {
  ups = [];
  document.body.innerHTML = "";
  window.addEventListener("mouseup", count);
  return () => {
    window.removeEventListener("mouseup", count);
  };
});

/** A card that React Flow has in hand: it marks the node that is being dragged. */
const inHand = (): void => {
  const card = document.createElement("div");
  card.className = "react-flow__node dragging";
  document.body.append(card);
};

describe("a card that is in hand when the mouse moves with no button down", () => {
  test("is let go of: the mouse up that was lost is sent to the window, where the pointer is", () => {
    render(<Canvas />);
    inHand();
    fireEvent.mouseMove(window, { buttons: 0, clientX: 120, clientY: 80 });
    expect(ups).toHaveLength(1);
    expect([ups[0]?.clientX, ups[0]?.clientY, ups[0]?.type]).toEqual([120, 80, "mouseup"]);
    // (With the window as its view: d3-drag takes its listeners off the window that the mouse up says.)
    expect(ups[0]?.view).toBe(window);
  });

  test("is not let go of while the button is down: a drag that is going on goes on", () => {
    render(<Canvas />);
    inHand();
    fireEvent.mouseMove(window, { buttons: 1, clientX: 120, clientY: 80 });
    expect(ups).toHaveLength(0);
  });

  test("is not let go of when there is no card in hand: a mouse that moves over the canvas sends nothing", () => {
    render(<Canvas />);
    document.body.append(Object.assign(document.createElement("div"), { className: "react-flow__node" }));
    fireEvent.mouseMove(window, { buttons: 0, clientX: 1, clientY: 1 });
    expect(ups).toHaveLength(0);
  });

  test("is let go of when the window loses the focus, with a card in hand and not without", () => {
    render(<Canvas />);
    fireEvent.blur(window);
    expect(ups).toHaveLength(0);
    inHand();
    fireEvent.blur(window);
    expect(ups).toHaveLength(1);
  });

  test("is let go of when the page is hidden, with a card in hand and not without", () => {
    const hide = (): void => {
      Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
      fireEvent(document, new Event("visibilitychange"));
      Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
    };
    render(<Canvas />);
    hide();
    expect(ups).toHaveLength(0);
    inHand();
    hide();
    expect(ups).toHaveLength(1);
  });

  test("is not let go of when the page is shown: it is the page being hidden that does it", () => {
    render(<Canvas />);
    inHand();
    Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
    fireEvent(document, new Event("visibilitychange"));
    expect(ups).toHaveLength(0);
  });

  test("is no business of the guard once the canvas is gone", () => {
    const view = render(<Canvas />);
    inHand();
    view.unmount();
    fireEvent.mouseMove(window, { buttons: 0, clientX: 1, clientY: 1 });
    fireEvent.blur(window);
    expect(ups).toHaveLength(0);
  });
});

describe("the canvas", () => {
  test("has the guard: a card that is in hand when the window loses the focus is let go of", async () => {
    await rig(<PatchView />);
    inHand();
    fireEvent.blur(window);
    expect(ups).toHaveLength(1);
  });
});
