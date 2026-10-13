// The mock engine: a stand-in for the server that lives in the page (or in a test) and
// speaks the same protocol, so that the interface can be built, shown in Storybook and
// tested without audio hardware or a running `tang serve` (docs/web-ui.md §9.2). It
// serves a snapshot, applies intents the way the controller does (`src/app/apply.rs`:
// clamp, round an enum, one revision per change), tells every subscribed connection what
// changed, and, for the connections that ask, makes telemetry up: the LFOs run, the
// parameters they drive follow them, and the master's meter rises while a note is held.
import { DEPTH_RANGE, GAIN_RANGES, MODULATOR_FIELD_RANGES, PROTOCOL } from "../protocol";
import type {
  ClientMessage,
  Edge,
  EdgeId,
  Event,
  Intent,
  LayoutEntry,
  ModulatorField,
  ModulatorKind,
  ModulatorSource,
  Node,
  NodeId,
  Plugin,
  Port,
  ServerMessage,
  Snapshot,
  StampedEvent,
  TelemetryChannel,
  TelemetryLayout,
  WireError,
} from "../protocol";
import type { Transport, TransportEvents, TransportFactory } from "./connection";
import { tracerSnapshot } from "./fixtures";
import { applyEvent } from "./reducers";

export interface MockOptions {
  /** The session it serves; the tracer by default. */
  snapshot?: Snapshot;
  /** A token the server wants first, as one that listens beyond loopback does. */
  token?: string;
  /** What `hello` says the protocol is, for testing a client against a server that is not its own. */
  protocol?: number;
  /** Milliseconds between the telemetry frames it makes (33 by default, like the real one). */
  telemetryPeriod?: number;
  /** The time the made-up telemetry follows, in seconds; the clock of the page by default. */
  clock?: () => number;
}

/**
 * What an intent comes to: the event it makes; or, if it changed the structure of the session, the session
 * as it is then, which the clients are given whole (the controller leaves that to its one projection of the
 * graph, `Applied::restructured`); or why it cannot be done.
 */
type Outcome = { event: Event } | { restructured: Snapshot } | { error: WireError };

const clamp = (value: number, low: number, high: number): number => Math.min(Math.max(value, low), high);

/**
 * Whether the controller can hold a number: its numbers are 32 bits, and one beyond them is infinity there (JSON
 * has no infinity to send; a number a person types, or a script sends, can be 1e39).
 */
const holds = (value: number): boolean => Number.isFinite(Math.fround(value));

/** A number rounded the way the controller does, in 32 bits: 0.49999999 is 0.5 there, which rounds up. */
const roundIn32Bits = (value: number): number => Math.round(Math.fround(value));

function pluginOf(node: Node): Plugin | null {
  return node.kind === "instrument" || node.kind === "effect" ? node.plugin : null;
}

function invalid(message: string): Outcome {
  return { error: { code: "invalid", message } };
}

/** A setting of a modulator: clamped to its range, rounded if it is an index, and only if the modulator has it. */
function fieldOutcome(node: Node, field: ModulatorField, value: number): Outcome {
  if (node.kind !== "modulator") return invalid("that node is not a modulator");
  if (!holds(value)) return invalid(`${value} is not a value for ${field}`);
  const [low, high] = MODULATOR_FIELD_RANGES[field];
  const clamped = clamp(value, low, high);
  const { type } = node.source;
  const fits = type === "lfo" ? field === "rate" || field === "waveform" : type === "envelope" && field !== "rate" && field !== "waveform";
  if (!fits) return invalid(`${field} does not apply to that modulator`);
  return { event: { type: "modulatorFieldSet", node: node.id, field, value: field === "waveform" ? roundIn32Bits(clamped) : clamped } };
}

/** The depth of a cable: clamped, and only of a cable that is there. */
function depthOutcome(snapshot: Snapshot, edge: number, depth: number): Outcome {
  if (!snapshot.graph.edges.some((e) => e.kind === "modulation" && e.id === edge)) {
    return { error: { code: "notFound", message: `there is no cable ${edge}` } };
  }
  if (!holds(depth)) return invalid(`${depth} is not a value for the depth of a cable`);
  return { event: { type: "edgeDepthSet", edge, depth: clamp(depth, DEPTH_RANGE[0], DEPTH_RANGE[1]) } };
}

