// @vitest-environment jsdom
// The canvas is not arranged again under a person's hands: what is drawn, added or taken out leaves the nodes where they are, and the
// nodes are put where the layout has them when the person asks for it.
import { act, cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { Graph, Intent } from "../../protocol";
import { rig } from "../testing";
import { PatchView } from "./PatchView";
import { toFlow } from "./toFlow";

afterEach(cleanup);

const lfo = nodeNamed(tracerSnapshot, "LFO");
const lane = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

type Places = Map<string, { x: number; y: number }>;

/** Where the canvas draws each node, in its own pixels, by the node's id. */
function placesOnCanvas(): Places {
  const places: Places = new Map();
  for (const node of document.querySelectorAll<HTMLElement>(".react-flow__node")) {
    const found = /translate\((-?[\d.e-]+)px,\s*(-?[\d.e-]+)px\)/.exec(node.style.transform);
    if (node.dataset.id && found?.[1] && found[2]) places.set(node.dataset.id, { x: Number(found[1]), y: Number(found[2]) });
  }
  return places;
}

/** Where the layout puts the nodes of `graph` when it remembers nothing. */
function laidOut(graph: Graph | undefined): Places {
  if (!graph) throw new Error("the client has no session");
  return new Map(toFlow(graph).nodes.map((node) => [node.id, node.position]));
}

const arrange = () => screen.findByRole("button", { name: "Arrange the nodes again" });

/** An envelope that drives the rate of the LFO: a modulator above the one it drives, which the layout makes a row for. */
const envelopeOnTheRate: Intent = { type: "modulate", to: { node: lfo.id, port: { type: "rate" } }, kind: "envelope", depth: null };

describe("a canvas that is arranged when it is asked to be", () => {
  test("keeps every node where it was when a modulator is drawn to the setting of another, which makes a row of modulators above it", async () => {
    const { server } = await rig(<PatchView />);
    await screen.findByTestId(`rf__node-${String(lfo.id)}`);
    const before = placesOnCanvas();
    expect(before.size).toBe(tracerSnapshot.graph.nodes.length);

    act(() => {
      server.apply(envelopeOnTheRate);
    });
    await waitFor(() => {
      expect(placesOnCanvas().size).toBe(before.size + 1);
    });

    const after = placesOnCanvas();
    expect([...before].map(([id]) => [id, after.get(id)])).toEqual([...before]);
  });

  test("keeps every node where it was when an effect is added to a lane, except the master, which goes along to the column the lane has grown to", async () => {
    const { server, client } = await rig(<PatchView />);
    await screen.findByTestId(`rf__node-${String(lfo.id)}`);
    const before = placesOnCanvas();
    const master = String(tracerSnapshot.graph.nodes.find((node) => node.kind === "master")?.id);

    act(() => {
      server.apply({ type: "addEffect", parent: lane.id, plugin: "builtin:reverb" });
    });
    await waitFor(() => {
      expect(placesOnCanvas().size).toBe(before.size + 1);
    });

    const after = placesOnCanvas();
    for (const [id, place] of before) {
      if (id === master) continue;
      expect(after.get(id), id).toEqual(place);
    }
    expect(after.get(master)?.x).toBeGreaterThan(before.get(master)?.x ?? Infinity);
    expect(after.get(master)?.y).toBe(before.get(master)?.y);
    expect(client.state.snapshot?.graph.nodes).toHaveLength(tracerSnapshot.graph.nodes.length + 1);
  });

  test("keeps the nodes where they were when the cable is taken out again, with the modulator that it came from", async () => {
    const { server, client } = await rig(<PatchView />);
    await screen.findByTestId(`rf__node-${String(lfo.id)}`);
    const before = placesOnCanvas();
    act(() => {
      server.apply(envelopeOnTheRate);
    });
    await waitFor(() => {
      expect(placesOnCanvas().size).toBe(before.size + 1);
    });
    const envelope = client.state.snapshot?.graph.nodes.find((node) => node.kind === "modulator" && !before.has(String(node.id)));
    if (!envelope) throw new Error("the session has no envelope");

    act(() => {
      server.apply({ type: "removeNode", node: envelope.id });
    });
    await waitFor(() => {
      expect(placesOnCanvas().size).toBe(before.size);
    });

    expect(placesOnCanvas()).toEqual(before);
  });

  test("has the button that arranges the nodes again dim while they are where the layout has them, and bright when they are not", async () => {
    const { server } = await rig(<PatchView />);
    expect((await arrange()).hasAttribute("disabled")).toBe(true);

    act(() => {
      server.apply(envelopeOnTheRate);
    });

    await waitFor(async () => {
      expect((await arrange()).hasAttribute("disabled")).toBe(false);
    });
  });

  test("puts every node where the layout has it when the button is pressed, and the button is dim again", async () => {
    const { server, client, requests } = await rig(<PatchView />);
    await screen.findByTestId(`rf__node-${String(lfo.id)}`);
    act(() => {
      server.apply(envelopeOnTheRate);
    });
    await waitFor(async () => {
      expect((await arrange()).hasAttribute("disabled")).toBe(false);
    });
    expect(placesOnCanvas()).not.toEqual(laidOut(client.state.snapshot?.graph));

    fireEvent.click(await arrange());

    await waitFor(() => {
      expect(placesOnCanvas()).toEqual(laidOut(client.state.snapshot?.graph));
    });
    await waitFor(async () => {
      expect((await arrange()).hasAttribute("disabled")).toBe(true);
    });
    // (None of the nodes was placed, so there is nothing to ask of the server.)
    expect(requests()).toEqual([]);
  });

  test("keeps the nodes where the layout had them when it was asked to arrange them, when the next modulator is drawn to the setting of another", async () => {
    const { server, client } = await rig(<PatchView />);
    await screen.findByTestId(`rf__node-${String(lfo.id)}`);
    act(() => {
      server.apply(envelopeOnTheRate);
    });
    await waitFor(async () => {
      expect((await arrange()).hasAttribute("disabled")).toBe(false);
    });
    fireEvent.click(await arrange());
    await waitFor(() => {
      expect(placesOnCanvas()).toEqual(laidOut(client.state.snapshot?.graph));
    });
    const arranged = placesOnCanvas();
    const envelope = client.state.snapshot?.graph.nodes.find((node) => node.kind === "modulator" && node.id !== lfo.id);
    if (!envelope) throw new Error("the session has no envelope");

    // A modulator that drives the attack of the envelope, which the layout would put a row above it: a row more above the strip.
    act(() => {
      server.apply({ type: "modulate", to: { node: envelope.id, port: { type: "attack" } }, kind: "lfo", depth: null });
    });
    await waitFor(() => {
      expect(placesOnCanvas().size).toBe(arranged.size + 1);
    });

    const after = placesOnCanvas();
    expect([...arranged].map(([id]) => [id, after.get(id)])).toEqual([...arranged]);
  });

  test("keeps the nodes where they are when a cable is drawn after the nodes that a person had placed were given back", async () => {
    const { server, client } = await rig(<PatchView />);
    await screen.findByTestId(`rf__node-${String(lfo.id)}`);
    act(() => {
      server.apply({ type: "setNodeUi", node: lane.id, ui: { x: 10, y: 20 } });
      server.apply({ type: "setNodeUi", node: filter.id, ui: { x: 300, y: -40 } });
    });
    await waitFor(async () => {
      expect((await arrange()).hasAttribute("disabled")).toBe(false);
    });
    fireEvent.click(await arrange());
    // (Only when the last of them is given back is every node where the layout has it.)
    await waitFor(() => {
      expect(placesOnCanvas()).toEqual(laidOut(client.state.snapshot?.graph));
    });
    const arranged = placesOnCanvas();

    act(() => {
      server.apply(envelopeOnTheRate);
    });
    await waitFor(() => {
      expect(placesOnCanvas().size).toBe(arranged.size + 1);
    });

    const after = placesOnCanvas();
    expect([...arranged].map(([id]) => [id, after.get(id)])).toEqual([...arranged]);
  });
});
