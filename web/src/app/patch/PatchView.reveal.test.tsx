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
import type { View } from "./view";

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

  test("judges a node against the view the canvas is moving to, and not against the one it is passing on the way", async () => {
    // (The canvas looks at the first lane, all of it, and the lane that is added below is out of the view.)
    canvas.view = { x: 60, y: -100, zoom: 1 };
    vi.useFakeTimers({ toFake: ["Date"] });
    const { server, show, rectFor } = await open();
    addLane(server);
    await waitFor(() => {
      expect(rectFor(3)).not.toBeNull();
    });
    act(() => {
      show.show(3);
    });
    await waitFor(() => {
      expect(canvas.setViewport).toHaveBeenCalledTimes(1);
    });
    const going = canvas.setViewport.mock.calls[0]?.[0] as View;
    // (The canvas has not left yet, as it has not a moment after it is told to: the first lane is all in the view it has, and not in the one it is going to.)
    const first = rectFor(0);
    if (!first) throw new Error("no rectangle");
    expect(viewToShow(first, canvas.view, { width: 1000, height: 700 }, 0.2)).toBeNull();
    const there = viewToShow(first, going, { width: 1000, height: 700 }, 0.2);
    expect(there).not.toBeNull();
    // Most of the move is still to go.
    vi.setSystemTime(Date.now() + 200);
    act(() => {
      show.show(0);
    });
    await waitFor(() => {
      expect(canvas.setViewport).toHaveBeenCalledTimes(2);
    });
    expect(canvas.setViewport).toHaveBeenLastCalledWith(there, { duration: 250 });
  });

  test("judges a node against where the canvas is looking once the move has had its time: a person who moved it since is not moved back", async () => {
    canvas.view = { x: 60, y: -100, zoom: 1 };
    vi.useFakeTimers({ toFake: ["Date"] });
    const { server, show, rectFor } = await open();
    addLane(server);
    await waitFor(() => {
      expect(rectFor(3)).not.toBeNull();
    });
    act(() => {
      show.show(3);
    });
    await waitFor(() => {
      expect(canvas.setViewport).toHaveBeenCalledTimes(1);
    });
    const going = canvas.setViewport.mock.calls[0]?.[0] as View;
    const first = rectFor(0);
    if (!first) throw new Error("no rectangle");
    expect(viewToShow(first, going, { width: 1000, height: 700 }, 0.2)).not.toBeNull();
    // The move is over, and the person has put the canvas back where the first lane is all in the view.
    vi.setSystemTime(Date.now() + 300);
    act(() => {
      show.show(0);
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
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
