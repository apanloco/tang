import { describe, expect, test } from "vitest";

import type { Intent, Node, Snapshot } from "../protocol";
import { historyFixture } from "./fixtures";
import { MockServer } from "./mock";

// The slots of the telemetry that the mock engine hands out are the controller's (`Controller::add_modulator`, `add_target`): a slot is never given again, and every cable to a
// parameter takes one, though the layout names a parameter once, by the first cable's slot.

// (Not the limiter's reduction, the last cell of the telemetry, which is never handed out: `HANDED_OUT` of `src/telemetry.rs`.)
const slotsOf = (snapshot: Snapshot): number[] => [...snapshot.telemetry.meters, ...snapshot.telemetry.modulation].filter((entry) => entry.kind !== "reduction").map((entry) => entry.slot);

/** Do `intent`, and say which node it made. */
function made(server: MockServer, intent: Intent): Node {
  const had = new Set(server.snapshot.graph.nodes.map((n) => n.id));
  server.apply(intent);
  const added = server.snapshot.graph.nodes.find((n) => !had.has(n.id));
  if (!added) throw new Error(`${JSON.stringify(intent)} made no node`);
  return added;
}

describe("the slots of the telemetry that the mock engine hands out", () => {
  const lane = historyFixture.snapshot.graph.nodes.find((n) => n.kind === "instrument");
  if (!lane) throw new Error("the fixture has a lane");
  // The LFO of the lane drives its detune (parameter 1) in the fixture's session.
  const detune = 1;

  test("are used up by a cable to a parameter that another cable drives, which the layout does not name", () => {
    const server = new MockServer({ snapshot: historyFixture.snapshot });
    const first = made(server, { type: "addFreeModulator", kind: "lfo", hears: lane.id });
    const effective = (): number => server.snapshot.telemetry.modulation.filter((entry) => entry.kind === "effective").length;
    const taken = Math.max(...slotsOf(server.snapshot));
    const named = effective();
    server.apply({ type: "addModEdge", modulator: first.id, to: { node: lane.id, port: { type: "param", index: detune } } });
    expect(effective(), "the cable drives a parameter that is named already").toBe(named);
    const next = made(server, { type: "addFreeModulator", kind: "envelope", hears: lane.id });
    const output = server.snapshot.telemetry.modulation.find((entry) => entry.kind === "modOutput" && entry.node === next.id);
    expect(output?.slot, "the cable took the slot after the last, and the modulator the one after that").toBe(taken + 2);
  });

  test("are used up by a cable to a parameter that no cable drives, which the layout names", () => {
    const server = new MockServer({ snapshot: historyFixture.snapshot });
    const first = made(server, { type: "addFreeModulator", kind: "lfo", hears: lane.id });
    const filter = server.snapshot.graph.nodes.find((n) => n.kind === "effect" && n.parent === lane.id && n.name === "Filter");
    if (!filter) throw new Error("the lane has a filter");
    const taken = Math.max(...slotsOf(server.snapshot));
    server.apply({ type: "addModEdge", modulator: first.id, to: { node: filter.id, port: { type: "param", index: 1 } } });
    expect(server.snapshot.telemetry.modulation.find((entry) => entry.kind === "effective" && entry.node === filter.id)?.slot).toBe(taken + 1);
  });
});
