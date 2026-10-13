import { describe, expect, test } from "vitest";

import type { Node, Snapshot, StampedEvent } from "../protocol";
import { MASTER, nodeNamed, tracerSnapshot } from "./fixtures";
import { applyEvent } from "./reducers";

const filter = nodeNamed(tracerSnapshot, "Filter");
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");

function paramOf(snapshot: Snapshot, node: Node, index: number) {
  const found = snapshot.graph.nodes.find((n) => n.id === node.id);
  if (found?.kind !== "instrument" && found?.kind !== "effect") throw new Error("no plugin");
  const param = found.plugin?.params.find((p) => p.index === index);
  if (!param) throw new Error(`no parameter ${index}`);
  return param;
}

function applied(snapshot: Snapshot, event: StampedEvent): Snapshot {
  const result = applyEvent(snapshot, event);
  if (!result.ok) throw new Error(result.reason);
  return result.snapshot;
}

/** The tracer with a group node (and a bus effect in it) added, for the gains that are about groups. */
function withGroup(): { snapshot: Snapshot; group: Node } {
  const group: Node = { id: 40, parent: null, name: "Pad", kind: "group", volume: 1, pan: 0 };
  return { snapshot: { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, group] } }, group };
}

describe("a parameter set", () => {
  const event: StampedEvent = { rev: 1, type: "paramSet", node: filter.id, param: 0, value: 0.9 };

  test("changes that parameter of that node, and the revision", () => {
    const after = applied(tracerSnapshot, event);
    expect(after.rev).toBe(1);
    expect(paramOf(after, filter, 0).value).toBe(0.9);
  });

  test("changes nothing else, and shares what it did not change", () => {
    const after = applied(tracerSnapshot, event);
    const before = paramOf(tracerSnapshot, filter, 1);
    expect(paramOf(after, filter, 1)).toBe(before);
    expect(after.graph.nodes.find((n) => n.id === oscillator.id)).toBe(oscillator);
    expect(after.graph.edges).toBe(tracerSnapshot.graph.edges);
    expect(after.telemetry).toBe(tracerSnapshot.telemetry);
  });

  test("leaves the snapshot it was applied to as it was", () => {
    const before = paramOf(tracerSnapshot, filter, 0).value;
    applied(tracerSnapshot, event);
    expect(paramOf(tracerSnapshot, filter, 0).value).toBe(before);
    expect(tracerSnapshot.rev).toBe(0);
  });

  test("is the next event, whatever the revision of the first one was", () => {
    const first = applied(tracerSnapshot, event);
    const second = applied(first, { rev: 2, type: "paramSet", node: filter.id, param: 0, value: 0.3 });
    expect(second.rev).toBe(2);
    expect(paramOf(second, filter, 0).value).toBe(0.3);
  });
});

describe("any event", () => {
  const events: [string, StampedEvent][] = [
    ["a parameter set", { rev: 1, type: "paramSet", node: filter.id, param: 0, value: 0.9 }],
    ["a gain set", { rev: 1, type: "gainSet", node: oscillator.id, gain: "volume", value: 0.5 }],
    ["a preset loaded", { rev: 1, type: "presetLoaded", node: filter.id, preset: "Dark", values: [{ param: 0, value: 0.2 }] }],
  ];

  test.each(events)("%s leaves the session with changes that are not saved, and nothing else of its meta changed", (_, event) => {
    expect(tracerSnapshot.meta.dirty).toBe(false);
    const after = applied(tracerSnapshot, event);
    expect(after.meta).toEqual({ ...tracerSnapshot.meta, dirty: true });
  });

  test.each(events)("%s shares the meta of a session that was not saved already", (_, event) => {
    const dirty = { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, dirty: true } };
    expect(applied(dirty, event).meta).toBe(dirty.meta);
  });
});

