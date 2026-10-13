import { afterEach, describe, expect, test } from "vitest";

import type { Intent, Node, ServerMessage, Snapshot, TelemetryLayout } from "../protocol";
import { Client, RequestError } from "./connection";
import type { ClientOptions, Transport } from "./connection";
import { MASTER, nodeNamed, tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";
import type { MockOptions } from "./mock";

const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");
const cutoff = (value: number): Intent => ({ type: "setParam", node: filter.id, param: 0, value });

/** Wait, a little at most, for something to be so. */
async function until(condition: () => boolean, what: string, timeout = 2000): Promise<void> {
  const deadline = Date.now() + timeout;
  while (!condition()) {
    if (Date.now() > deadline) throw new Error(`gave up waiting for ${what}`);
    await new Promise((resolve) => setTimeout(resolve, 2));
  }
}

const quiet = (ms = 40) => new Promise((resolve) => setTimeout(resolve, ms));

const clients: Client[] = [];
afterEach(() => {
  for (const client of clients.splice(0)) client.stop();
});

/** A client of a mock server, started. */
function connect(server: MockServer, options: Partial<ClientOptions> = {}): Client {
  const client = new Client({ transport: server.transport, reconnectDelay: () => 5, ...options });
  clients.push(client);
  client.start();
  return client;
}

async function open(server: MockServer, options: Partial<ClientOptions> = {}): Promise<Client> {
  const client = connect(server, options);
  await until(() => client.state.status === "open", "the session");
  return client;
}

/** The value of a parameter in the client's copy of the session. */
function valueOf(client: Client, node: typeof filter, param: number): number | undefined {
  const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node.id);
  if (found?.kind !== "effect" && found?.kind !== "instrument") return undefined;
  return found.plugin?.params.find((p) => p.index === param)?.value;
}

describe("connecting", () => {
  test("says hello, subscribes and has the session", async () => {
    const server = new MockServer();
    const client = connect(server);
    expect(client.state.status).toBe("connecting");
    await until(() => client.state.status === "open", "the session");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
    expect(client.state.hello).toEqual({ protocol: 1, rev: 0, features: ["notes", "telemetry"], needsToken: false });
    expect(client.state.problem).toBeNull();
    expect(server.received).toEqual([{ type: "subscribe" }]);
  });

  test("asks for its telemetry once the server has said hello", async () => {
    const server = new MockServer();
    await open(server, { telemetry: ["meters", "modulation"] });
    expect(server.received).toEqual([{ type: "subscribe" }, { type: "telemetry", channels: ["meters", "modulation"] }]);
  });

  test("refuses a server that speaks another protocol, and says so, and does not try again", async () => {
    const server = new MockServer({ protocol: 2 });
    const client = connect(server);
    await until(() => client.state.status === "stopped", "the refusal");
    expect(client.state.problem).toMatch(/protocol 1 and the server speaks 2/);
    expect(client.state.snapshot).toBeNull();
    await quiet();
    expect(server.received).toEqual([]);
    expect(server.connections).toBe(0);
    expect(client.state.reconnects).toBe(0);
  });
});

describe("the token", () => {
  test("is given to a server that wants one, before anything else", async () => {
    const server = new MockServer({ token: "secret" });
    await open(server, { token: "secret" });
    expect(server.received).toEqual([{ type: "auth", token: "secret" }, { type: "subscribe" }]);
  });

  test("is asked for in words when there is none", async () => {
    const server = new MockServer({ token: "secret" });
    const client = connect(server);
    await until(() => client.state.status === "stopped", "the refusal");
    expect(client.state.problem).toMatch(/token/);
    expect(server.received).toEqual([]);
  });

  test("being wrong ends it, with the server's reason, and does not try again", async () => {
    const server = new MockServer({ token: "secret" });
    const client = connect(server, { token: "not it" });
    await until(() => client.state.status === "stopped", "the refusal");
    expect(client.state.problem).toBe("the token is wrong");
    await quiet();
    expect(server.received).toEqual([{ type: "auth", token: "not it" }]);
    expect(client.state.reconnects).toBe(0);
  });
});

