// @vitest-environment jsdom
// Moving nodes: a node is dragged by the pointer, stays where it is put down, and the place goes to the server, which keeps it with
// the session and tells every browser.
import { act, cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import { rig } from "../testing";
import { PatchView } from "./PatchView";
import { toFlow } from "./toFlow";

afterEach(cleanup);

const filter = nodeNamed(tracerSnapshot, "Filter");
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");

/** The scale of the canvas: how many pixels of the screen a pixel of the canvas is. */
function zoomOf(view: HTMLElement): number {
  const transform = view.querySelector<HTMLElement>(".react-flow__viewport")?.style.transform ?? "";
  const found = /scale\(([-\d.e]+)\)/.exec(transform);
  return found?.[1] ? Number(found[1]) : 1;
}

/** Where the canvas draws a node, in its own pixels. */
function placeOf(node: HTMLElement): { x: number; y: number } {
  const found = /translate\((-?[\d.e-]+)px,\s*(-?[\d.e-]+)px\)/.exec(node.style.transform);
  if (!found?.[1] || !found[2]) throw new Error(`the node is not placed: ${node.style.transform}`);
  return { x: Number(found[1]), y: Number(found[2]) };
}

/** Where the layout puts a node of the tracer. */
function laidOut(id: number): { x: number; y: number } {
  const found = toFlow(tracerSnapshot.graph).nodes.find((node) => node.id === String(id))?.position;
  if (!found) throw new Error(`the layout has no node ${String(id)}`);
  return found;
}

/**
 * A pointer event as the browser makes it, with the window as its `view` (which d3, under React Flow, listens on for the moves):
 * jsdom accepts no window of a test's as the `view` of a `MouseEvent`, so it is put on the event after.
 */
function pointer(type: string, x: number, y: number): MouseEvent {
  const event = new MouseEvent(type, { bubbles: true, cancelable: true, button: 0, clientX: x, clientY: y });
  Object.defineProperty(event, "view", { value: window });
  return event;
}

/** The canvas of a test has no size, so that every point is at its edge, where a canvas pans on its own while a node is dragged: it is given one. */
function withASizedCanvas<T>(run: () => T): T {
  const measured = vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    const [width, height] = this.classList.contains("react-flow") ? [2000, 2000] : [0, 0];
    return { x: 0, y: 0, left: 0, top: 0, right: width, bottom: height, width, height, toJSON: () => ({}) };
  });
  try {
    return run();
  } finally {
    measured.mockRestore();
  }
}

/** The pointer goes down on `header` and moves by (dx, dy) screen pixels in a few steps (the first is the longest, so that the drag has begun by then); it is not let go. */
function pull(header: Element, dx: number, dy: number): void {
  withASizedCanvas(() => {
    fireEvent(header, pointer("mousedown", 100, 100));
    for (const step of [0.1, 0.5, 1]) fireEvent(window, pointer("mousemove", 100 + dx * step, 100 + dy * step));
  });
}

function release(dx: number, dy: number): void {
  withASizedCanvas(() => {
    fireEvent(window, pointer("mouseup", 100 + dx, 100 + dy));
  });
}

/** The pointer goes down on `header`, moves by (dx, dy) screen pixels and is let go. */
function drag(header: Element, dx: number, dy: number): void {
  pull(header, dx, dy);
  release(dx, dy);
}

const headerOf = async (id: number): Promise<{ node: HTMLElement; header: Element }> => {
  const node = await screen.findByTestId(`rf__node-${String(id)}`);
  const header = node.querySelector(".tang-node__header");
  if (!header) throw new Error("the node has no header");
  return { node, header };
};

interface Place {
  type: string;
  node: number;
  ui: { x: number; y: number } | null;
}
const intents = (requests: () => { intent: unknown }[]): Place[] => requests().map((r) => r.intent as Place);

