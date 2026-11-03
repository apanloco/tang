// @vitest-environment jsdom
// Putting an effect in another place of its chain: every effect has a button that moves it one place earlier and one that moves it one place
// later, what the server does with it is laid out on the canvas, and the page says why when the effect could not be moved.
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { movesFixture } from "../../client/fixtures";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// The session of the moves fixture: a group (0) with four bus effects (1 to 4), a lane in it (5) with four effects (6 to 9), and a lane in no
// group (10) with three (11 to 13). The canvas numbers the nodes of one name across the session in the order of the graph: the bus's are
// Filter 1, Reverb 1, Filter 2 and Reverb 2, the first lane's Filter 3, Reverb 3, Filter 4 and Reverb 4, the second lane's Filter 5, Reverb 5
// and Filter 6.
const NAMES: Record<number, string> = { 1: "Filter 1", 2: "Reverb 1", 3: "Filter 2", 4: "Reverb 2", 6: "Filter 3", 7: "Reverb 3", 8: "Filter 4", 9: "Reverb 4", 11: "Filter 5", 12: "Reverb 5", 13: "Filter 6" };

const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);

/** Where the canvas draws a node, in its own pixels. */
function placeOf(node: HTMLElement): { x: number; y: number } {
  const found = /translate\((-?[\d.e-]+)px,\s*(-?[\d.e-]+)px\)/.exec(node.style.transform);
  if (!found?.[1] || !found[2]) throw new Error(`the node is not placed: ${node.style.transform}`);
  return { x: Number(found[1]), y: Number(found[2]) };
}

/** The button of the effect `id` that moves it one place earlier (or later) in its chain. */
const button = async (id: number, direction: "earlier" | "later"): Promise<HTMLElement> => within(await nodeOf(id)).findByRole("button", { name: `Move ${NAMES[id] ?? "?"} ${direction} in its chain` });