describe("a request", () => {
  test("is applied by the server, comes back as an event, and resolves with the revision", async () => {
    const server = new MockServer();
    const client = await open(server);
    const rev = await client.request(cutoff(0.9));
    expect(rev).toBe(1);
    // The event came before the answer, so the change is in the store when the promise resolves.
    expect(client.state.snapshot?.rev).toBe(1);
    expect(valueOf(client, filter, 0)).toBe(0.9);
    expect(server.snapshot.rev).toBe(1);
  });

  test("is clamped by the server as the controller does, and the client hears the value that stands", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request(cutoff(5));
    expect(valueOf(client, filter, 0)).toBe(1);
    // a gain has its own range
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    await client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 3 });
    await client.request({ type: "setGain", node: filter.id, gain: "mix", value: 7 });
    const nodes = client.state.snapshot?.graph.nodes;
    expect(nodes?.find((n) => n.id === oscillator.id)).toMatchObject({ volume: 3 });
    expect(nodes?.find((n) => n.id === filter.id)).toMatchObject({ mix: 1 });
    await client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 9 });
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === oscillator.id)).toMatchObject({ volume: 4 });
    // an enum parameter is rounded: the filter's `type` is the one with labels
    const type = nodeNamed(tracerSnapshot, "Filter");
    const param = type.kind === "effect" ? type.plugin.params.find((p) => p.labels !== null) : undefined;
    expect(param).toBeDefined();
    await client.request({ type: "setParam", node: filter.id, param: param?.index ?? -1, value: 1.4 });
    expect(valueOf(client, filter, param?.index ?? -1)).toBe(1);
  });

  test("sets a modulator's settings and a cable's depth, clamped and rounded as the controller does", async () => {
    const server = new MockServer();
    const client = await open(server);
    const source = () => {
      const node = client.state.snapshot?.graph.nodes.find((n) => n.id === lfo.id);
      return node?.kind === "modulator" ? node.source : undefined;
    };
    const depth = () => {
      const cable = client.state.snapshot?.graph.edges.find((e) => e.kind === "modulation");
      return cable?.kind === "modulation" ? cable.amount : undefined;
    };
    const field = (name: "rate" | "waveform", value: number) => client.request({ type: "setModulatorField", node: lfo.id, field: name, value });
    await field("rate", 4);
    expect(source()).toMatchObject({ type: "lfo", rate: 4 });
    await field("rate", 900);
    expect(source()).toMatchObject({ rate: 50 });
    await field("rate", -3);
    expect(source()).toMatchObject({ rate: 0.01 });
    // a waveform is an index, rounded: 2.4 is the saw, and there is no waveform after the square
    await field("waveform", 2.4);
    expect(source()).toMatchObject({ waveform: "saw" });
    await field("waveform", 9);
    expect(source()).toMatchObject({ waveform: "square" });
    await field("waveform", -2);
    expect(source()).toMatchObject({ waveform: "sine" });
    // a cable's depth goes from -1 to 1
    await client.request({ type: "setEdgeDepth", edge: 0, depth: -0.5 });
    expect(depth()).toBe(-0.5);
    await client.request({ type: "setEdgeDepth", edge: 0, depth: 7 });
    expect(depth()).toBe(1);
    await client.request({ type: "setEdgeDepth", edge: 0, depth: -7 });
    expect(depth()).toBe(-1);
  });

  test("sets the stages of an envelope, clamped to their ranges", async () => {
    const envelope: Node = {
      id: 41,
      parent: 0,
      name: "ADSR",
      kind: "modulator",
      source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
    };
    const snapshot = { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [...tracerSnapshot.graph.nodes, envelope] } };
    const server = new MockServer({ snapshot });
    const client = await open(server);
    for (const [field, value] of [["attack", 99], ["decay", 0], ["sustain", 4], ["release", 2.5]] as const) {
      await client.request({ type: "setModulatorField", node: envelope.id, field, value });
    }
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === envelope.id);
    expect(node?.kind === "modulator" && node.source).toEqual({ type: "envelope", attack: 10, decay: 0.001, sustain: 1, release: 2.5 });
    // (an envelope has no rate and no waveform)
    for (const field of ["rate", "waveform"] as const) {
      await expect(client.request({ type: "setModulatorField", node: envelope.id, field, value: 1 })).rejects.toMatchObject({ code: "invalid" });
    }
  });

  test("that the server rejects fails with its code, and changes nothing", async () => {
    const server = new MockServer();
    const client = await open(server);
    const failure = await client.request({ type: "setParam", node: 424242, param: 0, value: 1 }).catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(RequestError);
    expect((failure as RequestError).code).toBe("notFound");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
    const nonsense = await client.request({ type: "setGain", node: filter.id, gain: "volume", value: 1 }).catch((e: unknown) => e);
    expect((nonsense as RequestError).code).toBe("invalid");
  });

  test("for a modulator's setting or a cable's depth that does not fit fails, and changes nothing", async () => {
    const server = new MockServer();
    const client = await open(server);
    const fails = async (intent: Intent) => (await client.request(intent).catch((e: unknown) => e)) as RequestError;
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    const refused = [
      [{ type: "setModulatorField", node: 424242, field: "rate", value: 1 }, "notFound", "there is no node 424242"],
      [{ type: "setModulatorField", node: oscillator.id, field: "rate", value: 1 }, "invalid", "that node is not a modulator"],
      [{ type: "setModulatorField", node: lfo.id, field: "attack", value: 1 }, "invalid", "attack does not apply to that modulator"],
      [{ type: "setEdgeDepth", edge: 99, depth: 0.5 }, "notFound", "there is no cable 99"],
    ] as const;
    for (const [intent, code, message] of refused) {
      const failure = await fails(intent);
      expect([failure.code, failure.message], JSON.stringify(intent)).toEqual([code, message]);
    }
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("that names the master is for a node that is not there (the master is not a node of the model), as the controller has it", async () => {
    const server = new MockServer();
    const client = await open(server);
    for (const intent of [
      { type: "setParam", node: MASTER, param: 0, value: 1 },
      { type: "setGain", node: MASTER, gain: "volume", value: 1 },
      { type: "setModulatorField", node: MASTER, field: "rate", value: 1 },
    ] as const) {
      const failure = (await client.request(intent).catch((e: unknown) => e)) as RequestError;
      expect([failure.code, failure.message], intent.type).toEqual(["notFound", `there is no node ${String(MASTER)}`]);
    }
  });

  test("with a number the controller cannot hold (beyond 32 bits: infinity there) is refused, and a half that rounds up in 32 bits is rounded up", async () => {
    const server = new MockServer();
    const client = await open(server);
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    for (const intent of [
      { type: "setParam", node: filter.id, param: 0, value: 1e39 },
      { type: "setGain", node: oscillator.id, gain: "volume", value: -1e39 },
      { type: "setModulatorField", node: lfo.id, field: "rate", value: 1e39 },
      { type: "setEdgeDepth", edge: 0, depth: 1e39 },
    ] as const) {
      const failure = (await client.request(intent).catch((e: unknown) => e)) as RequestError;
      expect([failure.code, intent.type], failure.message).toEqual(["invalid", intent.type]);
    }
    // 0.49999999 is 0.5 in 32 bits, which a waveform rounds to the next: the triangle, not the sine.
    await client.request({ type: "setModulatorField", node: lfo.id, field: "waveform", value: 0.49999999 });
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === lfo.id)).toMatchObject({ source: { waveform: "triangle" } });
    // So does an enum parameter: the oscillator's waveform, 1.49999999 being 1.5 and so the third.
    await client.request({ type: "setParam", node: oscillator.id, param: 0, value: 1.49999999 });
    const waveform = client.state.snapshot?.graph.nodes.find((n) => n.id === oscillator.id);
    expect(waveform?.kind === "instrument" && waveform.plugin?.params[0]?.value).toBe(2);
    expect(client.state.snapshot?.rev).toBe(2);
  });

  test("for a value that is not a number is refused in the server's own process (JSON has no such number to send)", () => {
    const server = new MockServer();
    expect(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "rate", value: Number.NaN });
    }).toThrow("NaN is not a value for rate");
    expect(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: Number.POSITIVE_INFINITY });
    }).toThrow("Infinity is not a value for the depth of a cable");
    expect(server.snapshot).toEqual(tracerSnapshot);
  });

  test("answers each of several at once", async () => {
    const server = new MockServer();
    const client = await open(server);
    const revs = await Promise.all([0.1, 0.2, 0.3].map((v) => client.request(cutoff(v))));
    expect(revs).toEqual([1, 2, 3]);
    expect(valueOf(client, filter, 0)).toBe(0.3);
  });

  test("when the client is not connected fails at once", async () => {
    const client = new Client({ transport: new MockServer().transport });
    await expect(client.request(cutoff(0.5))).rejects.toThrow(/Not connected/);
  });

  test("that is waiting when the connection is lost fails, and says so", async () => {
    const server = new MockServer();
    const client = await open(server);
    const waiting = client.request(cutoff(0.5));
    server.drop();
    await expect(waiting).rejects.toMatchObject({ code: null, message: expect.stringContaining("lost") as unknown });
  });
});

