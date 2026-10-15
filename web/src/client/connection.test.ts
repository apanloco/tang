import { afterEach, describe, expect, test } from "vitest";

import { MODULATOR_FIELD_DEFAULTS, MODULATOR_FIELD_RANGES, NODE_UI_LIMIT, SCOPE_FRAME_LENGTH } from "../protocol";
import type { CatalogEntry, ClientMessage, Edge, Intent, Node, Port, ServerMessage, Snapshot, TelemetryLayout, Waveform } from "../protocol";
import { Client, RequestError, SLOW_AFTER } from "./connection";
import type { ClientOptions, Transport, TransportFactory } from "./connection";
import { builtinCatalog, builtinEffects, builtinInstruments, builtinPresets, fieldsFixture, MASTER, nodeNamed, reachFixture, tracerSnapshot } from "./fixtures";
import { MockServer } from "./mock";
import type { MockOptions } from "./mock";
import { noise } from "./noise";
import { fractionOf, valueAt } from "./travel";
import { Spectrum } from "./fft";
import { decodeFrame } from "./telemetry";

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

/** The tracer with a second LFO that drives the same cutoff: two cables, one value, one slot for it in the layout. */
function withTwoCablesToTheCutoff(): Snapshot {
  const second: Node = { id: 5, parent: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1 } };
  const cable = { kind: "modulation", id: 1, amount: 0.5, from: { node: 5, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 0 } } } as const;
  const [output, ...rest] = tracerSnapshot.telemetry.modulation;
  return {
    ...tracerSnapshot,
    graph: { nodes: [...tracerSnapshot.graph.nodes, second], edges: [...tracerSnapshot.graph.edges, cable] },
    telemetry: { ...tracerSnapshot.telemetry, modulation: [...(output ? [output] : []), { kind: "modOutput", node: 5, port: { type: "modOut" }, slot: 6 }, ...rest] },
  };
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
    // a waveform is an index, rounded: 2.4 is the saw, 4.4 the random one, and there is no waveform after the smooth one
    await field("waveform", 2.4);
    expect(source()).toMatchObject({ waveform: "saw" });
    await field("waveform", 4.4);
    expect(source()).toMatchObject({ waveform: "random" });
    await field("waveform", 9);
    expect(source()).toMatchObject({ waveform: "smooth" });
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
      [{ type: "setModulatorField", node: lfo.id, field: "attack", value: 1 }, "invalid", "Attack does not apply to that modulator"],
      [{ type: "setEdgeDepth", edge: 99, depth: 0.5 }, "notFound", "there is no cable 99"],
    ] as const;
    for (const [intent, code, message] of refused) {
      const failure = await fails(intent);
      expect([failure.code, failure.message], JSON.stringify(intent)).toEqual([code, message]);
    }
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("for a parameter or a gain that does not fit is refused in the controller's own words (its names for a gain and a setting, and `inf` for a number beyond 32 bits), the value judged before the node it is for", () => {
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    const hollow: Snapshot = {
      ...tracerSnapshot,
      graph: { ...tracerSnapshot.graph, nodes: tracerSnapshot.graph.nodes.map((n) => (n.id === oscillator.id && n.kind === "instrument" ? { ...n, plugin: null } : n)) },
    };
    const refusal = (server: MockServer, intent: Intent): string => {
      try {
        server.apply(intent);
      } catch (thrown) {
        return thrown instanceof Error ? thrown.message : String(thrown);
      }
      return "done";
    };
    const refused: [string, MockServer, Intent, string][] = [
      ["a parameter the plugin has not", new MockServer(), { type: "setParam", node: filter.id, param: 99, value: 1 }, "the plugin has no parameter 99"],
      ["a modulator, which has no plugin", new MockServer(), { type: "setParam", node: lfo.id, param: 0, value: 1 }, "that node has no plugin"],
      ["a lane with no plugin in it", new MockServer({ snapshot: hollow }), { type: "setParam", node: oscillator.id, param: 0, value: 1 }, "that instrument has no plugin"],
      ["a mix for an instrument", new MockServer(), { type: "setGain", node: oscillator.id, gain: "mix", value: 0.5 }, "Mix does not apply to that node"],
      ["a volume for an effect", new MockServer(), { type: "setGain", node: filter.id, gain: "volume", value: 1 }, "Volume does not apply to that node"],
      ["a group's volume for an instrument", new MockServer(), { type: "setGain", node: oscillator.id, gain: "groupVolume", value: 1 }, "GroupVolume does not apply to that node"],
      ["a group's pan for an effect", new MockServer(), { type: "setGain", node: filter.id, gain: "groupPan", value: 0 }, "GroupPan does not apply to that node"],
      ["a number beyond 32 bits for a parameter", new MockServer(), { type: "setParam", node: filter.id, param: 0, value: 1e39 }, "inf is not a value for cutoff"],
      ["a number beyond 32 bits, the other way, for a gain", new MockServer(), { type: "setGain", node: oscillator.id, gain: "volume", value: -1e39 }, "-inf is not a value for Volume"],
      ["a number beyond 32 bits for a gain the node has not", new MockServer(), { type: "setGain", node: oscillator.id, gain: "mix", value: 1e39 }, "inf is not a value for Mix"],
      ["a number beyond 32 bits for a setting", new MockServer(), { type: "setModulatorField", node: lfo.id, field: "rate", value: 1e39 }, "inf is not a value for Rate"],
      ["a number beyond 32 bits for the depth of a cable", new MockServer(), { type: "setEdgeDepth", edge: 0, depth: -1e39 }, "-inf is not a value for the depth of a cable"],
    ];
    for (const [what, server, intent, message] of refused) expect(refusal(server, intent), what).toBe(message);
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
    }).toThrow("NaN is not a value for Rate");
    expect(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: Number.POSITIVE_INFINITY });
    }).toThrow("inf is not a value for the depth of a cable");
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

  test("keeps the slot of the value it drove while another cable still drives it", async () => {
    const server = new MockServer({ snapshot: withTwoCablesToTheCutoff() });
    const client = await open(server);
    const effective = () => client.state.snapshot?.telemetry.modulation.filter((entry) => entry.kind === "effective") ?? [];
    expect(effective()).toHaveLength(1);
    await client.request({ type: "removeEdge", edge: 0 });
    expect(cables(client.state.snapshot).map((e) => e.id)).toEqual([1]);
    expect(effective()).toHaveLength(1);
    expect(client.state.snapshot?.telemetry, "the layout is as it was").toEqual(withTwoCablesToTheCutoff().telemetry);
    await client.request({ type: "removeEdge", edge: 1 });
    expect(effective()).toEqual([]);
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

describe("taking a modulator out", () => {
  const cables = (snapshot: Snapshot | null | undefined) => snapshot?.graph.edges.filter((e) => e.kind === "modulation") ?? [];
  const names = (snapshot: Snapshot | null | undefined) => snapshot?.graph.nodes.map((n) => `${n.name}#${String(n.id)}`) ?? [];

  test("takes the node out, and its cables, and the slots of what it put out and drove; the client is told by a snapshot, the revision after the intent's", async () => {
    const server = new MockServer();
    const client = await open(server);
    expect(await client.request({ type: "removeNode", node: lfo.id })).toBe(1);
    await until(() => client.state.snapshot?.rev === 2, "the snapshot");
    expect(names(client.state.snapshot)).toEqual(["Oscillator#0", "Filter#1", "Master#4294967295"]);
    expect(client.state.snapshot?.graph.edges).toEqual(tracerSnapshot.graph.edges.filter((e) => e.kind !== "modulation"));
    expect(client.state.snapshot?.telemetry.modulation).toEqual([]);
    expect(client.state.snapshot?.telemetry.rev).not.toBe(tracerSnapshot.telemetry.rev);
    expect(client.state.snapshot?.telemetry.meters).toEqual(tracerSnapshot.telemetry.meters);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(server.snapshot).toEqual(client.state.snapshot);
  });

  test("takes the cables of the others that drive its settings, and what drives the depth of any of those", async () => {
    const server = new MockServer({ snapshot: withCablesThatDriveCables() });
    const client = await open(server);
    // The first LFO's depth is driven by the second (5), whose own depth is driven by the third (6).
    await client.request({ type: "removeNode", node: lfo.id });
    expect(cables(client.state.snapshot)).toEqual([]);
    // (The fixture lists its two modulators after the master; what is asked is that they stay, as they were.)
    expect(names(client.state.snapshot)).toEqual(["Oscillator#0", "Filter#1", "Master#4294967295", "LFO#5", "LFO#6"]);
  });

  test("takes the cable of another that drives its rate, which ends at it", async () => {
    // A second LFO (5) whose cable drives the rate of the first (2): the first has no other cable than the cutoff's.
    const second: Node = { id: 5, parent: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "sine", rate: 1 } };
    const cable = { kind: "modulation", id: 1, amount: 0.5, from: { node: 5, port: { type: "modOut" } }, to: { node: lfo.id, port: { type: "rate" } } } as const;
    const snapshot = { ...tracerSnapshot, graph: { nodes: [...tracerSnapshot.graph.nodes, second], edges: [...tracerSnapshot.graph.edges, cable] } };
    const server = new MockServer({ snapshot });
    const client = await open(server);
    await client.request({ type: "removeNode", node: lfo.id });
    expect(cables(client.state.snapshot)).toEqual([]);
    expect(names(client.state.snapshot)).toEqual(["Oscillator#0", "Filter#1", "Master#4294967295", "LFO#5"]);

    // Taking out the one that drives it takes its own cable and leaves the other's.
    const other = await open(new MockServer({ snapshot }));
    await other.request({ type: "removeNode", node: 5 });
    expect(cables(other.state.snapshot).map((e) => e.id)).toEqual([0]);
  });

  test("takes only what is its own and what drives it: the others' other cables stay", async () => {
    const server = new MockServer({ snapshot: withCablesThatDriveCables() });
    const client = await open(server);
    await client.request({ type: "removeNode", node: 5 });
    // Its own cable (1) goes, and the one that drove its depth (2): the cutoff's (0) stays.
    expect(cables(client.state.snapshot).map((e) => e.id)).toEqual([0]);
    await client.request({ type: "removeNode", node: 6 });
    expect(cables(client.state.snapshot).map((e) => e.id)).toEqual([0]);
  });

  test("keeps the slot of a value that another modulator still drives", async () => {
    const server = new MockServer({ snapshot: withTwoCablesToTheCutoff() });
    const client = await open(server);
    const layout = () => client.state.snapshot?.telemetry.modulation ?? [];
    await client.request({ type: "removeNode", node: lfo.id });
    expect(layout()).toEqual(withTwoCablesToTheCutoff().telemetry.modulation.filter((entry) => entry.node !== lfo.id));
    expect(layout().map((entry) => entry.kind)).toEqual(["modOutput", "effective"]);
    await client.request({ type: "removeNode", node: 5 });
    expect(layout()).toEqual([]);
  });

  test("is refused for the last lane of a session, and for what is not there, and changes nothing", async () => {
    const server = new MockServer();
    const client = await open(server);
    const refusal = async (node: number): Promise<RequestError> => (await client.request({ type: "removeNode", node }).catch((e: unknown) => e)) as RequestError;
    // (A session keeps a lane: the tracer has one.)
    const failure = await refusal(nodeNamed(tracerSnapshot, "Oscillator").id);
    expect(failure).toBeInstanceOf(RequestError);
    expect([failure.code, failure.message]).toEqual(["invalid", "a session keeps its last lane"]);
    expect((await refusal(99)).code).toBe("notFound");
    // (The master is a node of the graph and not of the model: the controller does not find it.)
    expect((await refusal(MASTER)).code).toBe("notFound");
    expect(client.state.snapshot).toEqual(tracerSnapshot);
    await expect(client.request({ type: "removeNode", node: lfo.id })).resolves.toBe(1);
    await expect(client.request({ type: "removeNode", node: lfo.id })).rejects.toMatchObject({ code: "notFound" });
  });

  test("does not give its id to the next node", async () => {
    const server = new MockServer();
    const client = await open(server);
    const parent = nodeNamed(tracerSnapshot, "Oscillator").id;
    await client.request({ type: "addModulator", parent, kind: "lfo" });
    await client.request({ type: "removeNode", node: 3 });
    await client.request({ type: "addModulator", parent, kind: "lfo" });
    expect(names(client.state.snapshot)).toEqual(["Oscillator#0", "Filter#1", "LFO#2", "LFO#4", "Master#4294967295"]);
  });
});

describe("taking an effect out", () => {
  const audioPath = (snapshot: Snapshot | null) => (snapshot?.graph.edges ?? []).filter((e) => e.kind !== "modulation").map((e) => `${e.kind} ${String(e.from.node)}→${String(e.to.node)}`);
  const strip = (snapshot: Snapshot | null) => (snapshot?.graph.nodes ?? []).map((n) => `${String(n.id)} ${n.name}`);
  const modulation = (snapshot: Snapshot | null) => (snapshot?.graph.edges ?? []).filter((e) => e.kind === "modulation");
  const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
  const reach = reachFixture.snapshot;

  test("takes the tracer's filter out with the cable that drives it, closes the audio path up, and leaves the LFO", async () => {
    const server = new MockServer();
    const client = await open(server);
    expect(await client.request({ type: "removeNode", node: filter.id })).toBe(1);
    await until(() => client.state.snapshot?.rev === 2, "the snapshot");
    const snapshot = client.state.snapshot;
    expect(strip(snapshot)).toEqual(["0 Oscillator", "2 LFO", `${String(MASTER)} Master`]);
    expect(audioPath(snapshot)).toEqual([`audio 0→${String(MASTER)}`]);
    expect(modulation(snapshot)).toEqual([]);
    // The value the cable drove has no slot in the layout any more; the LFO's output keeps its own, and the meters are as they were.
    expect(snapshot?.telemetry.modulation.map((entry) => entry.kind)).toEqual(["modOutput"]);
    expect(snapshot?.telemetry.rev).not.toBe(tracerSnapshot.telemetry.rev);
    expect(snapshot?.telemetry.meters).toEqual(tracerSnapshot.telemetry.meters);
    expect(snapshot?.meta.dirty).toBe(true);
    expect(server.snapshot).toEqual(snapshot);
  });

  test("closes the audio path up round the one in the middle of a chain, and the last one of a member lane's goes into the group", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request({ type: "addEffect", parent: oscillator.id, plugin: "builtin:reverb" }); // 3
    await client.request({ type: "addEffect", parent: oscillator.id, plugin: "builtin:filter" }); // 4
    expect(audioPath(client.state.snapshot)).toEqual(["audio 0→1", "audio 1→3", "audio 3→4", `audio 4→${String(MASTER)}`]);
    await client.request({ type: "removeNode", node: 3 });
    expect(audioPath(client.state.snapshot)).toEqual(["audio 0→1", "audio 1→4", `audio 4→${String(MASTER)}`]);

    // Lane 4 of the scene with groups is a member of group 0, and has a filter (5): the link into the group leaves the lane now.
    const grouped = await open(new MockServer({ snapshot: reach }));
    expect(audioPath(grouped.state.snapshot)).toContain("member 5→0");
    await grouped.request({ type: "removeNode", node: 5 });
    expect(audioPath(grouped.state.snapshot)).toContain("member 4→0");
    expect(audioPath(grouped.state.snapshot).filter((link) => link.includes("→5") || link.includes("5→"))).toEqual([]);
  });

  test("takes a bus effect out, and the group goes into the master", async () => {
    const client = await open(new MockServer({ snapshot: reach }));
    expect(audioPath(client.state.snapshot).slice(0, 2)).toEqual(["audio 0→1", `audio 1→${String(MASTER)}`]);
    await client.request({ type: "removeNode", node: 1 });
    expect(audioPath(client.state.snapshot).slice(0, 1)).toEqual([`audio 0→${String(MASTER)}`]);
    expect(strip(client.state.snapshot).slice(0, 3)).toEqual(["0 Group 1", "15 LFO", "2 Group 2"]);
  });

  test("takes the cables to it out, whoever's they are, and the ones that drive the depths of those", async () => {
    // The scene's modulators 15 (group 0's), 6 (lane 4's own) reach the filter 5 of lane 4; the cables are the fixture's own.
    const withCables: Snapshot = {
      ...reach,
      graph: {
        ...reach.graph,
        edges: [
          ...reach.graph.edges,
          { kind: "modulation", id: 100, amount: 0.5, from: { node: 6, port: { type: "modOut" } }, to: { node: 5, port: { type: "param", index: 0 } } },
          { kind: "modulation", id: 101, amount: 0.4, from: { node: 15, port: { type: "modOut" } }, to: { node: 5, port: { type: "param", index: 1 } } },
          { kind: "modulation", id: 102, amount: 0.3, from: { node: 15, port: { type: "modOut" } }, to: { node: 4, port: { type: "param", index: 1 } } },
          { kind: "modulation", id: 103, amount: 0.2, from: { node: 6, port: { type: "modOut" } }, to: { node: 15, port: { type: "depth", edge: 100 } } },
        ],
      },
    };
    const before = modulation(reach).length;
    const client = await open(new MockServer({ snapshot: withCables }));
    await client.request({ type: "removeNode", node: 5 });
    // The two cables to the filter go, and the one that drove the depth of the first; the group's cable to the lane's detune stays.
    const left = modulation(client.state.snapshot).map((e) => e.id);
    expect(left).toHaveLength(before + 1);
    expect(left).toContain(102);
    expect(left).not.toContain(100);
    expect(left).not.toContain(101);
    expect(left).not.toContain(103);
  });

  test("leaves the session as it was when an effect that was added is taken out", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request({ type: "addEffect", parent: oscillator.id, plugin: "builtin:reverb" });
    await client.request({ type: "removeNode", node: 3 });
    expect(client.state.snapshot?.graph).toEqual(tracerSnapshot.graph);
    expect(client.state.snapshot?.telemetry).toEqual(tracerSnapshot.telemetry);
  });

  test("is told to the other clients as a snapshot, like every change of structure", async () => {
    const server = new MockServer();
    const first = await open(server);
    const second = await open(server);
    await first.request({ type: "removeNode", node: filter.id });
    await until(() => (second.state.snapshot?.graph.nodes.length ?? 0) === 3, "the second client's snapshot");
    expect(second.state.snapshot).toEqual(first.state.snapshot);
  });

  test("does not give its id to the next node", async () => {
    const client = await open(new MockServer());
    await client.request({ type: "addEffect", parent: oscillator.id, plugin: "builtin:reverb" }); // 3
    await client.request({ type: "removeNode", node: 3 });
    await client.request({ type: "addEffect", parent: oscillator.id, plugin: "builtin:reverb" });
    expect((client.state.snapshot?.graph.nodes ?? []).filter((n) => n.kind === "effect").map((n) => n.id)).toEqual([1, 4]);
  });
});

