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

// The way out of a lane and of a group, a button under its mute and solo (the controller's `SetRoute`: what it makes of the request is in `apply.rs`, `history.rs` and `wiring.test.ts`, and the
// mock engine's agreement with it in `history.test.ts`): what the page shows of it, what it asks and when, and what it says when it is refused.

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

const wayOut = (name = "Oscillator") => screen.findByRole("button", { name: `Cut the way out of ${name}` });

/** What the page says when it could not do what was asked. */
function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

/** `snapshot` with the way out of the lanes and groups `ids` cut, and the links of the audio path that go with it taken out (as the controller's graph has them). */
function cutAt(snapshot: Snapshot, ...ids: number[]): Snapshot {
  const copy = structuredClone(snapshot);
  const nodes = copy.graph.nodes.map((n) => ((n.kind === "instrument" || n.kind === "group") && ids.includes(n.id) ? { ...n, route: "none" as const } : n));
  copy.graph = { nodes, edges: [...linksOf(nodes), ...copy.graph.edges.filter((e) => !isLink(e))] };
  return copy;
}

describe("the way out of a lane or a group", () => {
  test("is a button in the cell of every lane and every group, an empty lane too, and on nothing else", async () => {
    await rig(<PatchView />);
    expect(await wayOut()).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /^Cut the way out of / })).toHaveLength(1);
    cleanup();

    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    await screen.findAllByRole("button", { name: /^Cut the way out of / });
    const owners = reachFixture.snapshot.graph.nodes.filter((node) => node.kind === "instrument" || node.kind === "group");
    expect(owners.some((node) => node.kind === "instrument" && node.plugin === null)).toBe(true);
    expect(owners.some((node) => node.kind === "group")).toBe(true);
    expect(screen.getAllByRole("button", { name: /^Cut the way out of / })).toHaveLength(owners.length);
  });

  test("is under the mute and the solo, in their cell", async () => {
    await rig(<PatchView />);
    const cell = (await wayOut()).closest<HTMLElement>(".tang-cell");
    if (!cell) throw new Error("the button is in no cell");
    expect(within(cell).getAllByRole("button").map((button) => button.getAttribute("aria-label"))).toEqual(["Mute Oscillator", "Solo Oscillator", "Cut the way out of Oscillator"]);
  });

  test("is not on an effect, a modulator or the master", async () => {
    await rig(<PatchView />);
    await wayOut();
    for (const id of [filter.id, nodeNamed(tracerSnapshot, "LFO").id]) {
      const node = await screen.findByTestId(`rf__node-${String(id)}`);
      expect(within(node).queryByRole("button", { name: /^Cut the way out of / }), `node ${String(id)}`).toBeNull();
    }
    expect(screen.getAllByRole("button", { name: /^Cut the way out of / })).toHaveLength(1);
  });

  test("says where the audio goes: to the master from a group and from a lane that is in no group, to its group from a lane that is in one", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    const buttons = await screen.findAllByRole("button", { name: /^Cut the way out of / });
    expect(Object.fromEntries(buttons.map((button) => [button.getAttribute("aria-label"), button.textContent]))).toEqual({
      "Cut the way out of Group 1": "→ Master",
      "Cut the way out of Group 2": "→ Master",
      "Cut the way out of Oscillator 1": "→ Group 1",
      "Cut the way out of Oscillator 2": "→ Group 1",
      "Cut the way out of Oscillator 3": "→ Group 2",
      "Cut the way out of Oscillator 4": "→ Master",
      "Cut the way out of (empty)": "→ Master",
    });
  });

  test("is pressed, and says that there is none, when it is cut: a lane's, and a group's", async () => {
    await rig(<PatchView />);
    expect((await wayOut()).getAttribute("aria-pressed")).toBe("false");
    cleanup();
    await rig(<PatchView />, { snapshot: cutAt(tracerSnapshot, oscillator.id) });
    expect((await wayOut()).getAttribute("aria-pressed")).toBe("true");
    expect((await wayOut()).textContent).toBe("no out");
    cleanup();

    // The group's is the group's: the lanes in it keep theirs.
    await rig(<PatchView />, { snapshot: cutAt(reachFixture.snapshot, 0) });
    expect((await wayOut("Group 1")).getAttribute("aria-pressed")).toBe("true");
    expect((await wayOut("Group 1")).textContent).toBe("no out");
    expect((await wayOut("Group 2")).getAttribute("aria-pressed")).toBe("false");
    expect((await wayOut("Oscillator 1")).getAttribute("aria-pressed")).toBe("false");
    expect((await wayOut("Oscillator 1")).textContent).toBe("→ Group 1");
  });

  test("a press asks the server to cut it, and the button shows what the server has done; a second press gives it back", async () => {
    const { requests } = await rig(<PatchView />);
    fireEvent.click(await wayOut());
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Cut the way out of Oscillator" }).getAttribute("aria-pressed")).toBe("true");
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setRoute", node: oscillator.id, route: "none" }]);
    expect(screen.getByRole("button", { name: "Cut the way out of Oscillator" }).textContent).toBe("no out");
    fireEvent.click(screen.getByRole("button", { name: "Cut the way out of Oscillator" }));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Cut the way out of Oscillator" }).getAttribute("aria-pressed")).toBe("false");
    });
    expect(requests().map((request) => request.intent).at(-1)).toEqual({ type: "setRoute", node: oscillator.id, route: "out" });
    expect(screen.getByRole("button", { name: "Cut the way out of Oscillator" }).textContent).toBe("→ Master");
  });

  test("a press on a group's is for the group, and leaves the lanes in it as they were", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    const group = reachFixture.snapshot.graph.nodes.find((node) => node.kind === "group");
    if (!group) throw new Error("the fixture has a group");
    fireEvent.click(await wayOut(group.name));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: `Cut the way out of ${group.name}` }).getAttribute("aria-pressed")).toBe("true");
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setRoute", node: group.id, route: "none" }]);
    expect(screen.getByRole("button", { name: "Cut the way out of Oscillator 1" }).getAttribute("aria-pressed")).toBe("false");
  });

  test("has a title that says what a press does: it cuts the way out, to hear the node only through its cables, or gives it back, and where it goes", async () => {
    await rig(<PatchView />);
    expect((await wayOut()).title).toBe("Oscillator goes to Master: cut its way out, to hear it only through its cables");
    cleanup();
    await rig(<PatchView />, { snapshot: cutAt(tracerSnapshot, oscillator.id) });
    expect((await wayOut()).title).toBe("Oscillator has no way out and is heard only through its cables: give it its way out, to Master");
    cleanup();
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    expect((await wayOut("Oscillator 3")).title).toBe("Oscillator 3 goes to Group 2: cut its way out, to hear it only through its cables");
  });

  test("is refused, and says why among the notices, when giving it back would make the audio go round by the cables there are: the button is as it was", async () => {
    // Pad's output goes into Filter 5, an effect of the chain of lane 4, which is in Pad: with the lane's way out cut that makes no loop, and given back it would.
    const snapshot = cutAt(audioCablesFixture.snapshot, 4);
    const cable: Edge = { kind: "cable", id: 100, gain: 1, from: { node: 0, port: { type: "audioOut" } }, to: { node: 5, port: { type: "audioIn" } } };
    snapshot.graph = { ...snapshot.graph, edges: [...snapshot.graph.edges.filter((e) => e.kind !== "cable"), cable] };
    const { requests } = await rig(
      <>
        <PatchView />
        <Board />
      </>,
      { snapshot },
    );
    const lane = await wayOut("Oscillator 1");
    expect(lane.getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(lane);
    expect(await screen.findByText(/^Could not give Oscillator 1 its way out: giving that lane its way out would make the audio go round, by the cables there are/)).toBeTruthy();
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setRoute", node: 4, route: "out" }]);
    expect(screen.getByRole("button", { name: "Cut the way out of Oscillator 1" }).getAttribute("aria-pressed")).toBe("true");
    // (Cutting a way out makes no loop, and is done.)
    fireEvent.click(await wayOut("Oscillator 2"));
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Cut the way out of Oscillator 2" }).getAttribute("aria-pressed")).toBe("true");
    });
  });
});