describe("taking a cable out", () => {
  const cables = (snapshot: Snapshot | null | undefined) => snapshot?.graph.edges.filter((e) => e.kind === "modulation") ?? [];

  test("takes it out of the session, and the client is told by a snapshot, the revision after the intent's", async () => {
    const server = new MockServer();
    const client = await open(server);
    // The answer is the intent's revision; the snapshot that tells the change is the next (as the controller has it).
    expect(await client.request({ type: "removeEdge", edge: 0 })).toBe(1);
    await until(() => client.state.snapshot?.rev === 2, "the snapshot");
    expect(cables(client.state.snapshot)).toEqual([]);
    expect(client.state.snapshot?.graph.edges).toEqual(tracerSnapshot.graph.edges.filter((e) => e.kind !== "modulation"));
    expect(client.state.snapshot?.graph.nodes).toEqual(tracerSnapshot.graph.nodes);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(server.snapshot).toEqual(client.state.snapshot);
  });

  test("takes the slot of the value it drove from the layout of the telemetry, which has a new fingerprint", async () => {
    const server = new MockServer();
    const client = await open(server);
    const effective = (layout: TelemetryLayout) => layout.modulation.filter((entry) => entry.kind === "effective");
    expect(effective(tracerSnapshot.telemetry)).toHaveLength(1);
    await client.request({ type: "removeEdge", edge: 0 });
    await until(() => client.state.snapshot?.rev === 2, "the snapshot");
    const layout = client.state.snapshot?.telemetry;
    expect(layout && effective(layout)).toEqual([]);
    expect(layout?.modulation.filter((entry) => entry.kind === "modOutput")).toHaveLength(1);
    expect(layout?.rev).not.toBe(tracerSnapshot.telemetry.rev);
    expect(layout?.meters).toEqual(tracerSnapshot.telemetry.meters);
  });

  /** The tracer with two more modulators: one drives the depth of the cutoff's cable, the other the depth of that. */
  function withCablesThatDriveCables(): Snapshot {
    const modulator = (id: number, name: string): Node => ({ id, parent: 0, name, kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1 } });
    const drives = (from: number, edge: number, id: number) =>
      ({ kind: "modulation", id, amount: 0.5, from: { node: from, port: { type: "modOut" } }, to: { node: from === 5 ? 2 : 5, port: { type: "depth", edge } } }) as const;
    return {
      ...tracerSnapshot,
      graph: {
        nodes: [...tracerSnapshot.graph.nodes, modulator(5, "LFO"), modulator(6, "LFO")],
        edges: [...tracerSnapshot.graph.edges, drives(5, 0, 1), { ...drives(6, 1, 2), to: { node: 5, port: { type: "depth", edge: 1 } } }],
      },
    };
  }

  test("takes the cables that drive the depth of a cable that goes, and theirs in turn", async () => {
    const server = new MockServer({ snapshot: withCablesThatDriveCables() });
    const client = await open(server);
    expect(cables(client.state.snapshot)).toHaveLength(3);
    await client.request({ type: "removeEdge", edge: 0 });
    await until(() => client.state.snapshot?.rev === 2, "the snapshot");
    expect(cables(client.state.snapshot)).toEqual([]);
    // The modulators stay: what goes is what they drove.
    expect(client.state.snapshot?.graph.nodes).toHaveLength(tracerSnapshot.graph.nodes.length + 2);
  });

  test("takes only a cable that nothing drives, when it is the one that is asked for", async () => {
    const server = new MockServer({ snapshot: withCablesThatDriveCables() });
    const client = await open(server);
    await client.request({ type: "removeEdge", edge: 2 });
    await until(() => client.state.snapshot?.rev === 2, "the snapshot");
    expect(cables(client.state.snapshot).map((e) => e.id)).toEqual([0, 1]);
  });

  test("that is not there is refused, and changes nothing", async () => {
    const server = new MockServer();
    const client = await open(server);
    const failure = await client.request({ type: "removeEdge", edge: 99 }).catch((e: unknown) => e);
    expect(failure).toBeInstanceOf(RequestError);
    expect([(failure as RequestError).code, (failure as RequestError).message]).toEqual(["notFound", "there is no cable 99"]);
    expect(client.state.snapshot).toEqual(tracerSnapshot);
    // Nor is one there the second time.
    await client.request({ type: "removeEdge", edge: 0 });
    await expect(client.request({ type: "removeEdge", edge: 0 })).rejects.toMatchObject({ code: "notFound" });
  });

  test("is told to every client, and to the one that asked once, before the answer", async () => {
    const server = new MockServer();
    const [asker, other] = [await open(server), await open(server)];
    const heard: string[] = [];
    asker.store.subscribe((state, before) => {
      if (state.snapshot !== before.snapshot) heard.push(`snapshot ${String(state.snapshot?.rev)}`);
    });
    const rev = await asker.request({ type: "removeEdge", edge: 0 });
    heard.push(`answer ${String(rev)}`);
    await until(() => other.state.snapshot?.rev === 2, "the other client's snapshot");
    expect(cables(other.state.snapshot)).toEqual([]);
    expect(heard).toEqual(["snapshot 2", "answer 1"]);
  });

  test("by another front-end reaches the client as a snapshot", async () => {
    const server = new MockServer();
    const client = await open(server);
    server.apply({ type: "removeEdge", edge: 0 });
    await until(() => client.state.snapshot?.rev === 2, "the snapshot");
    expect(cables(client.state.snapshot)).toEqual([]);
    expect(server.received.filter((m) => m.type === "subscribe")).toHaveLength(1);
    expect(() => {
      server.apply({ type: "removeEdge", edge: 0 });
    }).toThrow("there is no cable 0");
  });
});