describe("saving the session", () => {
  const oscillator = nodeNamed(tracerSnapshot, "Oscillator");

  test("is acknowledged as a revision, and the client is told that nothing is unsaved, by an event", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 0.5 });
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(await client.request({ type: "save" })).toBe(2);
    expect(client.state.snapshot?.meta).toEqual({ ...tracerSnapshot.meta, dirty: false });
    expect(client.state.snapshot?.rev).toBe(2);
    expect(server.snapshot).toEqual(client.state.snapshot);
    // The next change is unsaved again.
    await client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 0.25 });
    expect(client.state.snapshot?.meta.dirty).toBe(true);
  });

  test("of a session that has no file is refused as an io error, and is no revision", async () => {
    const server = new MockServer({ snapshot: { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, file: null } } });
    const client = await open(server);
    const failure = (await client.request({ type: "save" }).catch((e: unknown) => e)) as RequestError;
    expect(failure).toBeInstanceOf(RequestError);
    expect([failure.code, failure.message]).toEqual(["io", "No session path — cannot save"]);
    expect(client.state.snapshot?.rev).toBe(0);
  });
});

describe("drawing a cable", () => {
  const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
  /** The plugin's own index of a parameter of a node of a snapshot. */
  const indexOf = (snapshot: Snapshot, id: number, name: string): number => {
    const node = snapshot.graph.nodes.find((n) => n.id === id);
    const param = (node?.kind === "instrument" || node?.kind === "effect" ? node.plugin?.params : undefined)?.find((p) => p.name === name);
    if (!param) throw new Error(`no ${name} in node ${String(id)}`);
    return param.index;
  };
  const detune = indexOf(tracerSnapshot, oscillator.id, "detune");
  const resonance = indexOf(tracerSnapshot, filter.id, "resonance");
  const modulations = (snapshot: Snapshot | null | undefined) => snapshot?.graph.edges.filter((e) => e.kind === "modulation") ?? [];
  const draw = (modulator: number, node: number, index: number): Intent => ({ type: "addModEdge", modulator, to: { node, port: { type: "param", index } } });
  const cable = (id: number, from: number, node: number, index: number, amount = 0.5) =>
    ({ kind: "modulation", id, amount, from: { node: from, port: { type: "modOut" } }, to: { node, port: { type: "param", index } } }) as const;

  test("joins the modulator to the parameter at half depth; the client is told by a snapshot, the revision after the intent's", async () => {
    const server = new MockServer();
    const client = await open(server);
    expect(await client.request(draw(lfo.id, oscillator.id, detune))).toBe(1);
    await until(() => client.state.snapshot?.rev === 2, "the snapshot");
    // After the cable the modulator already had, with an id nobody had.
    expect(client.state.snapshot?.graph.edges).toEqual([...tracerSnapshot.graph.edges, cable(1, lfo.id, oscillator.id, detune)]);
    expect(client.state.snapshot?.graph.nodes).toEqual(tracerSnapshot.graph.nodes);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(server.snapshot).toEqual(client.state.snapshot);
    // Another one to another parameter is the next.
    await client.request(draw(lfo.id, filter.id, resonance));
    expect(modulations(client.state.snapshot).map((e) => e.id)).toEqual([0, 1, 2]);
  });

  test("gives the value it drives a slot in the layout, with a new fingerprint, unless another cable drives that value already", async () => {
    const server = new MockServer();
    const client = await open(server);
    const effective = () => client.state.snapshot?.telemetry.modulation.filter((entry) => entry.kind === "effective") ?? [];
    await client.request(draw(lfo.id, oscillator.id, detune));
    expect(effective()).toEqual([
      ...tracerSnapshot.telemetry.modulation.filter((entry) => entry.kind === "effective"),
      { kind: "effective", node: oscillator.id, port: { type: "param", index: detune }, slot: 6 },
    ]);
    expect(client.state.snapshot?.telemetry.rev).not.toBe(tracerSnapshot.telemetry.rev);
    expect(client.state.snapshot?.telemetry.meters).toEqual(tracerSnapshot.telemetry.meters);
    // A second modulator on the cutoff the first one drives: one value, one slot in the layout (as the controller lists it).
    await client.request({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
    const layout = client.state.snapshot?.telemetry;
    const added = client.state.snapshot?.graph.nodes.find((n) => n.name === "ADSR");
    await client.request(draw(added?.id ?? -1, filter.id, 0));
    expect(effective()).toHaveLength(2);
    expect(client.state.snapshot?.telemetry.modulation).toEqual(layout?.modulation);
    expect(client.state.snapshot?.telemetry.rev).toBe(layout?.rev);
  });

  test("does not give a cable's id to the next one, once it is taken out", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request(draw(lfo.id, oscillator.id, detune));
    await client.request({ type: "removeEdge", edge: 1 });
    await client.request(draw(lfo.id, oscillator.id, detune));
    expect(modulations(client.state.snapshot).map((e) => e.id)).toEqual([0, 2]);
  });

  describe("in a session of groups and lanes", () => {
    const { snapshot } = reachFixture;
    const [groupLfo, lane0Lfo, lane3Control] = [15, 6, 13];
    const [osc0, osc1, osc3, osc2] = [4, 7, 11, 9];
    const detune0 = indexOf(snapshot, osc0, "detune");
    const refused = async (client: Client, intent: Intent): Promise<RequestError> => (await client.request(intent).catch((e: unknown) => e)) as RequestError;

    test("lists the cables of the groups' modulators before the lanes', as the controller does, whichever was drawn first", async () => {
      const server = new MockServer({ snapshot });
      const client = await open(server);
      await client.request(draw(lane0Lfo, osc0, detune0));
      await client.request(draw(groupLfo, osc0, detune0));
      await client.request(draw(lane3Control, osc3, indexOf(snapshot, osc3, "volume")));
      expect(modulations(client.state.snapshot).map((e) => e.from.node)).toEqual([groupLfo, lane0Lfo, lane3Control]);
      // And ahead of them, the audio path as it was.
      const edges = client.state.snapshot?.graph.edges ?? [];
      expect(edges.slice(0, edges.length - 3)).toEqual(snapshot.graph.edges);
    });

    test("names the value a lane's modulator drives before the value a group's drives, whichever cable was drawn first (the controller's layout lists the lanes' targets first)", async () => {
      const server = new MockServer({ snapshot });
      const client = await open(server);
      const named = (): string[] =>
        (client.state.snapshot?.telemetry.modulation ?? []).filter((e) => e.kind === "effective").map((e) => `${String(e.node)}:${e.port.type === "param" ? String(e.port.index) : e.port.type}`);
      const before = named();
      const [laneValue, groupValue] = [`${String(osc0)}:${String(detune0)}`, `${String(osc1)}:0`];
      expect(before).not.toContain(laneValue);
      expect(before).not.toContain(groupValue);
      // The group's modulator draws first, to a parameter of lane 1's oscillator; then lane 0's, to one of lane 0's.
      await client.request(draw(groupLfo, osc1, 0));
      await client.request(draw(lane0Lfo, osc0, detune0));
      const after = named();
      expect(after).toHaveLength(before.length + 2);
      expect(after.indexOf(laneValue)).toBeLessThan(after.indexOf(groupValue));
    });

    test("is for a parameter of a plugin the modulator reaches, which a client can tell from the graph", async () => {
      const server = new MockServer({ snapshot });
      const client = await open(server);
      // A group's modulator reaches its members' chains and its bus effects: lane 1's oscillator, lane 0's filter, the bus filter.
      for (const [from, to] of [[groupLfo, osc1], [groupLfo, 5], [groupLfo, 1]] as const) {
        await client.request(draw(from, to, 0));
      }
      expect(modulations(client.state.snapshot)).toHaveLength(3);
    });

    test("is refused when it starts at what is not a modulator, or ends at what is not a parameter, and nothing changes", async () => {
      const server = new MockServer({ snapshot });
      const client = await open(server);
      const failure = async (intent: Intent) => {
        const error = await refused(client, intent);
        expect(error).toBeInstanceOf(RequestError);
        return [error.code, error.message];
      };
      expect(await failure(draw(osc0, osc0, detune0))).toEqual(["invalid", "a cable starts at a modulator"]);
      expect(await failure(draw(0, osc0, detune0))).toEqual(["invalid", "a cable starts at a modulator"]);
      expect((await failure(draw(99_999, osc0, detune0)))[0]).toBe("notFound");
      expect((await failure(draw(MASTER, osc0, detune0)))[0]).toBe("notFound");
      for (const port of [{ type: "modOut" }, { type: "audioIn" }, { type: "audioOut" }] as const) {
        expect(await failure({ type: "addModEdge", modulator: lane0Lfo, to: { node: osc0, port } })).toEqual([
          "invalid",
          "a cable can only end at a plugin's parameter, or at a modulator's rate, stage or depth",
        ]);
      }
      // (The settings of a modulator are not a plugin's: a plugin has none to take.)
      for (const port of [{ type: "rate" }, { type: "attack" }, { type: "depth", edge: 0 }] as const) {
        expect(await failure({ type: "addModEdge", modulator: lane0Lfo, to: { node: osc0, port } })).toEqual(["invalid", "a cable ends at the settings of a modulator, and that is not one"]);
      }
      expect((await failure(draw(lane0Lfo, 99_999, 0)))[0]).toBe("notFound");
      expect((await failure(draw(lane0Lfo, MASTER, 0)))[0]).toBe("notFound");
      expect(await failure(draw(lane0Lfo, 0, 0))).toEqual(["invalid", "that node has no plugin"]); // a group
      expect(await failure(draw(lane0Lfo, groupLfo, 0))).toEqual(["invalid", "that node has no plugin"]); // a modulator
      expect(await failure(draw(lane0Lfo, 14, 0))).toEqual(["invalid", "that instrument has no plugin"]); // a lane with nothing in it
      expect(await failure(draw(lane0Lfo, osc0, 999))).toEqual(["invalid", "the plugin has no parameter 999"]);
      expect(client.state.snapshot).toEqual(snapshot);
      // And no cable id was used up.
      await client.request(draw(lane0Lfo, osc0, detune0));
      expect(modulations(client.state.snapshot).map((e) => e.id)).toEqual([0]);
    });

    test("is refused for a plugin out of the modulator's reach, as the controller refuses it", async () => {
      const server = new MockServer({ snapshot });
      const client = await open(server);
      const inLane = ["invalid", "a modulator of this lane cannot reach that plugin"];
      const inGroup = ["invalid", "a modulator of this group cannot reach that plugin"];
      const failure = async (intent: Intent) => {
        const error = await refused(client, intent);
        return [error.code, error.message];
      };
      expect(await failure(draw(lane0Lfo, osc1, 0))).toEqual(inLane); // the other lane of the same group
      expect(await failure(draw(lane0Lfo, osc3, 0))).toEqual(inLane); // a lane of its own
      expect(await failure(draw(lane0Lfo, 1, 0))).toEqual(inLane); // the bus filter of its group
      expect(await failure(draw(groupLfo, osc3, 0))).toEqual(inGroup); // a lane outside the group
      expect(await failure(draw(groupLfo, osc2, 0))).toEqual(inGroup); // a lane of another group
      expect(await failure(draw(groupLfo, 3, 0))).toEqual(inGroup); // the bus of another group
      expect(client.state.snapshot).toEqual(snapshot);
    });

    test("is refused when the modulator drives the parameter already, and allowed for another modulator", async () => {
      const server = new MockServer({ snapshot });
      const client = await open(server);
      await client.request(draw(lane0Lfo, osc0, detune0));
      const again = await refused(client, draw(lane0Lfo, osc0, detune0));
      expect([again.code, again.message]).toEqual(["invalid", "that modulator drives that parameter already"]);
      await client.request(draw(groupLfo, osc0, detune0));
      expect(modulations(client.state.snapshot)).toHaveLength(2);
    });
  });
});

describe("drawing a cable to a setting of another modulator", () => {
  const { snapshot, fields } = fieldsFixture;
  // Group 1's rack: an LFO, an envelope and a MIDI control; its first member (3) has an LFO, an envelope and a MIDI control of its own,
  // the second member (7) an LFO; a lane in no group (9) has an LFO, and a lane of the second group (11) has an LFO.
  const [groupLfo, groupEnvelope, groupControl] = [13, 14, 15];
  const [laneLfo, laneEnvelope, laneControl, member2Lfo, lane9Lfo, lane11Lfo] = [4, 5, 6, 8, 10, 12];
  const modulators = snapshot.graph.nodes.filter((n) => n.kind === "modulator");
  type Cable = Edge & { kind: "modulation"; id: number };
  const modulations = (s: Snapshot | null | undefined): Cable[] => (s?.graph.edges ?? []).filter((e): e is Cable => e.kind === "modulation");
  const cables = modulations(snapshot);
  const drawTo = (modulator: number, node: number, port: Port): Intent => ({ type: "addModEdge", modulator, to: { node, port } });
  /** Ask, and when it is done wait for the client to have the session as it is then; null if it was, or the code and the words it was refused with. */
  const attempt = async (client: Client, intent: Intent): Promise<[string | null, string] | null> => {
    try {
      const rev = await client.request(intent);
      await until(() => (client.state.snapshot?.rev ?? -1) >= rev, "the snapshot");
      return null;
    } catch (error) {
      if (!(error instanceof RequestError)) throw error;
      return [error.code, error.message];
    }
  };
  /** Ask for something that is to be done. */
  const done = async (client: Client, intent: Intent): Promise<void> => {
    expect(await attempt(client, intent)).toBeNull();
  };
  /** A port, and so a place on a node, as one word. */
  const key = (node: number, port: Port): string => `${String(node)}:${port.type}${port.type === "depth" ? String(port.edge) : ""}`;
  const SETTINGS: Port[] = [{ type: "rate" }, { type: "attack" }, { type: "decay" }, { type: "sustain" }, { type: "release" }];

  test("is allowed where the controller allowed it, from every modulator of a session of groups and lanes, and nowhere else", async () => {
    const client = await open(new MockServer({ snapshot }));
    // Every setting of every modulator, whether it has it or not, and the depth of every cable (as of the modulator it belongs to).
    const candidates: { node: number; port: Port }[] = [
      ...modulators.flatMap((target) => SETTINGS.map((port) => ({ node: target.id, port }))),
      ...cables.map((cable): { node: number; port: Port } => ({ node: cable.from.node, port: { type: "depth", edge: cable.id } })),
    ];
    for (const modulator of modulators) {
      const accepted: string[] = [];
      for (const { node, port } of candidates) {
        if ((await attempt(client, drawTo(modulator.id, node, port))) !== null) continue;
        accepted.push(key(node, port));
        // (Taken out again, to ask the next of the session as it was.)
        await done(client, { type: "removeEdge", edge: Math.max(...modulations(client.state.snapshot).map((e) => e.id)) });
      }
      const listed = (fields[String(modulator.id)] ?? []).map((field) => key(field.node, field.port));
      expect(accepted.sort(), `${modulator.name} ${String(modulator.id)}`).toEqual(listed.sort());
    }
    expect(client.state.snapshot?.graph).toEqual(snapshot.graph);
  });

  /** Every slot a layout has, meters and values. */
  const slotsOf = (layout: TelemetryLayout): number[] => [...layout.meters, ...layout.modulation].map((entry) => entry.slot);
  /** The values of the layout that are of a setting of a modulator, or of the depth of a cable. */
  const ofSettings = (layout: TelemetryLayout | undefined) => (layout?.modulation ?? []).filter((entry) => entry.kind === "effective" && entry.port.type !== "param");

  test("joins the modulator to the setting at half depth, among the cables of the modulator, and the value the setting is set to gets a slot in the layout of the telemetry", async () => {
    const client = await open(new MockServer({ snapshot }));
    await done(client, drawTo(laneEnvelope, laneLfo, { type: "rate" }));
    const added = modulations(client.state.snapshot).find((e) => e.id === 7);
    expect(added).toEqual({ kind: "modulation", id: 7, amount: 0.5, from: { node: laneEnvelope, port: { type: "modOut" } }, to: { node: laneLfo, port: { type: "rate" } } });
    // The groups' cables first, then the lanes': the envelope's cables are 1 and 2, and the new one comes after them and before the control's.
    expect(modulations(client.state.snapshot).map((e) => e.id)).toEqual([4, 5, 6, 0, 1, 2, 7, 3]);
    // The setting's value is a slot of the layout (f132), named at the port the cable ends at and with the next slot, and the fingerprint is another; the rest is as it was.
    // The controller lists the values that cables drive lane by lane and by the places of the modulators and of their cables: the envelope's come before the control's
    // (the value of the cable 3, slot 14), which this one comes before.
    const layout = client.state.snapshot?.telemetry;
    const entry = { kind: "effective", node: laneLfo, port: { type: "rate" }, slot: Math.max(...slotsOf(snapshot.telemetry)) + 1 } as const;
    const controls = snapshot.telemetry.modulation.findIndex((e) => e.slot === 14);
    expect(controls).toBeGreaterThan(0);
    expect(layout?.modulation).toEqual([...snapshot.telemetry.modulation.slice(0, controls), entry, ...snapshot.telemetry.modulation.slice(controls)]);
    expect(layout?.rev).not.toBe(snapshot.telemetry.rev);
    expect(layout?.meters).toEqual(snapshot.telemetry.meters);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
  });

  test("names the value of the depth of each cable of a modulator that is driven, and takes out that of the cable that goes and no other", async () => {
    // The envelope (5) has two cables, 1 and 2; the LFO (4) drives the depth of both.
    const client = await open(new MockServer({ snapshot }));
    await done(client, drawTo(laneLfo, laneEnvelope, { type: "depth", edge: 1 })); // 7
    await done(client, drawTo(laneLfo, laneEnvelope, { type: "depth", edge: 2 })); // 8
    const depths = () => ofSettings(client.state.snapshot?.telemetry).map((entry) => (entry.port.type === "depth" ? entry.port.edge : -1));
    expect(depths()).toEqual([1, 2]);
    // Taking out the cable 7 takes the value of the depth of cable 1 out of the layout, and not the other's.
    await done(client, { type: "removeEdge", edge: 7 });
    expect(depths()).toEqual([2]);
  });

  test("is allowed to more than one setting, and to the same one from another modulator", async () => {
    const client = await open(new MockServer({ snapshot }));
    await done(client, drawTo(laneEnvelope, laneLfo, { type: "rate" })); // 7
    await done(client, drawTo(laneControl, laneLfo, { type: "rate" })); // 8: the same rate, from the control
    await done(client, drawTo(laneEnvelope, laneLfo, { type: "depth", edge: 0 })); // 9: and the depth of the LFO's cable, from the envelope
    expect(modulations(client.state.snapshot).map((e) => e.id)).toEqual([4, 5, 6, 0, 1, 2, 7, 9, 3, 8]);
    // A value has one entry in the layout, whichever cables drive it: the rate is named once (by the first cable, 7) and the depth once; each cable took a slot (9 is the third).
    const layout = client.state.snapshot?.telemetry;
    const first = Math.max(...slotsOf(snapshot.telemetry)) + 1;
    expect(ofSettings(layout).map((entry) => [entry.node, entry.port, entry.slot])).toEqual([
      ...ofSettings(snapshot.telemetry).map((entry): [number, Port, number] => [entry.node, entry.port, entry.slot]),
      [laneLfo, { type: "rate" }, first],
      [laneLfo, { type: "depth", edge: 0 }, first + 2],
    ]);
  });

  test("takes the value a cable drove out of the layout when the cable goes, unless another cable drives it still", async () => {
    const client = await open(new MockServer({ snapshot }));
    await done(client, drawTo(laneEnvelope, laneLfo, { type: "rate" })); // 7
    await done(client, drawTo(laneControl, laneLfo, { type: "rate" })); // 8: the same rate
    const named = () => ofSettings(client.state.snapshot?.telemetry).filter((entry) => entry.node === laneLfo && entry.port.type === "rate");
    expect(named()).toHaveLength(1);
    await done(client, { type: "removeEdge", edge: 7 });
    expect(named(), "the control still drives the rate").toHaveLength(1);
    await done(client, { type: "removeEdge", edge: 8 });
    expect(named(), "nothing does").toEqual([]);
    expect(client.state.snapshot?.telemetry.modulation).toEqual(snapshot.telemetry.modulation);
  });

  test("is refused as the controller refuses it, and nothing changes", async () => {
    const client = await open(new MockServer({ snapshot }));
    const reachLane = ["invalid", "a modulator of this lane cannot reach that modulator"];
    const reachGroup = ["invalid", "a modulator of this group cannot reach that modulator"];
    const notModulator = ["invalid", "a cable ends at the settings of a modulator, and that is not one"];
    const own = ["invalid", "a modulator cannot drive its own settings"];
    const table: [string, Intent, string[]][] = [
      ["itself", drawTo(laneLfo, laneLfo, { type: "rate" }), own],
      ["the depth of its own cable", drawTo(laneLfo, laneLfo, { type: "depth", edge: 0 }), own],
      ["a node that is not there", drawTo(laneLfo, 99_999, { type: "rate" }), ["notFound", "there is no node 99999"]],
      ["the master", drawTo(laneLfo, MASTER, { type: "rate" }), ["notFound", `there is no node ${String(MASTER)}`]],
      ["a lane", drawTo(laneLfo, 3, { type: "rate" }), notModulator],
      ["a group", drawTo(laneLfo, 0, { type: "rate" }), notModulator],
      ["a bus effect", drawTo(groupLfo, 1, { type: "attack" }), notModulator],
      ["a parameter's port of a modulator", drawTo(laneEnvelope, laneLfo, { type: "param", index: 0 }), ["invalid", "that node has no plugin"]],
      ["a modulator of another lane of its group", drawTo(laneLfo, member2Lfo, { type: "rate" }), reachLane],
      ["a modulator of the group of its lane", drawTo(laneLfo, groupLfo, { type: "rate" }), reachLane],
      ["a modulator of a lane in no group", drawTo(laneLfo, lane9Lfo, { type: "rate" }), reachLane],
      ["a modulator of a lane in another group, from a group", drawTo(groupLfo, lane11Lfo, { type: "rate" }), reachGroup],
      ["a modulator of a lane in no group, from a group", drawTo(groupLfo, lane9Lfo, { type: "rate" }), reachGroup],
      ["the rate of an envelope", drawTo(laneLfo, laneEnvelope, { type: "rate" }), ["invalid", "that modulator has no rate"]],
      ["the rate of a MIDI control", drawTo(laneLfo, laneControl, { type: "rate" }), ["invalid", "that modulator has no rate"]],
      ["the attack of an LFO", drawTo(laneEnvelope, laneLfo, { type: "attack" }), ["invalid", "that modulator has no attack"]],
      ["the decay of an LFO", drawTo(laneEnvelope, laneLfo, { type: "decay" }), ["invalid", "that modulator has no decay"]],
      ["the sustain of an LFO", drawTo(laneEnvelope, laneLfo, { type: "sustain" }), ["invalid", "that modulator has no sustain"]],
      ["the release of a MIDI control", drawTo(laneEnvelope, laneControl, { type: "release" }), ["invalid", "that modulator has no release"]],
      ["the depth of a cable that is not there", drawTo(laneEnvelope, laneLfo, { type: "depth", edge: 99_999 }), ["notFound", "there is no cable 99999"]],
      ["the depth of a cable that the node named does not have (it is the modulator's own)", drawTo(laneEnvelope, laneLfo, { type: "depth", edge: 1 }), ["notFound", "there is no cable 1"]],
      ["the depth of a cable that the node named does not have (it is another modulator's)", drawTo(laneEnvelope, laneControl, { type: "depth", edge: 0 }), ["notFound", "there is no cable 0"]],
    ];
    for (const [what, intent, expected] of table) expect(await attempt(client, intent), what).toEqual(expected);
    expect(client.state.snapshot).toEqual(snapshot);
    // And no cable id was used up.
    await done(client, drawTo(laneEnvelope, laneLfo, { type: "rate" }));
    expect(modulations(client.state.snapshot).map((e) => e.id)).toContain(7);
  });

  test("is refused when the modulator drives the setting already, and allowed for another modulator", async () => {
    const client = await open(new MockServer({ snapshot }));
    await done(client, drawTo(laneEnvelope, laneLfo, { type: "rate" }));
    expect(await attempt(client, drawTo(laneEnvelope, laneLfo, { type: "rate" }))).toEqual(["invalid", "that modulator drives that setting already"]);
    await done(client, drawTo(laneControl, laneLfo, { type: "rate" }));
    // The depth of a cable too.
    await done(client, drawTo(laneEnvelope, laneLfo, { type: "depth", edge: 0 }));
    expect(await attempt(client, drawTo(laneEnvelope, laneLfo, { type: "depth", edge: 0 }))).toEqual(["invalid", "that modulator drives that setting already"]);
  });

  test("goes when it is taken out, which leaves the session as it was", async () => {
    const client = await open(new MockServer({ snapshot }));
    await done(client, drawTo(laneEnvelope, laneLfo, { type: "rate" }));
    await done(client, { type: "removeEdge", edge: 7 });
    expect(client.state.snapshot?.graph).toEqual(snapshot.graph);
    // (The layout is as it was: the value the cable drove has left it. The mock's fingerprint is a count of changes, which the controller's, of the entries, is not: not compared.)
    expect(client.state.snapshot?.telemetry.modulation).toEqual(snapshot.telemetry.modulation);
    expect(client.state.snapshot?.telemetry.meters).toEqual(snapshot.telemetry.meters);
  });

  test("goes with the modulator whose setting it drives, and with the one that drives it", async () => {
    const client = await open(new MockServer({ snapshot }));
    await done(client, drawTo(laneEnvelope, laneLfo, { type: "rate" })); // 7: the envelope drives the LFO's rate
    await done(client, drawTo(laneControl, laneEnvelope, { type: "attack" })); // 8: the control drives the envelope's attack
    await done(client, { type: "removeNode", node: laneLfo });
    // The LFO's own cable (0) went with it, and the one that drove its rate (7); the control's cable to the envelope stays.
    expect(modulations(client.state.snapshot).map((e) => e.id)).toEqual([4, 5, 6, 1, 2, 3, 8]);
    await done(client, { type: "removeNode", node: laneEnvelope });
    // Now the envelope's cables (1 and 2) and the one that drove its attack (8) have gone.
    expect(modulations(client.state.snapshot).map((e) => e.id)).toEqual([4, 5, 6, 3]);
  });

  test("goes with the cable whose depth it drives, and what drives the depth of that goes in turn", async () => {
    const client = await open(new MockServer({ snapshot }));
    await done(client, drawTo(laneLfo, laneEnvelope, { type: "depth", edge: 1 })); // 7: the LFO drives the depth of the envelope's cable 1
    await done(client, drawTo(laneControl, laneLfo, { type: "depth", edge: 7 })); // 8: the control drives the depth of that
    expect(modulations(client.state.snapshot)).toHaveLength(cables.length + 2);
    await done(client, { type: "removeEdge", edge: 1 });
    expect(modulations(client.state.snapshot).map((e) => e.id)).toEqual([4, 5, 6, 0, 2, 3]);
  });

  test("is drawn from a group's modulator to the settings of its members' modulators, and of the others in its rack", async () => {
    const client = await open(new MockServer({ snapshot }));
    await done(client, drawTo(groupLfo, laneEnvelope, { type: "attack" })); // 7
    await done(client, drawTo(groupLfo, member2Lfo, { type: "rate" })); // 8
    await done(client, drawTo(groupLfo, groupEnvelope, { type: "release" })); // 9
    await done(client, drawTo(groupControl, laneLfo, { type: "rate" })); // 10
    expect(modulations(client.state.snapshot).map((e) => e.id)).toEqual([4, 7, 8, 9, 5, 6, 10, 0, 1, 2, 3]);
  });
});

describe("the catalog of plugins", () => {
  const oscillator: CatalogEntry = { id: "builtin:osc", name: "Oscillator", format: "Built-in", isInstrument: true, params: 8, presets: 0 };
  const pianoteq: CatalogEntry = { id: "vst3:Pianoteq 8", name: "Pianoteq 8", format: "VST3", isInstrument: true, params: 300, presets: 12 };

  test("is not known until the server says, and the built-ins are what the mock says by default", async () => {
    const client = connect(new MockServer());
    expect(client.state.catalog).toBeNull();
    await until(() => client.state.catalog !== null, "the catalog");
    expect(client.state.catalog).toEqual({ entries: builtinCatalog, scanning: false });
    expect(builtinCatalog.map((entry) => entry.id)).toContain("builtin:osc");
  });

  test("is what the server has found so far, and whether it is still looking", async () => {
    const server = new MockServer({ catalog: { entries: [oscillator], scanning: true } });
    const client = await open(server);
    await until(() => client.state.catalog !== null, "the catalog");
    expect(client.state.catalog).toEqual({ entries: [oscillator], scanning: true });
  });

  test("follows the scan as it finds more, each telling replacing the last", async () => {
    const server = new MockServer({ catalog: { entries: [oscillator], scanning: true } });
    const client = await open(server);
    await until(() => client.state.catalog !== null, "the catalog");
    server.announceCatalog([oscillator, pianoteq], true);
    await until(() => client.state.catalog?.entries.length === 2, "the second batch");
    expect(client.state.catalog).toEqual({ entries: [oscillator, pianoteq], scanning: true });
    // (The scan ends with nothing more to add: that is told as well, with the same plugins.)
    server.announceCatalog([oscillator, pianoteq], false);
    await until(() => client.state.catalog?.scanning === false, "the end of the scan");
    expect(client.state.catalog?.entries).toEqual([oscillator, pianoteq]);
  });

  test("does not change the session, which it is no part of", async () => {
    const server = new MockServer({ catalog: { entries: [oscillator], scanning: true } });
    const client = await open(server);
    const before = client.state.snapshot;
    server.announceCatalog([oscillator, pianoteq], false);
    await until(() => client.state.catalog?.entries.length === 2, "the catalog");
    expect(client.state.snapshot).toBe(before);
    expect(client.state.snapshot?.meta.dirty).toBe(false);
    expect(client.state.snapshot?.rev).toBe(0);
  });

  test("is kept while the connection is made good, and told again by the server it comes back to", async () => {
    const server = new MockServer({ catalog: { entries: [oscillator], scanning: true } });
    const client = await open(server);
    await until(() => client.state.catalog !== null, "the catalog");
    server.drop();
    await until(() => client.state.status === "reconnecting", "the loss");
    expect(client.state.catalog).toEqual({ entries: [oscillator], scanning: true }); // what it knew is still there
    server.announceCatalog([oscillator, pianoteq], false); // while it was away: nobody hears of it
    await until(() => client.state.status === "open" && client.state.catalog?.scanning === false, "the catalog after the reconnection");
    expect(client.state.catalog?.entries).toEqual([oscillator, pianoteq]);
  });

  test("is not told to a connection that has not subscribed", async () => {
    const server = new MockServer({ token: "secret" });
    const heard: ServerMessage[] = [];
    const transport = server.transport();
    transport.connect({
      open: () => undefined,
      text: (data) => {
        heard.push(JSON.parse(data) as ServerMessage);
      },
      binary: () => undefined,
      close: () => undefined,
    });
    await quiet();
    server.announceCatalog([oscillator], false);
    await quiet();
    expect(heard.map((message) => message.type)).toEqual(["hello"]);
    transport.close();
  });
});

describe("what Undo and Redo would do", () => {
  const nothing = { undo: null, redo: null };

  /** A connection to the server that writes down what it is told, as the words of its types (an event by its own, a history by what it says). */
  function raw(server: MockServer) {
    const heard: string[] = [];
    const transport = server.transport();
    transport.connect({
      open: () => undefined,
      text: (data) => {
        const message = JSON.parse(data) as ServerMessage;
        heard.push(message.type === "event" ? `event ${message.event.type}` : message.type === "history" ? `history ${String(message.undo)}/${String(message.redo)}` : message.type);
      },
      binary: () => undefined,
      close: () => undefined,
    });
    return {
      heard,
      send: (message: ClientMessage) => {
        transport.send(JSON.stringify(message));
      },
      close: () => {
        transport.close();
      },
    };
  }

  test("is nothing to begin with, and the words of a button for what was done and what was undone", async () => {
    const client = await open(new MockServer());
    expect(client.state.history).toEqual(nothing);
    const before = valueOf(client, filter, 0);
    // (Each is a revision of its own, and the answer says which.)
    expect(await client.request(cutoff(0.3))).toBe(1);
    expect(client.state.history).toEqual({ undo: "set cutoff", redo: null });
    expect(await client.request({ type: "undo" })).toBe(2);
    expect(client.state.history).toEqual({ undo: null, redo: "set cutoff" });
    expect(valueOf(client, filter, 0)).toBe(before);
    expect(await client.request({ type: "redo" })).toBe(3);
    expect(client.state.history).toEqual({ undo: "set cutoff", redo: null });
    expect(valueOf(client, filter, 0)).toBe(0.3);
  });

  test("is told to a connection when it subscribes, after the snapshot and the catalog, and not before", async () => {
    const server = new MockServer();
    const { heard, send, close } = raw(server);
    await quiet();
    send({ type: "subscribe" });
    await quiet();
    expect(heard).toEqual(["hello", "snapshot", "catalog", "history null/null"]);
    close();
  });

  test("is told after the events of what changed it and before the answer, and only when it is not what the connection was told", async () => {
    const server = new MockServer();
    const { heard, send, close } = raw(server);
    send({ type: "subscribe" });
    await quiet();
    const seen = heard.length;
    send({ type: "request", reqId: 1, intent: cutoff(0.3) });
    await quiet();
    expect(heard.slice(seen)).toEqual(["event paramSet", "history set cutoff/null", "ack"]);
    // The same knob again at once is the same step: there is nothing new to tell of the history.
    send({ type: "request", reqId: 2, intent: cutoff(0.4) });
    await quiet();
    expect(heard.slice(seen + 3)).toEqual(["event paramSet", "ack"]);
    // What is refused changes nothing in it.
    send({ type: "request", reqId: 3, intent: { type: "setParam", node: 424242, param: 0, value: 1 } });
    await quiet();
    expect(heard.slice(seen + 5)).toEqual(["rejected"]);
    send({ type: "request", reqId: 4, intent: { type: "undo" } });
    await quiet();
    expect(heard.slice(seen + 6)).toEqual(["event paramSet", "history null/set cutoff", "ack"]);
    close();
  });

  test("is told when only what could be redone has changed: the step before the one that was undone has the same words", async () => {
    let seconds = 0;
    const client = await open(new MockServer({ clock: () => seconds }));
    await client.request(cutoff(0.3));
    seconds += 5;
    await client.request(cutoff(0.4));
    expect(client.state.history).toEqual({ undo: "set cutoff", redo: null });
    await client.request({ type: "undo" });
    // (What can be undone is "set cutoff" before and after; what can be redone is new.)
    expect(client.state.history).toEqual({ undo: "set cutoff", redo: "set cutoff" });
  });

  test("is as it was after the session is saved, which changes none of it", async () => {
    const client = await open(new MockServer());
    await client.request(cutoff(0.3));
    await client.request({ type: "save" });
    expect(client.state.history).toEqual({ undo: "set cutoff", redo: null });
    await client.request({ type: "undo" });
    expect(valueOf(client, filter, 0)).not.toBe(0.3);
  });

  test("is not told to a connection that has not subscribed", async () => {
    const server = new MockServer();
    const { heard, close } = raw(server);
    await quiet();
    server.apply(cutoff(0.3));
    await quiet();
    expect(heard).toEqual(["hello"]);
    close();
  });

  test("is told to be gone when what was done cannot be undone yet, with the snapshot that tells what was done", async () => {
    const server = new MockServer();
    const { heard, send, close } = raw(server);
    send({ type: "subscribe" });
    await quiet();
    send({ type: "request", reqId: 1, intent: cutoff(0.3) });
    await quiet();
    // A lane in a group, which a group made of it would take out of: nothing puts that back yet.
    const lane = nodeNamed(tracerSnapshot, "Oscillator").id;
    send({ type: "request", reqId: 2, intent: { type: "addGroup", members: [] } });
    await quiet();
    const group = server.snapshot.graph.nodes.find((n) => n.kind === "group");
    if (!group) throw new Error("the group that was added");
    send({ type: "request", reqId: 3, intent: { type: "setLaneGroup", node: lane, group: group.id } });
    await quiet();
    const seen = heard.length;
    send({ type: "request", reqId: 4, intent: { type: "addGroup", members: [lane] } });
    await quiet();
    expect(heard.slice(seen)).toEqual(["snapshot", "history null/null", "ack"]);
    close();
  });

  test("is told to every client that has subscribed, when the change is another front-end's", async () => {
    const server = new MockServer();
    const [one, other] = [await open(server), await open(server)];
    server.apply(cutoff(0.3));
    await until(() => one.state.history.undo !== null && other.state.history.undo !== null, "the history, told to both");
    expect([one.state.history, other.state.history]).toEqual([{ undo: "set cutoff", redo: null }, { undo: "set cutoff", redo: null }]);
    server.apply({ type: "undo" });
    await until(() => one.state.history.undo === null, "the history after the undo");
    expect(other.state.history).toEqual({ undo: null, redo: "set cutoff" });
  });

  test("is forgotten when the connection is lost, and what the server has is told again when it comes back", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request(cutoff(0.3));
    expect(client.state.history.undo).toBe("set cutoff");
    server.drop();
    await until(() => client.state.status === "reconnecting", "the loss");
    expect(client.state.history).toEqual(nothing);
    await until(() => client.state.status === "open" && client.state.history.undo !== null, "the history after the reconnection");
    expect(client.state.history).toEqual({ undo: "set cutoff", redo: null });
  });
});

