import { describe, expect, test } from "vitest";

import type { Edge, Node, Snapshot, StampedEvent } from "../protocol";
import { MASTER, nodeNamed, tracerSnapshot } from "./fixtures";
import { applyEvent } from "./reducers";

const filter = nodeNamed(tracerSnapshot, "Filter");
const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const lfo = nodeNamed(tracerSnapshot, "LFO");
/** The id of the cable from the LFO to the filter's cutoff, in the tracer. */
const CABLE = 0;

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

/** The tracer with an envelope and a MIDI-learn control added, for the settings an LFO does not have. */
function withEnvelope(): { snapshot: Snapshot; envelope: Node; learn: Node } {
  const envelope: Node = {
    id: 41,
    parent: oscillator.id,
    name: "ADSR",
    kind: "modulator",
    source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
  };
  const learn: Node = { id: 42, parent: oscillator.id, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } };
  return { snapshot: { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, envelope, learn] } }, envelope, learn };
}

function sourceOf(snapshot: Snapshot, node: Node) {
  const found = snapshot.graph.nodes.find((n) => n.id === node.id);
  if (found?.kind !== "modulator") throw new Error("not a modulator");
  return found.source;
}

function cableOf(snapshot: Snapshot): Extract<Edge, { kind: "modulation" }> {
  const found = snapshot.graph.edges.find((e): e is Extract<Edge, { kind: "modulation" }> => e.kind === "modulation");
  if (!found) throw new Error("no cable");
  return found;
}

function amountOf(snapshot: Snapshot, edge: number): number {
  const found = snapshot.graph.edges.find((e) => e.kind === "modulation" && e.id === edge);
  if (found?.kind !== "modulation") throw new Error(`no cable ${edge}`);
  return found.amount;
}

