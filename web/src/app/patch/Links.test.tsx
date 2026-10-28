// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { isLink } from "../../client/cables";
import { Editor } from "../../client/editor";
import { MASTER, nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import { linksOf } from "../../client/wiring";
import type { Snapshot } from "../../protocol";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

// The links of the audio path that a lane and a group make by themselves are cables to a person (step 88): amber, with the button that takes them out at their middle, and no slider, since they have no level.
// What taking one out is to the server (`SetRoute` for the end of a chain, which is the way out of its lane or group, and `SetFeed` for a link into an effect: what the controller makes of them is in
// `apply.rs`, `history.rs` and `wiring.test.ts`, and the mock engine's agreement with it in `history.test.ts`) and what draws one again (the menu that sends from the node it started at, or a drag: a cable between
// the two ends of a link that is cut, which the controller turns into the link given back: `wiring.test.ts`) is what these tests are about, as the page shows and asks it.

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

/** The button on a link, by the words of the link (`Filter → Master`). */
const remove = (words: string) => screen.findByRole("button", { name: `Remove cable ${words}` });

/** The words of every link that has a button on the canvas, in the order of the canvas (the cables of modulation have a `:` in theirs, and the tracer's and the reach fixture's have no other cables). */
const linksShown = () =>
  screen
    .getAllByRole("button", { name: /^Remove cable / })
    .map((button) => (button.getAttribute("aria-label") ?? "").replace(/^Remove cable /, ""))
    .filter((words) => !words.includes(":"));

/** `snapshot` with the way out of the lanes and groups `ids` cut, or the feed of the effects, and the links of the audio path that go with it taken out (as the controller's graph has them). */
function cutAt(snapshot: Snapshot, field: "route" | "feed", ...ids: number[]): Snapshot {
  const copy = structuredClone(snapshot);
  const nodes = copy.graph.nodes.map((n) => {
    if (!ids.includes(n.id)) return n;
    if (field === "route" && (n.kind === "instrument" || n.kind === "group")) return { ...n, route: "none" as const };
    if (field === "feed" && n.kind === "effect") return { ...n, feed: "none" as const };
    return n;
  });
  copy.graph = { nodes, edges: [...linksOf(nodes), ...copy.graph.edges.filter((e) => !isLink(e))] };
  return copy;
}

/** The menu that sends the audio of the node `name`, which is a button on its header. */
const sends = (name: string) => screen.getByRole("button", { name: `Send audio from ${name}` });

describe("a link of the audio path", () => {
  test("is a cable with a button that takes it out, every one of them: the end of the chain of each lane and group (its last effect, or its instrument if it has no effect) and each link into an effect", async () => {
    await rig(<PatchView />);
    await remove("Filter → Master");
    expect(linksShown()).toEqual(["Oscillator → Filter", "Filter → Master"]);
    cleanup();

    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    await remove("(empty) → Master");
    expect(linksShown()).toEqual([
      "Group 1 → Filter 1",
      "Filter 1 → Master",
      "Group 2 → Reverb 1",
      "Reverb 1 → Master",
      "Oscillator 1 → Filter 2",
      "Filter 2 → Group 1",
      "Oscillator 2 → Group 1",
      "Oscillator 3 → Reverb 2",
      "Reverb 2 → Group 2",
      "Oscillator 4 → Filter 3",
      "Filter 3 → Master",
      "(empty) → Master",
    ]);
    cleanup();
    // A chain of two: the link between the effects has one too, and the end of the chain is the second effect's.
    const { server } = await rig(<PatchView />);
    server.apply({ type: "addEffect", parent: oscillator.id, plugin: "builtin:delay" });
    await remove("Delay → Master");
    expect(linksShown()).toEqual(["Oscillator → Filter", "Filter → Delay", "Delay → Master"]);
  });

  test("is the only thing that says where a node's audio goes and where an effect is fed from: no node has a button of its own for that", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    await remove("(empty) → Master");
    expect(screen.queryAllByRole("button", { name: /^Cut the (way out|input) of / })).toHaveLength(0);
    // (A lane's and a group's cell has the mute and the solo, and an effect has no more than its own buttons.)
    for (const button of screen.getAllByRole("button", { name: /^Mute / })) {
      const cell = button.closest<HTMLElement>(".tang-cell");
      if (!cell) throw new Error("the mute is in no cell");
      expect(within(cell).getAllByRole("button")).toHaveLength(2);
    }
    expect(document.querySelectorAll(".tang-way-out, .tang-feed")).toHaveLength(0);
  });

  test("is not there where it is cut: a lane or a group whose way out is cut has no link out of its chain, and an effect whose feed is cut has none into it", async () => {
    await rig(<PatchView />, { snapshot: cutAt(tracerSnapshot, "route", oscillator.id) });
    await remove("Oscillator → Filter");
    expect(linksShown()).toEqual(["Oscillator → Filter"]);
    cleanup();

    await rig(<PatchView />, { snapshot: cutAt(tracerSnapshot, "feed", filter.id) });
    await remove("Filter → Master");
    expect(linksShown()).toEqual(["Filter → Master"]);
    cleanup();

    // A group's is the group's: the lanes' own links into it are there, and the links into its effects.
    await rig(<PatchView />, { snapshot: cutAt(reachFixture.snapshot, "route", 0) });
    await remove("Filter 2 → Group 1");
    expect(linksShown()).not.toContain("Filter 1 → Master");
    expect(linksShown()).toContain("Group 1 → Filter 1");
    expect(linksShown()).toContain("Oscillator 2 → Group 1");
  });

  test("has the words of the link for its name and its title, as every cable has", async () => {
    await rig(<PatchView />);
    expect((await remove("Filter → Master")).title).toBe("Remove cable Filter → Master");
  });

  test("is drawn in the colour of audio that is sent, and has no slider: a link has no level", async () => {
    await rig(<PatchView />);
    await remove("Filter → Master");
    expect(document.querySelectorAll(".tang-cable--audio")).toHaveLength(2);
    expect(document.querySelectorAll(".tang-cable--send")).toHaveLength(0);
    expect(screen.queryAllByRole("slider", { name: /^Level of / })).toHaveLength(0);
  });
});