describe("adding a modulator", () => {
  const lane = nodeNamed(tracerSnapshot, "Oscillator");
  const nodesOf = (client: Client): Node[] => client.state.snapshot?.graph.nodes ?? [];
  const names = (client: Client): string[] => nodesOf(client).map((n) => n.name);

  /** The tracer with its lane in a group (a node of the group first, as the controller lists them). */
  function inAGroup(): Snapshot {
    const group: Node = { id: 10, parent: null, name: "Group 1", kind: "group", volume: 1, pan: 0 };
    const nodes = tracerSnapshot.graph.nodes.map((n) => (n.id === lane.id ? { ...n, parent: 10 } : n));
    return { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes: [group, ...nodes] } };
  }

  test("puts a node of that kind after what the lane has, and the client is told by a snapshot, the revision after the intent's", async () => {
    const server = new MockServer();
    const client = await open(server);
    expect(await client.request({ type: "addModulator", parent: lane.id, kind: "envelope" })).toBe(1);
    await until(() => client.state.snapshot?.rev === 2, "the snapshot");
    expect(names(client)).toEqual(["Oscillator", "Filter", "LFO", "ADSR", "Master"]);
    expect(nodesOf(client)[3]).toEqual({
      id: 3,
      parent: lane.id,
      name: "ADSR",
      kind: "modulator",
      source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
    });
    // It drives nothing yet.
    expect(client.state.snapshot?.graph.edges).toEqual(tracerSnapshot.graph.edges);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(server.snapshot).toEqual(client.state.snapshot);
  });

  test("makes each kind what the controller makes it", async () => {
    const server = new MockServer();
    const client = await open(server);
    for (const kind of ["lfo", "envelope", "midiLearn"] as const) await client.request({ type: "addModulator", parent: lane.id, kind });
    const added = nodesOf(client).slice(3, 6);
    expect(added.map((n) => n.name)).toEqual(["LFO", "ADSR", "MIDI (learning…)"]);
    expect(added.map((n) => (n.kind === "modulator" ? n.source : null))).toEqual([
      { type: "lfo", waveform: "sine", rate: 1 },
      { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 },
      { type: "midiLearn", control: null, learning: true },
    ]);
    expect(added.map((n) => n.id)).toEqual([3, 4, 5]);
    expect(names(client).at(-1)).toBe("Master");
  });

  test("hands out ids that nobody had, whichever front-end asks", async () => {
    const server = new MockServer();
    const client = await open(server);
    server.apply({ type: "addModulator", parent: lane.id, kind: "lfo" });
    await client.request({ type: "addModulator", parent: lane.id, kind: "lfo" });
    const ids = nodesOf(client).filter((n) => n.id !== MASTER).map((n) => n.id);
    expect(ids).toEqual([0, 1, 2, 3, 4]);
  });

  test("gives the modulator's output a slot of its own in the layout, among the outputs and with a new fingerprint", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request({ type: "addModulator", parent: lane.id, kind: "lfo" });
    const layout = client.state.snapshot?.telemetry;
    expect(layout?.modulation).toEqual([
      ...tracerSnapshot.telemetry.modulation.slice(0, 1),
      { kind: "modOutput", node: 3, port: { type: "modOut" }, slot: 6 },
      ...tracerSnapshot.telemetry.modulation.slice(1),
    ]);
    expect(layout?.rev).not.toBe(tracerSnapshot.telemetry.rev);
    expect(layout?.meters).toEqual(tracerSnapshot.telemetry.meters);
    await client.request({ type: "addModulator", parent: lane.id, kind: "lfo" });
    const slots = client.state.snapshot?.telemetry.modulation.filter((e) => e.kind === "modOutput").map((e) => e.slot);
    expect(slots).toEqual([4, 6, 7]);
  });

  test("in a group goes in the group's strip: after the group, not after the lanes that are its members", async () => {
    const server = new MockServer({ snapshot: inAGroup() });
    const client = await open(server);
    await client.request({ type: "addModulator", parent: 10, kind: "lfo" });
    expect(names(client)).toEqual(["Group 1", "LFO", "Oscillator", "Filter", "LFO", "Master"]);
    expect(nodesOf(client)[1]).toMatchObject({ parent: 10, kind: "modulator" });
    // (A second goes after the first, still in the group's strip.)
    await client.request({ type: "addModulator", parent: 10, kind: "envelope" });
    expect(names(client)).toEqual(["Group 1", "LFO", "ADSR", "Oscillator", "Filter", "LFO", "Master"]);
    // (The lane is in the group, and so are the two modulators: the lane's own is in the lane.)
    expect(nodesOf(client).map((n) => n.parent)).toEqual([null, 10, 10, 10, 0, 0, null]);
    // The output of the group's modulator is listed before the lane's, as the controller lists them.
    expect(client.state.snapshot?.telemetry.modulation.filter((e) => e.kind === "modOutput").map((e) => e.node)).toEqual([11, 12, 2]);
  });

  test("goes after the lane's effects when it has no modulator yet", async () => {
    const bare: Snapshot = {
      ...tracerSnapshot,
      graph: { ...tracerSnapshot.graph, nodes: tracerSnapshot.graph.nodes.filter((n) => n.kind !== "modulator") },
      telemetry: { ...tracerSnapshot.telemetry, modulation: [] },
    };
    const server = new MockServer({ snapshot: bare });
    const client = await open(server);
    await client.request({ type: "addModulator", parent: lane.id, kind: "lfo" });
    expect(names(client)).toEqual(["Oscillator", "Filter", "LFO", "Master"]);
    // (Ids go on from what the session has, and slots from what the layout has: the meters', here, which end at 3.)
    expect(client.state.snapshot?.telemetry.modulation).toEqual([{ kind: "modOutput", node: 2, port: { type: "modOut" }, slot: 4 }]);
  });

  test("to something that is not a lane or a group is refused, and changes nothing", async () => {
    const server = new MockServer();
    const client = await open(server);
    const refusal = async (parent: number): Promise<RequestError> => (await client.request({ type: "addModulator", parent, kind: "lfo" }).catch((e: unknown) => e)) as RequestError;
    for (const parent of [filter.id, lfo.id]) {
      const failure = await refusal(parent);
      expect(failure).toBeInstanceOf(RequestError);
      expect([failure.code, failure.message]).toEqual(["invalid", "a modulator goes in a lane or in a group"]);
    }
    expect((await refusal(99)).code).toBe("notFound");
    // (The master is a node of the graph and not of the model: the controller does not find it.)
    expect((await refusal(MASTER)).code).toBe("notFound");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
    // And what was refused used up no id.
    await client.request({ type: "addModulator", parent: lane.id, kind: "lfo" });
    expect(nodesOf(client)[3]?.id).toBe(3);
  });
});