// The link that is a way out (the end of a lane's chain, or of a group's, to the group or the master) is the one link of the audio path that can be cut, and it has the button that does it, at its middle,
// as every cable that a person can take out has: the same as the button in the cell, which is the one that gives it back.

/** The button on the link of a way out, by the words of the link (`Filter → Master`). */
const disconnect = (words: string) => screen.findByRole("button", { name: `Disconnect ${words}` });

describe("the link of a way out", () => {
  test("has a button that disconnects it, and no other link of the audio path has one: the links inside a chain are the template's", async () => {
    // (The tracer's lane: its instrument goes to its filter, which is a link inside the chain, and the filter goes to the master, which is the way out.)
    await rig(<PatchView />);
    expect(await disconnect("Filter → Master")).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /^Disconnect / })).toHaveLength(1);
  });

  test("is on the link of every lane and every group, an empty lane too, from the end of the chain: the last effect, or the instrument if the lane has none", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    await disconnect("(empty) → Master");
    expect(screen.getAllByRole("button", { name: /^Disconnect / }).map((button) => button.getAttribute("aria-label"))).toEqual([
      "Disconnect Filter 1 → Master",
      "Disconnect Reverb 1 → Master",
      "Disconnect Filter 2 → Group 1",
      "Disconnect Oscillator 2 → Group 1",
      "Disconnect Reverb 2 → Group 2",
      "Disconnect Filter 3 → Master",
      "Disconnect (empty) → Master",
    ]);
  });

  test("is not on a link that is not there: a way out that is cut has no link, and so no button, and has it again when it is given back", async () => {
    await rig(<PatchView />, { snapshot: cutAt(tracerSnapshot, oscillator.id) });
    await wayOut();
    expect(screen.queryByRole("button", { name: /^Disconnect / })).toBeNull();
    fireEvent.click(await wayOut());
    expect(await disconnect("Filter → Master")).toBeTruthy();
  });

  test("a press asks the server to cut the way out of the lane that the link is the end of, and the link, with its button, is gone, and the way out says so", async () => {
    const { requests } = await rig(<PatchView />);
    fireEvent.click(await disconnect("Filter → Master"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: /^Disconnect / })).toBeNull();
    });
    // (The lane, not the filter that its chain ends in: a link is the way out of the lane, whatever node it starts at.)
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setRoute", node: oscillator.id, route: "none" }]);
    expect((await wayOut()).getAttribute("aria-pressed")).toBe("true");
    expect((await wayOut()).textContent).toBe("no out");
  });

  test("a press on a group's link is for the group, and leaves the lanes in it as they were", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    fireEvent.click(await disconnect("Filter 1 → Master"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Disconnect Filter 1 → Master" })).toBeNull();
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setRoute", node: 0, route: "none" }]);
    expect((await wayOut("Group 1")).getAttribute("aria-pressed")).toBe("true");
    expect((await wayOut("Oscillator 1")).getAttribute("aria-pressed")).toBe("false");
    // The focus is on the group's button, of all the way outs there are.
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cut the way out of Group 1" }));
    });
    // The lanes' own links into the group are there still.
    expect(screen.getByRole("button", { name: "Disconnect Filter 2 → Group 1" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Disconnect Oscillator 2 → Group 1" })).toBeTruthy();
  });

  test("a press on a lane's link in a group is for the lane: the group keeps its link to the master", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    fireEvent.click(await disconnect("Oscillator 2 → Group 1"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Disconnect Oscillator 2 → Group 1" })).toBeNull();
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setRoute", node: 7, route: "none" }]);
    expect((await wayOut("Oscillator 2")).getAttribute("aria-pressed")).toBe("true");
    expect((await wayOut("Group 1")).getAttribute("aria-pressed")).toBe("false");
    // The focus is on the lane's button, of all the way outs there are.
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cut the way out of Oscillator 2" }));
    });
    expect(screen.getByRole("button", { name: "Disconnect Filter 1 → Master" })).toBeTruthy();
  });

  test("puts the focus on the button that gives the way out back, once the link is gone", async () => {
    await rig(<PatchView />);
    const button = await disconnect("Filter → Master");
    button.focus();
    fireEvent.click(button);
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cut the way out of Oscillator" }));
    });
    // (Pressed again it gives the link back, as a keyboard would: the focus has not been lost to the page.)
    fireEvent.click(screen.getByRole("button", { name: "Cut the way out of Oscillator" }));
    expect(await disconnect("Filter → Master")).toBeTruthy();
  });

  test("leaves the link and the focus where they are when the server does not cut the way out", async () => {
    const asked = vi.spyOn(Editor.prototype, "setRoute").mockResolvedValue(false);
    await rig(<PatchView />);
    const button = await disconnect("Filter → Master");
    button.focus();
    fireEvent.click(button);
    await waitFor(() => {
      expect(asked).toHaveBeenCalledWith(oscillator.id, "none");
    });
    // (Long enough for a focus that was moved to be moved: it goes at once when the button is there, and else on the next frames.)
    await act(async () => {
      await new Promise((done) => setTimeout(done, 60));
    });
    expect(screen.getByRole("button", { name: "Disconnect Filter → Master" })).toBe(button);
    expect(document.activeElement).toBe(button);
    expect((await wayOut()).getAttribute("aria-pressed")).toBe("false");
  });

  test("has a title that says what a press does and where the way out is given back", async () => {
    await rig(<PatchView />);
    expect((await disconnect("Filter → Master")).title).toBe("Disconnect Filter → Master: cut the way out, to hear it only through its cables (the button under Mute / Solo gives it back)");
  });
});
