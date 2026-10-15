// @vitest-environment jsdom
// The canvas is moved to show every node when the nodes have been arranged again: once, and when they are where they will be.
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, test, vi } from "vitest";

import { Client } from "../../client/connection";
import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import { MockServer } from "../../client/mock";
import type { Intent } from "../../protocol";
import { ClientProvider } from "../ClientProvider";
import { holdable, rig } from "../testing";
import { PatchView } from "./PatchView";

const fitView = vi.hoisted(() => vi.fn(() => Promise.resolve(true)));

// (The canvas of a test has no size to fit anything to: what it is asked to is what there is to see.)
vi.mock("@xyflow/react", async (importOriginal) => {
  const original = await importOriginal<typeof import("@xyflow/react")>();
  return {
    ...original,
    useReactFlow: () => ({ ...original.useReactFlow(), fitView }),
  };
});

beforeEach(() => {
  fitView.mockClear();
});
afterEach(cleanup);

const lfo = nodeNamed(tracerSnapshot, "LFO");
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

const arrange = () => screen.findByRole("button", { name: "Arrange the nodes again" });

/** An envelope that drives the rate of the LFO: what the layout would put a row above, so that the nodes are not where it has them once it is there. */
const envelopeOnTheRate: Intent = { type: "modulate", to: { node: lfo.id, port: { type: "rate" } }, kind: "envelope", depth: null };

describe("the view when the nodes are arranged again", () => {
  test("is fitted to every node, once, with the room the canvas leaves at first, and is left alone by what changes in the session after", async () => {
    const { server } = await rig(<PatchView />);
    act(() => {
      server.apply(envelopeOnTheRate);
    });
    await waitFor(async () => {
      expect((await arrange()).hasAttribute("disabled")).toBe(false);
    });
    expect(fitView).not.toHaveBeenCalled();

    fireEvent.click(await arrange());

    await waitFor(() => {
      expect(fitView).toHaveBeenCalledTimes(1);
    });
    expect(fitView).toHaveBeenCalledWith({ padding: 0.12, maxZoom: 1, duration: 250 });
    // (A change of the session, with the canvas laid out again, does not move the view.)
    act(() => {
      server.apply({ type: "addEffect", parent: oscillator.id, plugin: "builtin:reverb" });
    });
    await waitFor(() => {
      expect(screen.getAllByText("Reverb").length).toBeGreaterThan(0);
    });
    expect(fitView).toHaveBeenCalledTimes(1);
  });

  test("is fitted when the nodes that were placed have all been given back, and not before: they are not where they will be until then", async () => {
    const server = new MockServer();
    const connection = holdable(server);
    const client = new Client({ transport: connection.transport, reconnectDelay: () => 60_000 });
    render(
      <ClientProvider client={client}>
        <PatchView />
      </ClientProvider>,
    );
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    act(() => {
      server.apply({ type: "setNodeUi", node: oscillator.id, ui: { x: 10, y: 20 } });
      server.apply({ type: "setNodeUi", node: filter.id, ui: { x: 300, y: -40 } });
    });
    await waitFor(async () => {
      expect((await arrange()).hasAttribute("disabled")).toBe(false);
    });
    connection.hold();

    fireEvent.click(await arrange());

    // The first is asked for, and its answer is held back: one is given back, and one is still placed.
    await waitFor(() => {
      expect(connection.answers()).toBe(1);
    });
    expect(fitView).not.toHaveBeenCalled();
    act(() => {
      connection.answer();
    });
    // The second is asked for when the first is answered.
    await waitFor(() => {
      expect(connection.answers()).toBe(1);
    });
    expect(client.state.snapshot?.graph.nodes.filter((node) => node.ui !== undefined)).toHaveLength(1);
    expect(fitView).not.toHaveBeenCalled();
    act(() => {
      connection.answer();
    });

    await waitFor(() => {
      expect(fitView).toHaveBeenCalledTimes(1);
    });
    expect(client.state.snapshot?.graph.nodes.filter((node) => node.ui !== undefined)).toHaveLength(0);
  });
});
