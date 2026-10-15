// @vitest-environment jsdom
// The scope of a node (docs/web-ui.md §9.3, D47): a button in the header of an instrument, an effect and a group puts a scope of what comes out of the node into it, a wave or a
// spectrum like the master's, and the server is asked for the signal of the node for as long as the scope is there. It is the viewer's own: it is not in the session.
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, test } from "vitest";

import { MASTER, nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { MockServer } from "../../client/mock";
import type { ClientMessage, NodeId, Snapshot } from "../../protocol";
import { rig } from "../testing";
import { PatchView } from "./PatchView";
import { NODE_SCOPE_HEIGHT } from "./toFlow";

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");

/** The node of the canvas that is the picture of this node of the session. */
const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);

/** The button of a node that shows its scope. */
const scopeButton = (node: HTMLElement, name: string): HTMLElement => within(node).getByRole("button", { name: `Scope of ${name}` });

/** The scope that a node shows, if it does. */
const scopeIn = (node: HTMLElement, name: string): HTMLElement | null => within(node).queryByRole("meter", { name: `Output of ${name}` });

/** The nodes that the client last asked the server for the signal of (none before it has asked). */
function asked(server: MockServer): NodeId[] {
  const probes = server.received.filter((message): message is Extract<ClientMessage, { type: "probes" }> => message.type === "probes");
  return probes.at(-1)?.nodes ?? [];
}

/** The tracer's lane in a group, with the group between the filter and the master. */
function grouped(): Snapshot {
  const snapshot: Snapshot = structuredClone(tracerSnapshot);
  snapshot.graph.nodes.unshift({ id: 77, parent: null, name: "Pad", kind: "group", volume: 0.8, pan: -0.25 });
  for (const node of snapshot.graph.nodes) if (node.id === oscillator.id) node.parent = 77;
  snapshot.graph.edges = snapshot.graph.edges.flatMap((edge) => {
    if (edge.kind === "audio" && edge.to.node === MASTER) {
      return [
        { kind: "member" as const, from: edge.from, to: { node: 77, port: { type: "audioIn" as const } } },
        { kind: "audio" as const, from: { node: 77, port: { type: "audioOut" as const } }, to: edge.to },
      ];
    }
    return [edge];
  });
  return snapshot;
}

describe("the button of a node that shows its scope", () => {
  test("is in the header of an instrument, an effect and a group, not pressed to begin with, and is on no modulator and not on the master, which has its scope without one", async () => {
    await rig(<PatchView />, { snapshot: grouped() });
    for (const [id, name] of [
      [oscillator.id, "Oscillator"],
      [filter.id, "Filter"],
      [77, "Pad"],
    ] as const) {
      const node = await nodeOf(id);
      const button = scopeButton(node, name);
      expect(button.getAttribute("aria-expanded"), name).toBe("false");
      expect(button.closest(".tang-node__header"), name).not.toBeNull();
      expect(scopeIn(node, name), name).toBeNull();
    }
    expect(within(await nodeOf(lfo.id)).queryByRole("button", { name: /^Scope of / })).toBeNull();
    expect(within(await nodeOf(MASTER)).queryByRole("button", { name: /^Scope of / })).toBeNull();
    // (The only scope there is is the master's.)
    expect(screen.getAllByRole("meter", { name: /scope/i }).map((meter) => meter.getAttribute("aria-label"))).toEqual(["Output scope"]);
  });

  test("is not on a lane that has no instrument, which has no signal to show, though an effect in the lane has one", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    for (const node of snapshot.graph.nodes) if (node.kind === "instrument") node.plugin = null;
    await rig(<PatchView />, { snapshot });
    const lane = await nodeOf(oscillator.id);
    expect(within(lane).queryByRole("button", { name: /^Scope of / })).toBeNull();
    expect(scopeButton(await nodeOf(filter.id), "Filter")).toBeTruthy();
  });

  test("puts the scope of the node in it when it is pressed, a wave to begin with, with the choices of its view and its time, and takes it away when it is pressed again", async () => {
    await rig(<PatchView />);
    const node = await nodeOf(oscillator.id);
    expect(scopeIn(node, "Oscillator")).toBeNull();
    fireEvent.click(scopeButton(node, "Oscillator"));
    const scope = scopeIn(node, "Oscillator");
    expect(scope?.getAttribute("data-view")).toBe("wave");
    expect(scopeButton(node, "Oscillator").getAttribute("aria-expanded")).toBe("true");
    const optionsOf = (select: HTMLElement): string[] => Array.from(select.querySelectorAll("option"), (option) => option.textContent);
    expect(optionsOf(within(node).getByRole("combobox", { name: "View" }))).toEqual(["Wave", "Spectrum"]);
    expect(optionsOf(within(node).getByRole("combobox", { name: "Time" }))).toEqual(["5 ms", "11 ms", "21 ms", "43 ms"]);
    expect(within(node).getByRole<HTMLSelectElement>("combobox", { name: "Time" }).value).toBe("2");
    // (The choices are not where the node is dragged from, nor panned or scrolled.)
    expect(["nodrag", "nopan", "nowheel"].every((name) => node.querySelector(".tang-scope-choices")?.classList.contains(name))).toBe(true);

    fireEvent.click(scopeButton(node, "Oscillator"));
    expect(scopeIn(node, "Oscillator")).toBeNull();
    expect(within(node).queryByRole("combobox", { name: "View" })).toBeNull();
    expect(scopeButton(node, "Oscillator").getAttribute("aria-expanded")).toBe("false");
  });

  test("shows the scope of one node and not another: each has its own, with its own choices", async () => {
    await rig(<PatchView />);
    const [lane, effect] = [await nodeOf(oscillator.id), await nodeOf(filter.id)];
    fireEvent.click(scopeButton(lane, "Oscillator"));
    expect(scopeIn(lane, "Oscillator")).not.toBeNull();
    expect(scopeIn(effect, "Filter")).toBeNull();
    fireEvent.click(scopeButton(effect, "Filter"));
    fireEvent.change(within(lane).getByRole("combobox", { name: "View" }), { target: { value: "1" } });
    expect(scopeIn(lane, "Oscillator")?.getAttribute("data-view")).toBe("spectrum");
    expect(scopeIn(effect, "Filter")?.getAttribute("data-view")).toBe("wave");
    // (The master's is its own too.)
    expect(within(await nodeOf(MASTER)).getByRole("meter", { name: "Output scope" }).getAttribute("data-view")).toBe("wave");
  });

  test("a group shows one too", async () => {
    await rig(<PatchView />, { snapshot: grouped() });
    const group = await nodeOf(77);
    fireEvent.click(scopeButton(group, "Pad"));
    expect(scopeIn(group, "Pad")).not.toBeNull();
  });

  test("changes nothing of the session: the scopes that are shown are the viewer's own, and no request is made for them", async () => {
    const { requests } = await rig(<PatchView />);
    const node = await nodeOf(oscillator.id);
    fireEvent.click(scopeButton(node, "Oscillator"));
    fireEvent.click(scopeButton(await nodeOf(filter.id), "Filter"));
    fireEvent.click(scopeButton(node, "Oscillator"));
    expect(requests()).toEqual([]);
  });
});