describe("a parameter set", () => {
  const event: StampedEvent = { rev: 1, type: "paramSet", node: filter.id, param: 0, value: 0.9 };

  test("changes that parameter of that node, and the revision", () => {
    const after = applied(tracerSnapshot, event);
    expect(after.rev).toBe(1);
    expect(paramOf(after, filter, 0).value).toBe(0.9);
  });

  test("leaves the default of the parameter as it was: only a preset moves that", () => {
    const before = paramOf(tracerSnapshot, filter, 0).default;
    expect(before).not.toBe(0.9);
    expect(paramOf(applied(tracerSnapshot, event), filter, 0).default).toBe(before);
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
    ["a range set", { rev: 1, type: "rangeSet", node: oscillator.id, range: { low: 36, high: 84 } }],
    ["a modulator field set", { rev: 1, type: "modulatorFieldSet", node: lfo.id, field: "rate", value: 4 }],
    ["an edge depth set", { rev: 1, type: "edgeDepthSet", edge: CABLE, depth: -0.5 }],
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

describe("a saved event", () => {
  const unsaved = { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, dirty: true } };

  test("leaves the session with nothing unsaved, names its file, and changes nothing of the graph", () => {
    const after = applied(unsaved, { rev: 1, type: "saved", file: "song.toml" });
    expect(after.meta).toEqual({ ...tracerSnapshot.meta, dirty: false, file: "song.toml" });
    expect(after.rev).toBe(1);
    expect(after.graph).toBe(unsaved.graph);
    expect(after.telemetry).toBe(unsaved.telemetry);
  });

  test("says when the session has no file", () => {
    expect(applied(unsaved, { rev: 1, type: "saved", file: null }).meta.file).toBeNull();
  });

  test("is an event like the others: one that skips a revision means events were missed, and the next change is unsaved again", () => {
    expect(applyEvent(unsaved, { rev: 2, type: "saved", file: "song.toml" }).ok).toBe(false);
    const saved = applied(unsaved, { rev: 1, type: "saved", file: "song.toml" });
    expect(applied(saved, { rev: 2, type: "gainSet", node: oscillator.id, gain: "volume", value: 0.5 }).meta.dirty).toBe(true);
  });

  test("is nothing to do for a snapshot that has it already", () => {
    expect(applyEvent(unsaved, { rev: 0, type: "saved", file: "song.toml" })).toEqual({ ok: true, snapshot: unsaved });
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

describe("a range set", () => {
  const keys = (snapshot: Snapshot) => {
    const node = snapshot.graph.nodes.find((n) => n.id === oscillator.id);
    if (node?.kind !== "instrument") throw new Error("not a lane");
    return node.range;
  };

  test("sets the keys of a lane, and none is every note", () => {
    expect(keys(tracerSnapshot)).toBeNull();
    const after = applied(tracerSnapshot, { rev: 1, type: "rangeSet", node: oscillator.id, range: { low: 36, high: 84 } });
    expect(keys(after)).toEqual({ low: 36, high: 84 });
    expect(keys(applied(after, { rev: 2, type: "rangeSet", node: oscillator.id, range: null }))).toBeNull();
  });

  test("changes that lane and nothing else: every other node is the object it was", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "rangeSet", node: oscillator.id, range: { low: 36, high: 84 } });
    for (const node of tracerSnapshot.graph.nodes) {
      const now = after.graph.nodes.find((n) => n.id === node.id);
      if (node.id === oscillator.id) expect(now).not.toBe(node);
      else expect(now).toBe(node);
    }
  });

  test("keeps the lane's own plugin, volume and the rest", () => {
    const before = tracerSnapshot.graph.nodes.find((n) => n.id === oscillator.id);
    const after = applied(tracerSnapshot, { rev: 1, type: "rangeSet", node: oscillator.id, range: { low: 60, high: 60 } }).graph.nodes.find((n) => n.id === oscillator.id);
    expect(after).toEqual({ ...before, range: { low: 60, high: 60 } });
  });

  test("is refused by a node that is not a lane, and by one that is not there", () => {
    for (const node of [filter.id, lfo.id, MASTER]) {
      expect(applyEvent(tracerSnapshot, { rev: 1, type: "rangeSet", node, range: null }).ok, `node ${node}`).toBe(false);
    }
    const result = applyEvent(tracerSnapshot, { rev: 1, type: "rangeSet", node: 4242, range: null });
    expect(result).toEqual({ ok: false, reason: "there is no node 4242" });
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

  test("makes the values it set the parameters' defaults, which is what the controller keeps them as", () => {
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
    expect([paramOf(after, filter, 0).default, paramOf(after, filter, 1).default]).toEqual([0.2, 4]);
    expect(paramOf(after, filter, 2).default).toBe(paramOf(tracerSnapshot, filter, 2).default);
  });

  test("is refused if it sets a parameter the plugin does not have", () => {
    const result = applyEvent(tracerSnapshot, { rev: 1, type: "presetLoaded", node: filter.id, preset: "x", values: [{ param: 99, value: 1 }] });
    expect(result.ok).toBe(false);
  });
});

describe("a modulator field set", () => {
  test("sets the rate of an LFO", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "modulatorFieldSet", node: lfo.id, field: "rate", value: 4 });
    expect(sourceOf(after, lfo)).toEqual({ type: "lfo", waveform: "sine", rate: 4 });
  });

  test("sets the waveform of an LFO by its index: sine, triangle, saw, square", () => {
    const waveforms = ["sine", "triangle", "saw", "square"];
    waveforms.forEach((name, index) => {
      const after = applied(tracerSnapshot, { rev: 1, type: "modulatorFieldSet", node: lfo.id, field: "waveform", value: index });
      expect(sourceOf(after, lfo), `index ${index}`).toEqual({ type: "lfo", waveform: name, rate: 0.5 });
    });
  });

  test("sets each stage of an envelope and leaves the others", () => {
    const { snapshot, envelope } = withEnvelope();
    const stages = [
      ["attack", { attack: 0.5, decay: 0.3, sustain: 0.7, release: 0.5 }],
      ["decay", { attack: 0.01, decay: 1.5, sustain: 0.7, release: 0.5 }],
      ["sustain", { attack: 0.01, decay: 0.3, sustain: 0.25, release: 0.5 }],
      ["release", { attack: 0.01, decay: 0.3, sustain: 0.7, release: 2 }],
    ] as const;
    const values = { attack: 0.5, decay: 1.5, sustain: 0.25, release: 2 };
    for (const [field, expected] of stages) {
      const after = applied(snapshot, { rev: 1, type: "modulatorFieldSet", node: envelope.id, field, value: values[field] });
      expect(sourceOf(after, envelope), field).toEqual({ type: "envelope", ...expected });
    }
  });

  test("changes only the modulator it names, and shares the rest of the graph", () => {
    const { snapshot, envelope } = withEnvelope();
    const after = applied(snapshot, { rev: 1, type: "modulatorFieldSet", node: lfo.id, field: "rate", value: 4 });
    expect(after.graph.nodes.find((n) => n.id === envelope.id)).toBe(envelope);
    expect(after.graph.nodes.find((n) => n.id === oscillator.id)).toBe(oscillator);
    expect(after.graph.edges).toBe(snapshot.graph.edges);
    expect(sourceOf(snapshot, lfo)).toEqual({ type: "lfo", waveform: "sine", rate: 0.5 });
  });

  test("is refused by what it does not fit, and says why", () => {
    const { snapshot, envelope, learn } = withEnvelope();
    const refused = [
      { node: 12345, field: "rate" as const, value: 1, reason: "there is no node 12345" },
      { node: oscillator.id, field: "rate" as const, value: 1, reason: `node ${oscillator.id} is not a modulator` },
      { node: MASTER, field: "rate" as const, value: 1, reason: `node ${MASTER} is not a modulator` },
      { node: lfo.id, field: "attack" as const, value: 1, reason: `attack does not apply to node ${lfo.id}` },
      { node: lfo.id, field: "sustain" as const, value: 1, reason: `sustain does not apply to node ${lfo.id}` },
      { node: envelope.id, field: "rate" as const, value: 1, reason: `rate does not apply to node ${envelope.id}` },
      { node: envelope.id, field: "waveform" as const, value: 1, reason: `waveform does not apply to node ${envelope.id}` },
      { node: learn.id, field: "rate" as const, value: 1, reason: `rate does not apply to node ${learn.id}` },
      { node: lfo.id, field: "waveform" as const, value: 4, reason: "4 is not a waveform" },
      { node: lfo.id, field: "waveform" as const, value: 1.5, reason: "1.5 is not a waveform" },
      { node: lfo.id, field: "waveform" as const, value: -1, reason: "-1 is not a waveform" },
      { node: lfo.id, field: "waveform" as const, value: Number.NaN, reason: "NaN is not a waveform" },
    ];
    for (const { node, field, value, reason } of refused) {
      expect(applyEvent(snapshot, { rev: 1, type: "modulatorFieldSet", node, field, value }), `${field} of ${node}`).toEqual({ ok: false, reason });
    }
  });
});

describe("an edge depth set", () => {
  test("sets the depth of that cable", () => {
    const after = applied(tracerSnapshot, { rev: 1, type: "edgeDepthSet", edge: CABLE, depth: -0.5 });
    expect(amountOf(after, CABLE)).toBe(-0.5);
    expect(amountOf(tracerSnapshot, CABLE)).toBe(0.4);
  });

  test("changes only that cable, and shares the nodes and the other edges", () => {
    const second = { ...cableOf(tracerSnapshot), id: 7, amount: 0.9 };
    const snapshot = { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, edges: [...tracerSnapshot.graph.edges, second] } };
    const after = applied(snapshot, { rev: 1, type: "edgeDepthSet", edge: CABLE, depth: 0.1 });
    expect(after.graph.nodes).toBe(snapshot.graph.nodes);
    expect(after.graph.edges.filter((e) => e.kind !== "modulation")).toEqual(snapshot.graph.edges.filter((e) => e.kind !== "modulation"));
    expect(after.graph.edges.at(-1)).toBe(second);
    expect([amountOf(after, CABLE), amountOf(after, 7)]).toEqual([0.1, 0.9]);
  });

  test("is refused for a cable that is not there, and says so", () => {
    expect(applyEvent(tracerSnapshot, { rev: 1, type: "edgeDepthSet", edge: 99, depth: 0.5 })).toEqual({ ok: false, reason: "there is no cable 99" });
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