const isModulation = (e: Edge): e is Edge & { kind: "modulation"; id: EdgeId } => e.kind === "modulation";

/** A parameter of a node as one key, if the port is one. */
const paramKey = (node: NodeId, port: Port): string | null => (port.type === "param" ? `${String(node)}:${String(port.index)}` : null);

/**
 * The cables that go when those of `start` do: they, and, in turn, the ones that drive the depth of a cable that
 * goes (what is left pointing at a cable that is not there would drive whichever one is put there next).
 */
function cablesThatGo(cables: readonly Edge[], start: Iterable<EdgeId>): Set<EdgeId> {
  const going = new Set<EdgeId>(start);
  for (let grown = true; grown; ) {
    grown = false;
    for (const e of cables) {
      if (isModulation(e) && !going.has(e.id) && e.to.port.type === "depth" && going.has(e.to.port.edge)) {
        going.add(e.id);
        grown = true;
      }
    }
  }
  return going;
}

/**
 * The session without the cables `going` and the nodes `nodesGone`. The layout of the telemetry follows: the
 * outputs of the modulators that go leave it, and so does the slot of the value a cable drove, unless another
 * cable still drives it (the layout names one slot for a parameter, whichever cables are left).
 */
function without(snapshot: Snapshot, going: ReadonlySet<EdgeId>, nodesGone: ReadonlySet<NodeId>): Snapshot {
  const cables = snapshot.graph.edges;
  const edges = cables.filter((e) => !(isModulation(e) && going.has(e.id)));
  const aimedBefore = new Set(cables.map((e) => paramKey(e.to.node, e.to.port)));
  const aimedAfter = new Set(edges.map((e) => paramKey(e.to.node, e.to.port)));
  const leaves = (entry: LayoutEntry): boolean => {
    if (entry.kind === "modOutput") return nodesGone.has(entry.node);
    const key = paramKey(entry.node, entry.port);
    return entry.kind === "effective" && aimedBefore.has(key) && !aimedAfter.has(key);
  };
  const modulation = snapshot.telemetry.modulation.filter((entry) => !leaves(entry));
  const changed = modulation.length !== snapshot.telemetry.modulation.length;
  return {
    ...snapshot,
    graph: { nodes: snapshot.graph.nodes.filter((n) => !nodesGone.has(n.id)), edges },
    telemetry: changed ? { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, modulation } : snapshot.telemetry,
  };
}

/** A cable taken out of the session, and with it the cables that drive the depth of one that goes (and theirs in turn). */
function removeEdgeOutcome(snapshot: Snapshot, edge: EdgeId): Outcome {
  if (!snapshot.graph.edges.some((e) => isModulation(e) && e.id === edge)) return { error: { code: "notFound", message: `there is no cable ${edge}` } };
  return { restructured: without(snapshot, cablesThatGo(snapshot.graph.edges, [edge]), new Set()) };
}

/**
 * A modulator taken out of the session: its own cables go with it, and so do those of the others that drove
 * its settings (a cable that ends at the modulator), and what drove the depth of any of them. Nothing else can be
 * taken out yet.
 */
function removeNodeOutcome(snapshot: Snapshot, node: NodeId): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!found) return { error: { code: "notFound", message: `there is no node ${node}` } };
  if (found.kind !== "modulator") return invalid("only a modulator can be removed so far");
  const touching = snapshot.graph.edges.filter((e) => isModulation(e) && (e.from.node === node || e.to.node === node)).map((e) => (e as { id: EdgeId }).id);
  return { restructured: without(snapshot, cablesThatGo(snapshot.graph.edges, touching), new Set([node])) };
}

/**
 * What the controller hands out and does not hand out again, however much is taken out after: the ids of
 * nodes and the telemetry slots. They go on from what the session has.
 */
class Counters {
  #node: number;
  #slot: number;

  constructor(snapshot: Snapshot) {
    const { graph, telemetry } = snapshot;
    // (The master is a node of the graph and not of the model: its id is not one the model has handed out.)
    this.#node = 1 + Math.max(-1, ...graph.nodes.filter((n) => n.kind !== "master").map((n) => n.id));
    this.#slot = 1 + Math.max(-1, ...[...telemetry.meters, ...telemetry.modulation].map((entry) => entry.slot));
  }