describe("what other front-ends do", () => {
  test("reaches the client as it happens", async () => {
    const server = new MockServer();
    const client = await open(server);
    server.apply(cutoff(0.25));
    await until(() => client.state.snapshot?.rev === 1, "the event");
    expect(valueOf(client, filter, 0)).toBe(0.25);
  });

  test("that the client was not told of makes it start again from a snapshot", async () => {
    const server = new MockServer();
    const client = await open(server);
    server.applyQuietly(cutoff(0.3)); // revision 1: nobody hears of it
    server.apply({ type: "setParam", node: filter.id, param: 1, value: 5 }); // revision 2: the client sees there is a gap
    await until(() => client.state.snapshot?.rev === 2 && client.state.status === "open", "the snapshot after the gap");
    expect(valueOf(client, filter, 0)).toBe(0.3);
    expect(valueOf(client, filter, 1)).toBe(5);
    expect(server.received.filter((m) => m.type === "subscribe")).toHaveLength(2);
  });
});

describe("losing the connection", () => {
  test("is made good: the client reconnects, and has the session as it is now", async () => {
    const server = new MockServer();
    const client = await open(server);
    server.drop();
    await until(() => client.state.status === "reconnecting", "the loss");
    expect(client.state.snapshot).toEqual(tracerSnapshot); // what it knew is still shown
    server.apply(cutoff(0.2)); // while it was away
    await until(() => client.state.status === "open", "the reconnection");
    expect(valueOf(client, filter, 0)).toBe(0.2);
    expect(client.state.snapshot?.rev).toBe(1);
    expect(client.state.reconnects).toBe(1);
  });

  test("is made good as often as it takes, waiting longer each time", async () => {
    const server = new MockServer();
    const delays: number[] = [];
    const client = await open(server, {
      reconnectDelay: (attempt) => {
        delays.push(attempt);
        return 2;
      },
    });
    for (let n = 1; n <= 3; n++) {
      server.drop();
      await until(() => client.state.reconnects === n && client.state.status === "open", `reconnection ${n}`);
    }
    expect(delays).toEqual([1, 1, 1]); // each loss that was made good starts the count again
  });

  test("when the server cannot be reached counts the attempts up", async () => {
    const attempts: number[] = [];
    const refusing = (): Transport => ({
      connect: (events) => {
        queueMicrotask(() => {
          events.close({ code: 1006, reason: "" });
        });
      },
      send: () => undefined,
      close: () => undefined,
    });
    const client = new Client({
      transport: refusing,
      reconnectDelay: (attempt) => {
        attempts.push(attempt);
        return 1;
      },
    });
    clients.push(client);
    client.start();
    await until(() => attempts.length >= 4, "four attempts");
    expect(attempts.slice(0, 4)).toEqual([1, 2, 3, 4]);
    expect(client.state.status).toBe("reconnecting");
  });

  test("does not happen after stop, which also leaves the server", async () => {
    const server = new MockServer();
    const client = await open(server);
    client.stop();
    expect(client.state.status).toBe("stopped");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
    await quiet();
    expect(server.connections).toBe(0);
    expect(server.received).toHaveLength(1);
  });

  test("is not made good if the client is stopped while it waits to try again", async () => {
    const server = new MockServer();
    const client = await open(server, { reconnectDelay: () => 30 });
    server.drop();
    await until(() => client.state.status === "reconnecting", "the loss");
    client.stop();
    await quiet(80);
    expect(server.connections).toBe(0);
    expect(client.state.status).toBe("stopped");
  });

  test("a client that was stopped can be started again", async () => {
    const server = new MockServer();
    const client = await open(server);
    client.stop();
    client.start();
    await until(() => client.state.status === "open", "the second session");
    expect(server.received.filter((m) => m.type === "subscribe")).toHaveLength(2);
  });
});

