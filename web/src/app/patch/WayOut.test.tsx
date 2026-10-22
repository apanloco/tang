// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { isLink } from "../../client/cables";
import { audioCablesFixture, nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import { linksOf } from "../../client/wiring";
import type { Edge, Snapshot } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

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