describe("taking a link out", () => {
  test("a press on the end of a lane's chain asks the server to cut the way out of the lane, and not of the node the link starts at: the link and its button are gone", async () => {
    const { requests } = await rig(<PatchView />);
    fireEvent.click(await remove("Filter → Master"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Remove cable Filter → Master" })).toBeNull();
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setRoute", node: oscillator.id, route: "none" }]);
    // (The link into the filter is there still: only the way out is cut.)
    expect(screen.getByRole("button", { name: "Remove cable Oscillator → Filter" })).toBeTruthy();
    expect(document.querySelectorAll(".tang-cable--audio")).toHaveLength(1);
  });

  test("a press on a link into an effect asks the server to cut the input of the effect that it goes into", async () => {
    const { requests } = await rig(<PatchView />);
    fireEvent.click(await remove("Oscillator → Filter"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Remove cable Oscillator → Filter" })).toBeNull();
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setFeed", node: filter.id, feed: "none" }]);
    // (The link out of the effect is there still: the chain goes on from it.)
    expect(screen.getByRole("button", { name: "Remove cable Filter → Master" })).toBeTruthy();
  });

  test("a press on a group's own link out is for the group, and leaves the lanes in it as they were; one into the first of its bus effects cuts that effect's input", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    fireEvent.click(await remove("Filter 1 → Master"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Remove cable Filter 1 → Master" })).toBeNull();
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setRoute", node: 0, route: "none" }]);
    // The lanes' own links into the group are there still.
    expect(screen.getByRole("button", { name: "Remove cable Filter 2 → Group 1" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Remove cable Oscillator 2 → Group 1" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Remove cable Group 1 → Filter 1" }));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Remove cable Group 1 → Filter 1" })).toBeNull();
    });
    expect(requests().map((request) => request.intent).at(-1)).toEqual({ type: "setFeed", node: 1, feed: "none" });
  });

  test("a press on a lane's link into its group is for the lane: the group keeps its link out", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    fireEvent.click(await remove("Oscillator 2 → Group 1"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Remove cable Oscillator 2 → Group 1" })).toBeNull();
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setRoute", node: 6, route: "none" }]);
    expect(screen.getByRole("button", { name: "Remove cable Filter 1 → Master" })).toBeTruthy();
  });

  test("a press on the link between two effects is for the second, and leaves the first as it was", async () => {
    const { requests, server } = await rig(<PatchView />);
    server.apply({ type: "addEffect", parent: oscillator.id, plugin: "builtin:delay" });
    const delay = nodeNamed(server.snapshot, "Delay");
    fireEvent.click(await remove("Filter → Delay"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Remove cable Filter → Delay" })).toBeNull();
    });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setFeed", node: delay.id, feed: "none" }]);
    expect(screen.getByRole("button", { name: "Remove cable Oscillator → Filter" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Remove cable Delay → Master" })).toBeTruthy();
  });

  test("puts the focus on the menu that sends the audio of the node the link started at, where the next cable is drawn from, once the link is gone", async () => {
    await rig(<PatchView />);
    const button = await remove("Filter → Master");
    button.focus();
    fireEvent.click(button);
    await waitFor(() => {
      expect(document.activeElement).toBe(sends("Filter"));
    });
    cleanup();

    // (The link into an effect starts at the instrument of the lane.)
    await rig(<PatchView />);
    fireEvent.click(await remove("Oscillator → Filter"));
    await waitFor(() => {
      expect(document.activeElement).toBe(sends("Oscillator"));
    });
  });

  test("leaves the link and the focus where they are when the server does not take it out: the way out, and the input", async () => {
    const route = vi.spyOn(Editor.prototype, "setRoute").mockResolvedValue(false);
    const feed = vi.spyOn(Editor.prototype, "setFeed").mockResolvedValue(false);
    await rig(<PatchView />);
    for (const [words, asked, args] of [
      ["Filter → Master", route, [oscillator.id, "none"]],
      ["Oscillator → Filter", feed, [filter.id, "none"]],
    ] as const) {
      const button = await remove(words);
      button.focus();
      fireEvent.click(button);
      await waitFor(() => {
        expect(asked).toHaveBeenCalledWith(...args);
      });
      // (Long enough for a focus that was moved to be moved: it goes at once when the button is there, and else on the next frames.)
      await act(async () => {
        await new Promise((done) => setTimeout(done, 60));
      });
      expect(screen.getByRole("button", { name: `Remove cable ${words}` })).toBe(button);
      expect(document.activeElement).toBe(button);
    }
  });
});

describe("drawing a link again", () => {
  test("is done by the menu that sends the audio of the node it started at, which offers the end of the link that is cut: the server is asked for a cable, and it is the link that comes back, with no level of its own", async () => {
    const { requests, server } = await rig(<PatchView />);
    fireEvent.click(await remove("Filter → Master"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Remove cable Filter → Master" })).toBeNull();
    });
    // (The menu had nothing to offer while the link was there: its end was taken.)
    await waitFor(() => {
      expect(sends("Filter").hasAttribute("disabled")).toBe(false);
    });
    fireEvent.click(sends("Filter"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Master"]);
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Master" }));
    expect(await remove("Filter → Master")).toBeTruthy();
    expect(requests().map((request) => request.intent).at(-1)).toEqual({ type: "addCable", from: { node: filter.id, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } }, gain: null });
    // (A link given back is a step in the words of the way out, and no cable of a person's: it has no slider and is not counted among the cables that have one.)
    expect(server.history.undo).toBe("give the lane of Oscillator its way out");
    expect(document.querySelectorAll(".tang-cable--send")).toHaveLength(0);
    expect(screen.queryAllByRole("slider", { name: /^Level of / })).toHaveLength(0);
  });

  test("brings back the link into an effect the same way, from the menu of the node before it", async () => {
    const { requests, server } = await rig(<PatchView />);
    fireEvent.click(await remove("Oscillator → Filter"));
    await waitFor(() => {
      expect(screen.queryByRole("button", { name: "Remove cable Oscillator → Filter" })).toBeNull();
    });
    await waitFor(() => {
      expect(sends("Oscillator").hasAttribute("disabled")).toBe(false);
    });
    fireEvent.click(sends("Oscillator"));
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Filter" }));
    expect(await remove("Oscillator → Filter")).toBeTruthy();
    expect(requests().map((request) => request.intent).at(-1)).toEqual({ type: "addCable", from: { node: oscillator.id, port: { type: "audioOut" } }, to: { node: filter.id, port: { type: "audioIn" } }, gain: null });
    expect(server.history.undo).toBe("give Filter its input back");
    expect(document.querySelectorAll(".tang-cable--send")).toHaveLength(0);
  });

  test("offers nothing where the link is there: a link is a cable that is drawn already", async () => {
    await rig(<PatchView />);
    await remove("Filter → Master");
    // (The filter's only place to send to is the master, which its link goes to; the oscillator's are the master and no effect, since the filter is the one it goes to.)
    expect(sends("Filter").hasAttribute("disabled")).toBe(true);
    fireEvent.click(sends("Oscillator"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Master"]);
  });
});
