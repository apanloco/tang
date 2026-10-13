import { describe, expect, test } from "vitest";

import { nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import { nodeHeight, NODE_WIDTH, toFlow } from "./toFlow";
import { MARGIN, rectOf, viewToShow } from "./view";

const SIZE = { width: 1000, height: 700 };
const MIN_ZOOM = 0.2;

describe("where the canvas has to go to show a rectangle", () => {
  test("is nowhere when all of it is in the view already, with room at the edges", () => {
    const view = { x: 0, y: 0, zoom: 1 };
    expect(viewToShow({ x: MARGIN, y: MARGIN, width: 300, height: 200 }, view, SIZE, MIN_ZOOM)).toBeNull();
    expect(viewToShow({ x: 100, y: 100, width: SIZE.width - 200, height: SIZE.height - 200 }, view, SIZE, MIN_ZOOM)).toBeNull();
    // (In the view as it is moved and zoomed: the screen is the canvas times the zoom, and the view's own offset.)
    expect(viewToShow({ x: 100, y: 100, width: 100, height: 100 }, { x: 50, y: -20, zoom: 2 }, SIZE, MIN_ZOOM)).toBeNull();
  });

  test("is nowhere when it just fits, with the margin to spare at every edge", () => {
    const view = { x: 0, y: 0, zoom: 1 };
    const whole = { x: MARGIN, y: MARGIN, width: SIZE.width - 2 * MARGIN, height: SIZE.height - 2 * MARGIN };
    expect(viewToShow(whole, view, SIZE, MIN_ZOOM)).toBeNull();
    // (A pixel more at any edge and it is not.)
    expect(viewToShow({ ...whole, x: whole.x - 1 }, view, SIZE, MIN_ZOOM)).not.toBeNull();
    expect(viewToShow({ ...whole, y: whole.y - 1 }, view, SIZE, MIN_ZOOM)).not.toBeNull();
    expect(viewToShow({ ...whole, width: whole.width + 1 }, view, SIZE, MIN_ZOOM)).not.toBeNull();
    expect(viewToShow({ ...whole, height: whole.height + 1 }, view, SIZE, MIN_ZOOM)).not.toBeNull();
  });

  test("is the view with the rectangle in the middle, at the zoom it has, when it is out of the view", () => {
    // Below the bottom: its middle (250, 1000) goes to the middle of the screen (500, 350).
    expect(viewToShow({ x: 100, y: 800, width: 300, height: 400 }, { x: 0, y: 0, zoom: 1 }, SIZE, MIN_ZOOM)).toEqual({ x: 250, y: -650, zoom: 1 });
    // To the right of it, and above: the same.
    expect(viewToShow({ x: 1500, y: -300, width: 200, height: 100 }, { x: 0, y: 0, zoom: 1 }, SIZE, MIN_ZOOM)).toEqual({ x: -1100, y: 600, zoom: 1 });
  });

  test("keeps the zoom the person has, whatever it is, when the rectangle fits at it", () => {
    const next = viewToShow({ x: 0, y: 1200, width: 304, height: 380 }, { x: 10, y: 10, zoom: 0.5 }, SIZE, MIN_ZOOM);
    expect(next?.zoom).toBe(0.5);
    // Its middle (152, 1390) at that zoom is at (76, 695) from the corner of the canvas: the view moves it to the middle of the screen.
    expect(next).toEqual({ x: 500 - 152 * 0.5, y: 350 - 1390 * 0.5, zoom: 0.5 });
  });

  test("is not in the view when it is within the margin of the edge, or half out of it", () => {
    const view = { x: 0, y: 0, zoom: 1 };
    expect(viewToShow({ x: MARGIN - 1, y: 100, width: 100, height: 100 }, view, SIZE, MIN_ZOOM)).not.toBeNull();
    expect(viewToShow({ x: 100, y: SIZE.height - MARGIN - 99, width: 100, height: 100 }, view, SIZE, MIN_ZOOM)).not.toBeNull();
    expect(viewToShow({ x: SIZE.width - 50, y: 100, width: 100, height: 100 }, view, SIZE, MIN_ZOOM)).not.toBeNull();
  });

  test("zooms out, and never in, when the rectangle does not fit", () => {
    // 900 tall in a screen of 700 with a margin of 32 each side: 636 / 900.
    const next = viewToShow({ x: 0, y: 0, width: 300, height: 900 }, { x: 0, y: 0, zoom: 1 }, SIZE, MIN_ZOOM);
    expect(next?.zoom).toBeCloseTo((SIZE.height - 2 * MARGIN) / 900, 10);
    // Wide: the width decides.
    const wide = viewToShow({ x: 0, y: 0, width: 2000, height: 100 }, { x: 0, y: 0, zoom: 1 }, SIZE, MIN_ZOOM);
    expect(wide?.zoom).toBeCloseTo((SIZE.width - 2 * MARGIN) / 2000, 10);
    // A small one off the screen does not zoom in on the way.
    expect(viewToShow({ x: 3000, y: 0, width: 10, height: 10 }, { x: 0, y: 0, zoom: 0.6 }, SIZE, MIN_ZOOM)?.zoom).toBe(0.6);
  });

  test("does not zoom out past what the canvas allows", () => {
    expect(viewToShow({ x: 0, y: 0, width: 100_000, height: 100_000 }, { x: 0, y: 0, zoom: 1 }, SIZE, MIN_ZOOM)?.zoom).toBe(MIN_ZOOM);
  });

  test("is nowhere when the canvas has no size to judge by (it is not on the screen)", () => {
    expect(viewToShow({ x: 5000, y: 5000, width: 300, height: 300 }, { x: 0, y: 0, zoom: 1 }, { width: 0, height: 0 }, MIN_ZOOM)).toBeNull();
    expect(viewToShow({ x: 5000, y: 5000, width: 300, height: 300 }, { x: 0, y: 0, zoom: 1 }, { width: 1000, height: 0 }, MIN_ZOOM)).toBeNull();
    expect(viewToShow({ x: 5000, y: 5000, width: 300, height: 300 }, { x: 0, y: 0, zoom: 1 }, { width: 0, height: 700 }, MIN_ZOOM)).toBeNull();
  });

  test("puts the rectangle exactly in the middle of the screen, and in the view, when it moves", () => {
    const rect = { x: 777, y: -444, width: 304, height: 380 };
    const next = viewToShow(rect, { x: 0, y: 0, zoom: 1 }, SIZE, MIN_ZOOM);
    if (!next) throw new Error("the canvas did not move");
    // Where it is on the screen after: its middle is the screen's, so it is in the view, and the margin is the same all round.
    const left = rect.x * next.zoom + next.x;
    const top = rect.y * next.zoom + next.y;
    expect(left + (rect.width * next.zoom) / 2).toBeCloseTo(SIZE.width / 2, 10);
    expect(top + (rect.height * next.zoom) / 2).toBeCloseTo(SIZE.height / 2, 10);
    expect(viewToShow(rect, next, SIZE, MIN_ZOOM)).toBeNull();
  });
});

describe("where a node is on the canvas", () => {
  test("is where the layout put it, as wide as every node is and as tall as the layout allows for the node", () => {
    const graph = tracerSnapshot.graph;
    const flow = toFlow(graph);
    for (const node of graph.nodes) {
      const placed = flow.nodes.find((n) => n.id === String(node.id));
      const pinned = placed?.data.pinned.length ?? 0;
      expect(rectOf(graph, flow, node.id), node.name).toEqual({ x: placed?.position.x, y: placed?.position.y, width: NODE_WIDTH, height: nodeHeight(node, pinned) });
    }
  });

  test("is the tracer's oscillator at the left of the strip it is in", () => {
    const graph = tracerSnapshot.graph;
    const rect = rectOf(graph, toFlow(graph), nodeNamed(tracerSnapshot, "Oscillator").id);
    expect(rect?.x).toBe(0);
    expect(rect?.height).toBeGreaterThan(300);
  });

  test("is none for a node that is not there", () => {
    const graph = tracerSnapshot.graph;
    expect(rectOf(graph, toFlow(graph), 424242)).toBeNull();
  });

  test("is below the lanes there were for a lane that came after them", () => {
    const graph = reachFixture.snapshot.graph;
    const lanes = graph.nodes.filter((node) => node.kind === "instrument");
    const flow = toFlow(graph);
    const tops = lanes.map((lane) => rectOf(graph, flow, lane.id)?.y ?? Number.NaN);
    expect(tops).toEqual([...tops].sort((a, b) => a - b));
    expect(new Set(tops).size).toBe(tops.length);
  });
});
