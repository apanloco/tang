// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import type { NodeChange } from "@xyflow/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import type { NodeId, NodeUi } from "../../protocol";
import type { PatchNode } from "./toFlow";
import { KEYS_REST_MS, useMovableNodes } from "./useMovableNodes";

afterEach(() => {
  vi.useRealTimers();
});

// The nodes as a person moves them (step 107: the nodes that are chosen are moved together, with the node that is taken or with the arrow keys): where each is put down is told to the server, once, and a node is where the pointer
// has taken it until the server has said where it is.

/** A node of the canvas at a place, as `toFlow` makes one (the parts of it that moving reads: the id on the canvas, the session's node and the place). */
function node(id: number, x: number, y: number): PatchNode {
  const data = { node: id, name: `Node ${String(id)}` };
  return { id: String(id), type: "effect", position: { x, y }, data: data as unknown as PatchNode["data"] };
}

const ONE = node(1, 0, 0);
const TWO = node(2, 100, 0);
const THREE = node(3, 200, 0);
const LAID_OUT = [ONE, TWO, THREE];

/** A request to put a node somewhere that is answered when the test says: `answer(true)` for what the server kept. */
function server() {
  const told: { node: NodeId; ui: NodeUi }[] = [];
  const waiting: ((kept: boolean) => void)[] = [];
  const moveNode = (target: NodeId, ui: NodeUi): Promise<boolean> => {
    told.push({ node: target, ui });
    return new Promise((resolve) => waiting.push(resolve));
  };
  return {
    told,
    moveNode,
    answer: (kept = true) => {
      for (const resolve of waiting.splice(0)) resolve(kept);
    },
  };
}

const positions = (nodes: readonly PatchNode[]): [string, number, number][] => nodes.map((each) => [each.id, each.position.x, each.position.y]);
const at = (target: PatchNode, x: number, y: number, dragging = true): PatchNode => ({ ...target, position: { x, y }, dragging });
const settle = async (): Promise<void> => {
  await act(async () => {
    await Promise.resolve();
  });
};

describe("moving nodes", () => {
  test("tells the server where a node was put down, and keeps it there until the server has said where it is", async () => {
    const { moveNode, told, answer } = server();
    const { result, rerender } = renderHook(({ laidOut }) => useMovableNodes(laidOut, moveNode), { initialProps: { laidOut: LAID_OUT } });
    const taken = ONE;
    act(() => {
      result.current.onNodeDragStart({} as MouseEvent, taken, [taken]);
    });
    act(() => {
      result.current.onNodesChange([{ id: "1", type: "position", position: { x: 40, y: 30 }, dragging: true }]);
    });
    expect(positions(result.current.nodes)[0]).toEqual(["1", 40, 30]);
    act(() => {
      result.current.onNodeDragStop({} as MouseEvent, at(taken, 40, 30, false), [at(taken, 40, 30, false)]);
    });
    expect(told).toEqual([{ node: 1, ui: { x: 40, y: 30 } }]);
    // The session is laid out again while the server has not answered: the node stays where it was put.
    rerender({ laidOut: [...LAID_OUT] });
    expect(positions(result.current.nodes)[0]).toEqual(["1", 40, 30]);
    answer(true);
    await settle();
    // (Answered: the layout is the session's, which has it where the server says.)
    rerender({ laidOut: [node(1, 40, 30), TWO, THREE] });
    expect(positions(result.current.nodes)[0]).toEqual(["1", 40, 30]);
  });

  test("tells nothing of a node that was put down where it was picked up", async () => {
    const { moveNode, told } = server();
    const { result } = renderHook(() => useMovableNodes(LAID_OUT, moveNode));
    const taken = TWO;
    act(() => {
      result.current.onNodeDragStart({} as MouseEvent, taken, [taken]);
      result.current.onNodeDragStop({} as MouseEvent, at(taken, 100, 0, false), [at(taken, 100, 0, false)]);
    });
    await settle();
    expect(told).toEqual([]);
  });

  test("moves every node that is chosen with the one that is taken, tells the server where each was put and holds them all until it has answered", async () => {
    const { moveNode, told, answer } = server();
    const { result, rerender } = renderHook(({ laidOut }) => useMovableNodes(laidOut, moveNode), { initialProps: { laidOut: LAID_OUT } });
    const [one, two, three] = [ONE, TWO, THREE];
    act(() => {
      result.current.onNodeDragStart({} as MouseEvent, one, [one, two]);
    });
    act(() => {
      result.current.onNodesChange([
        { id: "1", type: "position", position: { x: 10, y: 20 }, dragging: true },
        { id: "2", type: "position", position: { x: 110, y: 20 }, dragging: true },
      ]);
    });
    act(() => {
      result.current.onNodeDragStop({} as MouseEvent, at(one, 10, 20, false), [at(one, 10, 20, false), at(two, 110, 20, false)]);
    });
    expect(told).toEqual([
      { node: 1, ui: { x: 10, y: 20 } },
      { node: 2, ui: { x: 110, y: 20 } },
    ]);
    // Both are held where they were put while the server has not answered, and the node that was not moved is where the layout has it.
    rerender({ laidOut: [...LAID_OUT] });
    expect(positions(result.current.nodes)).toEqual([
      ["1", 10, 20],
      ["2", 110, 20],
      ["3", three.position.x, three.position.y],
    ]);
    answer(true);
    await settle();
  });

  test("tells only of the nodes that were moved when some of those that are chosen were put down where they were", async () => {
    const { moveNode, told } = server();
    const { result } = renderHook(() => useMovableNodes(LAID_OUT, moveNode));
    const [one, two] = [ONE, TWO];
    act(() => {
      result.current.onNodeDragStart({} as MouseEvent, one, [one, two]);
      result.current.onNodeDragStop({} as MouseEvent, at(one, 5, 0, false), [at(one, 5, 0, false), at(two, 100, 0, false)]);
    });
    await settle();
    expect(told).toEqual([{ node: 1, ui: { x: 5, y: 0 } }]);
  });

  test("takes the node that was taken for the one that moved when React Flow says of no others", async () => {
    const { moveNode, told } = server();
    const { result } = renderHook(() => useMovableNodes(LAID_OUT, moveNode));
    const one = ONE;
    act(() => {
      result.current.onNodeDragStart({} as MouseEvent, one, []);
      result.current.onNodeDragStop({} as MouseEvent, at(one, 7, 8, false), []);
    });
    await settle();
    expect(told).toEqual([{ node: 1, ui: { x: 7, y: 8 } }]);
  });

  test("goes back to where the session has a node when the server refused where it was put", async () => {
    const { moveNode, answer } = server();
    const { result, rerender } = renderHook(({ laidOut }) => useMovableNodes(laidOut, moveNode), { initialProps: { laidOut: LAID_OUT } });
    const one = ONE;
    act(() => {
      result.current.onNodeDragStart({} as MouseEvent, one, [one]);
      result.current.onNodesChange([{ id: "1", type: "position", position: { x: 40, y: 30 }, dragging: true }]);
      result.current.onNodeDragStop({} as MouseEvent, at(one, 40, 30, false), [at(one, 40, 30, false)]);
    });
    rerender({ laidOut: [...LAID_OUT] });
    answer(false);
    await settle();
    expect(positions(result.current.nodes)[0]).toEqual(["1", 0, 0]);
  });
});