describe("adding an effect", () => {
  const audioPath = (snapshot: Snapshot | null) => (snapshot?.graph.edges ?? []).filter((e) => e.kind !== "modulation").map((e) => `${e.kind} ${String(e.from.node)}→${String(e.to.node)}`);
  const strip = (snapshot: Snapshot | null) => (snapshot?.graph.nodes ?? []).map((n) => `${String(n.id)} ${n.name}`);
  const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
  const add = (parent: number, plugin: string): Intent => ({ type: "addEffect", parent, plugin });
  const reach = reachFixture.snapshot;

  test("puts an effect after the effects a lane has and before its modulators, in the audio path, with the controller's own node", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request(add(oscillator.id, "builtin:reverb"));
    const snapshot = client.state.snapshot;
    expect(strip(snapshot)).toEqual(["0 Oscillator", "1 Filter", "3 Reverb", "2 LFO", `${String(MASTER)} Master`]);
    expect(audioPath(snapshot)).toEqual(["audio 0→1", "audio 1→3", `audio 3→${String(MASTER)}`]);
    // The node is what the controller made for a reverb that is new: its plugin, its parameters at their defaults, its mix.
    const node = snapshot?.graph.nodes.find((n) => n.id === 3);
    expect(node).toEqual({ id: 3, parent: oscillator.id, ...builtinEffects["builtin:reverb"] });
    expect(node?.kind === "effect" && node.mix).toBe(0.5);
    // The cable that was there is as it was, the layout of the telemetry is the same, and the session has something to save.
    expect(snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toEqual(tracerSnapshot.graph.edges.filter((e) => e.kind === "modulation"));
    expect(snapshot?.telemetry).toEqual(tracerSnapshot.telemetry);
    expect(snapshot?.meta.dirty).toBe(true);
    expect(server.snapshot).toEqual(snapshot);
  });

  test("starts a filter fully wet and anything else half wet", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request(add(oscillator.id, "builtin:filter"));
    await client.request(add(oscillator.id, "builtin:reverb"));
    const mixes = (client.state.snapshot?.graph.nodes ?? []).filter((n) => n.kind === "effect").map((n) => [n.name, n.mix]);
    expect(mixes).toEqual([["Filter", 1], ["Filter", 1], ["Reverb", 0.5]]);
  });

  test("goes at the end of a group's bus, and a lane that is a member of a group still ends in the group", async () => {
    const server = new MockServer({ snapshot: reach });
    const client = await open(server);
    // Group 0: its bus has a filter (1). The new effect is the node 18.
    await client.request(add(0, "builtin:reverb"));
    expect(strip(client.state.snapshot).slice(0, 5)).toEqual(["0 Group 1", "1 Filter", "18 Reverb", "15 LFO", "2 Group 2"]);
    expect(audioPath(client.state.snapshot).slice(0, 4)).toEqual(["audio 0→1", "audio 1→18", `audio 18→${String(MASTER)}`, "audio 2→3"]);
    // The lane 4 is a member of group 0, with a filter (5) and an LFO (6): the effect goes between them, and the path still ends in the group.
    await client.request(add(4, "builtin:filter"));
    const snapshot = client.state.snapshot;
    expect(strip(snapshot).slice(strip(snapshot).indexOf("4 Oscillator"), strip(snapshot).indexOf("4 Oscillator") + 4)).toEqual(["4 Oscillator", "5 Filter", "19 Filter", "6 LFO"]);
    expect(audioPath(snapshot)).toContain("audio 5→19");
    expect(audioPath(snapshot)).toContain("member 19→0");
    expect(audioPath(snapshot)).not.toContain("member 5→0");
  });

  test("goes into a lane that has nothing in it, and into one that is a member of a group and has no effects", async () => {
    const server = new MockServer({ snapshot: reach });
    const client = await open(server);
    await client.request(add(14, "builtin:filter")); // the lane with no instrument
    await client.request(add(7, "builtin:reverb")); // a member of group 0 with no effects
    const path = audioPath(client.state.snapshot);
    expect(path).toContain("audio 14→18");
    expect(path).toContain(`audio 18→${String(MASTER)}`);
    expect(path).toContain("audio 7→19");
    expect(path).toContain("member 19→0");
    expect(path).not.toContain("member 7→0");
    const nodes = strip(client.state.snapshot);
    expect(nodes.slice(nodes.indexOf("7 Oscillator"), nodes.indexOf("7 Oscillator") + 3)).toEqual(["7 Oscillator", "19 Reverb", "8 ADSR"]);
  });

  test("makes a node a modulator can be drawn to at once, if the modulator reaches the lane or group it is in", async () => {
    const server = new MockServer({ snapshot: reach });
    const client = await open(server);
    await client.request(add(4, "builtin:reverb")); // node 18, in lane 4, which group 0's modulator (15) and lane 4's own (6) reach
    const damping = (builtinEffects["builtin:reverb"]?.plugin.params ?? []).find((p) => p.name === "damping")?.index ?? -1;
    const draw = (modulator: number): Intent => ({ type: "addModEdge", modulator, to: { node: 18, port: { type: "param", index: damping } } });
    await client.request(draw(6));
    await client.request(draw(15));
    const refused = await client.request(draw(8)).catch((e: unknown) => e); // lane 7's modulator does not reach lane 4
    expect(refused).toBeInstanceOf(RequestError);
    expect((refused as RequestError).message).toBe("a modulator of this lane cannot reach that plugin");
    const cables = (client.state.snapshot?.graph.edges ?? []).filter((e) => e.kind === "modulation" && e.to.node === 18);
    expect(cables.map((e) => e.from.node).sort((a, b) => a - b)).toEqual([6, 15]);
  });

  test("numbers the nodes after the ones that have been made, whatever has been taken out", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request(add(oscillator.id, "builtin:reverb")); // 3
    await client.request({ type: "removeNode", node: lfo.id });
    await client.request(add(oscillator.id, "builtin:filter"));
    expect((client.state.snapshot?.graph.nodes ?? []).filter((n) => n.kind === "effect").map((n) => n.id)).toEqual([1, 3, 4]);
  });

  test("is refused for what is not a lane or a group, a plugin the catalog does not name, an instrument, or a plugin that cannot be loaded, and changes nothing", async () => {
    const pianoteq = { id: "vst3:Pianoteq 8", name: "Pianoteq 8", format: "VST3", isInstrument: false, params: 300, presets: 12 };
    const server = new MockServer({ catalog: { entries: [...builtinCatalog, pianoteq], scanning: false } });
    const client = await open(server);
    const filterNode = nodeNamed(tracerSnapshot, "Filter");
    const refused = [
      [add(filterNode.id, "builtin:reverb"), "invalid", "an effect goes in a lane or in a group"],
      [add(lfo.id, "builtin:reverb"), "invalid", "an effect goes in a lane or in a group"],
      [add(424242, "builtin:reverb"), "notFound", "there is no node 424242"],
      [add(MASTER, "builtin:reverb"), "notFound", `there is no node ${String(MASTER)}`],
      [add(oscillator.id, "./examples/plugin.clap"), "invalid", "./examples/plugin.clap is not a plugin that is installed"],
      [add(oscillator.id, "builtin:osc"), "invalid", "Oscillator is an instrument, not an effect"],
      [add(oscillator.id, pianoteq.id), "invalid", "Pianoteq 8 could not be loaded"],
    ] as const;
    for (const [intent, code, message] of refused) {
      const failure = (await client.request(intent).catch((e: unknown) => e)) as RequestError;
      expect([failure.code, failure.message], JSON.stringify(intent)).toEqual([code, message]);
    }
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("is told to the other clients as a snapshot, as every change of structure is", async () => {
    const server = new MockServer();
    const first = await open(server);
    const second = await open(server);
    await first.request(add(oscillator.id, "builtin:reverb"));
    await until(() => (second.state.snapshot?.graph.nodes.length ?? 0) === 5, "the second client's snapshot");
    expect(second.state.snapshot).toEqual(first.state.snapshot);
  });

  test("has a node for each effect the catalog of the mock names, and for no other", () => {
    expect(Object.keys(builtinEffects).sort()).toEqual(builtinCatalog.filter((entry) => !entry.isInstrument).map((entry) => entry.id).sort());
    for (const [id, effect] of Object.entries(builtinEffects)) {
      expect(effect.plugin.id, id).toBe(id);
      expect(effect.plugin.isInstrument, id).toBe(false);
      expect(effect.name, id).toBe(builtinCatalog.find((entry) => entry.id === id)?.name);
      expect(effect.plugin.params.length, id).toBe(builtinCatalog.find((entry) => entry.id === id)?.params);
    }
  });
});

