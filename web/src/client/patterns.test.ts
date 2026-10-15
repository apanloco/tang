import { describe, expect, test } from "vitest";

import type { Event, Intent, Node, NodeId } from "../protocol";
import { MASTER, patternsFixture } from "./fixtures";
import { MockServer } from "./mock";

/** The event without the revision it is stamped with: the fixture says what the controller told, and the mock's is a revision of its own. */
function unstamped(event: Record<string, unknown>): Event {
  const rest = { ...event };
  delete rest.rev;
  return rest as unknown as Event;
}

/** JSON with the keys of every object in order: two objects that say the same are the same text (the fixture has its keys as Rust writes them, the mock's are in the order it makes them). */
const canonical = (value: unknown): string =>
  JSON.stringify(value, (_key, inner: unknown) =>
    inner !== null && typeof inner === "object" && !Array.isArray(inner) ? Object.fromEntries(Object.entries(inner).sort(([a], [b]) => (a < b ? -1 : 1))) : inner,
  );

/** What the mock engine makes of the intent: the event it tells, or the words it refuses with. */
function asked(server: MockServer, intent: Intent): { event: Event } | { error: string } {
  try {
    const told = server.apply(intent);
    if (told.events.length !== 1) throw new Error(`${String(told.events.length)} events for ${JSON.stringify(intent)}`);
    return { event: unstamped(told.events[0] as unknown as Record<string, unknown>) };
  } catch (error) {
    if (error instanceof Error && error.message.includes(" events for ")) throw error;
    return { error: error instanceof Error ? error.message : String(error) };
  }
}

const patternOf = (nodes: readonly Node[], id: NodeId) => {
  const node = nodes.find((n) => n.id === id);
  return node?.kind === "instrument" ? node.pattern : undefined;
};

describe("the mock engine's patterns", () => {
  test("are what the controller makes of them, for every script its fixture has: the same event, or the same words of a refusal, one intent after the other", () => {
    const wrong: string[] = [];
    patternsFixture.scripts.forEach((script, number) => {
      const server = new MockServer({ snapshot: patternsFixture.snapshot });
      script.forEach((step, place) => {
        const got = asked(server, step.intent);
        const what = `script ${String(number)}, step ${String(place)} (${JSON.stringify(step.intent)})`;
        if ("error" in step) {
          if (!("error" in got) || got.error !== step.error) wrong.push(`${what}: ${JSON.stringify(got)}, and the controller refuses with ${step.error}`);
        } else if (!("event" in got) || canonical(got.event) !== canonical(step.event)) {
          wrong.push(`${what}: ${canonical(got)}, and the controller tells ${canonical(step.event)}`);
        }
      });
    });
    expect(wrong).toEqual([]);
  });

  test("have the session show what the event says: the pattern of the lane afterwards is the event's, and nothing else of the session has changed but that it is unsaved", () => {
    const before = patternsFixture.snapshot;
    for (const [number, script] of patternsFixture.scripts.entries()) {
      const server = new MockServer({ snapshot: before });
      const touched = new Set<NodeId>();
      for (const step of script) {
        const got = asked(server, step.intent);
        if (!("event" in got) || got.event.type !== "patternSet") continue;
        touched.add(got.event.node);
        expect(patternOf(server.snapshot.graph.nodes, got.event.node), `script ${String(number)}`).toEqual(got.event.pattern);
      }
      for (const node of before.graph.nodes) {
        if (touched.has(node.id)) continue;
        expect(server.snapshot.graph.nodes.find((n) => n.id === node.id), `script ${String(number)}: node ${String(node.id)}`).toEqual(node);
      }
      expect(server.snapshot.graph.edges).toEqual(before.graph.edges);
      expect(server.snapshot.telemetry).toEqual(before.telemetry);
      expect(server.snapshot.meta).toEqual({ ...before.meta, dirty: touched.size > 0 });
    }
  });

  test("refuse a setting that is not a number before they look at the node it is for, in the controller's words, and change nothing", () => {
    const server = new MockServer({ snapshot: patternsFixture.snapshot });
    const withPattern = patternsFixture.snapshot.graph.nodes.find((node) => node.kind === "instrument" && node.pattern !== null);
    const effect = patternsFixture.snapshot.graph.nodes.find((node) => node.kind === "effect");
    if (!withPattern || !effect) throw new Error("the fixture has a lane with a pattern and an effect");
    const words = { length: "Length", enabled: "Enabled", looping: "Looping", transpose: "Transpose" } as const;
    for (const node of [withPattern.id, effect.id, MASTER, 9999]) {
      for (const [value, said] of [[NaN, "NaN"], [Infinity, "inf"], [-Infinity, "-inf"], [1e39, "inf"], [-1e39, "-inf"]] as const) {
        for (const field of ["length", "enabled", "looping", "transpose"] as const) {
          expect(asked(server, { type: "setPatternField", node, field, value }), `${field} ${String(value)} for node ${String(node)}`).toEqual({ error: `${said} is not a value for ${words[field]}` });
        }
      }
    }
    expect(server.snapshot).toEqual(patternsFixture.snapshot);
  });

  test("have been tried with every kind of intent and every refusal: the fixture is not empty", () => {
    const steps = patternsFixture.scripts.flat();
    const kinds = new Set(steps.map((step) => step.intent.type));
    expect([...kinds].sort()).toEqual(["clearPattern", "setPatternField", "setRecording"]);
    const fields = new Set(steps.flatMap((step) => (step.intent.type === "setPatternField" ? [step.intent.field] : [])));
    expect([...fields].sort()).toEqual(["enabled", "length", "looping", "transpose"]);
    const words = new Set(steps.flatMap((step) => ("error" in step ? [step.error] : [])));
    for (const sentence of ["a pattern belongs to a lane", "that lane has no pattern", "there is no node 9999"]) {
      expect(words.has(sentence), sentence).toBe(true);
    }
    const told = steps.flatMap((step) => ("event" in step && step.event.type === "patternSet" ? [step.event] : []));
    expect(told.length).toBeGreaterThan(100);
    // A pattern that is told, one that is taken away, one that is recording, and a length that was held to its range.
    expect(told.some((event) => event.pattern === null)).toBe(true);
    expect(told.some((event) => event.pattern?.recording === true)).toBe(true);
    expect(told.some((event) => event.pattern?.lengthBeats === 32)).toBe(true);
    expect(told.some((event) => event.pattern?.lengthBeats === 1)).toBe(true);
  });
});
