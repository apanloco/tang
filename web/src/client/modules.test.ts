import { describe, expect, test } from "vitest";

import type { Intent } from "../protocol";
import { tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";

// Chains of effects saved as modules (`Intent::SaveModule`), against the mock engine.
const saveModule = (node: number, name: string): Intent => ({ type: "saveModule", node, name });

describe("the mock engine's library of modules", () => {
  test("is not there: a module is refused as a program that keeps no library refuses it, and nothing changes", () => {
    const server = new MockServer({ snapshot: tracerSnapshot });
    const lane = tracerSnapshot.graph.nodes.find((node) => node.kind === "instrument");
    if (lane === undefined) throw new Error("the tracer has a lane");
    const before = server.snapshot;
    expect(() => server.apply(saveModule(lane.id, "Space"))).toThrow("this program keeps no library of modules");
    expect(server.snapshot).toBe(before);
  });
});