describe("moving a node", () => {
  test("dragging it by its header sends where it was put down as the node's place, and the node is there", async () => {
    const { requests, view } = await rig(<PatchView />);
    const { node, header } = await headerOf(filter.id);
    const start = laidOut(filter.id);
    const zoom = zoomOf(view.container);

    drag(header, 60, 30);

    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    const [sent] = intents(requests);
    expect([sent?.type, sent?.node]).toEqual(["setNodeUi", filter.id]);
    // (The drag begins when the pointer has gone a few pixels, and the node follows it from there: it is a little behind.)
    const moved = { x: (sent?.ui?.x ?? 0) - start.x, y: (sent?.ui?.y ?? 0) - start.y };
    expect(moved.x).toBeGreaterThan(40 / zoom);
    expect(moved.x).toBeLessThanOrEqual(60 / zoom + 0.5);
    expect(moved.y).toBeGreaterThan(20 / zoom);
    expect(moved.y).toBeLessThanOrEqual(30 / zoom + 0.5);
    // The server kept it (to a tenth), and the node is drawn there.
    await waitFor(() => {
      expect(placeOf(node).x).toBeCloseTo(Math.round((sent?.ui?.x ?? 0) * 10) / 10, 6);
      expect(placeOf(node).y).toBeCloseTo(Math.round((sent?.ui?.y ?? 0) * 10) / 10, 6);
    });
  });

  test("keeps the node where it was put down once the session says so: the place is the session's, so another browser's copy of it is there too", async () => {
    const { server, client } = await rig(<PatchView />);
    const { node, header } = await headerOf(filter.id);
    drag(header, 90, 60);
    await waitFor(() => {
      expect(client.state.snapshot?.graph.nodes.find((n) => n.id === filter.id)?.ui).toBeDefined();
    });
    const told = server.snapshot.graph.nodes.find((n) => n.id === filter.id)?.ui;
    expect(told).toBeDefined();
    // (To the pixel: the canvas adds the sizes of the nodes it draws to what it is told, and what it says has the noise of that.)
    expect(placeOf(node).x).toBeCloseTo(told?.x ?? Number.NaN, 6);
    expect(placeOf(node).y).toBeCloseTo(told?.y ?? Number.NaN, 6);
  });

  test("a node that was moved here goes where the session says afterwards, whoever says it", async () => {
    const { server, client } = await rig(<PatchView />);
    const { node, header } = await headerOf(filter.id);
    drag(header, 90, 60);
    await waitFor(() => {
      expect(client.state.snapshot?.graph.nodes.find((n) => n.id === filter.id)?.ui).toBeDefined();
    });
    act(() => {
      server.apply({ type: "setNodeUi", node: filter.id, ui: { x: 1000, y: 2000 } });
    });
    await waitFor(() => {
      expect(placeOf(node)).toEqual({ x: 1000, y: 2000 });
    });
  });

  test("nothing is sent for a click, or for a drag that is put down where it was picked up", async () => {
    const { requests } = await rig(<PatchView />);
    const { header } = await headerOf(filter.id);
    // A click: down and up, no move.
    withASizedCanvas(() => {
      fireEvent(header, pointer("mousedown", 100, 100));
      fireEvent(window, pointer("mouseup", 100, 100));
    });
    // A move of a pixel is less than a drag.
    drag(header, 1, 1);
    // A drag that has begun and is put down where it began (the node follows the pointer from the point the drag begins at).
    withASizedCanvas(() => {
      fireEvent(header, pointer("mousedown", 100, 100));
      fireEvent(window, pointer("mousemove", 110, 100));
      fireEvent(window, pointer("mouseup", 110, 100));
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    expect(requests()).toEqual([]);
  });

  test("a node that another browser moves is drawn where it was moved to", async () => {
    const { server } = await rig(<PatchView />);
    const node = await screen.findByTestId(`rf__node-${String(oscillator.id)}`);
    const before = placeOf(node);
    expect(before).toEqual(laidOut(oscillator.id));
    act(() => {
      server.apply({ type: "setNodeUi", node: oscillator.id, ui: { x: 1234, y: -56.5 } });
    });
    await waitFor(() => {
      expect(placeOf(node)).toEqual({ x: 1234, y: -56.5 });
    });
    // And given back to the layout, it goes where the layout has it.
    act(() => {
      server.apply({ type: "setNodeUi", node: oscillator.id, ui: null });
    });
    await waitFor(() => {
      expect(placeOf(node)).toEqual(before);
    });
  });

  test("a node in hand stays in the hand when the session is laid out again, and is put down where the pointer lets go", async () => {
    const { server, requests } = await rig(<PatchView />);
    const { node, header } = await headerOf(filter.id);
    pull(header, 80, 40);
    const inHand = placeOf(node);
    expect(inHand).not.toEqual(laidOut(filter.id));
    // Another front-end adds a modulator to the lane: the session is laid out again, with the filter where the layout has it.
    act(() => {
      server.apply({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
    });
    await waitFor(() => {
      expect(screen.getAllByText("ADSR", { selector: ".tang-node__name" })).toHaveLength(1);
    });
    expect(placeOf(await screen.findByTestId(`rf__node-${String(filter.id)}`))).toEqual(inHand);
    release(80, 40);
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(intents(requests)[0]?.node).toBe(filter.id);
  });

  test("a node that is put down and not kept goes back to where the session has it", async () => {
    const { client, requests } = await rig(<PatchView />);
    const { node, header } = await headerOf(filter.id);
    pull(header, 80, 40);
    expect(placeOf(node)).not.toEqual(laidOut(filter.id));
    client.stop(); // (the connection is lost while the node is in hand)
    release(80, 40);
    await waitFor(() => {
      expect(placeOf(node)).toEqual(laidOut(filter.id));
    });
    expect(requests()).toEqual([]);
  });
});

describe("arranging the nodes again", () => {
  const arrange = () => screen.findByRole("button", { name: "Arrange the nodes again" });

  test("is a button of the canvas, which has nothing to do until a node has been moved", async () => {
    const { server } = await rig(<PatchView />);
    expect((await arrange()).hasAttribute("disabled")).toBe(true);
    act(() => {
      server.apply({ type: "setNodeUi", node: oscillator.id, ui: { x: 10, y: 20 } });
    });
    await waitFor(async () => {
      expect((await arrange()).hasAttribute("disabled")).toBe(false);
    });
    act(() => {
      server.apply({ type: "setNodeUi", node: oscillator.id, ui: null });
    });
    await waitFor(async () => {
      expect((await arrange()).hasAttribute("disabled")).toBe(true);
    });
  });

  test("puts every node that was moved back where the layout has it, and asks for each", async () => {
    const { server, requests } = await rig(<PatchView />);
    act(() => {
      server.apply({ type: "setNodeUi", node: oscillator.id, ui: { x: 10, y: 20 } });
      server.apply({ type: "setNodeUi", node: filter.id, ui: { x: 300, y: -40 } });
    });
    const node = await screen.findByTestId(`rf__node-${String(filter.id)}`);
    await waitFor(() => {
      expect(placeOf(node)).toEqual({ x: 300, y: -40 });
    });
    fireEvent.click(await arrange());
    await waitFor(() => {
      expect(placeOf(node)).toEqual(laidOut(filter.id));
    });
    expect(placeOf(await screen.findByTestId(`rf__node-${String(oscillator.id)}`))).toEqual(laidOut(oscillator.id));
    expect(intents(requests).map((sent) => [sent.type, sent.node, sent.ui])).toEqual([
      ["setNodeUi", oscillator.id, null],
      ["setNodeUi", filter.id, null],
    ]);
  });
});