describe("the signal that a node's scope is of", () => {
  test("is asked of the server while the scope is shown, and for no longer", async () => {
    const { server } = await rig(<PatchView />);
    expect(asked(server)).toEqual([]);
    const node = await nodeOf(oscillator.id);
    fireEvent.click(scopeButton(node, "Oscillator"));
    await waitFor(() => {
      expect(asked(server)).toEqual([oscillator.id]);
    });
    fireEvent.click(scopeButton(node, "Oscillator"));
    await waitFor(() => {
      expect(asked(server)).toEqual([]);
    });
  });

  test("is asked for every node that shows a scope, in the order the scopes were shown, and a node's is let go of when its scope is", async () => {
    const { server } = await rig(<PatchView />);
    fireEvent.click(scopeButton(await nodeOf(filter.id), "Filter"));
    fireEvent.click(scopeButton(await nodeOf(oscillator.id), "Oscillator"));
    await waitFor(() => {
      expect(asked(server)).toEqual([filter.id, oscillator.id]);
    });
    fireEvent.click(scopeButton(await nodeOf(filter.id), "Filter"));
    await waitFor(() => {
      expect(asked(server)).toEqual([oscillator.id]);
    });
  });

  test("is let go of when the patch is taken off the page, and not asked for again when it is put back: the scopes that were shown went with the view", async () => {
    let flip: () => void = () => undefined;
    const Switch = () => {
      const [there, setThere] = useState(true);
      flip = () => {
        setThere((now) => !now);
      };
      return there ? <PatchView /> : null;
    };
    const { server } = await rig(<Switch />);
    fireEvent.click(scopeButton(await nodeOf(oscillator.id), "Oscillator"));
    await waitFor(() => {
      expect(asked(server)).toEqual([oscillator.id]);
    });
    act(() => {
      flip();
    });
    await waitFor(() => {
      expect(asked(server)).toEqual([]);
    });
    act(() => {
      flip();
    });
    expect(scopeIn(await nodeOf(oscillator.id), "Oscillator")).toBeNull();
    expect(asked(server)).toEqual([]);
  });

  test("is drawn: a note that is held is a value on the scope's meter, and the node that has no scope is not sent one", async () => {
    const { client, clock } = await rig(<PatchView />);
    const node = await nodeOf(oscillator.id);
    fireEvent.click(scopeButton(node, "Oscillator"));
    client.noteOn(69);
    let time = 0;
    await waitFor(() => {
      time += 16;
      clock.tick(time);
      expect(Number(scopeIn(node, "Oscillator")?.getAttribute("aria-valuenow"))).toBeGreaterThan(0.1);
    });
    expect(client.telemetry.probeFrames(oscillator.id)).toBeGreaterThan(0);
    expect(client.telemetry.probeFrames(filter.id)).toBe(0);
  });

  test("stops coming when the scope is hidden", async () => {
    const { client } = await rig(<PatchView />);
    const node = await nodeOf(oscillator.id);
    fireEvent.click(scopeButton(node, "Oscillator"));
    await waitFor(() => {
      expect(client.telemetry.probeFrames(oscillator.id)).toBeGreaterThan(2);
    });
    fireEvent.click(scopeButton(node, "Oscillator"));
    await new Promise((resolve) => setTimeout(resolve, 30)); // (frames that were on their way)
    const frames = client.telemetry.probeFrames(oscillator.id);
    await new Promise((resolve) => setTimeout(resolve, 80));
    expect(client.telemetry.probeFrames(oscillator.id)).toBe(frames);
  });

  test("is let go of when the node is taken out, and asked for again when undo puts the node back: the scope was the viewer's, and it is shown again", async () => {
    const { server, client } = await rig(<PatchView />);
    fireEvent.click(scopeButton(await nodeOf(filter.id), "Filter"));
    await waitFor(() => {
      expect(asked(server)).toEqual([filter.id]);
    });
    act(() => {
      server.apply({ type: "removeNode", node: filter.id });
    });
    await waitFor(() => {
      expect(client.state.snapshot?.graph.nodes.some((node) => node.id === filter.id)).toBe(false);
    });
    await waitFor(() => {
      expect(asked(server)).toEqual([]);
    });
    act(() => {
      server.apply({ type: "undo" });
    });
    expect(scopeIn(await nodeOf(filter.id), "Filter")).not.toBeNull();
    await waitFor(() => {
      expect(asked(server)).toEqual([filter.id]);
    });
  });
});