describe("adding an instrument", () => {
  const audioPath = (snapshot: Snapshot | null) => (snapshot?.graph.edges ?? []).filter((e) => e.kind !== "modulation").map((e) => `${e.kind} ${String(e.from.node)}→${String(e.to.node)}`);
  const strip = (snapshot: Snapshot | null) => (snapshot?.graph.nodes ?? []).map((n) => `${String(n.id)} ${n.name}`);
  const add = (plugin: string): Intent => ({ type: "addInstrument", plugin });
  const nextSlot = (snapshot: Snapshot) => 1 + Math.max(...[...snapshot.telemetry.meters, ...snapshot.telemetry.modulation].map((entry) => entry.slot));
  const reach = reachFixture.snapshot;

  test("puts a lane after the lanes there are and before the master, with the controller's own node, its link to the master after the others and no group", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request(add("builtin:osc"));
    const snapshot = client.state.snapshot;
    expect(strip(snapshot)).toEqual(["0 Oscillator", "1 Filter", "2 LFO", "3 Oscillator", `${String(MASTER)} Master`]);
    expect(audioPath(snapshot)).toEqual(["audio 0→1", `audio 1→${String(MASTER)}`, `audio 3→${String(MASTER)}`]);
    // (The graph lists the links of the audio path first, and the modulation cables after them all.)
    expect(snapshot?.graph.edges.map((e) => e.kind)).toEqual(["audio", "audio", "audio", "modulation"]);
    // The node is what the controller made for an oscillator that is new: its plugin with its parameters at their defaults, full range, its volume 1.
    const node = snapshot?.graph.nodes.find((n) => n.id === 3);
    expect(node).toEqual({ id: 3, parent: null, ...builtinInstruments["builtin:osc"] });
    expect(node).toMatchObject({ kind: "instrument", range: null, transpose: 0, volume: 1 });
    // What was there is as it was, and the session has something to save.
    expect(snapshot?.graph.edges.filter((e) => e.kind === "modulation")).toEqual(tracerSnapshot.graph.edges.filter((e) => e.kind === "modulation"));
    expect(snapshot?.meta.dirty).toBe(true);
    expect(server.snapshot).toEqual(snapshot);
  });

  test("gives the lane a peak and a clip count at the end of the meters, on slots that nothing has used, and a layout of a revision of its own", async () => {
    const server = new MockServer();
    const client = await open(server);
    const first = nextSlot(tracerSnapshot);
    await client.request(add("builtin:osc"));
    const { telemetry } = client.state.snapshot ?? tracerSnapshot;
    expect(telemetry.meters.slice(0, -2)).toEqual(tracerSnapshot.telemetry.meters);
    expect(telemetry.meters.slice(-2)).toEqual([
      { slot: first, node: 3, port: { type: "audioOut" }, kind: "peak" },
      { slot: first + 1, node: 3, port: { type: "audioOut" }, kind: "clips" },
    ]);
    expect(telemetry.modulation).toEqual(tracerSnapshot.telemetry.modulation);
    expect(telemetry.rev).toBe(tracerSnapshot.telemetry.rev + 1);
    // A second lane goes on from there, and the client reads frames by the layout it was told.
    await client.request(add("builtin:osc"));
    expect(client.state.snapshot?.telemetry.meters.slice(-2).map((entry) => entry.slot)).toEqual([first + 2, first + 3]);
    expect(client.telemetry.values("meters")?.length).toBe(tracerSnapshot.telemetry.meters.length + 4);
    expect(client.telemetry.find("meters", 3, { type: "audioOut" }, "peak")).toBeDefined();
    expect(client.telemetry.find("meters", 4, { type: "audioOut" }, "clips")).toBeDefined();
  });

  test("goes after the last lane of a session with groups, in no group, and is the last of the audio path's links", async () => {
    const server = new MockServer({ snapshot: reach });
    const client = await open(server);
    await client.request(add("builtin:osc"));
    const nodes = strip(client.state.snapshot);
    // (The lanes come after the groups, each with its effects and modulators; the last is the empty lane 14, with an LFO.)
    expect(nodes.slice(-4)).toEqual(["14 (empty)", "17 LFO", "18 Oscillator", `${String(MASTER)} Master`]);
    expect(client.state.snapshot?.graph.nodes.find((n) => n.id === 18)?.parent).toBeNull();
    expect(audioPath(client.state.snapshot).at(-1)).toBe(`audio 18→${String(MASTER)}`);
    // The links of the lanes that were there, and the groups', are where they were.
    expect(audioPath(client.state.snapshot).slice(0, -1)).toEqual(audioPath(reach));
  });

  test("numbers the nodes after the ones that have been made, whatever has been taken out", async () => {
    const server = new MockServer();
    const client = await open(server);
    await client.request(add("builtin:osc")); // 3
    await client.request({ type: "removeNode", node: lfo.id });
    await client.request(add("builtin:osc"));
    expect((client.state.snapshot?.graph.nodes ?? []).filter((n) => n.kind === "instrument").map((n) => n.id)).toEqual([0, 3, 4]);
  });

  test("is refused for a plugin the catalog does not name, an effect, or a plugin that cannot be loaded, and changes nothing", async () => {
    const pianoteq = { id: "vst3:Pianoteq 8", name: "Pianoteq 8", format: "VST3", isInstrument: true, params: 300, presets: 12 };
    const server = new MockServer({ catalog: { entries: [...builtinCatalog, pianoteq], scanning: false } });
    const client = await open(server);
    const refused = [
      [add("./examples/plugin.clap"), "invalid", "./examples/plugin.clap is not a plugin that is installed"],
      [add("clap:com.example.nothing"), "invalid", "clap:com.example.nothing is not a plugin that is installed"],
      [add("builtin:reverb"), "invalid", "Reverb is an effect, not an instrument"],
      [add(pianoteq.id), "invalid", "Pianoteq 8 could not be loaded"],
    ] as const;
    for (const [intent, code, message] of refused) {
      const failure = (await client.request(intent).catch((e: unknown) => e)) as RequestError;
      expect([failure.code, failure.message], JSON.stringify(intent)).toEqual([code, message]);
    }
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("is told to the other clients as a snapshot, as every change of structure is", async () => {
    const server = new MockServer();
    const first = await open(server);
    const second = await open(server);
    await first.request(add("builtin:osc"));
    await until(() => (second.state.snapshot?.graph.nodes.length ?? 0) === 5, "the second client's snapshot");
    expect(second.state.snapshot).toEqual(first.state.snapshot);
  });

  test("has a node for each instrument the catalog of the mock names, and for no other", () => {
    expect(Object.keys(builtinInstruments).sort()).toEqual(builtinCatalog.filter((entry) => entry.isInstrument).map((entry) => entry.id).sort());
    for (const [id, instrument] of Object.entries(builtinInstruments)) {
      expect(instrument.plugin.id, id).toBe(id);
      expect(instrument.plugin.isInstrument, id).toBe(true);
      expect(instrument.name, id).toBe(builtinCatalog.find((entry) => entry.id === id)?.name);
      expect(instrument.plugin.params.length, id).toBe(builtinCatalog.find((entry) => entry.id === id)?.params);
    }
  });
});

describe("replacing an instrument", () => {
  const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
  const replace = (node: number, plugin: string): Intent => ({ type: "replaceInstrument", node, plugin });
  const detune = (builtinInstruments["builtin:osc"]?.plugin.params ?? []).find((p) => p.name === "detune")?.index ?? -1;
  const cables = (snapshot: Snapshot | null) => (snapshot?.graph.edges ?? []).filter((e) => e.kind === "modulation");
  const reach = reachFixture.snapshot;

  /** The tracer with its instrument edited the way a person would have: a saw, a volume of 0.25 of its own, and a range, a transposition and a volume of the lane's. */
  function edited(): Snapshot {
    const nodes = tracerSnapshot.graph.nodes.map((node): Node => {
      if (node.id !== oscillator.id || node.kind !== "instrument" || !node.plugin) return node;
      const params = node.plugin.params.map((p) => (p.name === "waveform" ? { ...p, value: 3 } : p.name === "volume" ? { ...p, value: 0.25 } : p));
      return { ...node, plugin: { ...node.plugin, params, currentPreset: "Pad" }, range: { low: 36, high: 59 }, transpose: 2, volume: 0.5 };
    });
    return { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } };
  }

  test("puts the new instrument in the lane under the same node: its name and its plugin with its own parameters, and the lane's range, transposition and volume as they were", async () => {
    const server = new MockServer({ snapshot: edited() });
    const client = await open(server);
    await client.request(replace(oscillator.id, "builtin:osc"));
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === oscillator.id);
    expect(node).toEqual({ id: oscillator.id, parent: null, ...builtinInstruments["builtin:osc"], range: { low: 36, high: 59 }, transpose: 2, volume: 0.5 });
    // (The edits and the preset do not survive a swap.)
    expect(node?.kind === "instrument" && node.plugin?.currentPreset).toBeNull();
    // The rest of the session is as it was, and there is something to save.
    expect(client.state.snapshot?.graph.nodes.map((n) => n.id)).toEqual(tracerSnapshot.graph.nodes.map((n) => n.id));
    expect(client.state.snapshot?.graph.edges).toEqual(tracerSnapshot.graph.edges);
    expect(client.state.snapshot?.telemetry).toEqual(tracerSnapshot.telemetry);
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    expect(server.snapshot).toEqual(client.state.snapshot);
  });

  test("takes the cables that drive the old instrument's parameters out, and the slot of the value they drove with them, and leaves the others", async () => {
    const server = new MockServer();
    const client = await open(server);
    // The lane's LFO also drives the oscillator's detune: its own slot in the layout.
    await client.request({ type: "addModEdge", modulator: lfo.id, to: { node: oscillator.id, port: { type: "param", index: detune } } });
    expect(cables(client.state.snapshot)).toHaveLength(2);
    const driven = client.state.snapshot?.telemetry.modulation.filter((e) => e.kind === "effective" && e.node === oscillator.id);
    expect(driven).toHaveLength(1);
    const before = client.state.snapshot?.telemetry;

    await client.request(replace(oscillator.id, "builtin:osc"));
    const snapshot = client.state.snapshot;
    expect(cables(snapshot)).toEqual(tracerSnapshot.graph.edges.filter((e) => e.kind === "modulation"));
    expect(snapshot?.telemetry.modulation.filter((e) => e.node === oscillator.id)).toEqual([]);
    expect(snapshot?.telemetry.modulation).toEqual(tracerSnapshot.telemetry.modulation);
    expect(snapshot?.telemetry.rev).toBe((before?.rev ?? 0) + 1);
  });

  test("takes the cables of a group's modulator to the member's instrument out too, and those that drive the depth of a cable that goes, and no other", async () => {
    const volume = (builtinInstruments["builtin:osc"]?.plugin.params ?? []).find((p) => p.name === "volume")?.index ?? -1;
    const draw = (modulator: number, node: number, param: number): Intent => ({ type: "addModEdge", modulator, to: { node, port: { type: "param", index: param } } });
    // Lane 4 (a member of group 0, with a filter 5 and an LFO 6): its LFO drives the instrument's detune, the group's LFO (15) its volume, and
    // a second modulator of the lane (18) the filter's cutoff.
    const server = new MockServer({ snapshot: reach });
    const first = await open(server);
    await first.request({ type: "addModulator", parent: 4, kind: "lfo" });
    await first.request(draw(6, 4, detune));
    await first.request(draw(15, 4, volume));
    await first.request(draw(18, 5, 0));
    // (The graph lists the groups' cables first.)
    const [fromTheGroup, fromTheLane, toTheFilter] = cables(first.state.snapshot);
    if (fromTheGroup?.kind !== "modulation" || fromTheLane?.kind !== "modulation" || toTheFilter?.kind !== "modulation") throw new Error("the three cables were not drawn");
    expect([fromTheGroup, fromTheLane, toTheFilter].map((e) => [e.from.node, e.to.node])).toEqual([
      [15, 4],
      [6, 4],
      [18, 5],
    ]);
    // The depth of the lane's own cable to the detune is driven by the second modulator, which no intent draws: a session that has it is made by hand.
    const snapshot = first.state.snapshot;
    if (!snapshot) throw new Error("no session");
    const drivesTheDepth: Edge = { kind: "modulation", id: 99, amount: 0.5, from: { node: 18, port: { type: "modOut" } }, to: { node: 6, port: { type: "depth", edge: fromTheLane.id } } };
    const byHand: Snapshot = { ...snapshot, graph: { ...snapshot.graph, edges: [...snapshot.graph.edges, drivesTheDepth] } };
    const client = await open(new MockServer({ snapshot: byHand }));

    await client.request(replace(4, "builtin:osc"));
    // What is left is the cable to the filter: the two to the instrument are gone, and the one that drove the depth of one of them.
    expect(cables(client.state.snapshot).map((e) => [e.from.node, e.to.node])).toEqual([[18, 5]]);
  });

  test("fills a lane that has no instrument, which has a name then, and leaves its audio path and its meters as they were", async () => {
    const server = new MockServer({ snapshot: reach });
    const client = await open(server);
    await client.request(replace(14, "builtin:osc"));
    const node = client.state.snapshot?.graph.nodes.find((n) => n.id === 14);
    expect(node).toEqual({ id: 14, parent: null, ...builtinInstruments["builtin:osc"] });
    expect(client.state.snapshot?.graph.edges).toEqual(reach.graph.edges);
    expect(client.state.snapshot?.telemetry).toEqual(reach.telemetry);
  });

  test("is refused for what is not a lane, a plugin the catalog does not name, an effect, or a plugin that cannot be loaded, and changes nothing", async () => {
    const pianoteq = { id: "vst3:Pianoteq 8", name: "Pianoteq 8", format: "VST3", isInstrument: true, params: 300, presets: 12 };
    const server = new MockServer({ catalog: { entries: [...builtinCatalog, pianoteq], scanning: false } });
    const client = await open(server);
    const filterNode = nodeNamed(tracerSnapshot, "Filter");
    const notALane = "an instrument goes in a lane";
    const refused = [
      [replace(filterNode.id, "builtin:osc"), "invalid", notALane],
      [replace(lfo.id, "builtin:osc"), "invalid", notALane],
      [replace(424242, "builtin:osc"), "notFound", "there is no node 424242"],
      [replace(MASTER, "builtin:osc"), "notFound", `there is no node ${String(MASTER)}`],
      [replace(oscillator.id, "./examples/plugin.clap"), "invalid", "./examples/plugin.clap is not a plugin that is installed"],
      [replace(oscillator.id, "builtin:reverb"), "invalid", "Reverb is an effect, not an instrument"],
      [replace(oscillator.id, pianoteq.id), "invalid", "Pianoteq 8 could not be loaded"],
    ] as const;
    for (const [intent, code, message] of refused) {
      const failure = (await client.request(intent).catch((e: unknown) => e)) as RequestError;
      expect([failure.code, failure.message], JSON.stringify(intent)).toEqual([code, message]);
    }
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("asks for the node first and the plugin after, as the controller does: a node that is not a lane is refused whatever the plugin", async () => {
    const server = new MockServer();
    const client = await open(server);
    const failure = (await client.request(replace(lfo.id, "./examples/plugin.clap")).catch((e: unknown) => e)) as RequestError;
    expect(failure.message).toBe("an instrument goes in a lane");
  });

  test("is told to the other clients as a snapshot", async () => {
    const server = new MockServer({ snapshot: edited() });
    const first = await open(server);
    const second = await open(server);
    await first.request(replace(oscillator.id, "builtin:osc"));
    await until(() => second.state.snapshot?.graph.nodes.find((n) => n.id === oscillator.id)?.kind === "instrument" && second.state.snapshot.meta.dirty, "the second client's snapshot");
    expect(second.state.snapshot).toEqual(first.state.snapshot);
  });
});

describe("loading a preset", () => {
  const reach = reachFixture.snapshot;
  const load = (node: number, preset: string): Intent => ({ type: "loadPreset", node, preset });
  /** The reverb of lane 9, which is in a group, and the one on group 2's bus. */
  const LANE_REVERB = 10;
  const BUS_REVERB = 3;
  const paramsOf = (snapshot: Snapshot | null, node: number) => {
    const found = snapshot?.graph.nodes.find((n) => n.id === node);
    if (found?.kind !== "effect") throw new Error(`node ${String(node)} is not an effect`);
    return found.plugin;
  };

  test("is what the controller says it is: the preset's name, and the values the plugin has for its parameters after it", async () => {
    const client = await open(new MockServer({ snapshot: reach }));
    await client.request(load(LANE_REVERB, "cathedral"));
    const plugin = paramsOf(client.state.snapshot, LANE_REVERB);
    expect(plugin.currentPreset).toBe("Cathedral");
    const wanted = builtinPresets["builtin:reverb"]?.cathedral ?? [];
    expect(wanted.length).toBeGreaterThan(5);
    for (const { param, value } of wanted) {
      const row = plugin.params.find((p) => p.index === param);
      // (The value is the parameter's, and its default too: the controller keeps a preset's values as the baseline.)
      expect([row?.value, row?.default], `parameter ${String(param)}`).toEqual([value, value]);
    }
    // The plugin of the other reverb, and the rest of the session, are as they were.
    expect(paramsOf(client.state.snapshot, BUS_REVERB)).toEqual(paramsOf(reach, BUS_REVERB));
    expect(client.state.snapshot?.meta.dirty).toBe(true);
  });

  test("has values of its own for each preset of the reverb, and they name the reverb's parameters", () => {
    const reverb = builtinEffects["builtin:reverb"]?.plugin;
    const presets = builtinPresets["builtin:reverb"] ?? {};
    expect(Object.keys(presets).sort()).toEqual((reverb?.presets ?? []).map((p) => p.id).sort());
    const indices = new Set((reverb?.params ?? []).map((p) => p.index));
    for (const [id, values] of Object.entries(presets)) {
      expect(values.length, id).toBe(indices.size);
      for (const { param } of values) expect(indices.has(param), `${id}: parameter ${String(param)}`).toBe(true);
    }
    // They differ: loading another preset changes something.
    expect(JSON.stringify(presets.cathedral)).not.toBe(JSON.stringify(presets.room));
  });

  test("goes from one preset to another, and the last is the one the plugin has", async () => {
    const client = await open(new MockServer({ snapshot: reach }));
    await client.request(load(LANE_REVERB, "cathedral"));
    await client.request(load(LANE_REVERB, "room"));
    const plugin = paramsOf(client.state.snapshot, LANE_REVERB);
    expect(plugin.currentPreset).toBe("Small Room");
    const room = builtinPresets["builtin:reverb"]?.room ?? [];
    for (const { param, value } of room) expect(plugin.params.find((p) => p.index === param)?.value).toBe(value);
  });

  test("is told to the other clients as the event it is, and their copy of the plugin has the values too", async () => {
    const server = new MockServer({ snapshot: reach });
    const first = await open(server);
    const second = await open(server);
    await first.request(load(LANE_REVERB, "plate"));
    await until(() => paramsOf(second.state.snapshot, LANE_REVERB).currentPreset === "Vintage Plate", "the second client's copy");
    expect(paramsOf(second.state.snapshot, LANE_REVERB)).toEqual(paramsOf(first.state.snapshot, LANE_REVERB));
  });

  test("is refused for a bus effect, a node that has no plugin, an instrument that has none and a preset the plugin does not have, and changes nothing", async () => {
    const client = await open(new MockServer({ snapshot: reach }));
    const refused = [
      [load(BUS_REVERB, "cathedral"), "invalid", "a bus effect has no presets to load"],
      [load(0, "cathedral"), "invalid", "that node has no plugin"], // a group
      [load(15, "cathedral"), "invalid", "that node has no plugin"], // a modulator
      [load(14, "cathedral"), "invalid", "that instrument has no plugin"], // a lane with no instrument
      [load(LANE_REVERB, "no-such-preset"), "invalid", "the plugin has no preset no-such-preset"],
      [load(4, "cathedral"), "invalid", "the plugin has no preset cathedral"], // an oscillator has no presets
      [load(424242, "cathedral"), "notFound", "there is no node 424242"],
      [load(MASTER, "cathedral"), "notFound", `there is no node ${String(MASTER)}`],
    ] as const;
    for (const [intent, code, message] of refused) {
      const failure = (await client.request(intent).catch((e: unknown) => e)) as RequestError;
      expect([failure.code, failure.message], JSON.stringify(intent)).toEqual([code, message]);
    }
    expect(client.state.snapshot).toEqual(reach);
  });

  test("of a plugin the mock has no values for is told with none (a plugin of the machine: its values are its own)", async () => {
    // A node whose plugin is not a built-in, with a preset the mock cannot say anything about.
    const foreign = structuredClone(reach);
    const node = foreign.graph.nodes.find((n) => n.id === LANE_REVERB);
    if (node?.kind !== "effect") throw new Error("no reverb");
    node.plugin = { ...node.plugin, id: "vst3:Reverbator", format: "VST3", presets: [{ id: "big", name: "Big" }] };
    const client = await open(new MockServer({ snapshot: foreign }));
    await client.request(load(LANE_REVERB, "big"));
    const plugin = paramsOf(client.state.snapshot, LANE_REVERB);
    expect(plugin.currentPreset).toBe("Big");
    expect(plugin.params).toEqual(paramsOf(foreign, LANE_REVERB).params);
  });
});

describe("setting the keys of a lane", () => {
  const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
  const keys = (client: Client, node: number = oscillator.id) => {
    const found = client.state.snapshot?.graph.nodes.find((n) => n.id === node);
    if (found?.kind !== "instrument") throw new Error(`node ${String(node)} is not a lane`);
    return found.range;
  };
  const set = (range: { low: number; high: number } | null, node: number = oscillator.id): Intent => ({ type: "setRange", node, range });

  test("is what the controller says it is: the lane has the range, and none is every note", async () => {
    const client = await open(new MockServer());
    expect(keys(client)).toBeNull();
    await client.request(set({ low: 36, high: 84 }));
    expect(keys(client)).toEqual({ low: 36, high: 84 });
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    await client.request(set(null));
    expect(keys(client)).toBeNull();
  });

  test("takes a single note, and the whole keyboard", async () => {
    const client = await open(new MockServer());
    await client.request(set({ low: 60, high: 60 }));
    expect(keys(client)).toEqual({ low: 60, high: 60 });
    await client.request(set({ low: 0, high: 127 }));
    expect(keys(client)).toEqual({ low: 0, high: 127 });
  });

  test("is told to the other clients as the event it is", async () => {
    const server = new MockServer();
    const first = await open(server);
    const second = await open(server);
    await first.request(set({ low: 48, high: 71 }));
    await until(() => keys(second) !== null, "the second client's copy");
    expect(keys(second)).toEqual({ low: 48, high: 71 });
  });

  test("is refused for what is not a lane, and for what is not a range of notes, in the controller's words, and changes nothing", async () => {
    const client = await open(new MockServer({ snapshot: reachFixture.snapshot }));
    const lane = reachFixture.snapshot.graph.nodes.find((n) => n.kind === "instrument" && n.plugin !== null)?.id ?? -1;
    const refused = [
      [set({ low: 40, high: 90 }, 10), "invalid", "a key range belongs to a lane"], // an effect
      [set({ low: 40, high: 90 }, 3), "invalid", "a key range belongs to a lane"], // a bus effect
      [set({ low: 40, high: 90 }, 0), "invalid", "a key range belongs to a lane"], // a group
      [set({ low: 40, high: 90 }, 15), "invalid", "a key range belongs to a lane"], // a modulator
      [set({ low: 40, high: 90 }, 424242), "notFound", "there is no node 424242"],
      [set({ low: 40, high: 90 }, MASTER), "notFound", `there is no node ${String(MASTER)}`],
      [set({ low: 90, high: 40 }, lane), "invalid", "the range goes from F#6 down to E2: its low note has to be the lower"],
      [set({ low: 40, high: 128 }, lane), "invalid", "128 is not a note: the notes are 0 to 127"],
      [set({ low: -1, high: 40 }, lane), "invalid", "-1 is not a note: the notes are 0 to 127"],
      [set({ low: 40.5, high: 60 }, lane), "invalid", "40.5 is not a note: the notes are 0 to 127"],
    ] as const;
    for (const [intent, code, message] of refused) {
      const failure = (await client.request(intent).catch((e: unknown) => e)) as RequestError;
      expect([failure.code, failure.message], JSON.stringify(intent)).toEqual([code, message]);
    }
    expect(client.state.snapshot).toEqual(reachFixture.snapshot);
  });

  test("is for a lane that has no instrument too", async () => {
    const client = await open(new MockServer({ snapshot: reachFixture.snapshot }));
    await client.request(set({ low: 0, high: 59 }, 14));
    expect(keys(client, 14)).toEqual({ low: 0, high: 59 });
  });
});

describe("placing a node", () => {
  const placeOf = (client: Client, node: number) => client.state.snapshot?.graph.nodes.find((n) => n.id === node)?.ui;
  const put = (node: number, ui: { x: number; y: number } | null): Intent => ({ type: "setNodeUi", node, ui });

  test("is what the controller says it is: the node has the place, none gives it back, and the session has something unsaved", async () => {
    const client = await open(new MockServer());
    expect(placeOf(client, filter.id)).toBeUndefined();
    await client.request(put(filter.id, { x: 376, y: -40 }));
    expect(placeOf(client, filter.id)).toEqual({ x: 376, y: -40 });
    expect(client.state.snapshot?.meta.dirty).toBe(true);
    await client.request(put(filter.id, null));
    expect(placeOf(client, filter.id)).toBeUndefined();
  });

  test("is for every node, the master too", async () => {
    const client = await open(new MockServer({ snapshot: reachFixture.snapshot }));
    const nodes = reachFixture.snapshot.graph.nodes;
    expect(nodes.some((node) => node.id === MASTER)).toBe(true);
    for (const [i, node] of nodes.entries()) {
      await client.request(put(node.id, { x: i, y: 1 }));
      expect(placeOf(client, node.id), `node ${String(node.id)}`).toEqual({ x: i, y: 1 });
    }
  });

  test("is kept to a tenth of a pixel, half away from zero, and to the limits of the canvas, as the controller keeps it", async () => {
    const server = new MockServer();
    const client = await open(server);
    const told = async (x: number, y: number) => {
      await client.request(put(filter.id, { x, y }));
      return placeOf(client, filter.id);
    };
    expect(await told(376.38211, -40.04)).toEqual({ x: 376.4, y: -40 });
    expect(await told(0.25, -0.25)).toEqual({ x: 0.3, y: -0.3 });
    expect(await told(1e9, -1e9)).toEqual({ x: NODE_UI_LIMIT, y: -NODE_UI_LIMIT });
    // (The largest a 32-bit number holds: what is beyond is not a number to the controller, which is not asked to keep one.)
    expect(await told(3.4e38, -3.4e38)).toEqual({ x: NODE_UI_LIMIT, y: -NODE_UI_LIMIT });
    // Not `-0`: it would be written to a file as it is. (JSON has no `-0`, so it is the server's own copy that shows it.)
    await told(-0.04, -0);
    const own = server.snapshot.graph.nodes.find((n) => n.id === filter.id)?.ui;
    expect([Object.is(own?.x, 0), Object.is(own?.y, 0)], JSON.stringify(own)).toEqual([true, true]);
  });

  test("is told to the other clients as the event it is", async () => {
    const server = new MockServer();
    const first = await open(server);
    const second = await open(server);
    await first.request(put(lfo.id, { x: 10, y: 20 }));
    await until(() => placeOf(second, lfo.id) !== undefined, "the second client's copy");
    expect(placeOf(second, lfo.id)).toEqual({ x: 10, y: 20 });
  });

  test("is refused for a node that is not there and for a place that is not one, in the controller's words, and changes nothing", async () => {
    const client = await open(new MockServer());
    const refused = [
      [put(424242, { x: 1, y: 1 }), "notFound", "there is no node 424242"],
      [put(424242, null), "notFound", "there is no node 424242"],
    ] as const;
    for (const [intent, code, message] of refused) {
      const failure = (await client.request(intent).catch((e: unknown) => e)) as RequestError;
      expect([failure.code, failure.message], JSON.stringify(intent)).toEqual([code, message]);
    }
    expect(client.state.snapshot).toEqual(tracerSnapshot);
  });

  test("for a place that is not numbers is refused in the server's own process (JSON has no such number to send)", () => {
    const server = new MockServer();
    expect(() => {
      server.apply(put(filter.id, { x: Number.NaN, y: 1 }));
    }).toThrow("(NaN, 1) is not a place");
    expect(() => {
      server.apply(put(filter.id, { x: 1, y: Number.POSITIVE_INFINITY }));
    }).toThrow("(1, Infinity) is not a place");
    expect(server.snapshot).toEqual(tracerSnapshot);
  });
});

describe("the held notes", () => {
  test("are told to a client that asks for them as a value for each note, one for each that any client holds", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const player = await open(server);
    const watcher = await open(server, { telemetry: ["held"] });
    await until(() => watcher.telemetry.has("held"), "the first frame");
    expect(watcher.telemetry.values("held")?.length).toBe(128);
    expect([60, 64].map((note) => watcher.telemetry.isHeld(note))).toEqual([false, false]);
    player.noteOn(60);
    player.noteOn(64);
    await until(() => watcher.telemetry.isHeld(60) && watcher.telemetry.isHeld(64), "the two notes");
    expect(Array.from(watcher.telemetry.values("held") ?? []).filter((value) => value > 0)).toHaveLength(2);
    player.noteOff(60);
    await until(() => !watcher.telemetry.isHeld(60), "the first note ending");
    expect(watcher.telemetry.isHeld(64)).toBe(true);
  });

  test("end with the client that held them", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const player = await open(server);
    const watcher = await open(server, { telemetry: ["held"] });
    player.noteOn(67);
    await until(() => watcher.telemetry.isHeld(67), "the note");
    player.stop();
    await until(() => !watcher.telemetry.isHeld(67), "the note ending with the client");
  });

  test("are a frame as the server makes it: channel 2, no layout, a value for each of the notes", () => {
    const server = new MockServer();
    // (A session that has changed has a revision of its own, which this frame does not carry.)
    server.apply(cutoff(0.25));
    const bytes = server.frame("held", 0, new Map(), 0);
    expect(new Uint8Array(bytes)[0]).toBe(2);
    const decoded = decodeFrame(bytes);
    expect(decoded?.channel).toBe("held");
    expect(decoded?.layoutRev).toBe(0);
    expect(decoded?.values).toHaveLength(128);
  });

  test("are forgotten when the connection is lost: a client that cannot hear the server does not know what is held", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const player = await open(server);
    const watcher = await open(server, { telemetry: ["held"], reconnectDelay: () => 60_000 });
    player.noteOn(60);
    await until(() => watcher.telemetry.isHeld(60), "the note");
    server.drop();
    await until(() => watcher.state.status === "reconnecting", "the loss");
    expect(watcher.telemetry.isHeld(60)).toBe(false);
  });

  test("are known again when the client is back, as they are then", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const player = await open(server);
    const watcher = await open(server, { telemetry: ["held"], reconnectDelay: () => 5 });
    player.noteOn(60);
    await until(() => watcher.telemetry.isHeld(60), "the note");
    server.drop();
    await until(() => watcher.state.reconnects === 1 && watcher.state.status === "open", "the reconnection");
    // (The server ends the notes of a connection that is gone: the one who played is not holding it any more.)
    await until(() => watcher.telemetry.has("held") && !watcher.telemetry.isHeld(60), "the notes after it");
  });

  test("are not told to a client that did not ask for them", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const player = await open(server, { telemetry: ["meters"] });
    player.noteOn(60);
    await quiet(60);
    expect(player.telemetry.has("held")).toBe(false);
  });
});