describe("moving nodes with the keys", () => {
  const key = (id: string, x: number, y: number): NodeChange<PatchNode> => ({ id, type: "position", position: { x, y }, dragging: false });

  test("tells the server once the keys have rested, where each node that was moved is, and not before", async () => {
    vi.useFakeTimers();
    const { moveNode, told, answer } = server();
    const { result } = renderHook(() => useMovableNodes(LAID_OUT, moveNode));
    act(() => {
      result.current.onNodesChange([key("1", 5, 0), key("2", 105, 0)]);
    });
    act(() => {
      vi.advanceTimersByTime(KEYS_REST_MS - 1);
    });
    expect(told).toEqual([]);
    // Another step of the same keys: the rest starts again, and the place that is told is the last.
    act(() => {
      result.current.onNodesChange([key("1", 10, 0), key("2", 110, 0)]);
    });
    act(() => {
      vi.advanceTimersByTime(KEYS_REST_MS - 1);
    });
    expect(told).toEqual([]);
    act(() => {
      vi.advanceTimersByTime(1);
    });
    expect(told).toEqual([
      { node: 1, ui: { x: 10, y: 0 } },
      { node: 2, ui: { x: 110, y: 0 } },
    ]);
    answer(true);
    await act(async () => {
      await Promise.resolve();
    });
  });

  test("holds a node that the keys moved where it is until the server has been told and has answered, whatever the session is laid out as meanwhile", async () => {
    vi.useFakeTimers();
    const { moveNode, answer } = server();
    const { result, rerender } = renderHook(({ laidOut }) => useMovableNodes(laidOut, moveNode), { initialProps: { laidOut: LAID_OUT } });
    act(() => {
      result.current.onNodesChange([key("3", 230, 0)]);
    });
    // Before it is told: laid out again, and the node stays.
    rerender({ laidOut: [...LAID_OUT] });
    expect(positions(result.current.nodes)[2]).toEqual(["3", 230, 0]);
    act(() => {
      vi.advanceTimersByTime(KEYS_REST_MS);
    });
    // Told and not answered: it stays.
    rerender({ laidOut: [...LAID_OUT] });
    expect(positions(result.current.nodes)[2]).toEqual(["3", 230, 0]);
    // A key pressed while the server is answering is told as well.
    act(() => {
      result.current.onNodesChange([key("3", 235, 0)]);
    });
    answer(true);
    await act(async () => {
      await Promise.resolve();
    });
    expect(positions(result.current.nodes)[2]).toEqual(["3", 235, 0]);
    act(() => {
      vi.advanceTimersByTime(KEYS_REST_MS);
    });
    answer(true);
    await act(async () => {
      await Promise.resolve();
    });
  });

  test("lets go of a node that the keys moved more than once, when the server has answered: it is held once, and not once for each step", async () => {
    vi.useFakeTimers();
    const { moveNode, answer } = server();
    const { result, rerender } = renderHook(({ laidOut }) => useMovableNodes(laidOut, moveNode), { initialProps: { laidOut: LAID_OUT } });
    act(() => {
      result.current.onNodesChange([key("3", 210, 0)]);
    });
    act(() => {
      result.current.onNodesChange([key("3", 220, 0)]);
    });
    act(() => {
      vi.advanceTimersByTime(KEYS_REST_MS);
    });
    answer(true);
    await act(async () => {
      await Promise.resolve();
    });
    // The session is laid out as it says, and the node is where it says: it is not held.
    rerender({ laidOut: [ONE, TWO, node(3, 250, 40)] });
    expect(positions(result.current.nodes)[2]).toEqual(["3", 250, 40]);
  });

  test("tells a move of the keys of a node that was dragged before: the drag is over when the node is put down", () => {
    vi.useFakeTimers();
    const { moveNode, told } = server();
    const { result } = renderHook(() => useMovableNodes(LAID_OUT, moveNode));
    const one = ONE;
    act(() => {
      result.current.onNodeDragStart({} as MouseEvent, one, [one]);
    });
    act(() => {
      result.current.onNodeDragStop({} as MouseEvent, at(one, 40, 30, false), [at(one, 40, 30, false)]);
    });
    expect(told).toEqual([{ node: 1, ui: { x: 40, y: 30 } }]);
    act(() => {
      result.current.onNodesChange([key("1", 45, 30)]);
    });
    act(() => {
      vi.advanceTimersByTime(KEYS_REST_MS);
    });
    expect(told).toEqual([
      { node: 1, ui: { x: 40, y: 30 } },
      { node: 1, ui: { x: 45, y: 30 } },
    ]);
  });

  test("tells nothing of a move that is a drag's: the last place of a drag is told when the node is put down, and not by the keys' rest", () => {
    vi.useFakeTimers();
    const { moveNode, told } = server();
    const { result } = renderHook(() => useMovableNodes(LAID_OUT, moveNode));
    const one = ONE;
    act(() => {
      result.current.onNodeDragStart({} as MouseEvent, one, [one]);
    });
    // (React Flow says where a node is put down once more, before it says that it was.)
    act(() => {
      result.current.onNodesChange([key("1", 40, 30)]);
    });
    act(() => {
      vi.advanceTimersByTime(KEYS_REST_MS * 2);
    });
    expect(told).toEqual([]);
    act(() => {
      result.current.onNodeDragStop({} as MouseEvent, at(one, 40, 30, false), [at(one, 40, 30, false)]);
    });
    expect(told).toEqual([{ node: 1, ui: { x: 40, y: 30 } }]);
  });

  test("tells nothing of a node that went while the keys rested, and nothing of a change that is no move", () => {
    vi.useFakeTimers();
    const { moveNode, told } = server();
    const { result, rerender } = renderHook(({ laidOut }) => useMovableNodes(laidOut, moveNode), { initialProps: { laidOut: LAID_OUT } });
    act(() => {
      result.current.onNodesChange([{ id: "1", type: "select", selected: true }, { id: "2", type: "position", position: { x: 1, y: 1 }, dragging: true }, { id: "3", type: "position", dragging: false }]);
    });
    act(() => {
      vi.advanceTimersByTime(KEYS_REST_MS * 2);
    });
    expect(told).toEqual([]);
    act(() => {
      result.current.onNodesChange([key("3", 230, 0)]);
    });
    rerender({ laidOut: [ONE, TWO] });
    act(() => {
      vi.advanceTimersByTime(KEYS_REST_MS);
    });
    expect(told).toEqual([]);
  });

  test("lets go of the wait when it is taken away", () => {
    vi.useFakeTimers();
    const { moveNode, told } = server();
    const { result, unmount } = renderHook(() => useMovableNodes(LAID_OUT, moveNode));
    act(() => {
      result.current.onNodesChange([key("1", 5, 0)]);
    });
    unmount();
    vi.advanceTimersByTime(KEYS_REST_MS * 2);
    expect(told).toEqual([]);
  });
});