describe("telemetry", () => {
  /** A clock the test moves. */
  function clock(): { now: () => number; set: (seconds: number) => void } {
    let seconds = 0;
    return {
      now: () => seconds,
      set: (to) => {
        seconds = to;
      },
    };
  }
  const fast: MockOptions = { telemetryPeriod: 2 };

  test("flows into the buffers, as the layout of the snapshot says", async () => {
    const time = clock();
    const server = new MockServer({ ...fast, clock: time.now });
    time.set(0.5); // a quarter of a period of the LFO's half hertz: its sine is at the top
    const client = await open(server, { telemetry: ["modulation"] });
    await until(() => client.telemetry.frames > 0, "a frame");
    expect(client.telemetry.value("modulation", lfo.id, { type: "modOut" }, "modOutput")).toBeCloseTo(1, 5);
    // The cutoff the user set is 0.5; the LFO is at its top with a depth of 0.4 of the range.
    expect(client.telemetry.value("modulation", filter.id, { type: "param", index: 0 }, "effective")).toBeCloseTo(0.9, 5);
    // The meters were not asked for.
    expect(client.telemetry.value("meters", MASTER, { type: "audioIn" }, "peak")).toBe(0);
  });

  test("follows what the user sets", async () => {
    const time = clock();
    const server = new MockServer({ ...fast, clock: time.now });
    time.set(0); // the sine is at zero: the parameter is where it was set
    const client = await open(server, { telemetry: ["modulation"] });
    await client.request(cutoff(0.25));
    await until(() => client.telemetry.value("modulation", filter.id, { type: "param", index: 0 }, "effective") === 0.25, "the effective value");
  });

  test("stops when the client asks for none, and starts again when it asks again", async () => {
    const server = new MockServer(fast);
    const client = await open(server, { telemetry: ["meters"] });
    await until(() => client.telemetry.frames > 3, "frames");
    client.setTelemetry([]);
    await quiet(20);
    const frames = client.telemetry.frames;
    await quiet(30);
    expect(client.telemetry.frames).toBe(frames);
    client.setTelemetry(["meters"]);
    await until(() => client.telemetry.frames > frames, "frames again");
  });

  test("is asked for again after a reconnection", async () => {
    const server = new MockServer(fast);
    const client = await open(server, { telemetry: ["meters"] });
    await until(() => client.telemetry.frames > 0, "a frame");
    server.drop();
    await until(() => client.state.reconnects === 1 && client.state.status === "open", "the reconnection");
    const frames = client.telemetry.frames;
    await until(() => client.telemetry.frames > frames + 1, "frames after it");
  });

  test("follows a new layout the server announces, and ignores frames of the old", async () => {
    const server = new MockServer(fast);
    const client = await open(server, { telemetry: ["meters"] });
    await until(() => client.telemetry.frames > 0, "a frame");
    const smaller: TelemetryLayout = { ...tracerSnapshot.telemetry, rev: 77, meters: tracerSnapshot.telemetry.meters.slice(0, 2) };
    server.announceLayout(smaller);
    await until(() => client.telemetry.values("meters")?.length === 2, "the new layout");
    expect(client.state.snapshot?.telemetry).toEqual(smaller);
    const frames = client.telemetry.frames;
    await until(() => client.telemetry.frames > frames, "a frame of the new layout"); // the server makes them of it from now on
  });

  test("shows a note in the master's meter while it is held, and lets it fall after", async () => {
    const time = clock();
    const server = new MockServer({ ...fast, clock: time.now });
    const client = await open(server, { telemetry: ["meters"] });
    const peak = (): number | undefined => client.telemetry.value("meters", MASTER, { type: "audioIn" }, "peak");
    client.noteOn(60, 100);
    await until(() => (peak() ?? 0) > 0.3, "the meter to rise");
    expect([...server.heldNotes]).toEqual([60]);
    client.noteOff(60);
    await until(() => server.heldNotes.size === 0, "the note to end");
    time.set(2); // two seconds on, the peak has fallen by 80 dB
    await until(() => (peak() ?? 1) < 0.001, "the meter to fall");
  });

  test("notes end with the connection that held them", async () => {
    const server = new MockServer();
    const client = await open(server);
    client.noteOn(64);
    await until(() => server.heldNotes.size === 1, "the note");
    expect(server.received).toContainEqual({ type: "noteOn", note: 64, velocity: 100 });
    server.drop();
    expect(server.heldNotes.size).toBe(0);
  });

  /** The tracer with its LFO's waveform changed. */
  function withWaveform(waveform: "sine" | "triangle" | "saw" | "square"): Snapshot {
    const nodes = tracerSnapshot.graph.nodes.map((node) =>
      node.kind === "modulator" && node.source.type === "lfo" ? { ...node, source: { ...node.source, waveform } } : node,
    );
    return { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } };
  }

  test("makes the LFO's output as its waveform says", async () => {
    // The LFO runs at half a hertz: a phase of 0.05, 0.25, 0.5 and 0.75 is at 0.1, 0.5, 1 and 1.5 seconds.
    const expected = {
      sine: [Math.sin(2 * Math.PI * 0.05), 1, 0, -1],
      triangle: [0.2, 1, 0, -1],
      saw: [-0.9, -0.5, 0, 0.5],
      square: [1, 1, -1, -1],
    } as const;
    for (const [waveform, outputs] of Object.entries(expected) as [keyof typeof expected, readonly number[]][]) {
      const time = clock();
      const server = new MockServer({ ...fast, clock: time.now, snapshot: withWaveform(waveform) });
      const client = await open(server, { telemetry: ["modulation"] });
      for (const [i, seconds] of [0.1, 0.5, 1, 1.5].entries()) {
        time.set(seconds);
        const frames = client.telemetry.frames;
        await until(() => client.telemetry.frames > frames + 1, "a frame");
        expect(client.telemetry.value("modulation", lfo.id, { type: "modOut" }, "modOutput"), `${waveform} at ${seconds}`).toBeCloseTo(outputs[i] ?? NaN, 5);
      }
      client.stop();
    }
  });

  test("lets an envelope's peak be the value the user set", async () => {
    // An envelope aimed at the cutoff the user set to 0.5 with a depth of 0.4: the value is
    // value - depth * (1 - output) * (value - min), so the user's value is the envelope's peak.
    const nodes = tracerSnapshot.graph.nodes.map((node) =>
      node.kind === "modulator" ? { ...node, source: { type: "envelope" as const, attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } } : node,
    );
    const snapshot: Snapshot = { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } };
    const server = new MockServer({ ...fast, snapshot });
    const client = await open(server, { telemetry: ["modulation"] });
    const effective = (): number | undefined => client.telemetry.value("modulation", filter.id, { type: "param", index: 0 }, "effective");
    // No note: the envelope is closed and its output is 0, which takes the cutoff down by 0.4 of the way to its minimum.
    await until(() => Math.abs((effective() ?? 0) - (0.5 - 0.4 * 1 * 0.5)) < 1e-6, "the cutoff at rest");
    // A note holds the envelope at its sustain of 0.7.
    client.noteOn(60);
    await until(() => Math.abs((effective() ?? 0) - (0.5 - 0.4 * 0.3 * 0.5)) < 1e-6, "the cutoff with the envelope open");
  });
});