  node(): NodeId {
    this.#node += 1;
    return this.#node - 1;
  }

  slot(): number {
    this.#slot += 1;
    return this.#slot - 1;
  }
}

/** What a node of each kind of modulator is called (`ModSourceSlot::node_name`), and what it is to begin with. */
const NEW_MODULATORS: Record<ModulatorKind, { name: string; source: ModulatorSource }> = {
  lfo: { name: "LFO", source: { type: "lfo", waveform: "sine", rate: 1 } },
  envelope: { name: "ADSR", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } },
  midiLearn: { name: "MIDI (learning…)", source: { type: "midiLearn", control: null, learning: true } },
};

/**
 * A modulator added to a lane or a group. The graph lists a lane's nodes together (the lane, its effects, its
 * modulators), so the new one goes after the last of them; and the layout of the telemetry lists the modulators'
 * outputs in the order of the graph, ahead of the parameters' values.
 */
function addModulatorOutcome(snapshot: Snapshot, parent: NodeId, kind: ModulatorKind, counters: Counters): Outcome {
  const owner = snapshot.graph.nodes.find((n) => n.id === parent && n.kind !== "master");
  if (!owner) return { error: { code: "notFound", message: `there is no node ${parent}` } };
  if (owner.kind !== "instrument" && owner.kind !== "group") return invalid("a modulator goes in a lane or in a group");
  const { nodes } = snapshot.graph;
  const strip = (n: Node): boolean => n.id === parent || (n.parent === parent && (n.kind === "effect" || n.kind === "modulator"));
  const at = nodes.findLastIndex(strip) + 1;
  const { name, source } = NEW_MODULATORS[kind];
  const added: Node = { id: counters.node(), parent, name, kind: "modulator", source };
  const before = new Set(nodes.slice(0, at).filter((n) => n.kind === "modulator").map((n) => n.id));
  const outputs = snapshot.telemetry.modulation;
  const place = outputs.findLastIndex((entry) => entry.kind === "modOutput" && before.has(entry.node)) + 1;
  const entry: LayoutEntry = { slot: counters.slot(), node: added.id, port: { type: "modOut" }, kind: "modOutput" };
  const modulation = [...outputs.slice(0, place), entry, ...outputs.slice(place)];
  return {
    restructured: {
      ...snapshot,
      graph: { ...snapshot.graph, nodes: [...nodes.slice(0, at), added, ...nodes.slice(at)] },
      telemetry: { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, modulation },
    },
  };
}

/** Do what the controller does with an intent, to a snapshot. */
function outcomeOf(snapshot: Snapshot, intent: Intent, counters: Counters): Outcome {
  if (intent.type === "setEdgeDepth") return depthOutcome(snapshot, intent.edge, intent.depth);
  if (intent.type === "removeEdge") return removeEdgeOutcome(snapshot, intent.edge);
  if (intent.type === "addModulator") return addModulatorOutcome(snapshot, intent.parent, intent.kind, counters);
  if (intent.type === "removeNode") return removeNodeOutcome(snapshot, intent.node);
  // (The master is a node of the graph and not of the model: the controller does not find it.)
  const node = snapshot.graph.nodes.find((n) => n.id === intent.node && n.kind !== "master");
  if (!node) return { error: { code: "notFound", message: `there is no node ${intent.node}` } };
  switch (intent.type) {
    case "setParam": {
      const param = pluginOf(node)?.params.find((p) => p.index === intent.param);
      if (!param) return invalid(`that node has no parameter ${intent.param}`);
      if (!holds(intent.value)) return invalid(`${intent.value} is not a value for ${param.name}`);
      const clamped = clamp(intent.value, param.min, param.max);
      const value = param.labels === null ? clamped : roundIn32Bits(clamped);
      return { event: { type: "paramSet", node: node.id, param: param.index, value } };
    }
    case "setGain": {
      const fits =
        (intent.gain === "volume" && node.kind === "instrument") ||
        (intent.gain === "mix" && node.kind === "effect") ||
        ((intent.gain === "groupVolume" || intent.gain === "groupPan") && node.kind === "group");
      if (!fits) return invalid(`${intent.gain} does not apply to that node`);
      if (!holds(intent.value)) return invalid(`${intent.value} is not a value for ${intent.gain}`);
      const [low, high] = GAIN_RANGES[intent.gain];
      return { event: { type: "gainSet", node: node.id, gain: intent.gain, value: clamp(intent.value, low, high) } };
    }
    case "loadPreset": {
      const preset = pluginOf(node)?.presets.find((p) => p.id === intent.preset);
      if (!preset) return invalid(`the plugin has no preset ${intent.preset}`);
      // (The values a preset sets are the plugin's business; a mock has none to report.)
      return { event: { type: "presetLoaded", node: node.id, preset: preset.name, values: [] } };
    }
    case "setModulatorField":
      return fieldOutcome(node, intent.field, intent.value);
  }
}