describe("the room that a node's scope takes on the canvas", () => {
  /** Where the canvas draws each node, in its own pixels, by the node's id. */
  function placesOnCanvas(): Map<string, { x: number; y: number }> {
    const places = new Map<string, { x: number; y: number }>();
    for (const node of document.querySelectorAll<HTMLElement>(".react-flow__node")) {
      const found = /translate\((-?[\d.e-]+)px,\s*(-?[\d.e-]+)px\)/.exec(node.style.transform);
      if (node.dataset.id && found?.[1] && found[2]) places.set(node.dataset.id, { x: Number(found[1]), y: Number(found[2]) });
    }
    return places;
  }

  /** The session with a second lane, below the first (two lanes of one name are numbered: the first is "Oscillator 1"). */
  async function twoLanes() {
    const made = await rig(<PatchView />);
    act(() => {
      made.server.apply({ type: "addInstrument", plugin: "builtin:osc" });
    });
    await waitFor(() => {
      expect(placesOnCanvas().size).toBe(tracerSnapshot.graph.nodes.length + 1);
    });
    const lane = [...placesOnCanvas().keys()].map(Number).find((id) => !tracerSnapshot.graph.nodes.some((node) => node.id === id));
    if (lane === undefined) throw new Error("no new lane");
    return { ...made, lane: String(lane) };
  }

  test("makes the lanes below move down by the height of the scope, and nothing else", async () => {
    const { lane } = await twoLanes();
    const before = placesOnCanvas();
    fireEvent.click(scopeButton(await nodeOf(oscillator.id), "Oscillator 1"));
    const after = placesOnCanvas();
    expect((after.get(lane)?.y ?? 0) - (before.get(lane)?.y ?? 0)).toBe(NODE_SCOPE_HEIGHT);
    for (const [id, place] of before) {
      if (id === lane) continue;
      expect(after.get(id), id).toEqual(place);
    }
  });

  test("leaves them where they are when the scope is hidden again", async () => {
    const { lane } = await twoLanes();
    const before = placesOnCanvas();
    const node = await nodeOf(oscillator.id);
    fireEvent.click(scopeButton(node, "Oscillator 1"));
    const open = placesOnCanvas();
    fireEvent.click(scopeButton(node, "Oscillator 1"));
    expect(placesOnCanvas()).toEqual(open);
    expect(open.get(lane)?.y).not.toBe(before.get(lane)?.y);
  });

  test("is still there when the nodes are arranged again: the layout is made with it, and the lane below is not drawn where it was for a moment", async () => {
    const { lane, server } = await twoLanes();
    const closed = placesOnCanvas();
    fireEvent.click(scopeButton(await nodeOf(oscillator.id), "Oscillator 1"));
    const open = placesOnCanvas();
    expect(open.get(lane)?.y).not.toBe(closed.get(lane)?.y);
    // A node that has been moved is something to arrange again (this one is out of the way of the rest).
    act(() => {
      server.apply({ type: "setNodeUi", node: filter.id, ui: { x: 700, y: 900 } });
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLButtonElement>("button", { name: "Arrange the nodes again" }).disabled).toBe(false);
    });
    fireEvent.click(screen.getByRole("button", { name: "Arrange the nodes again" }));
    // (At once, before the server has answered: the layout that is drawn first is the one that is kept.)
    expect(placesOnCanvas().get(lane)?.y).toBe(open.get(lane)?.y);
  });
});