describe("what the server says", () => {
  /** A server that says what the script says, the moment the client connects. */
  function scripted(messages: (ServerMessage | string)[]): Client {
    const transport = (): Transport => ({
      connect: (events) => {
        queueMicrotask(() => {
          for (const message of messages) events.text(typeof message === "string" ? message : JSON.stringify(message));
        });
      },
      send: () => undefined,
      close: () => undefined,
    });
    const client = new Client({ transport });
    clients.push(client);
    client.start();
    return client;
  }

  test("through a transport the client has left is not listened to", async () => {
    // This transport goes on talking after it has been closed, as a socket may for a moment.
    const server = new MockServer();
    const client = scripted([server.hello(), { type: "snapshot", snapshot: tracerSnapshot }]);
    client.stop();
    await quiet();
    expect(client.state.status).toBe("stopped");
    expect(client.state.snapshot).toBeNull();
    expect(client.state.hello).toBeNull();
  });

  test("that is not JSON stops the client, which says so", async () => {
    const client = scripted(["this is not json"]);
    await until(() => client.state.status === "stopped", "the refusal");
    expect(client.state.problem).toMatch(/not a message/);
  });

  test("of a kind this client does not know is ignored", async () => {
    const server = new MockServer();
    const client = scripted([server.hello(), '{"type":"somethingNew","x":1}', { type: "snapshot", snapshot: tracerSnapshot }]);
    await until(() => client.state.status === "open", "the session");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("closing, with a reason that is not final, is followed by another try", async () => {
    let attempts = 0;
    const transport = (): Transport => ({
      connect: (events) => {
        attempts += 1;
        queueMicrotask(() => {
          events.text(JSON.stringify({ type: "closing", error: { code: "overloaded", message: "you fell behind" } }));
          events.close({ code: 1013, reason: "overloaded" });
        });
      },
      send: () => undefined,
      close: () => undefined,
    });
    const client = new Client({ transport, reconnectDelay: () => 1 });
    clients.push(client);
    client.start();
    await until(() => attempts >= 3, "more attempts");
    expect(client.state.status).not.toBe("stopped");
  });
});