describe("the scope", () => {
  test("is told to a client that asks for it as the two sides of the output, as long as the real server's", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const watcher = await open(server, { telemetry: ["scope"] });
    await until(() => watcher.telemetry.has("scope"), "the first frame");
    expect(watcher.telemetry.values("scope")?.length).toBe(2 * SCOPE_FRAME_LENGTH);
  });

  test("is a frame as the server makes it: channel 3, no layout, the left side and then the right", () => {
    const server = new MockServer();
    server.apply(cutoff(0.25));
    const bytes = server.frame("scope", 0, new Map(), 0);
    expect(new Uint8Array(bytes)[0]).toBe(3);
    const decoded = decodeFrame(bytes);
    expect(decoded?.channel).toBe("scope");
    expect(decoded?.layoutRev).toBe(0);
    expect(decoded?.values).toHaveLength(2 * SCOPE_FRAME_LENGTH);
  });

  test("is not told to a client that did not ask for it", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const player = await open(server, { telemetry: ["meters"] });
    await until(() => player.telemetry.has("meters"), "the meters");
    await quiet(60);
    expect(player.telemetry.has("scope")).toBe(false);
  });

  /** The latest scope frame of a mock server as its clock says, a side of it. */
  function sideOf(server: MockServer, side: "left" | "right", seconds = 1): Float32Array {
    const decoded = decodeFrame(server.frame("scope", seconds, new Map(), 0));
    const values = decoded?.values ?? new Float32Array(0);
    return side === "left" ? values.slice(0, SCOPE_FRAME_LENGTH) : values.slice(SCOPE_FRAME_LENGTH);
  }

  /** The bin of the loudest frequency in a side, and its level. */
  function loudest(samples: Float32Array): { bin: number; level: number } {
    const levels = new Spectrum(samples.length).decibels(samples);
    const bin = levels.indexOf(Math.max(...levels));
    return { bin, level: levels[bin] ?? Number.NaN };
  }

  /** A mock server with a client that holds these notes, once the server knows it does. */
  async function withNotes(...notes: number[]): Promise<MockServer> {
    const server = new MockServer({ telemetryPeriod: 5 });
    const player = await open(server);
    for (const note of notes) player.noteOn(note);
    await until(() => notes.every((note) => server.heldNotes.has(note)), "the notes");
    return server;
  }

  test("is silence, on both sides, while no note is held", () => {
    const server = new MockServer();
    expect(Array.from(sideOf(server, "left")).every((s) => s === 0)).toBe(true);
    expect(Array.from(sideOf(server, "right")).every((s) => s === 0)).toBe(true);
  });

  test("is a sound at the pitch of the note that is held, as the mock has no engine to hear: at the frequency of A4, the same on both sides", async () => {
    const server = await withNotes(69);
    const left = sideOf(server, "left");
    // 440 Hz at 48 kHz is between the bins 18 (422 Hz) and 19 (445 Hz) of a block of 2048.
    expect([18, 19]).toContain(loudest(left).bin);
    expect(Array.from(sideOf(server, "right"))).toEqual(Array.from(left));
    const peak = Math.max(...Array.from(left, Math.abs));
    expect(peak).toBeGreaterThan(0.1);
    expect(peak).toBeLessThanOrEqual(0.35 + 1e-6);
  });

  test("goes up an octave with the note, and has a note's own frequency in each that is held", async () => {
    // 880 Hz is the bin 37.5 (a bin is 23.4 Hz).
    expect([37, 38]).toContain(loudest(sideOf(await withNotes(81), "left")).bin);
    const server = await withNotes(57, 69);
    const levels = new Spectrum(SCOPE_FRAME_LENGTH).decibels(sideOf(server, "left"));
    // 220 Hz is the bin 9.4 and 440 Hz the bin 18.8: both are over what is between them, which is not a sound.
    expect(Math.max(levels[9] ?? -999, levels[10] ?? -999)).toBeGreaterThan((levels[14] ?? 0) + 20);
    expect(Math.max(levels[18] ?? -999, levels[19] ?? -999)).toBeGreaterThan((levels[14] ?? 0) + 20);
  });

  test("goes on from one frame to the next: a note that is held is the same wave a moment later, a period on", async () => {
    const server = await withNotes(69);
    // The 440 Hz of A4 goes round 440 times in a second: a second on is the same place on the wave, and so is a tenth of a second (44 turns).
    const now = sideOf(server, "left", 1);
    const later = sideOf(server, "left", 1.1);
    for (let n = 0; n < 100; n += 1) expect(later[n] ?? 9).toBeCloseTo(now[n] ?? -9, 3);
    const apart = sideOf(server, "left", 1 + 1 / 880);
    expect(Math.abs((apart[0] ?? 0) - (now[0] ?? 0))).toBeGreaterThan(0.01);
  });

  test("is no louder for the notes of a chord than for one note", async () => {
    const server = await withNotes(60, 64, 67, 72);
    expect(Math.max(...Array.from(sideOf(server, "left"), Math.abs))).toBeLessThanOrEqual(0.35 + 1e-6);
  });

  test("has the newest frame last: a sample's time on, the wave has gone one place down the frame", async () => {
    const server = await withNotes(69);
    const rate = server.snapshot.meta.sampleRate;
    const now = sideOf(server, "left", 1.0003);
    const later = sideOf(server, "left", 1.0003 + 1 / rate);
    for (let place = 1; place < SCOPE_FRAME_LENGTH; place += 1) expect(later[place - 1] ?? 9).toBeCloseTo(now[place] ?? -9, 4);
  });

  test("is forgotten when the connection is lost: a client that cannot hear the server does not know what the output is", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const player = await open(server);
    const watcher = await open(server, { telemetry: ["scope"], reconnectDelay: () => 60_000 });
    player.noteOn(69);
    await until(() => Math.max(...Array.from(watcher.telemetry.values("scope") ?? [], Math.abs)) > 0.1, "the sound");
    server.drop();
    await until(() => watcher.state.status === "reconnecting", "the loss");
    expect(Array.from(watcher.telemetry.values("scope") ?? []).every((s) => s === 0)).toBe(true);
  });
});

