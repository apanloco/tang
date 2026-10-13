// @vitest-environment jsdom
// The canvas moves to a node the page has added, when the node is not all in the view. (What
// the canvas knows of itself, its size and where it is looking, is made up here: in jsdom
// nothing has a size.)
import { act, cleanup, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { tracerSnapshot } from "../../client/fixtures";
import type { Reveals } from "../../client/reveals";
import { useReveals } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";
import { toFlow } from "./toFlow";
import { rectOf, viewToShow } from "./view";

// What the canvas says of itself: set by each test. (Made before the imports, which is when the mock below is made.)
const canvas = vi.hoisted(() => ({
  state: { width: 1000, height: 700, minZoom: 0.2 },
  view: { x: 0, y: 0, zoom: 1 },
  setViewport: vi.fn<(view: unknown, options?: unknown) => Promise<boolean>>(() => Promise.resolve(true)),
}));

vi.mock("@xyflow/react", async (importOriginal) => {
  const original = await importOriginal<typeof import("@xyflow/react")>();
  // (The same object every time, as the canvas's own is: what a view depends on it for does not change when it renders.)
  const instance = { getViewport: () => canvas.view, setViewport: canvas.setViewport };
  return {
    ...original,
    useReactFlow: () => instance,
    useStore: (select: (state: typeof canvas.state) => unknown) => select(canvas.state),
  };
});

beforeEach(() => {
  canvas.state = { width: 1000, height: 700, minZoom: 0.2 };
  canvas.view = { x: 0, y: 0, zoom: 1 };
  canvas.setViewport.mockClear();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

/** The page's way to ask for a node to be shown, as the editor does, and the session's layout as a view of it is made. */
async function open() {
  let reveals: Reveals | undefined;
  function Probe() {
    reveals = useReveals();
    return null;
  }
  const made = await rig(
    <>
      <PatchView />
      <Probe />
    </>,
  );
  if (!reveals) throw new Error("no reveals");
  const show = reveals;
  const rectFor = (id: number) => {
    const graph = made.client.state.snapshot?.graph;
    if (!graph) throw new Error("no session");
    return rectOf(graph, toFlow(graph), id);
  };
  return { ...made, show, rectFor };
}

const addLane = (server: { apply: (intent: { type: "addInstrument"; plugin: string }) => void }) => {
  act(() => {
    server.apply({ type: "addInstrument", plugin: "builtin:osc" });
  });
};

describe("showing a node the page added", () => {
  test("moves the canvas to it when it is out of the view: the node in the middle, at the zoom the person has", async () => {
    const { server, show, rectFor } = await open();
    addLane(server);
    await waitFor(() => {
      expect(rectFor(3)).not.toBeNull();
    });
    act(() => {
      show.show(3);
    });
    const rect = rectFor(3);
    if (!rect) throw new Error("no rectangle");
    const wanted = viewToShow(rect, canvas.view, { width: 1000, height: 700 }, 0.2);
    expect(wanted).not.toBeNull();
    await waitFor(() => {
      expect(canvas.setViewport).toHaveBeenCalledTimes(1);
    });
    expect(canvas.setViewport).toHaveBeenCalledWith(wanted, { duration: 250 });
  });

  test("does not move it when all of the node is in the view already", async () => {
    // (A screen tall enough for both lanes, looking at the canvas from a little way in, as the canvas does when it fits the nodes in.)
    canvas.state = { width: 1000, height: 5000, minZoom: 0.2 };
    canvas.view = { x: 60, y: 60, zoom: 1 };
    const { server, show } = await open();
    addLane(server);
    await waitFor(() => {
      expect(server.snapshot.graph.nodes.some((n) => n.id === 3)).toBe(true);
    });
    act(() => {
      show.show(3);
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    expect(canvas.setViewport).not.toHaveBeenCalled();
  });

  test("waits for the layout that has the node when it is asked for before the canvas has it, and moves then", async () => {
    const { server, show } = await open();
    act(() => {
      show.show(3); // the session has no node 3 yet
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    expect(canvas.setViewport).not.toHaveBeenCalled();
    addLane(server);
    await waitFor(() => {
      expect(canvas.setViewport).toHaveBeenCalledTimes(1);
    });
  });

  test("shows a node once: a layout that comes after does not move the canvas again", async () => {
    const { server, show } = await open();
    addLane(server);
    act(() => {
      show.show(3);
    });
    await waitFor(() => {
      expect(canvas.setViewport).toHaveBeenCalledTimes(1);
    });
    act(() => {
      server.apply({ type: "addModulator", parent: 0, kind: "lfo" });
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    expect(canvas.setViewport).toHaveBeenCalledTimes(1);
  });

  test("moves for what the page added, and not for what another front-end added", async () => {
    const { server } = await open();
    addLane(server);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    expect(canvas.setViewport).not.toHaveBeenCalled();
  });

  test("gives up on a node that does not come: it is not shown when it does, much later", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    const { server, show } = await open();
    act(() => {
      show.show(3); // the session has no node 3 yet
    });
    vi.setSystemTime(Date.now() + 5000);
    addLane(server); // and when it has, nobody is waiting for it
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    expect(canvas.setViewport).not.toHaveBeenCalled();
    // What is asked for after is shown as usual.
    act(() => {
      show.show(3);
    });
    await waitFor(() => {
      expect(canvas.setViewport).toHaveBeenCalledTimes(1);
    });
  });

  test("does nothing for a canvas that is not on the screen (it has no size)", async () => {
    canvas.state = { width: 0, height: 0, minZoom: 0.2 };
    const { server, show } = await open();
    addLane(server);
    act(() => {
      show.show(3);
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 50));
    });
    expect(canvas.setViewport).not.toHaveBeenCalled();
    expect(tracerSnapshot.graph.nodes.length).toBe(4);
  });
});
