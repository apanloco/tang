// @vitest-environment jsdom
// How often the canvas is laid out: when the session's structure changes, and not for a value.
import { act, cleanup, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import { rig } from "../testing";
import { PatchView } from "./PatchView";
import * as flow from "./toFlow";

vi.mock("./toFlow", async (importOriginal) => {
  const original = await importOriginal<typeof import("./toFlow")>();
  return { ...original, toFlow: vi.fn(original.toFlow) };
});

afterEach(cleanup);

const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");

describe("laying out the canvas", () => {
  test("is done once for a session, and not again for the values that change in it", async () => {
    const { server, client } = await rig(<PatchView />);
    const made = vi.mocked(flow.toFlow).mock.calls.length;
    expect(made).toBeGreaterThanOrEqual(1);
    const rev = client.state.snapshot?.rev ?? 0;
    act(() => {
      for (let step = 1; step <= 5; step += 1) server.apply({ type: "setParam", node: filter.id, param: 0, value: step / 10 });
      server.apply({ type: "setGain", node: filter.id, gain: "mix", value: 0.5 });
      // A modulator's settings and a cable's depth are turned at the pace of the frames, like a knob.
      server.apply({ type: "setModulatorField", node: lfo.id, field: "rate", value: 7 });
      server.apply({ type: "setModulatorField", node: lfo.id, field: "waveform", value: 3 });
      for (let step = 1; step <= 5; step += 1) server.apply({ type: "setEdgeDepth", edge: 0, depth: step / 10 });
    });
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBe(rev + 13);
    });
    expect(vi.mocked(flow.toFlow).mock.calls.length).toBe(made);
  });

  test("is done again when the structure changes", async () => {
    const { client } = await rig(<PatchView />);
    const made = vi.mocked(flow.toFlow).mock.calls.length;
    const renamed = structuredClone(tracerSnapshot);
    const first = renamed.graph.nodes[0];
    if (first) first.name = "Renamed";
    act(() => {
      client.store.setState({ snapshot: { ...renamed, rev: 1 } });
    });
    await waitFor(() => {
      expect(vi.mocked(flow.toFlow).mock.calls.length).toBeGreaterThan(made);
    });
  });
});