/** A waveform at a phase (0 to 1), from -1 to 1. */
function shape(waveform: "sine" | "triangle" | "saw" | "square", phase: number): number {
  const p = phase - Math.floor(phase);
  switch (waveform) {
    case "sine":
      return Math.sin(2 * Math.PI * p);
    case "triangle":
      return 4 * Math.abs(((p + 0.75) % 1) - 0.5) - 1;
    case "saw":
      return 2 * p - 1;
    case "square":
      return p < 0.5 ? 1 : -1;
  }
}

/** The bytes of a telemetry frame, as the server writes them (`telemetry_frame`). */
function frameBytes(channel: TelemetryChannel, layoutRev: number, values: readonly number[]): ArrayBuffer {
  const buffer = new ArrayBuffer(8 + 4 * values.length);
  const view = new DataView(buffer);
  view.setUint8(0, channel === "meters" ? 0 : 1);
  view.setUint16(2, values.length, true);
  view.setUint32(4, layoutRev, true);
  values.forEach((value, i) => {
    view.setFloat32(8 + 4 * i, value, true);
  });
  return buffer;
}

/** How fast a meter's peak falls, as the engine's does: 40 dB a second. */
const PEAK_FALL_DB_PER_SECOND = 40;

export class MockServer {
  /** Everything the clients have sent, in order. */
  readonly received: ClientMessage[] = [];
  /** A factory of transports to this server, for a client. */
  readonly transport: TransportFactory = () => new MockConnection(this);

  readonly #options: MockOptions;
  #snapshot: Snapshot;
  readonly #counters: Counters;
  readonly #connections = new Set<MockConnection>();

  constructor(options: MockOptions = {}) {
    this.#options = options;
    this.#snapshot = options.snapshot ?? tracerSnapshot;
    this.#counters = new Counters(this.#snapshot);
  }

  /** The session as the server has it now. */
  get snapshot(): Snapshot {
    return this.#snapshot;
  }

  /** How many connections are up. */
  get connections(): number {
    return this.#connections.size;
  }