describe("a gain set", () => {
  test("sets the volume of an instrument", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "gainSet", node: oscillator.id, gain: "volume", value: 0.5 });
    const node = after.graph.nodes.find((n) => n.id === oscillator.id);
    expect(node?.kind === "instrument" && node.volume).toBe(0.5);
  });

  test("sets the mix of an effect", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "gainSet", node: filter.id, gain: "mix", value: 0.25 });
    const node = after.graph.nodes.find((n) => n.id === filter.id);
    expect(node?.kind === "effect" && node.mix).toBe(0.25);
  });

  test("sets the volume and the pan of a group", () => {
    const { snapshot, group } = withGroup();
    const louder = applied(snapshot, { rev: 1, type: "gainSet", node: group.id, gain: "groupVolume", value: 2 });
    const panned = applied(louder, { rev: 2, type: "gainSet", node: group.id, gain: "groupPan", value: -0.5 });
    const node = panned.graph.nodes.find((n) => n.id === group.id);
    expect(node?.kind === "group" && [node.volume, node.pan]).toEqual([2, -0.5]);
  });

  test("is refused by a node that does not have that gain", () => {
    const refused = [
      { node: filter.id, gain: "volume" as const },
      { node: oscillator.id, gain: "mix" as const },
      { node: oscillator.id, gain: "groupVolume" as const },
      { node: MASTER, gain: "groupPan" as const },
    ];
    for (const { node, gain } of refused) {
      const result = applyEvent(tracerSnapshot, { rev: 1, type: "gainSet", node, gain, value: 1 });
      expect(result.ok, `${gain} on ${node}`).toBe(false);
    }
  });
});

describe("a preset loaded", () => {
  test("names the preset and sets the parameters it set", () => {
    const event: StampedEvent = {
      rev: 1,
      type: "presetLoaded",
      node: filter.id,
      preset: "Dark",
      values: [
        { param: 0, value: 0.2 },
        { param: 1, value: 4 },
      ],
    };
    const after = applied(tracerSnapshot, event);
    const node = after.graph.nodes.find((n) => n.id === filter.id);
    expect(node?.kind === "effect" && node.plugin.currentPreset).toBe("Dark");
    expect([paramOf(after, filter, 0).value, paramOf(after, filter, 1).value]).toEqual([0.2, 4]);
    expect(paramOf(after, filter, 2)).toBe(paramOf(tracerSnapshot, filter, 2));
  });

  test("is refused if it sets a parameter the plugin does not have", () => {
    const result = applyEvent(tracerSnapshot, { rev: 1, type: "presetLoaded", node: filter.id, preset: "x", values: [{ param: 99, value: 1 }] });
    expect(result.ok).toBe(false);
  });
});

describe("an event that does not fit", () => {
  test("for a node that is not there says so", () => {
    const result = applyEvent(tracerSnapshot, { rev: 1, type: "paramSet", node: 12345, param: 0, value: 1 });
    expect(result).toEqual({ ok: false, reason: "there is no node 12345" });
  });

  test("for a parameter the plugin does not have says so", () => {
    const result = applyEvent(tracerSnapshot, { rev: 1, type: "paramSet", node: filter.id, param: 99, value: 1 });
    expect(result.ok).toBe(false);
    expect(!result.ok && result.reason).toContain("no parameter 99");
  });

  test("for a node with no plugin says so", () => {
    for (const node of [MASTER, nodeNamed(tracerSnapshot, "LFO").id]) {
      const result = applyEvent(tracerSnapshot, { rev: 1, type: "paramSet", node, param: 0, value: 1 });
      expect(!result.ok && result.reason, `node ${node}`).toContain("has no plugin");
    }
  });

  test("that skips a revision says the client has missed something", () => {
    const result = applyEvent(tracerSnapshot, { rev: 3, type: "paramSet", node: filter.id, param: 0, value: 1 });
    expect(result).toEqual({ ok: false, reason: "the session is at revision 0 and the next event is 3" });
  });
});

describe("an event the snapshot already has", () => {
  test("changes nothing, and gives the same snapshot back", () => {
    const at5 = { ...tracerSnapshot, rev: 5 };
    for (const rev of [5, 4, 0]) {
      const result = applyEvent(at5, { rev, type: "paramSet", node: filter.id, param: 0, value: 0.1 });
      expect(result.ok && result.snapshot).toBe(at5);
    }
  });
});
