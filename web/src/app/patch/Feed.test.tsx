// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { isLink } from "../../client/cables";
import { Editor } from "../../client/editor";
import { audioCablesFixture, nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import { linksOf } from "../../client/wiring";
import type { Edge, Snapshot } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

// The link into an effect from the stage before it in its chain (the controller's `SetFeed`: what it makes of the request is in `apply.rs`, `history.rs` and `wiring.test.ts`, and the mock engine's agreement
// with it in `history.test.ts`): a button in the header of every effect, which says where it is fed from and gives the link back when it is cut, and the link itself, which has the button that cuts it, at its
// middle, as every link of the audio path has. What the page shows of it, what it asks and when, and what it says when it is refused.

const filter = nodeNamed(tracerSnapshot, "Filter");

/** The button of the input of an effect, by the name the canvas gives it. */
const input = (name = "Filter") => screen.findByRole("button", { name: `Cut the input of ${name}` });

/** What the page says when it could not do what was asked. */
function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

/** `snapshot` with the feed of the effects `ids` cut, and the links of the audio path that go with it taken out (as the controller's graph has them). */
function cutAt(snapshot: Snapshot, ...ids: number[]): Snapshot {
  const copy = structuredClone(snapshot);
  const nodes = copy.graph.nodes.map((n) => (n.kind === "effect" && ids.includes(n.id) ? { ...n, feed: "none" as const } : n));
  copy.graph = { nodes, edges: [...linksOf(nodes), ...copy.graph.edges.filter((e) => !isLink(e))] };
  return copy;
}

describe("the input of an effect", () => {
  test("is a button in the header of every effect, a lane's and a bus's, and on nothing else", async () => {
    await rig(<PatchView />);
    expect(await input()).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /^Cut the input of / })).toHaveLength(1);
    cleanup();

    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    await screen.findAllByRole("button", { name: /^Cut the input of / });
    const effects = reachFixture.snapshot.graph.nodes.filter((node) => node.kind === "effect");
    expect(effects.length).toBe(5);
    expect(screen.getAllByRole("button", { name: /^Cut the input of / })).toHaveLength(effects.length);
    // (Not on a lane, a group, a modulator or the master: they take no audio in from a chain.)
    for (const node of reachFixture.snapshot.graph.nodes.filter((n) => n.kind !== "effect" && n.kind !== "master")) {
      const found = await screen.findByTestId(`rf__node-${String(node.id)}`);
      expect(within(found).queryByRole("button", { name: /^Cut the input of / }), `node ${String(node.id)}`).toBeNull();
    }
  });

  test("is in the header, on the line of the badge and the button that sends the audio of the node", async () => {
    await rig(<PatchView />);
    const button = await input();
    const title = button.closest<HTMLElement>(".tang-node__title");
    if (!title) throw new Error("the button is not in the title of a node");
    expect(within(title).getAllByRole("button").map((b) => b.getAttribute("aria-label"))).toEqual(["Send audio from Filter", "Cut the input of Filter"]);
  });

  test("says where the effect is fed from: the lane's instrument for the first effect of a lane, the effect before it for the others, and the group for the first effect of a bus", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    const buttons = await screen.findAllByRole("button", { name: /^Cut the input of / });
    expect(Object.fromEntries(buttons.map((button) => [button.getAttribute("aria-label"), button.textContent]))).toEqual({
      "Cut the input of Filter 1": "← Group 1",
      "Cut the input of Reverb 1": "← Group 2",
      "Cut the input of Filter 2": "← Oscillator 1",
      "Cut the input of Reverb 2": "← Oscillator 3",
      "Cut the input of Filter 3": "← Oscillator 4",
    });
    cleanup();
    // Two effects in a chain: the second is fed by the first.
    await rig(<PatchView />, { snapshot: audioCablesFixture.snapshot });
    expect((await input("Delay 2")).textContent).toBe("← Filter 2");
    expect((await input("Filter 2")).textContent).toBe("← Oscillator 1");
  });

  test("is pressed, and says that there is none, when it is cut: an effect of a lane's chain, and of a bus", async () => {
    await rig(<PatchView />);
    expect((await input()).getAttribute("aria-pressed")).toBe("false");
    cleanup();
    await rig(<PatchView />, { snapshot: cutAt(tracerSnapshot, filter.id) });
    expect((await input()).getAttribute("aria-pressed")).toBe("true");
    expect((await input()).textContent).toBe("no in");
    cleanup();

    // The effect's own: the others of the chain, and the other chains, keep theirs.
    await rig(<PatchView />, { snapshot: cutAt(reachFixture.snapshot, 1) });
    expect((await input("Filter 1")).getAttribute("aria-pressed")).toBe("true");
    expect((await input("Filter 1")).textContent).toBe("no in");
    expect((await input("Reverb 1")).getAttribute("aria-pressed")).toBe("false");
    expect((await input("Filter 2")).getAttribute("aria-pressed")).toBe("false");
  });

  test("a press asks the server to cut it, and the button shows what the server has done; a second press gives it back", async () => {
    const { requests } = await rig(<PatchView />);
    fireEvent.click(await input());
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Cut the input of Filter" }).getAttribute("aria-pressed")).toBe("true");
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setFeed", node: filter.id, feed: "none" }]);
    expect(screen.getByRole("button", { name: "Cut the input of Filter" }).textContent).toBe("no in");
    fireEvent.click(screen.getByRole("button", { name: "Cut the input of Filter" }));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Cut the input of Filter" }).getAttribute("aria-pressed")).toBe("false");
    });
    expect(requests().map((request) => request.intent).at(-1)).toEqual({ type: "setFeed", node: filter.id, feed: "chain" });
    expect(screen.getByRole("button", { name: "Cut the input of Filter" }).textContent).toBe("← Oscillator");
  });

  test("a press on a bus effect's is for that effect, and leaves the others as they were", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    fireEvent.click(await input("Filter 1"));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Cut the input of Filter 1" }).getAttribute("aria-pressed")).toBe("true");
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setFeed", node: 1, feed: "none" }]);
    expect(screen.getByRole("button", { name: "Cut the input of Filter 2" }).getAttribute("aria-pressed")).toBe("false");
    expect(screen.getByRole("button", { name: "Cut the input of Reverb 1" }).getAttribute("aria-pressed")).toBe("false");
  });

  test("has a title that says what a press does: it cuts the link, to feed the effect by its cables only, or gives it back, and from where", async () => {
    await rig(<PatchView />);
    expect((await input()).title).toBe("Filter is fed by Oscillator: cut its input, to feed it only by cables");
    cleanup();
    await rig(<PatchView />, { snapshot: cutAt(tracerSnapshot, filter.id) });
    expect((await input()).title).toBe("Filter has no input from its chain and is fed only by its cables: give it its input back, from Oscillator");
  });

  test("is refused, and says why among the notices, when giving it back would make the audio go round by the cables there are: the button is as it was", async () => {
    // Delay 2 is after Filter 2 in the chain of lane 4, and a cable sends it back to Filter 2: with the delay's link cut that makes no loop, and given back it would.
    const snapshot = cutAt(audioCablesFixture.snapshot, 6);
    const cable: Edge = { kind: "cable", id: 100, gain: 1, from: { node: 6, port: { type: "audioOut" } }, to: { node: 5, port: { type: "audioIn" } } };
    snapshot.graph = { ...snapshot.graph, edges: [...snapshot.graph.edges.filter((e) => e.kind !== "cable"), cable] };
    const { requests } = await rig(
      <>
        <PatchView />
        <Board />
      </>,
      { snapshot },
    );
    const delay = await input("Delay 2");
    expect(delay.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(delay);
    expect(await screen.findByText(/^Could not give Delay 2 its input back: giving that effect its input back would make the audio go round, by the cables there are/)).toBeTruthy();
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setFeed", node: 6, feed: "chain" }]);
    expect(screen.getByRole("button", { name: "Cut the input of Delay 2" }).getAttribute("aria-pressed")).toBe("true");
    // (Cutting a link makes no loop, and is done.)
    fireEvent.click(await input("Filter 2"));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Cut the input of Filter 2" }).getAttribute("aria-pressed")).toBe("true");
    });
  });
});