  /** The notes held by every connection together, by note number. */
  get heldNotes(): ReadonlySet<number> {
    return new Set([...this.#connections].flatMap((c) => [...c.notes]));
  }

  /** Another front-end's intent (the TUI's, or another browser's): applied, and every subscribed client is told. */
  apply(intent: Intent): void {
    const outcome = outcomeOf(this.#snapshot, intent, this.#counters);
    if ("error" in outcome) throw new Error(outcome.error.message);
    this.#commit(outcome, true);
  }

  /** Change the session without telling anybody: the clients will see a revision missing at the next change. */
  applyQuietly(intent: Intent): void {
    const outcome = outcomeOf(this.#snapshot, intent, this.#counters);
    if ("error" in outcome) throw new Error(outcome.error.message);
    this.#commit(outcome, false);
  }

  /** The server's idea of what the telemetry slots mean changes (a node came or went): the subscribed clients are told. */
  announceLayout(layout: TelemetryLayout): void {
    this.#snapshot = { ...this.#snapshot, telemetry: layout };
    for (const connection of this.#connections) {
      if (connection.subscribed) connection.tell({ type: "layout", layout });
    }
  }

  /** The network goes: every connection ends without a goodbye. */
  drop(): void {
    for (const connection of [...this.#connections]) connection.lose();
  }

  /** What this server says in `hello`. */
  hello(): ServerMessage {
    return {
      type: "hello",
      protocol: this.#options.protocol ?? PROTOCOL,
      rev: this.#snapshot.rev,
      features: ["notes", "telemetry"],
      needsToken: this.#options.token !== undefined,
    };
  }

  /** The token the server wants, if it wants one. */
  get token(): string | undefined {
    return this.#options.token;
  }

  get telemetryPeriod(): number {
    return this.#options.telemetryPeriod ?? 33;
  }

  /** The time the made-up telemetry follows, in seconds. */
  now(): number {
    return this.#options.clock ? this.#options.clock() : performance.now() / 1000;
  }

  add(connection: MockConnection): void {
    this.#connections.add(connection);
  }

  remove(connection: MockConnection): void {
    this.#connections.delete(connection);
  }

  /** A message from a connection: what the real server's connection and owner do with it, together. */
  receive(connection: MockConnection, text: string): void {
    const message = JSON.parse(text) as ClientMessage;
    this.received.push(message);
    if (!connection.authorized) {
      if (message.type === "auth" && message.token === this.token) connection.authorized = true;
      else connection.refuse("unauthorized", message.type === "auth" ? "the token is wrong" : "send the token first");
      return;
    }
    switch (message.type) {
      case "auth":
        break;
      case "subscribe":
        connection.subscribed = true;
        connection.tell({ type: "snapshot", snapshot: this.#snapshot });
        break;
      case "telemetry":
        connection.wantTelemetry(message.channels);
        break;
      case "request": {
        const outcome = outcomeOf(this.#snapshot, message.intent, this.#counters);
        if ("error" in outcome) {
          connection.tell({ type: "rejected", reqId: message.reqId, error: outcome.error });
        } else {
          // (The events are told before the answer, and so is the snapshot that tells a change of structure.)
          const rev = this.#commit(outcome, true);
          connection.tell({ type: "ack", reqId: message.reqId, rev });
        }
        break;
      }
      case "noteOn":
        connection.notes.add(message.note);
        break;
      case "noteOff":
        connection.notes.delete(message.note);
        break;
    }
  }

  /**
   * The next revision: apply what the intent did to the model and, if `tell`, tell the subscribed clients. The
   * revision of the intent. (A change of structure is that, and the snapshot that tells it is the revision after,
   * as `Controller::apply` has it.)
   */
  #commit(outcome: Exclude<Outcome, { error: WireError }>, tell: boolean): number {
    const rev = this.#snapshot.rev + 1;
    if ("restructured" in outcome) {
      this.#snapshot = { ...outcome.restructured, rev: rev + 1, meta: { ...outcome.restructured.meta, dirty: true } };
      if (tell) {
        for (const connection of this.#connections) {
          if (connection.subscribed) connection.tell({ type: "snapshot", snapshot: this.#snapshot });
        }
      }
      return rev;
    }
    const stamped: StampedEvent = { ...outcome.event, rev };
    const applied = applyEvent(this.#snapshot, stamped);
    if (!applied.ok) throw new Error(applied.reason);
    this.#snapshot = applied.snapshot;
    if (tell) {
      for (const connection of this.#connections) {
        if (connection.subscribed) connection.tell({ type: "event", event: stamped });
      }
    }
    return rev;
  }

  // -- Telemetry made up --

  /** What a modulator puts out at `seconds`, from -1 to 1 for an LFO and from 0 to 1 for the rest. */
  #modulatorOutput(node: Node, seconds: number): number {
    if (node.kind !== "modulator") return 0;
    const { source } = node;
    switch (source.type) {
      case "lfo":
        return shape(source.waveform, source.rate * seconds);
      case "envelope":
        return this.heldNotes.size > 0 ? source.sustain : 0;
      case "midiLearn":
        return 0.5;
    }
  }

  /** The values of a channel's list in the layout, as of `seconds`; `peaks` is the meters' state, kept from frame to frame. */
  frame(channel: TelemetryChannel, seconds: number, peaks: Map<number, number>, elapsed: number): ArrayBuffer {
    const { telemetry } = this.#snapshot;
    const nodes = this.#snapshot.graph.nodes;
    const outputOf = (id: NodeId): number => {
      const node = nodes.find((n) => n.id === id);
      return node ? this.#modulatorOutput(node, seconds) : 0;
    };
    const fall = 10 ** ((-PEAK_FALL_DB_PER_SECOND * elapsed) / 20);
    const level = this.heldNotes.size > 0 ? 0.35 : 0;
    const value = (entry: LayoutEntry): number => {
      switch (entry.kind) {
        case "peak": {
          const peak = Math.max(level, (peaks.get(entry.slot) ?? 0) * fall);
          peaks.set(entry.slot, peak);
          return peak;
        }
        case "clips":
          return 0;
        case "modOutput":
          return outputOf(entry.node);
        case "effective":
          return this.#effective(entry, outputOf);
      }
    };
    return frameBytes(channel, telemetry.rev, telemetry[channel].map(value));
  }

  /** The value a parameter is set to: what the user set, plus what the modulators aimed at it add. */
  #effective(entry: LayoutEntry, outputOf: (id: NodeId) => number): number {
    const node = this.#snapshot.graph.nodes.find((n) => n.id === entry.node);
    if (entry.port.type !== "param") return 0;
    const index = entry.port.index;
    const param = node ? pluginOf(node)?.params.find((p) => p.index === index) : undefined;
    if (!param) return 0;
    let value = param.value;
    for (const edge of this.#snapshot.graph.edges) {
      if (edge.kind !== "modulation" || edge.to.node !== entry.node || edge.to.port.type !== "param" || edge.to.port.index !== index) continue;
      const output = outputOf(edge.from.node);
      const source = this.#snapshot.graph.nodes.find((n) => n.id === edge.from.node);
      const isEnvelope = source?.kind === "modulator" && source.source.type === "envelope";
      // An LFO or a control moves the value around where the user set it; an envelope's peak is that value.
      value = isEnvelope ? value - edge.amount * (1 - output) * (value - param.min) : value + output * edge.amount * (param.max - param.min);
    }
    return clamp(value, param.min, param.max);
  }
}

/** One client's connection to a `MockServer`; what the client holds is its transport. */
export class MockConnection implements Transport {
  authorized: boolean;
  subscribed = false;
  readonly notes = new Set<number>();

  #events: TransportEvents | null = null;
  #timer: ReturnType<typeof setInterval> | undefined;
  #channels = new Set<TelemetryChannel>();
  readonly #peaks = new Map<number, number>();
  #last = 0;

  constructor(private readonly server: MockServer) {
    this.authorized = server.token === undefined;
  }

  connect(events: TransportEvents): void {
    this.#events = events;
    this.server.add(this);
    queueMicrotask(() => {
      if (this.#events !== events) return;
      events.open();
      this.tell(this.server.hello());
    });
  }

  /** The client sends a message (its text): the server gets it after a moment, in order. */
  send(text: string): void {
    queueMicrotask(() => {
      if (this.#events) this.server.receive(this, text);
    });
  }

  /** The server sends a message to the client, in order with the others. */
  tell(message: ServerMessage): void {
    const events = this.#events;
    const text = JSON.stringify(message);
    queueMicrotask(() => {
      events?.text(text);
    });
  }

  /** The client closes: nothing more is said either way, and the notes it held end. */
  close(): void {
    this.#end();
  }

  /** The connection is lost: the client is told it closed, abnormally. */
  lose(): void {
    const events = this.#events;
    this.#end();
    if (events) queueMicrotask(() => { events.close({ code: 1006, reason: "" }); });
  }

  /** The server closes the connection for breaking a rule: it says why, and closes. */
  refuse(code: WireError["code"], message: string): void {
    const events = this.#events;
    this.tell({ type: "closing", error: { code, message } });
    this.#end();
    if (events) queueMicrotask(() => { events.close({ code: 1008, reason: "closing" }); });
  }

  wantTelemetry(channels: readonly TelemetryChannel[]): void {
    this.#channels = new Set(channels);
    clearInterval(this.#timer);
    this.#timer = undefined;
    if (this.#channels.size === 0) return;
    this.#last = this.server.now();
    this.#timer = setInterval(() => {
      const events = this.#events;
      if (!events) return;
      const now = this.server.now();
      for (const channel of this.#channels) {
        events.binary(this.server.frame(channel, now, this.#peaks, Math.max(now - this.#last, 0)));
      }
      this.#last = now;
    }, this.server.telemetryPeriod);
  }

  #end(): void {
    this.#events = null;
    this.server.remove(this);
    clearInterval(this.#timer);
    this.#timer = undefined;
  }
}