describe("a channel that is asked for for as long as something shows it", () => {
  test("comes in addition to the channels the client was started with, and stops when the one that asked lets go, the others going on", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const client = await open(server, { telemetry: ["meters"] });
    await until(() => client.telemetry.framesOf("meters") > 0, "meters");
    expect(client.telemetry.framesOf("scope")).toBe(0);
    const release = client.watchTelemetry("scope");
    await until(() => client.telemetry.framesOf("scope") > 2, "scope frames");
    release();
    await quiet(30); // (frames that were on their way)
    const frames = client.telemetry.framesOf("scope");
    const meters = client.telemetry.framesOf("meters");
    await quiet(80);
    expect(client.telemetry.framesOf("scope"), "no more of the scope").toBe(frames);
    expect(client.telemetry.framesOf("meters"), "the meters go on").toBeGreaterThan(meters);
  });

  test("is asked for until the last of those that asked lets go, and letting go twice is letting go once", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const client = await open(server, { telemetry: [] });
    const first = client.watchTelemetry("scope");
    const second = client.watchTelemetry("scope");
    await until(() => client.telemetry.framesOf("scope") > 1, "scope frames");
    first();
    first();
    await quiet(30);
    const frames = client.telemetry.framesOf("scope");
    await until(() => client.telemetry.framesOf("scope") > frames + 2, "the scope going on for the one that is left");
    second();
    await quiet(30);
    const last = client.telemetry.framesOf("scope");
    await quiet(80);
    expect(client.telemetry.framesOf("scope")).toBe(last);
  });

  test("is kept by a new list of channels, which does not say what is watched", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const client = await open(server, { telemetry: ["meters"] });
    client.watchTelemetry("scope");
    client.setTelemetry(["modulation"]);
    await until(() => client.telemetry.framesOf("scope") > 1, "scope frames");
    await until(() => client.telemetry.framesOf("modulation") > 0, "modulation frames");
    const meters = client.telemetry.framesOf("meters");
    await quiet(80);
    expect(client.telemetry.framesOf("meters"), "the list was replaced").toBe(meters);
  });

  test("is asked for on every connection, the one that was lost and the one that follows", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const client = await open(server, { telemetry: [], reconnectDelay: () => 5 });
    client.watchTelemetry("scope");
    await until(() => client.telemetry.framesOf("scope") > 1, "scope frames");
    server.drop();
    await until(() => client.state.reconnects === 1 && client.state.status === "open", "the reconnection");
    const frames = client.telemetry.framesOf("scope");
    await until(() => client.telemetry.framesOf("scope") > frames + 2, "scope frames after it");
  });

  test("is asked for when the connection is made, if something asked before", async () => {
    const server = new MockServer({ telemetryPeriod: 5 });
    const client = connect(server, { telemetry: [] });
    client.watchTelemetry("scope");
    await until(() => client.telemetry.framesOf("scope") > 1, "scope frames");
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
  function withWaveform(waveform: Waveform): Snapshot {
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

  /** What the LFO of a `withWaveform` tracer puts out at each of some times (the clock is set to each, and a frame waited for). */
  async function lfoOutputAt(waveform: Waveform, times: readonly number[]): Promise<number[]> {
    const time = clock();
    const server = new MockServer({ ...fast, clock: time.now, snapshot: withWaveform(waveform) });
    const client = await open(server, { telemetry: ["modulation"] });
    const outputs: number[] = [];
    for (const seconds of times) {
      time.set(seconds);
      const frames = client.telemetry.frames;
      await until(() => client.telemetry.frames > frames + 1, "a frame");
      outputs.push(client.telemetry.value("modulation", lfo.id, { type: "modOut" }, "modOutput") ?? Number.NaN);
    }
    client.stop();
    return outputs;
  }

  test("makes a random LFO's output the number of its cycle, from the engine's hash with the node's id for the seed, held until the next cycle", async () => {
    // The LFO runs at half a hertz, so that a cycle is two seconds: these times are in cycles 0, 0, 1, 1, 2 and 7.
    const outputs = await lfoOutputAt("random", [0.1, 1.9, 2.1, 3.9, 4.5, 14.5]);
    expect(outputs).toEqual([0, 0, 1, 1, 2, 7].map((cycle) => noise(lfo.id, cycle)));
    expect(new Set(outputs).size, "a number for each cycle").toBe(4);
  });

  test("makes a smooth LFO's output glide from the number of its cycle to that of the next, easing at both ends", async () => {
    // At half a hertz the phase is 0, 0.25, 0.5 and 0.75 at 0, 0.5, 1 and 1.5 seconds, and the next cycle begins at the number that this one ended at.
    const outputs = await lfoOutputAt("smooth", [0, 0.5, 1, 1.5, 2]);
    const [from, to] = [noise(lfo.id, 0), noise(lfo.id, 1)];
    const eased = (phase: number): number => from + (to - from) * (phase * phase * (3 - 2 * phase));
    [eased(0), eased(0.25), eased(0.5), eased(0.75), to].forEach((expected, i) => {
      expect(outputs[i], `at ${String(i * 0.5)} seconds`).toBeCloseTo(expected, 6);
    });
    expect(outputs[2]).toBeCloseTo((from + to) / 2, 6);
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

  test("tells what an envelope makes of the rate of an LFO and of the depth of its cable, as the engine does", async () => {
    // (Finding f132: the knob of a setting that another modulator drives showed nothing.) An envelope drawn to the LFO's rate, which is 0.5 Hz (0.01 to 50, turned on a log scale),
    // and to the depth of the LFO's cable, which is 0.4 (-1 to 1): at rest the value is the user's less depth of the way down to the lowest, down the knob for the rate, and an open
    // envelope holds the sustain.
    const server = new MockServer(fast);
    const client = await open(server, { telemetry: ["modulation"] });
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    await client.request({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
    const envelope = client.state.snapshot?.graph.nodes.find((n) => n.name === "ADSR");
    const edge = tracerSnapshot.graph.edges.find((e) => e.kind === "modulation");
    expect(edge?.kind === "modulation" ? edge.amount : undefined).toBe(0.4);
    await client.request({ type: "addModEdge", modulator: envelope?.id ?? -1, to: { node: lfo.id, port: { type: "rate" } } });
    await client.request({ type: "addModEdge", modulator: envelope?.id ?? -1, to: { node: lfo.id, port: { type: "depth", edge: edge?.kind === "modulation" ? edge.id : -1 } } });
    const rate = (): number | undefined => client.telemetry.value("modulation", lfo.id, { type: "rate" }, "effective");
    const depth = (): number | undefined => client.telemetry.value("modulation", lfo.id, { type: "depth", edge: edge?.kind === "modulation" ? edge.id : -1 }, "effective");
    const place = fractionOf(0.5, 0.01, 50, "log");
    const rateAt = (taken: number): number => valueAt(place - taken * place, 0.01, 50, "log");
    await until(() => Math.abs((rate() ?? 0) - rateAt(0.5 * 1)) < 1e-5, "the rate at rest");
    await until(() => Math.abs((depth() ?? 9) - (0.4 - 0.5 * 1 * (0.4 + 1))) < 1e-5, "the depth at rest");
    client.noteOn(60);
    await until(() => Math.abs((rate() ?? 0) - rateAt(0.5 * 0.3)) < 1e-5, "the rate with the envelope open");
    await until(() => Math.abs((depth() ?? 9) - (0.4 - 0.5 * 0.3 * (0.4 + 1))) < 1e-5, "the depth with the envelope open");
  });
});

describe("the value of a setting that an LFO drives, and of each stage of an envelope", () => {
  test("is the user's value and what the LFO adds to it, by the knob's travel for a rate, which is turned on a log scale", async () => {
    // A second LFO (a sine, at its default rate) drives the rate of the first at the cable's default depth, 0.5; a quarter of a period in it is at its top.
    const time = { seconds: 0.25 / MODULATOR_FIELD_DEFAULTS.rate };
    const server = new MockServer({ telemetryPeriod: 2, clock: () => time.seconds });
    const client = await open(server, { telemetry: ["modulation"] });
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    await client.request({ type: "addModulator", parent: oscillator.id, kind: "lfo" });
    const second = (client.state.snapshot?.graph.nodes ?? []).filter((n) => n.kind === "modulator" && n.id !== lfo.id).at(-1);
    await client.request({ type: "addModEdge", modulator: second?.id ?? -1, to: { node: lfo.id, port: { type: "rate" } } });
    const rate = (): number | undefined => client.telemetry.value("modulation", lfo.id, { type: "rate" }, "effective");
    const [low, high] = MODULATOR_FIELD_RANGES.rate;
    const set = lfo.kind === "modulator" && lfo.source.type === "lfo" ? lfo.source.rate : Number.NaN;
    await until(() => Math.abs((rate() ?? 0) - valueAt(fractionOf(set, low, high, "log") + 1 * 0.5, low, high, "log")) < 1e-4, "the rate with the second LFO at its top");
  });

  test("is kept to the range for a setting that is turned by distances, a sustain, when the LFO would take it past the end, and for a setting on the knob, a rate, when it would take it past either", async () => {
    // A second LFO at its top (a sine, a quarter of a period in) drives the sustain of an envelope, 0.7 on 0 to 1, at depth 1: 0.7 + 1 x 1 is past the top.
    const time = { seconds: 0.25 / MODULATOR_FIELD_DEFAULTS.rate };
    const server = new MockServer({ telemetryPeriod: 2, clock: () => time.seconds });
    const client = await open(server, { telemetry: ["modulation"] });
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    await client.request({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
    await client.request({ type: "addModulator", parent: oscillator.id, kind: "lfo" });
    const nodes = client.state.snapshot?.graph.nodes ?? [];
    const envelope = nodes.filter((n) => n.kind === "modulator" && n.source.type === "envelope").at(-1);
    const second = nodes.filter((n) => n.kind === "modulator" && n.source.type === "lfo" && n.id !== lfo.id).at(-1);
    await client.request({ type: "addModEdge", modulator: second?.id ?? -1, to: { node: envelope?.id ?? -1, port: { type: "sustain" } } });
    await client.request({ type: "addModEdge", modulator: second?.id ?? -1, to: { node: lfo.id, port: { type: "rate" } } });
    const cables = (client.state.snapshot?.graph.edges ?? []).filter((e) => e.kind === "modulation" && e.from.node === second?.id);
    for (const cable of cables) await client.request({ type: "setEdgeDepth", edge: cable.kind === "modulation" ? cable.id : -1, depth: 1 });
    const sustain = (): number | undefined => client.telemetry.value("modulation", envelope?.id ?? -1, { type: "sustain" }, "effective");
    const rate = (): number | undefined => client.telemetry.value("modulation", lfo.id, { type: "rate" }, "effective");
    await until(() => sustain() === MODULATOR_FIELD_RANGES.sustain[1], "the sustain at the top of its range");
    // The rate, 0.5 Hz, is two thirds of the way up its knob and a whole travel more is past the top: it is at the highest value, and not above it.
    await until(() => rate() === MODULATOR_FIELD_RANGES.rate[1], "the rate at the top of its range");
  });

  test.each(["attack", "decay", "sustain", "release"] as const)("is, for the %s of an envelope that another envelope drives, the user's value less what is taken at rest", async (stage) => {
    const server = new MockServer({ telemetryPeriod: 2 });
    const client = await open(server, { telemetry: ["modulation"] });
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    await client.request({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
    await client.request({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
    const [driver, driven] = (client.state.snapshot?.graph.nodes ?? []).filter((n) => n.name === "ADSR");
    await client.request({ type: "addModEdge", modulator: driver?.id ?? -1, to: { node: driven?.id ?? -1, port: { type: stage } } });
    const value = (): number | undefined => client.telemetry.value("modulation", driven?.id ?? -1, { type: stage }, "effective");
    // The driver is at rest (no note): the value is the user's less half the way down to the lowest, the cable being at half depth: down the knob for a time, which is turned on a log
    // scale, and down the range for the sustain, a level. (Each stage has a value of its own.)
    const base = MODULATOR_FIELD_DEFAULTS[stage];
    const [min, max] = MODULATOR_FIELD_RANGES[stage];
    const expected = stage === "sustain" ? base - 0.5 * (base - min) : valueAt(fractionOf(base, min, max, "log") * 0.5, min, max, "log");
    await until(() => Math.abs((value() ?? 99) - expected) < 1e-5, `the ${stage} at rest`);
  });
});

describe("the value of a setting that two cables drive", () => {
  test("is what the last of them makes of it, as the engine's writes are last-wins, and one entry of the layout names it", async () => {
    // Two envelopes at rest drive the rate of the LFO (0.5 Hz, 0.01 to 50, turned on a log scale), the first at depth 0.5 and the second at 0.2: each pulls it down by its depth of the
    // way down the knob to the lowest, and the later one writes last.
    const client = await open(new MockServer({ telemetryPeriod: 2 }), { telemetry: ["modulation"] });
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    await client.request({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
    await client.request({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
    const [first, second] = (client.state.snapshot?.graph.nodes ?? []).filter((n) => n.name === "ADSR");
    for (const modulator of [first, second]) await client.request({ type: "addModEdge", modulator: modulator?.id ?? -1, to: { node: lfo.id, port: { type: "rate" } } });
    const cables = (client.state.snapshot?.graph.edges ?? []).filter((e) => e.kind === "modulation" && e.to.port.type === "rate");
    const [one, two] = cables.map((e) => (e.kind === "modulation" ? e.id : -1));
    await client.request({ type: "setEdgeDepth", edge: one ?? -1, depth: 0.5 });
    await client.request({ type: "setEdgeDepth", edge: two ?? -1, depth: 0.2 });
    const named = (client.state.snapshot?.telemetry.modulation ?? []).filter((e) => e.kind === "effective" && e.node === lfo.id && e.port.type === "rate");
    expect(named, "the layout names the value once").toHaveLength(1);
    const rate = (): number | undefined => client.telemetry.value("modulation", lfo.id, { type: "rate" }, "effective");
    const place = fractionOf(0.5, 0.01, 50, "log");
    await until(() => Math.abs((rate() ?? 0) - valueAt(place - 0.2 * 1 * place, 0.01, 50, "log")) < 1e-5, "the rate the last cable makes of it");
  });
});

describe("the value of a parameter that several cables drive", () => {
  const cutoffOf = (client: Client): number | undefined => client.telemetry.value("modulation", filter.id, { type: "param", index: 0 }, "effective");
  const resonanceOf = (client: Client): number | undefined => client.telemetry.value("modulation", filter.id, { type: "param", index: 1 }, "effective");
  /** The group that the oscillator's lane is put in, with an LFO of the group's own (a sine at its default rate) that has a cable to the cutoff and to the resonance. */
  async function groupWithAnLfo(client: Client): Promise<void> {
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    await client.request({ type: "addGroup", members: [oscillator.id] });
    const group = (client.state.snapshot?.graph.nodes ?? []).find((n) => n.kind === "group");
    await client.request({ type: "addModulator", parent: group?.id ?? -1, kind: "lfo" });
    const groupLfo = (client.state.snapshot?.graph.nodes ?? []).find((n) => n.kind === "modulator" && n.parent === group?.id);
    for (const index of [0, 1]) await client.request({ type: "addModEdge", modulator: groupLfo?.id ?? -1, to: { node: filter.id, port: { type: "param", index } } });
  }

  test("is the user's value and what each cable makes of it, added, as the engine adds them: an envelope after an LFO takes its share of the value the user set, not of the LFO's swing", async () => {
    // The tracer's LFO (0.5 Hz, depth 0.4) is at its top at 0.5 s and puts the cutoff, which the user set to 0.5, 0.4 up. An envelope at rest and at the default depth, 0.5, drawn
    // after it takes half of the way from the user's value to the lowest: 0.25 off, whatever the LFO has done.
    const server = new MockServer({ telemetryPeriod: 2, clock: () => 0.5 });
    const client = await open(server, { telemetry: ["modulation"] });
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    await client.request({ type: "addModulator", parent: oscillator.id, kind: "envelope" });
    const envelope = (client.state.snapshot?.graph.nodes ?? []).find((n) => n.name === "ADSR");
    await client.request({ type: "addModEdge", modulator: envelope?.id ?? -1, to: { node: filter.id, port: { type: "param", index: 0 } } });
    await until(() => Math.abs((cutoffOf(client) ?? 0) - (0.5 + 0.4 - 0.25)) < 1e-5, "the cutoff with both");
    // With a note held the envelope is at its sustain, 0.7, and takes 0.3 of the 0.5: 0.075.
    client.noteOn(60);
    await until(() => Math.abs((cutoffOf(client) ?? 0) - (0.5 + 0.4 - 0.075)) < 1e-5, "the cutoff with both, the envelope open");
  });

  test("is stopped at the ends of the parameter's range, as the engine clamps the sum", async () => {
    // The LFO is at its top and puts the cutoff 0.4 up: from 0.9 that is 1.3, and the cutoff runs 0 to 1.
    const server = new MockServer({ telemetryPeriod: 2, clock: () => 0.5 });
    const client = await open(server, { telemetry: ["modulation"] });
    await client.request(cutoff(0.9));
    await until(() => cutoffOf(client) === 1, "the cutoff at the top of its range");
  });

  test("is what the lane's own modulators make of it alone where they drive it too: the group's offset is not added, as the engine does it", async () => {
    // A quarter of a period of the group's LFO (1 Hz, at its top) is 0.25 s, when the lane's LFO (0.5 Hz) is at sin(pi/4) of the way: the group's LFO would add 0.5 to the cutoff.
    const server = new MockServer({ telemetryPeriod: 2, clock: () => 0.25 / MODULATOR_FIELD_DEFAULTS.rate });
    const client = await open(server, { telemetry: ["modulation"] });
    await groupWithAnLfo(client);
    await until(() => Math.abs((cutoffOf(client) ?? 9) - (0.5 + 0.4 * Math.sin(Math.PI / 4))) < 1e-5, "the cutoff the lane's LFO makes of it");
  });

  test("is what the group's modulators make of it where no modulator of the lane drives it", async () => {
    const server = new MockServer({ telemetryPeriod: 2, clock: () => 0.25 / MODULATOR_FIELD_DEFAULTS.rate });
    const client = await open(server, { telemetry: ["modulation"] });
    await groupWithAnLfo(client);
    // The group's LFO is at its top and adds its depth, 0.5, of the range of the resonance (0.5 to 10) to the 2 the user set.
    await until(() => Math.abs((resonanceOf(client) ?? 0) - (2 + 0.5 * (10 - 0.5))) < 1e-5, "the resonance the group's LFO makes of it");
  });
});

describe("what a MIDI control puts out and does in the mock engine, which has no MIDI: it rests where the engine's does (f137)", () => {
  const withControl = (control: { type: "cc"; number: number } | { type: "pitchBend" } | { type: "aftertouch" } | null): Snapshot => {
    const nodes = tracerSnapshot.graph.nodes.map((node) =>
      node.kind === "modulator" ? { ...node, source: { type: "midiLearn" as const, control, learning: control === null } } : node,
    );
    return { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } };
  };
  const cutoffOf = (client: Client): number | undefined => client.telemetry.value("modulation", filter.id, { type: "param", index: 0 }, "effective");
  const outputOf = (client: Client): number | undefined => client.telemetry.value("modulation", lfo.id, { type: "modOut" }, "modOutput");

  test.each([
    ["not bound yet", null, 0],
    ["a CC", { type: "cc", number: 74 }, 0],
    ["key pressure", { type: "aftertouch" }, 0],
    ["the pitch bend wheel", { type: "pitchBend" }, 0.5],
  ] as const)("puts out what it rests at when it is %s: nothing, and the middle for the pitch bend wheel", async (_name, control, rest) => {
    const client = await open(new MockServer({ telemetryPeriod: 2, snapshot: withControl(control) }), { telemetry: ["modulation"] });
    // (The buffers hold 0 until a frame comes: a rest of 0 is looked at after two.)
    await until(() => client.telemetry.frames > 1, "frames");
    expect(outputOf(client)).toBe(rest);
  });

  test.each([
    ["not bound yet", null],
    ["a CC", { type: "cc", number: 74 }],
    ["key pressure", { type: "aftertouch" }],
    ["the pitch bend wheel", { type: "pitchBend" }],
  ] as const)("leaves a parameter where the user set it when it is %s and at rest", async (_name, control) => {
    // The cutoff, which the user set to 0.5, has the cable of the tracer's LFO at depth 0.4, from this control now.
    const client = await open(new MockServer({ telemetryPeriod: 2, snapshot: withControl(control) }), { telemetry: ["modulation"] });
    await until(() => cutoffOf(client) === 0.5, "the cutoff where the user set it");
  });

  test("leaves the setting of a modulator where the user set it too, for the pitch bend wheel at rest and for a control that is not bound", async () => {
    for (const control of [null, { type: "pitchBend" } as const]) {
      const server = new MockServer({ telemetryPeriod: 2, snapshot: withControl(control) });
      const client = await open(server, { telemetry: ["modulation"] });
      const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
      await client.request({ type: "addModulator", parent: oscillator.id, kind: "lfo" });
      const second = (client.state.snapshot?.graph.nodes ?? []).filter((n) => n.kind === "modulator" && n.id !== lfo.id).at(-1);
      // The control drives the rate of the LFO that is new (a rate of 1 Hz, which the user set): the rate is 1.
      await client.request({ type: "addModEdge", modulator: lfo.id, to: { node: second?.id ?? -1, port: { type: "rate" } } });
      const rate = (): number | undefined => client.telemetry.value("modulation", second?.id ?? -1, { type: "rate" }, "effective");
      await until(() => rate() === MODULATOR_FIELD_DEFAULTS.rate, `the rate where the user set it, for ${JSON.stringify(control)}`);
      client.stop();
    }
  });
});

/** A transport to a mock server whose words to the client can be held back, and let through all at once. */
function gated(server: MockServer): { transport: TransportFactory; hold: () => void; release: () => void } {
  let holding = false;
  const held: (() => void)[] = [];
  const transport: TransportFactory = () => {
    const inner = server.transport();
    return {
      connect: (events) => {
        inner.connect({
          ...events,
          text: (text) => {
            if (holding) {
              held.push(() => {
                events.text(text);
              });
            } else {
              events.text(text);
            }
          },
        });
      },
      send: (text) => {
        inner.send(text);
      },
      close: () => {
        inner.close();
      },
    };
  };
  return {
    transport,
    hold: () => {
      holding = true;
    },
    release: () => {
      holding = false;
      for (const tell of held.splice(0)) tell();
    },
  };
}

describe("a request that the server is slow to answer", () => {
  const slow = (client: Client): number => client.slow.getState().count;
  const slowClient = async (slowAfter = 30) => {
    const server = new MockServer();
    const connection = gated(server);
    const client = await open(server, { transport: connection.transport, slowAfter });
    return { client, connection, server };
  };

  test("is counted as slow once it has waited as long as the client was told to, and not before", async () => {
    const { client, connection } = await slowClient(60);
    expect(slow(client)).toBe(0);
    connection.hold();
    const answered = client.request(cutoff(0.3));
    expect(slow(client)).toBe(0);
    await quiet(20);
    expect(slow(client)).toBe(0);
    // (Within a quarter of a second, which is not the time that a client that is not told waits: it was told.)
    await until(() => slow(client) === 1, "the request to be slow", 250);
    connection.release();
    await answered;
    expect(slow(client)).toBe(0);
  });

  test("waits 400 milliseconds when it is not told how long", async () => {
    const server = new MockServer();
    const connection = gated(server);
    const client = await open(server, { transport: connection.transport });
    expect(SLOW_AFTER).toBe(400);
    connection.hold();
    const answered = client.request(cutoff(0.3));
    await quiet(250);
    expect(slow(client)).toBe(0);
    await until(() => slow(client) === 1, "the request to be slow", 1500);
    connection.release();
    await answered;
  });

  test("is never slow when it is answered in time", async () => {
    // (A time to wait that a stall of the machine is not likely to be longer than: the answer is a few milliseconds away.)
    const { client } = await slowClient(250);
    const seen: number[] = [];
    client.slow.subscribe((state) => seen.push(state.count));
    await client.request(cutoff(0.3));
    await quiet(320);
    expect(seen).toEqual([]);
    expect(slow(client)).toBe(0);
  });

  test("is not slow any more when it is refused, and the refusal is told as it always was", async () => {
    const { client, connection } = await slowClient();
    connection.hold();
    const refused = client.request({ type: "setParam", node: 99_999, param: 0, value: 1 }).then(
      () => null,
      (error: unknown) => error,
    );
    await until(() => slow(client) === 1, "the request to be slow");
    connection.release();
    expect(await refused).toBeInstanceOf(RequestError);
    expect(slow(client)).toBe(0);
  });

  test("is counted once each, and the stopping of the client takes them all out, rejected as they always were", async () => {
    const { client, connection } = await slowClient();
    connection.hold();
    const results = [client.request(cutoff(0.1)), client.request(cutoff(0.2)), client.request(cutoff(0.3))].map((request) => request.then(() => "answered", () => "rejected"));
    await until(() => slow(client) === 3, "three slow requests");
    client.stop();
    expect(await Promise.all(results)).toEqual(["rejected", "rejected", "rejected"]);
    expect(slow(client)).toBe(0);
  });

  test("is not slow any more when the connection is lost before the answer: it is rejected as it was, and is not counted again by the one that is made good", async () => {
    const { client, connection, server } = await slowClient();
    connection.hold();
    const lost = client.request(cutoff(0.3)).then(
      () => "answered",
      (error: unknown) => (error instanceof RequestError ? "rejected" : "something else"),
    );
    await until(() => slow(client) === 1, "the request to be slow");
    server.drop();
    expect(await lost).toBe("rejected");
    expect(slow(client)).toBe(0);
    connection.release(); // (what the old connection was holding, and the words of the new one)
    await until(() => client.state.status === "open", "the reconnection");
    await quiet(80);
    expect(slow(client)).toBe(0);
  });

  test("is counted when it is the second that is slow: the first, answered, is not", async () => {
    const { client, connection } = await slowClient(40);
    await client.request(cutoff(0.1));
    connection.hold();
    const second = client.request(cutoff(0.2));
    await until(() => slow(client) === 1, "the second to be slow");
    connection.release();
    await second;
    expect(slow(client)).toBe(0);
  });

  test("does not touch the state of the session, which every control is listening to", async () => {
    const { client, connection } = await slowClient();
    connection.hold();
    let told = 0;
    client.store.subscribe(() => {
      told += 1;
    });
    const answered = client.request(cutoff(0.3));
    await until(() => slow(client) === 1, "the request to be slow");
    expect(told).toBe(0);
    connection.release();
    await answered;
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

  test("a catalog replaces the one before it, whole", async () => {
    const server = new MockServer();
    const entry = (id: string): CatalogEntry => ({ id, name: id, format: "CLAP", isInstrument: false, params: 1, presets: 0 });
    const client = scripted([
      server.hello(),
      { type: "snapshot", snapshot: tracerSnapshot },
      { type: "catalog", rev: 1, scanning: true, entries: [entry("a"), entry("b")] },
      { type: "catalog", rev: 2, scanning: false, entries: [entry("c")] },
    ]);
    await until(() => client.state.catalog?.scanning === false, "the second catalog");
    expect(client.state.catalog).toEqual({ scanning: false, entries: [entry("c")] });
  });

  test("a history replaces the one before it, whole, and one that says the same changes nothing, one that changes only what can be redone or only what can be undone included", async () => {
    const server = new MockServer();
    const client = scripted([
      server.hello(),
      { type: "snapshot", snapshot: tracerSnapshot },
      { type: "history", undo: "set cutoff", redo: null },
      { type: "history", undo: "set cutoff", redo: null },
      { type: "history", undo: "set cutoff", redo: "set mix" },
      { type: "history", undo: null, redo: "set mix" },
    ]);
    // (The messages come in one go, after this: every change of the store is counted from here.)
    let seen = client.state.history;
    let changes = 0;
    client.store.subscribe((state) => {
      if (state.history !== seen) {
        seen = state.history;
        changes += 1;
      }
    });
    await until(() => client.state.history.undo === null && client.state.history.redo !== null, "the last history");
    expect(client.state.history).toEqual({ undo: null, redo: "set mix" });
    expect(changes).toBe(3);
  });

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