describe("moving an effect along its chain", () => {
  test("is offered on every effect, a lane's and a bus's, by two buttons that name it and the direction, and on no other node", async () => {
    await rig(<PatchView />, { snapshot: movesFixture.snapshot });
    for (const id of Object.keys(NAMES).map(Number)) {
      expect(await button(id, "earlier")).toBeTruthy();
      expect(await button(id, "later")).toBeTruthy();
    }
    expect(screen.getAllByRole("button", { name: /^Move .* (earlier|later) in its chain$/ })).toHaveLength(22);
    // None on a group, a lane, a modulator or the master: what is moved is in a chain.
    for (const id of [0, 5, 10, 14, 15, 16, 4294967295]) {
      expect(within(await nodeOf(id)).queryByRole("button", { name: /^Move / })).toBeNull();
    }
  });

  test("says of the first effect of a chain that it cannot go earlier and of the last that it cannot go later, and the others can go both ways", async () => {
    await rig(<PatchView />, { snapshot: movesFixture.snapshot });
    const unavailable = async (id: number, direction: "earlier" | "later") => (await button(id, direction)).getAttribute("aria-disabled") === "true";
    // A lane's chain (6 to 9), a bus (1 to 4), and the chain of a lane in no group (11 to 13).
    for (const [first, last] of [[6, 9], [1, 4], [11, 13]] as const) {
      expect([await unavailable(first, "earlier"), await unavailable(first, "later")]).toEqual([true, false]);
      expect([await unavailable(last, "earlier"), await unavailable(last, "later")]).toEqual([false, true]);
    }
    expect([await unavailable(7, "earlier"), await unavailable(7, "later")]).toEqual([false, false]);
  });

  test("asks for nothing when a button that cannot go that way is pressed", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: movesFixture.snapshot });
    fireEvent.click(await button(6, "earlier"));
    fireEvent.click(await button(9, "later"));
    fireEvent.click(await button(1, "earlier"));
    expect(requests()).toEqual([]);
  });

  test("moves the effect one place later when its button is pressed: the intent names the place, and the canvas lays the chain out in the new order", async () => {
    const { requests, view } = await rig(<PatchView />, { snapshot: movesFixture.snapshot });
    const [filter, reverb] = [await nodeOf(6), await nodeOf(7)];
    const before = [placeOf(filter), placeOf(reverb)];
    expect(before[0]?.x).toBeLessThan(before[1]?.x ?? 0);
    const cables = view.container.querySelectorAll(".tang-cable--modulation").length;

    fireEvent.click(await button(6, "later"));
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "moveEffect", node: 6, index: 1 }]);
    });
    // The reverb is where the filter was and the filter where the reverb was; the cables are the same cables, on the same plugins.
    await waitFor(() => {
      expect(placeOf(filter).x).toBeGreaterThan(placeOf(reverb).x);
    });
    expect([placeOf(reverb), placeOf(filter)]).toEqual(before);
    expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(cables);
    // And the audio path goes lane, reverb, filter, reverb, ...: its links are drawn from the nodes they leave.
    expect(view.container.querySelector(`[id="audio:5>7"]`)).toBeTruthy();
    expect(view.container.querySelector(`[id="audio:7>6"]`)).toBeTruthy();
    expect(view.container.querySelector(`[id="audio:6>8"]`)).toBeTruthy();
    expect(view.container.querySelector(`[id="audio:5>6"]`)).toBeNull();
  });

  test("moves the effect one place earlier when its button is pressed", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: movesFixture.snapshot });
    fireEvent.click(await button(8, "earlier"));
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "moveEffect", node: 8, index: 1 }]);
    });
    await waitFor(() => {
      expect(placeOf(screen.getByTestId("rf__node-8")).x).toBeLessThan(placeOf(screen.getByTestId("rf__node-7")).x);
    });
  });

  test("moves a bus effect in the bus, and the effects of the lanes are where they were", async () => {
    const { requests, server } = await rig(<PatchView />, { snapshot: movesFixture.snapshot });
    const lanes = [placeOf(await nodeOf(6)), placeOf(await nodeOf(11))];
    fireEvent.click(await button(1, "later"));
    await waitFor(() => {
      expect(requests().map((request) => request.intent)).toEqual([{ type: "moveEffect", node: 1, index: 1 }]);
    });
    await waitFor(() => {
      expect(placeOf(screen.getByTestId("rf__node-1")).x).toBeGreaterThan(placeOf(screen.getByTestId("rf__node-2")).x);
    });
    expect([placeOf(await nodeOf(6)), placeOf(await nodeOf(11))]).toEqual(lanes);
    expect(server.snapshot.graph.nodes.filter((n) => n.kind === "effect" && n.parent === 0).map((n) => n.id)).toEqual([2, 1, 3, 4]);
  });

  test("keeps the focus on the button that was pressed, through each move, and the button of a direction the effect can no longer go says so", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: movesFixture.snapshot });
    const later = await button(6, "later");
    later.focus();
    for (const place of [1, 2, 3]) {
      fireEvent.click(later);
      await waitFor(() => {
        expect(document.activeElement).toBe(later);
        // (Each step moves the filter one place: the others are told by the buttons they have.)
        expect(later.getAttribute("aria-disabled")).toBe(place === 3 ? "true" : "false");
      });
    }
    // (The canvas numbers the nodes of one name in the order of the graph, so the filter that has come to the end of the chain is Filter 4 now.)
    expect(within(await nodeOf(6)).getByRole("button", { name: "Move Filter 4 earlier in its chain" }).getAttribute("aria-disabled")).toBe("false");
    // At the end of the chain pressing it again does nothing: nothing was asked.
    fireEvent.click(later);
    expect(document.activeElement).toBe(later);
    expect(requests()).toHaveLength(3);
  });

  test("gives the focus back to the button that was pressed when the browser lets it go as the canvas puts the effect in its place", async () => {
    await rig(<PatchView />, { snapshot: movesFixture.snapshot });
    const later = await button(6, "later");
    later.focus();
    fireEvent.click(later);
    // (A browser lets the focus of an element go when the element is moved among its siblings, as a node is when the chain is laid out in
    // another order: it is on the page itself. jsdom keeps it; the focus is let go here.)
    later.blur();
    expect(document.activeElement).toBe(document.body);
    await waitFor(() => {
      expect(document.activeElement).toBe(later);
    });
    // (The focus is kept for twenty frames: the test waits them out, or it would go on into the next test, which has a button of the same id.)
    await new Promise((resolve) => setTimeout(resolve, 500));
  });

  test("does not give the focus to a button that was pressed without having it (a browser does not give a button the focus when it is clicked, in Safari)", async () => {
    await rig(<PatchView />, { snapshot: movesFixture.snapshot });
    // (Another effect than the one above: what is kept for a button goes on for a few frames, into the next test.)
    const later = await button(11, "later");
    expect(document.activeElement).toBe(document.body);
    fireEvent.click(later);
    await waitFor(() => {
      expect(placeOf(screen.getByTestId("rf__node-11")).x).toBeGreaterThan(placeOf(screen.getByTestId("rf__node-12")).x);
    });
    await new Promise((resolve) => setTimeout(resolve, 150));
    expect(document.activeElement).toBe(document.body);
  });

  test("says why when the effect could not be moved, and it stays where it was", async () => {
    function Board() {
      return <NoticeBoard notices={useNotices()} />;
    }
    const { server, client } = await rig(
      <>
        <PatchView />
        <Board />
      </>,
      { snapshot: movesFixture.snapshot },
    );
    const filter = await nodeOf(6);
    const before = placeOf(filter);
    server.drop();
    await waitFor(() => {
      expect(client.state.status).not.toBe("open");
    });
    fireEvent.click(await button(6, "later"));
    expect(await screen.findByText("Could not move Filter 3 to place 2 of its chain: Not connected to the server.")).toBeTruthy();
    expect(placeOf(filter)).toEqual(before);
  });
});