// Every link of the audio path is a cable that can be cut, and has the button that does it at its middle: the end of a chain is the way out of its lane or its group (`WayOut.test.tsx`), and a link into an
// effect is the input of that effect, the same as the button in its header, which is the one that gives it back.

/** The button on a link, by the words of the link (`Oscillator → Filter`). */
const disconnect = (words: string) => screen.findByRole("button", { name: `Disconnect ${words}` });

describe("the link into an effect", () => {
  test("has a button that disconnects it, as the links that are the way out of a chain have: every link of the audio path has one", async () => {
    // (The tracer's lane: its instrument goes to its filter, which goes to the master.)
    await rig(<PatchView />);
    expect(await disconnect("Oscillator → Filter")).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /^Disconnect / }).map((button) => button.getAttribute("aria-label"))).toEqual(["Disconnect Oscillator → Filter", "Disconnect Filter → Master"]);
  });

  test("is on every link into an effect, a lane's and a bus's, from the stage before it, whichever of the lane or the group or the effect that is", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    await disconnect("Oscillator 4 → Filter 3");
    expect(screen.getAllByRole("button", { name: /^Disconnect / }).map((button) => button.getAttribute("aria-label"))).toEqual([
      "Disconnect Group 1 → Filter 1",
      "Disconnect Filter 1 → Master",
      "Disconnect Group 2 → Reverb 1",
      "Disconnect Reverb 1 → Master",
      "Disconnect Oscillator 1 → Filter 2",
      "Disconnect Filter 2 → Group 1",
      "Disconnect Oscillator 2 → Group 1",
      "Disconnect Oscillator 3 → Reverb 2",
      "Disconnect Reverb 2 → Group 2",
      "Disconnect Oscillator 4 → Filter 3",
      "Disconnect Filter 3 → Master",
      "Disconnect (empty) → Master",
    ]);
    cleanup();
    // A chain of two: the link between the effects has one too.
    await rig(<PatchView />, { snapshot: audioCablesFixture.snapshot });
    expect(await disconnect("Filter 2 → Delay 2")).toBeTruthy();
  });

  test("is not on a link that is not there: an effect whose input is cut has no link into it, and so no button, and has it again when it is given its input back", async () => {
    await rig(<PatchView />, { snapshot: cutAt(tracerSnapshot, filter.id) });
    await input();
    expect(screen.queryByRole("button", { name: "Disconnect Oscillator → Filter" })).toBeNull();
    // (The link out of the effect is there still: the chain goes on from it.)
    expect(screen.getByRole("button", { name: "Disconnect Filter → Master" })).toBeTruthy();
    fireEvent.click(await input());
    expect(await disconnect("Oscillator → Filter")).toBeTruthy();
  });

  test("a press asks the server to cut the input of the effect that the link goes into, and the link, with its button, is gone, and the effect says so", async () => {
    const { requests } = await rig(<PatchView />);
    fireEvent.click(await disconnect("Oscillator → Filter"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Disconnect Oscillator → Filter" })).toBeNull();
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setFeed", node: filter.id, feed: "none" }]);
    expect((await input()).getAttribute("aria-pressed")).toBe("true");
    expect((await input()).textContent).toBe("no in");
  });

  test("a press on the link between two effects is for the second, and leaves the first as it was", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: audioCablesFixture.snapshot });
    fireEvent.click(await disconnect("Filter 2 → Delay 2"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Disconnect Filter 2 → Delay 2" })).toBeNull();
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setFeed", node: 6, feed: "none" }]);
    expect((await input("Delay 2")).getAttribute("aria-pressed")).toBe("true");
    expect((await input("Filter 2")).getAttribute("aria-pressed")).toBe("false");
    // The focus is on the button of the effect, of all the inputs there are.
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cut the input of Delay 2" }));
    });
    // The link into the first is there still.
    expect(screen.getByRole("button", { name: "Disconnect Oscillator 1 → Filter 2" })).toBeTruthy();
  });

  test("puts the focus on the button that gives the input back, once the link is gone, and giving it back brings the link", async () => {
    await rig(<PatchView />);
    const button = await disconnect("Oscillator → Filter");
    button.focus();
    fireEvent.click(button);
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cut the input of Filter" }));
    });
    fireEvent.click(screen.getByRole("button", { name: "Cut the input of Filter" }));
    expect(await disconnect("Oscillator → Filter")).toBeTruthy();
  });

  test("leaves the link and the focus where they are when the server does not cut the input", async () => {
    const asked = vi.spyOn(Editor.prototype, "setFeed").mockResolvedValue(false);
    await rig(<PatchView />);
    const button = await disconnect("Oscillator → Filter");
    button.focus();
    fireEvent.click(button);
    await waitFor(() => {
      expect(asked).toHaveBeenCalledWith(filter.id, "none");
    });
    // (Long enough for a focus that was moved to be moved: it goes at once when the button is there, and else on the next frames.)
    await act(async () => {
      await new Promise((done) => setTimeout(done, 60));
    });
    expect(screen.getByRole("button", { name: "Disconnect Oscillator → Filter" })).toBe(button);
    expect(document.activeElement).toBe(button);
    expect((await input()).getAttribute("aria-pressed")).toBe("false");
  });

  test("has a title that says what a press does and where the input is given back", async () => {
    await rig(<PatchView />);
    expect((await disconnect("Oscillator → Filter")).title).toBe("Disconnect Oscillator → Filter: cut the link, to feed the effect only by its cables (the button in the effect's header gives it back)");
  });
});
