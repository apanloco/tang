// The mock engine: a stand-in for the server that lives in the page (or in a test) and
// speaks the same protocol, so that the interface can be built, shown in Storybook and
// tested without audio hardware or a running `tang serve` (docs/web-ui.md §9.2). It
// serves a snapshot, applies intents the way the controller does (`src/app/apply.rs`:
// clamp, round an enum, one revision per change), tells every subscribed connection what
// changed, and, for the connections that ask, makes telemetry up: the LFOs run, the
// parameters they drive follow them, and the master's meter rises while a note is held.
import { DEPTH_RANGE, GAIN_RANGES, MODULATOR_FIELD_RANGES, NAME_LIMIT, NODE_UI_LIMIT, NODE_UI_PARTS, PINNED_LIMIT, PROTOCOL } from "../protocol";
import type {
  ClientMessage,
  CatalogEntry,
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
  NodeUi,
  NoteRange,
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
import { MASTER, builtinCatalog, builtinEffects, builtinInstruments, builtinPresets, tracerSnapshot } from "./fixtures";
import { cablesThatGo, isModulation } from "./cables";
import { Counters } from "./counters";
import { History, planOf, removedStep, stepOf, takesOut } from "./history";
import type { HistoryState, Made, Plan } from "./history";
import { cablesIntoLaneFromItsGroup } from "./membership";
import { noteName } from "./notes";
import { fieldsOf, modulatorsInReach, reachFrom, samePort } from "./reach";
import { applyEvent } from "./reducers";
import { parseScale } from "./scales";

export interface MockOptions {
  /** The session it serves; the tracer by default. */
  snapshot?: Snapshot;
  /** The plugins it says can be added, and whether it is still looking for more; the built-ins, found, by default. */
  catalog?: { entries: readonly CatalogEntry[]; scanning: boolean };
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

/** A number the controller cannot hold, as it says it: Rust writes the infinite 32-bit numbers `inf` and `-inf`. */
function shown(value: number): string {
  const held = Math.fround(value);
  if (Number.isNaN(held) || Number.isFinite(held)) return String(value);
  return held > 0 ? "inf" : "-inf";
}

/** What the controller calls a gain or a setting when it says what is wrong with one: the name of its variant, `Volume`, `GroupPan`, `Attack`. */
const variant = (name: string): string => name.charAt(0).toUpperCase() + name.slice(1);

function pluginOf(node: Node): Plugin | null {
  return node.kind === "instrument" || node.kind === "effect" ? node.plugin : null;
}

function invalid(message: string): Outcome {
  return { error: { code: "invalid", message } };
}

function noNode(id: NodeId): Outcome {
  return { error: { code: "notFound", message: `there is no node ${id}` } };
}

/** A setting of a modulator: clamped to its range, rounded if it is an index, and only if the modulator has it. */
function fieldOutcome(node: Node, field: ModulatorField, value: number): Outcome {
  if (node.kind !== "modulator") return invalid("that node is not a modulator");
  if (!holds(value)) return invalid(`${shown(value)} is not a value for ${variant(field)}`);
  const [low, high] = MODULATOR_FIELD_RANGES[field];
  const clamped = clamp(value, low, high);
  const { type } = node.source;
  const fits = type === "lfo" ? field === "rate" || field === "waveform" : type === "envelope" && field !== "rate" && field !== "waveform";
  if (!fits) return invalid(`${variant(field)} does not apply to that modulator`);
  return { event: { type: "modulatorFieldSet", node: node.id, field, value: field === "waveform" ? roundIn32Bits(clamped) : clamped } };
}

/** The depth of a cable: clamped, and only of a cable that is there. */
function depthOutcome(snapshot: Snapshot, edge: number, depth: number): Outcome {
  if (!snapshot.graph.edges.some((e) => e.kind === "modulation" && e.id === edge)) {
    return { error: { code: "notFound", message: `there is no cable ${edge}` } };
  }
  if (!holds(depth)) return invalid(`${shown(depth)} is not a value for the depth of a cable`);
  return { event: { type: "edgeDepthSet", edge, depth: clamp(depth, DEPTH_RANGE[0], DEPTH_RANGE[1]) } };
}

/** A parameter of a node as one key, if the port is one. */
const paramKey = (node: NodeId, port: Port): string | null => (port.type === "param" ? `${String(node)}:${String(port.index)}` : null);

/** How deep a cable pulls when it is first drawn (`NEW_CABLE_DEPTH`). */
const NEW_CABLE_DEPTH = 0.5;

/**
 * A cable drawn from a modulator, at half depth: to a parameter of a plugin it reaches (`reachFrom`: the controller's rule,
 * read from the graph), or to a setting of another modulator it reaches, or to the depth of one of that modulator's cables
 * (`addFieldCable`).
 */
function addModEdgeOutcome(snapshot: Snapshot, modulator: NodeId, to: Edge["to"], counters: Counters): Outcome {
  const from = snapshot.graph.nodes.find((n) => n.id === modulator && n.kind !== "master");
  if (!from) return noNode(modulator);
  if (from.kind !== "modulator") return invalid("a cable starts at a modulator");
  switch (to.port.type) {
    case "param":
      return addParamCable(snapshot, from, to.node, to.port.index, counters);
    case "rate":
    case "attack":
    case "decay":
    case "sustain":
    case "release":
    case "depth":
      return addFieldCable(snapshot, from, to, counters);
    default:
      return invalid("a cable can only end at a plugin's parameter, or at a modulator's rate, stage or depth");
  }
}

/**
 * `edges` with the cable `added` of the modulator `modulator` among them, where the graph lists a modulator's cables (the groups'
 * modulators' first, then the lanes', each modulator's cables together): after the last cable of a modulator that comes before this
 * one in the graph, this one's own included.
 */
function withCable(nodes: readonly Node[], edges: readonly Edge[], modulator: NodeId, added: Edge): Edge[] {
  const position = new Map(nodes.map((n, i) => [n.id, i]));
  const mine = position.get(modulator) ?? 0;
  const after = edges.findLastIndex((e) => isModulation(e) && (position.get(e.from.node) ?? Infinity) <= mine);
  const first = edges.findIndex(isModulation);
  const at = after >= 0 ? after + 1 : first >= 0 ? first : edges.length;
  return [...edges.slice(0, at), added, ...edges.slice(at)];
}

/** What a modulator of a lane or of a group is told of what it cannot reach: whose it is. */
const ownerOf = (nodes: readonly Node[], modulator: Node): "group" | "lane" => (nodes.find((n) => n.id === modulator.parent)?.kind === "group" ? "group" : "lane");

/**
 * A cable to parameter `index` of the plugin in `node`. The value it drives gets a slot in the layout of the telemetry unless
 * another cable drives it already (the layout names one slot for a parameter); the cable takes a slot either way.
 */
function addParamCable(snapshot: Snapshot, from: Node, node: NodeId, index: number, counters: Counters): Outcome {
  const { nodes, edges } = snapshot.graph;
  const target = nodes.find((n) => n.id === node && n.kind !== "master");
  if (!target) return noNode(node);
  if (target.kind !== "instrument" && target.kind !== "effect") return invalid("that node has no plugin");
  if (!target.plugin) return invalid("that instrument has no plugin");
  if (!reachFrom(snapshot.graph, from.id).has(node)) return invalid(`a modulator of this ${ownerOf(nodes, from)} cannot reach that plugin`);
  if (!target.plugin.params.some((p) => p.index === index)) return invalid(`the plugin has no parameter ${index}`);
  const port: Port = { type: "param", index };
  const drivesIt = (e: Edge): boolean => isModulation(e) && e.from.node === from.id && e.to.node === node && samePort(e.to.port, port);
  if (edges.some(drivesIt)) return invalid("that modulator drives that parameter already");

  const added: Edge = { from: { node: from.id, port: { type: "modOut" } }, to: { node, port }, kind: "modulation", id: counters.edge(), amount: NEW_CABLE_DEPTH };
  const key = paramKey(node, port);
  const aimed = edges.some((e) => isModulation(e) && paramKey(e.to.node, e.to.port) === key);
  // (Every cable takes a slot, as the controller's `add_target` has it, though the layout names a parameter once: the one that is not named is never read.)
  const slot = counters.slot();
  const modulation = aimed ? snapshot.telemetry.modulation : [...snapshot.telemetry.modulation, { slot, node, port, kind: "effective" as const }];
  const telemetry = aimed ? snapshot.telemetry : { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, modulation };
  return { restructured: { ...snapshot, graph: { nodes, edges: withCable(nodes, edges, from.id, added) }, telemetry } };
}

/**
 * A cable to a setting of another modulator (an LFO's rate, a stage of an envelope) or to the depth of one of its cables, as the
 * controller lets it (`add_field_cable`): the modulator is in reach of the one that is drawn from (`modulatorsInReach`), has the
 * setting (`fieldsOf`), and is not driven there already. A value of that kind has no slot in the layout of the telemetry.
 */
function addFieldCable(snapshot: Snapshot, from: Node, to: Edge["to"], counters: Counters): Outcome {
  const { nodes, edges } = snapshot.graph;
  const target = nodes.find((n) => n.id === to.node && n.kind !== "master");
  if (!target) return noNode(to.node);
  if (target.id === from.id) return invalid("a modulator cannot drive its own settings");
  if (target.kind !== "modulator") return invalid("a cable ends at the settings of a modulator, and that is not one");
  if (!modulatorsInReach(snapshot.graph, from.id).some((n) => n.id === target.id)) return invalid(`a modulator of this ${ownerOf(nodes, from)} cannot reach that modulator`);
  if (!fieldsOf(snapshot.graph, from.id).some((field) => field.node === to.node && samePort(field.port, to.port))) {
    if (to.port.type === "depth") return { error: { code: "notFound", message: `there is no cable ${to.port.edge}` } };
    return invalid(`that modulator has no ${to.port.type}`);
  }
  if (edges.some((e) => isModulation(e) && e.from.node === from.id && e.to.node === to.node && samePort(e.to.port, to.port))) return invalid("that modulator drives that setting already");

  const added: Edge = { from: { node: from.id, port: { type: "modOut" } }, to, kind: "modulation", id: counters.edge(), amount: NEW_CABLE_DEPTH };
  return { restructured: { ...snapshot, graph: { nodes, edges: withCable(nodes, edges, from.id, added) } } };
}

/**
 * The session without the cables `going` and the nodes `nodesGone`. The layout of the telemetry follows: the
 * outputs of the modulators that go leave it, and so does the slot of the value a cable drove, unless another
 * cable still drives it (the layout names one slot for a parameter, whichever cables are left), and the meters of
 * a lane or a group that goes.
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
  const meters = snapshot.telemetry.meters.filter((entry) => !nodesGone.has(entry.node));
  const changed = modulation.length !== snapshot.telemetry.modulation.length || meters.length !== snapshot.telemetry.meters.length;
  return {
    ...snapshot,
    graph: { nodes: snapshot.graph.nodes.filter((n) => !nodesGone.has(n.id)), edges },
    telemetry: changed ? { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, meters, modulation } : snapshot.telemetry,
  };
}

/** A cable taken out of the session, and with it the cables that drive the depth of one that goes (and theirs in turn). */
function removeEdgeOutcome(snapshot: Snapshot, edge: EdgeId): Outcome {
  if (!snapshot.graph.edges.some((e) => isModulation(e) && e.id === edge)) return { error: { code: "notFound", message: `there is no cable ${edge}` } };
  return { restructured: without(snapshot, cablesThatGo(snapshot.graph.edges, [edge]), new Set()) };
}

/**
 * An effect (or a bus effect) taken out of the session, as the controller does it (`remove_effect`): the cables that drive
 * its parameters go first, a lane's and a group's, with what drove the depth of any of them (what they were driving goes with
 * the plugin: nothing is given back), and the audio path closes up: the link into it and the link out of it are one, from
 * where the first came to where the second goes, of the second's kind (the link that ends a member lane's chain goes into
 * its group).
 */
function removeEffectOutcome(snapshot: Snapshot, effect: Node): Outcome {
  const touching = snapshot.graph.edges.filter(isModulation).filter((e) => e.to.node === effect.id).map((e) => e.id);
  const taken = without(snapshot, cablesThatGo(snapshot.graph.edges, touching), new Set([effect.id]));
  const { edges } = taken.graph;
  const into = edges.findIndex((e) => e.kind !== "modulation" && e.to.node === effect.id && e.to.port.type === "audioIn");
  const out = edges.findIndex((e) => e.kind !== "modulation" && e.from.node === effect.id && e.from.port.type === "audioOut");
  const incoming = edges[into];
  const outgoing = edges[out];
  if (!incoming || incoming.kind === "modulation" || !outgoing || outgoing.kind === "modulation") return invalid("that has no audio path to close up");
  const joined: Edge = { from: incoming.from, to: outgoing.to, kind: outgoing.kind };
  const closed = edges.flatMap((e, i) => (i === into ? [joined] : i === out ? [] : [e]));
  return { restructured: { ...taken, graph: { ...taken.graph, edges: closed } } };
}

/**
 * An effect (a lane's or a bus's) moved to place `index` of its chain, as the controller does it (`Controller::move_effect`,
 * `move_bus_effect`): the effects between its old place and its new one move one place the other way, and the audio path goes through
 * them in the new order. The graph lists the nodes of a lane, or of a group's bus, together (the lane or the group, its effects, its
 * modulators), so the effects change places among themselves, in the places they take in the list; the links of the chain stay where
 * they are in the list of edges, from the lane or the group through the effects in their new order to where the chain ended (a member
 * lane's chain ends in its group, a bus's and an ungrouped lane's in the master). The cables and the layout of the telemetry are what
 * they were: they name the plugins by their nodes, which are the same. Refused for what is no effect, a place the chain has not, and
 * the place the effect is at.
 */
function moveEffectOutcome(snapshot: Snapshot, node: NodeId, index: number): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!found) return noNode(node);
  if (found.kind !== "effect") return invalid("only an effect can be moved");
  const { nodes, edges } = snapshot.graph;
  const chain = nodes.filter((n) => n.kind === "effect" && n.parent === found.parent);
  if (!Number.isInteger(index) || index < 0 || index >= chain.length) return invalid(`the chain has no place ${String(index)}`);
  const from = chain.indexOf(found);
  if (index === from) return invalid("that effect is at that place already");
  const owner = nodes.find((n) => n.id === found.parent);
  const moved = [...chain];
  moved.splice(from, 1);
  moved.splice(index, 0, found);

  // The links of the chain, from the owner through the effects to where it ends: where they are in the list, and what the last one is.
  const joined = new Set<NodeId>([...(owner ? [owner.id] : []), ...chain.map((n) => n.id)]);
  const slots = edges.flatMap((e, i) => (e.kind !== "modulation" && joined.has(e.from.node) ? [i] : []));
  const out = edges[slots.at(-1) ?? -1];
  if (!owner || slots.length !== chain.length + 1 || !out || out.kind === "modulation") return invalid("that has no audio path to reorder");
  const path = [owner.id, ...moved.map((n) => n.id)];
  const links = path.map((leaves, i): Edge => {
    const next = path[i + 1];
    const from = { node: leaves, port: { type: "audioOut" } } as const;
    return next === undefined ? { from, to: out.to, kind: out.kind } : { from, to: { node: next, port: { type: "audioIn" } }, kind: "audio" };
  });

  const inChain = new Set(chain);
  let nextNode = 0;
  let nextLink = 0;
  return {
    restructured: {
      ...snapshot,
      graph: {
        nodes: nodes.map((n) => (inChain.has(n) ? (moved[nextNode++] ?? n) : n)),
        edges: edges.map((e, i) => (slots.includes(i) ? (links[nextLink++] ?? e) : e)),
      },
    },
  };
}

/**
 * A lane taken out of the session, as the controller does it (`Controller::remove_lane`): its instrument, its effects and its
 * modulators go, with the links of its audio path, the cables of its own modulators (they reach only its own chain) and those of its
 * group that end in it, and what drove the depth of any of them. A session keeps a lane: the last one is refused. (What the cables of
 * its group were driving is gone with it: nothing is given back.)
 */
function removeLaneOutcome(snapshot: Snapshot, lane: Node): Outcome {
  const { nodes, edges } = snapshot.graph;
  if (nodes.filter((n) => n.kind === "instrument").length < 2) return invalid("a session keeps its last lane");
  const inside = new Set<NodeId>([lane.id, ...nodes.filter((n) => n.parent === lane.id).map((n) => n.id)]);
  // (The cables of the lane's own modulators end inside the lane, so the cables that end there are the lane's own and its group's.)
  const touching = edges.filter(isModulation).filter((e) => inside.has(e.to.node)).map((e) => e.id);
  const taken = without(snapshot, cablesThatGo(edges, touching), inside);
  return { restructured: { ...taken, graph: { ...taken.graph, edges: taken.graph.edges.filter((e) => e.kind === "modulation" || !inside.has(e.from.node)) } } };
}

/**
 * A group taken out of the session, as the controller does it (`Controller::remove_group`): its bus effects and its modulators go with it,
 * and the links of its audio path, and every cable of it (and what drove the depth of any of them); its members stay, in no group, their last
 * link going into the master. The groups after it that have no name of their own are called by their places now (the graph has only the name a
 * group is called, so a name that is "Group" and its own place is taken for the default one). (What the cables were driving in the
 * members has the value that was set, which is the audio thread's to do.)
 */
function removeGroupOutcome(snapshot: Snapshot, group: Node): Outcome {
  const { nodes, edges } = snapshot.graph;
  const master = nodes.find((n) => n.kind === "master");
  if (!master) return invalid("the session has no master");
  const inside = new Set<NodeId>([group.id, ...nodes.filter((n) => n.parent === group.id && n.kind !== "instrument").map((n) => n.id)]);
  const touching = edges.filter(isModulation).filter((e) => inside.has(e.from.node) || inside.has(e.to.node)).map((e) => e.id);
  const taken = without(snapshot, cablesThatGo(edges, touching), inside);
  const groups = nodes.filter((n) => n.kind === "group");
  const place = (id: NodeId): number => groups.findIndex((n) => n.id === id);
  const renumbered = (n: Node): Node => {
    const was = place(n.id);
    return n.kind === "group" && was > place(group.id) && n.name === `Group ${was + 1}` ? { ...n, name: `Group ${was}` } : n;
  };
  const members = new Set(nodes.filter((n) => n.parent === group.id && n.kind === "instrument").map((n) => n.id));
  return {
    restructured: {
      ...taken,
      graph: {
        nodes: taken.graph.nodes.map((n) => renumbered(members.has(n.id) ? { ...n, parent: null } : n)),
        edges: taken.graph.edges.flatMap((e): Edge[] => {
          if (e.kind === "modulation") return [e];
          if (inside.has(e.from.node)) return [];
          return e.to.node === group.id ? [{ from: e.from, to: { node: master.id, port: { type: "audioIn" } }, kind: "audio" }] : [e];
        }),
      },
    },
  };
}

/**
 * A node taken out of the session. A modulator: its own cables go with it, and so do those of the others that drove
 * its settings (a cable that ends at the modulator), and what drove the depth of any of them. An effect: see
 * `removeEffectOutcome`; a lane, see `removeLaneOutcome`; a group, `removeGroupOutcome`. Nothing else can be taken out.
 */
function removeNodeOutcome(snapshot: Snapshot, node: NodeId): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!found) return noNode(node);
  if (found.kind === "effect") return removeEffectOutcome(snapshot, found);
  if (found.kind === "instrument") return removeLaneOutcome(snapshot, found);
  if (found.kind === "group") return removeGroupOutcome(snapshot, found);
  // (What is left is a modulator: the master is no node of the model, as far as the controller goes.)
  if (found.kind !== "modulator") return noNode(node);
  const touching = snapshot.graph.edges.filter((e) => isModulation(e) && (e.from.node === node || e.to.node === node)).map((e) => (e as { id: EdgeId }).id);
  return { restructured: without(snapshot, cablesThatGo(snapshot.graph.edges, touching), new Set([node])) };
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
  if (!owner) return noNode(parent);
  if (owner.kind !== "instrument" && owner.kind !== "group") return invalid("a modulator goes in a lane or in a group");
  return { restructured: withModulator(snapshot, parent, kind, counters).session };
}

/** The session with a modulator of `kind` at the end of the rack of the lane or group `parent` (which is one), and the node it is. */
function withModulator(snapshot: Snapshot, parent: NodeId, kind: ModulatorKind, counters: Counters): { session: Snapshot; node: Node } {
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
  const session: Snapshot = {
    ...snapshot,
    graph: { ...snapshot.graph, nodes: [...nodes.slice(0, at), added, ...nodes.slice(at)] },
    telemetry: { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, modulation },
  };
  return { session, node: added };
}

/**
 * A modulator of `kind` made for what the port `to` of a node is, with a cable from it to the port, as one change (`Controller::modulate_intent`; the TUI's `m` and
 * `l`). It goes at the end of the rack that reaches `to`: the lane's for the plugin of a lane (its instrument or an effect of its chain), the group's for a bus effect,
 * and the rack a modulator is in for a setting of it, where the new one is a sibling that drives it. The cable is drawn as `addModEdge` draws one (what it reaches is read
 * from the graph, so the modulator is made first), at half depth, or at `depth` clamped as a depth that is set is; if it cannot be drawn, nothing is made and no id is used.
 */
function modulateOutcome(snapshot: Snapshot, to: Edge["to"], kind: ModulatorKind, depth: number | null, counters: Counters): Outcome {
  // (In the controller's order: the number, the node, and then what the cable is refused for.)
  if (depth !== null && !holds(depth)) return invalid(`${shown(depth)} is not a value for the depth of a cable`);
  const target = snapshot.graph.nodes.find((n) => n.id === to.node && n.kind !== "master");
  if (!target) return noNode(to.node);
  if (target.kind === "group") return invalid("a modulator drives a parameter of a plugin or a setting of a modulator, and that is neither");
  // The rack that reaches it: a lane's, for its instrument and the effects of its chain (which sit in it), a group's for a bus effect, and a modulator's own.
  const parent = target.kind === "instrument" ? target.id : target.parent;
  if (parent === null) return invalid("that node is in no lane or group");
  const mark = counters.mark();
  const { session, node } = withModulator(snapshot, parent, kind, counters);
  const drawn = addModEdgeOutcome(session, node.id, to, counters);
  if (!("restructured" in drawn)) {
    counters.rewind(mark);
    return drawn;
  }
  if (depth === null) return drawn;
  const edges = drawn.restructured.graph.edges.map((e) => (isModulation(e) && e.from.node === node.id ? { ...e, amount: clamp(depth, DEPTH_RANGE[0], DEPTH_RANGE[1]) } : e));
  return { restructured: { ...drawn.restructured, graph: { ...drawn.restructured.graph, edges } } };
}

/**
 * An effect put at the end of the chain of a lane or a group, as the controller does it (`Controller::add_effect`): the plugin
 * has to be one the catalog names (the mock's, `catalog`), not an instrument, and one the mock can make a node of (the built-ins:
 * the controller's own nodes, as a fixture). The graph lists the nodes of a lane, or of a group's bus, together (the lane or the group, its
 * effects, its modulators), so the effect goes after the last effect it has; and the audio path goes through it: the link that
 * led out of the chain (to the master, or, for a lane in a group, to the group) leaves from it now.
 */
function addEffectOutcome(snapshot: Snapshot, parent: NodeId, plugin: string, counters: Counters, catalog: readonly CatalogEntry[]): Outcome {
  const owner = snapshot.graph.nodes.find((n) => n.id === parent && n.kind !== "master");
  if (!owner) return noNode(parent);
  if (owner.kind !== "instrument" && owner.kind !== "group") return invalid("an effect goes in a lane or in a group");
  const entry = catalog.find((e) => e.id === plugin);
  if (!entry) return invalid(`${plugin} is not a plugin that is installed`);
  if (entry.isInstrument) return invalid(`${entry.name} is an instrument, not an effect`);
  const made = builtinEffects[plugin];
  if (!made) return invalid(`${entry.name} could not be loaded`);

  const { nodes, edges } = snapshot.graph;
  const lastEffect = nodes.findLastIndex((n) => n.parent === parent && n.kind === "effect");
  const end = lastEffect >= 0 ? nodes[lastEffect] : owner;
  const at = nodes.indexOf(end ?? owner) + 1;
  const added: Node = { id: counters.node(), parent, name: made.name, kind: "effect", plugin: made.plugin, mix: made.mix };
  const out = edges.findIndex((e) => e.kind !== "modulation" && e.from.node === (end ?? owner).id && e.from.port.type === "audioOut");
  const leaving = edges[out];
  if (!leaving || leaving.kind === "modulation") return invalid("that has no audio path to put an effect in");
  const into: Edge = { from: leaving.from, to: { node: added.id, port: { type: "audioIn" } }, kind: "audio" };
  const onwards: Edge = { from: { node: added.id, port: { type: "audioOut" } }, to: leaving.to, kind: leaving.kind };
  return {
    restructured: {
      ...snapshot,
      graph: { nodes: [...nodes.slice(0, at), added, ...nodes.slice(at)], edges: [...edges.slice(0, out), into, onwards, ...edges.slice(out + 1)] },
    },
  };
}

/**
 * The instrument `plugin` as the controller makes a node of it (`load_from_catalog`): the plugin has to be one the catalog names
 * (the mock's, `catalog`), an instrument and not an effect, and one the mock can make a node of (the built-ins: the controller's
 * own nodes, as a fixture); or why not.
 */
function instrumentFor(plugin: string, catalog: readonly CatalogEntry[]): { made: (typeof builtinInstruments)[string] } | { error: Outcome } {
  const entry = catalog.find((e) => e.id === plugin);
  if (!entry) return { error: invalid(`${plugin} is not a plugin that is installed`) };
  if (!entry.isInstrument) return { error: invalid(`${entry.name} is an effect, not an instrument`) };
  const made = builtinInstruments[plugin];
  if (!made) return { error: invalid(`${entry.name} could not be loaded`) };
  return { made };
}

/**
 * A lane added at the end with an instrument in it, as the controller does it (`Controller::add_instrument_lane`): full range, in no
 * group. The graph lists the groups, then the lanes, then the master, so the lane goes before the master; its link to the master
 * comes after the links the others have (the modulation cables come after all of them); and the layout of the telemetry gets its
 * two meters, a peak and a clip count, after those of the lanes there are.
 */
function addInstrumentOutcome(snapshot: Snapshot, plugin: string, counters: Counters, catalog: readonly CatalogEntry[]): Outcome {
  const found = instrumentFor(plugin, catalog);
  if ("error" in found) return found.error;
  const { made } = found;
  const { nodes, edges } = snapshot.graph;
  const master = nodes.find((n) => n.kind === "master");
  if (!master) return invalid("the session has no master to hear a lane");

  const added: Node = { id: counters.node(), parent: null, name: made.name, kind: "instrument", plugin: made.plugin, range: made.range, transpose: made.transpose, volume: made.volume };
  const atNode = nodes.indexOf(master);
  const link: Edge = { from: { node: added.id, port: { type: "audioOut" } }, to: { node: master.id, port: { type: "audioIn" } }, kind: "audio" };
  const firstCable = edges.findIndex(isModulation);
  const atEdge = firstCable >= 0 ? firstCable : edges.length;
  const meters: LayoutEntry[] = [
    { slot: counters.slot(), node: added.id, port: { type: "audioOut" }, kind: "peak" },
    { slot: counters.slot(), node: added.id, port: { type: "audioOut" }, kind: "clips" },
  ];
  return {
    restructured: {
      ...snapshot,
      graph: { nodes: [...nodes.slice(0, atNode), added, ...nodes.slice(atNode)], edges: [...edges.slice(0, atEdge), link, ...edges.slice(atEdge)] },
      telemetry: { ...snapshot.telemetry, rev: snapshot.telemetry.rev + 1, meters: [...snapshot.telemetry.meters, ...meters] },
    },
  };
}

/**
 * An instrument put in a lane in place of the one it has (or in an empty lane), as the controller does it (`Controller::put_instrument`):
 * the node stays, with its name and plugin the new instrument's and its range, transposition and volume as they were. The cables that
 * drove the old plugin's parameters go first, a lane's and a group's, with what drove the depth of any of them: they name a parameter
 * by the old plugin's own index.
 */
function replaceInstrumentOutcome(snapshot: Snapshot, lane: NodeId, plugin: string, catalog: readonly CatalogEntry[]): Outcome {
  const target = snapshot.graph.nodes.find((n) => n.id === lane && n.kind !== "master");
  if (!target) return noNode(lane);
  if (target.kind !== "instrument") return invalid("an instrument goes in a lane");
  const found = instrumentFor(plugin, catalog);
  if ("error" in found) return found.error;
  const { made } = found;

  const touching = snapshot.graph.edges.filter(isModulation).filter((e) => e.to.node === lane).map((e) => e.id);
  const taken = without(snapshot, cablesThatGo(snapshot.graph.edges, touching), new Set());
  const replaced: Node = { ...target, name: made.name, plugin: made.plugin };
  return { restructured: { ...taken, graph: { ...taken.graph, nodes: taken.graph.nodes.map((n) => (n.id === lane ? replaced : n)) } } };
}

/**
 * A preset loaded into the plugin of `node`, as the controller does it (`load_preset`): the plugin has to be one of a lane (a bus
 * effect's cannot be loaded into: the audio thread addresses a preset by the lane's slot), and has the preset; the event says
 * the preset's name and the values the plugin has for its parameters after it. A built-in's are the controller's own (a fixture);
 * a plugin of the machine's the mock knows nothing of, so it says none.
 */
function presetOutcome(snapshot: Snapshot, node: Node, id: string): Outcome {
  if (node.kind !== "instrument" && node.kind !== "effect") return invalid("that node has no plugin");
  if (node.kind === "effect" && snapshot.graph.nodes.find((n) => n.id === node.parent)?.kind === "group") return invalid("a bus effect has no presets to load");
  if (!node.plugin) return invalid("that instrument has no plugin");
  const preset = node.plugin.presets.find((p) => p.id === id);
  if (!preset) return invalid(`the plugin has no preset ${id}`);
  const values = builtinPresets[node.plugin.id]?.[id] ?? [];
  return { event: { type: "presetLoaded", node: node.id, preset: preset.name, values } };
}

/**
 * The keys of a lane set, as the controller's `set_range` does it: only a lane has them (an empty one too), and they are
 * notes, whole numbers from 0 to 127, the low one not above the high one; none is every note.
 */
function rangeOutcome(node: Node, range: NoteRange | null): Outcome {
  if (node.kind !== "instrument") return invalid("a key range belongs to a lane");
  if (range !== null) {
    for (const note of [range.low, range.high]) {
      if (!Number.isInteger(note) || note < 0 || note > 127) return invalid(`${String(note)} is not a note: the notes are 0 to 127`);
    }
    if (range.low > range.high) return invalid(`the range goes from ${noteName(range.low)} down to ${noteName(range.high)}: its low note has to be the lower`);
  }
  return { event: { type: "rangeSet", node: node.id, range } };
}

/**
 * The piano's scale set, as the controller's `set_scale` does it: the text has to be a root and a kind of scale (the flats and
 * the long names too, in any case). The event tells the scale as it is saved, and its notes.
 */
function scaleOutcome(text: string): Outcome {
  const scale = parseScale(text);
  if (!scale) return invalid(`${JSON.stringify(text)} is not a scale: a root and a kind of scale, like C major or F# dorian`);
  return { event: { type: "scaleSet", ...scale } };
}

/** A coordinate of a place as the controller keeps it (`NodeUi::tidy`): to a tenth, half away from zero, within the limits, and never `-0`. */
function tidyCoordinate(value: number): number {
  const kept = (Math.sign(value) * Math.round(Math.abs(clamp(value, -NODE_UI_LIMIT, NODE_UI_LIMIT)) * NODE_UI_PARTS)) / NODE_UI_PARTS;
  return kept + 0;
}

/**
 * A node's place set, as the controller's `set_node_ui` does it: any node of the graph, the master too (which the controller
 * finds though it is not a node of the model), and a place that is two numbers, kept as `tidyCoordinate` says; none gives the
 * node back to the layout.
 */
function placeOutcome(snapshot: Snapshot, node: NodeId, ui: NodeUi | null): Outcome {
  if (!snapshot.graph.nodes.some((n) => n.id === node)) return noNode(node);
  if (ui === null) return { event: { type: "nodeUiSet", node, ui } };
  if (!holds(ui.x) || !holds(ui.y)) return invalid(`(${String(ui.x)}, ${String(ui.y)}) is not a place`);
  return { event: { type: "nodeUiSet", node, ui: { x: tidyCoordinate(ui.x), y: tidyCoordinate(ui.y) } } };
}

/**
 * Parameters pinned to a node's plugin, as the controller's `set_pinned` does it: a node that has a plugin (the master is no node of the
 * model, as far as the controller goes), a list that is not longer than a node may have, of parameters the plugin has, each once. The
 * controller refuses a separator too, which the page's graph does not have.
 */
function pinnedOutcome(snapshot: Snapshot, node: NodeId, params: readonly number[]): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node);
  if (!found || node === MASTER) return noNode(node);
  if (found.kind !== "instrument" && found.kind !== "effect") return invalid("that node has no plugin");
  const plugin = found.plugin;
  if (!plugin) return invalid("that instrument has no plugin");
  if (params.length > PINNED_LIMIT) return invalid(`at most ${PINNED_LIMIT} parameters can be pinned to a node`);
  for (const [i, param] of params.entries()) {
    if (plugin.params.every((p) => p.index !== param)) return invalid(`the plugin has no parameter ${param}`);
    if (params.indexOf(param) < i) return invalid(`parameter ${param} is listed twice`);
  }
  return { event: { type: "pinnedSet", node, params: [...params] } };
}

/**
 * A group named, as the controller's `rename_group` does it: only a group has a name (the master is no node of the model, as far as the
 * controller goes), taken without the spaces round it (what Rust's `trim` takes: the Unicode white space), at most `NAME_LIMIT`
 * characters (the characters that a person sees, not the halves of a pair that a string counts), and none of them a control character.
 * A name of nothing is no name: the group is called "Group" and its place among the groups, counting from one.
 */
function renameGroupOutcome(snapshot: Snapshot, node: NodeId, name: string): Outcome {
  const found = snapshot.graph.nodes.find((n) => n.id === node);
  if (!found || node === MASTER) return noNode(node);
  if (found.kind !== "group") return invalid("only a group has a name to change");
  const trimmed = name.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, "");
  if (Array.from(trimmed).length > NAME_LIMIT) return invalid(`a name is at most ${NAME_LIMIT} characters`);
  if (/\p{Cc}/u.test(trimmed)) return invalid("a name has no control characters");
  const place = snapshot.graph.nodes.filter((n) => n.kind === "group").findIndex((n) => n.id === node);
  return { event: { type: "groupRenamed", node, name: trimmed === "" ? `Group ${place + 1}` : trimmed } };
}

/** The lane `lane` put in the group `group` (or in none), the snapshot as it is: see `setLaneGroupOutcome`. Both are known to be what they should be. */
function lanePutIn(snapshot: Snapshot, lane: NodeId, group: NodeId | null): Snapshot {
  const found = snapshot.graph.nodes.find((n) => n.id === lane);
  const master = snapshot.graph.nodes.find((n) => n.kind === "master");
  if (!found || !master || found.parent === group) return snapshot;
  const { nodes } = snapshot.graph;
  // The cables of the group it leaves that go into the lane: to its plugins, to its modulators' settings and to the depths of their cables.
  const taken = without(snapshot, cablesThatGo(snapshot.graph.edges, cablesIntoLaneFromItsGroup(snapshot.graph, lane)), new Set());
  // The lane's node is a member of the group, and the last link of its chain, the one that leaves the chain, goes into the group (or the master).
  const chain = new Set<NodeId>([lane, ...nodes.filter((n) => n.parent === lane && n.kind === "effect").map((n) => n.id)]);
  const edges = taken.graph.edges.map((e): Edge => {
    if (e.kind === "modulation" || !chain.has(e.from.node) || chain.has(e.to.node)) return e;
    return { from: e.from, to: { node: group ?? master.id, port: { type: "audioIn" } }, kind: group === null ? "audio" : "member" };
  });
  return { ...taken, graph: { nodes: taken.graph.nodes.map((n) => (n.id === lane ? { ...n, parent: group } : n)), edges } };
}

/**
 * A lane put in a group, or taken out of its group (`null`), as the controller does it (`Controller::set_lane_group`): the node has to be a
 * lane and the group a group (the master is no node of the model, as far as the controller goes). The cables of the group the lane leaves
 * go first (`lanePutIn`; what they were driving has the value that was set again, which is the audio thread's to do), and the audio path of
 * the lane ends in its new group, or in the master.
 */
function setLaneGroupOutcome(snapshot: Snapshot, node: NodeId, group: NodeId | null): Outcome {
  const lane = snapshot.graph.nodes.find((n) => n.id === node && n.kind !== "master");
  if (!lane) return noNode(node);
  if (lane.kind !== "instrument") return invalid("only a lane can be put in a group");
  if (group !== null) {
    const target = snapshot.graph.nodes.find((n) => n.id === group && n.kind !== "master");
    if (!target) return noNode(group);
    if (target.kind !== "group") return invalid("that node is not a group");
  }
  return { restructured: lanePutIn(snapshot, node, group) };
}

/**
 * A group added at the end, with the lanes in `members` put in it, as the controller does it (`add_group_intent`): every member is looked at
 * first, and one that is no lane refuses the whole. The graph lists the groups (each with its effects and modulators), then the lanes, then the
 * master, so the group goes before the first lane; its link to the master comes after the links of the groups there are, and its two meters in
 * the layout after theirs (the master's, the groups', the lanes').
 */
function addGroupOutcome(snapshot: Snapshot, members: readonly NodeId[], counters: Counters): Outcome {
  for (const member of members) {
    const lane = snapshot.graph.nodes.find((n) => n.id === member && n.kind !== "master");
    if (!lane) return noNode(member);
    if (lane.kind !== "instrument") return invalid("only a lane can be put in a group");
  }
  const { nodes, edges } = snapshot.graph;
  const master = nodes.find((n) => n.kind === "master");
  if (!master) return invalid("the session has no master to hear a group");

  const id = counters.node();
  const added: Node = { id, parent: null, name: `Group ${nodes.filter((n) => n.kind === "group").length + 1}`, kind: "group", volume: 1, pan: 0 };
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const inLane = (node: NodeId): boolean => {
    const found = byId.get(node);
    const owner = found?.kind === "effect" && found.parent !== null ? byId.get(found.parent) : found;
    return owner?.kind === "instrument";
  };
  const firstLane = nodes.findIndex((n) => n.kind === "instrument");
  const atNode = firstLane >= 0 ? firstLane : nodes.indexOf(master);
  const firstLink = edges.findIndex((e) => e.kind !== "modulation" && inLane(e.from.node));
  const firstCable = edges.findIndex(isModulation);
  const atEdge = firstLink >= 0 ? firstLink : firstCable >= 0 ? firstCable : edges.length;
  const link: Edge = { from: { node: id, port: { type: "audioOut" } }, to: { node: master.id, port: { type: "audioIn" } }, kind: "audio" };
  const meters: LayoutEntry[] = [
    { slot: counters.slot(), node: id, port: { type: "audioOut" }, kind: "peak" },
    { slot: counters.slot(), node: id, port: { type: "audioOut" }, kind: "clips" },
  ];
  const firstMeter = snapshot.telemetry.meters.findIndex((m) => byId.get(m.node)?.kind === "instrument");
  const atMeter = firstMeter >= 0 ? firstMeter : snapshot.telemetry.meters.length;
  const made: Snapshot = {
    ...snapshot,
    graph: { nodes: [...nodes.slice(0, atNode), added, ...nodes.slice(atNode)], edges: [...edges.slice(0, atEdge), link, ...edges.slice(atEdge)] },
    telemetry: {
      ...snapshot.telemetry,
      rev: snapshot.telemetry.rev + 1,
      meters: [...snapshot.telemetry.meters.slice(0, atMeter), ...meters, ...snapshot.telemetry.meters.slice(atMeter)],
    },
  };
  return { restructured: members.reduce((session, lane) => lanePutIn(session, lane, id), made) };
}

/** The session saved to its own file, or why it cannot be (the controller's words for a session with no file). */
function saveOutcome(snapshot: Snapshot): Outcome {
  if (snapshot.meta.file === null) return { error: { code: "io", message: "No session path — cannot save" } };
  return { event: { type: "saved", file: snapshot.meta.file } };
}

/** Do what the controller does with an intent, to a snapshot, with the plugins its catalog names. */
function outcomeOf(snapshot: Snapshot, intent: Intent, counters: Counters, catalog: readonly CatalogEntry[]): Outcome {
  if (intent.type === "save") return saveOutcome(snapshot);
  // (Undo and Redo are not the outcome of an intent on a snapshot: the history is the server's, `MockServer#carry`.)
  if (intent.type === "undo" || intent.type === "redo") return invalid(`${intent.type} is done by the history`);
  if (intent.type === "setScale") return scaleOutcome(intent.scale);
  if (intent.type === "setLocked") return { event: { type: "lockedSet", locked: intent.locked } };
  if (intent.type === "setNodeUi") return placeOutcome(snapshot, intent.node, intent.ui);
  if (intent.type === "setPinned") return pinnedOutcome(snapshot, intent.node, intent.params);
  if (intent.type === "renameGroup") return renameGroupOutcome(snapshot, intent.node, intent.name);
  if (intent.type === "setLaneGroup") return setLaneGroupOutcome(snapshot, intent.node, intent.group);
  if (intent.type === "addGroup") return addGroupOutcome(snapshot, intent.members, counters);
  if (intent.type === "addEffect") return addEffectOutcome(snapshot, intent.parent, intent.plugin, counters, catalog);
  if (intent.type === "addInstrument") return addInstrumentOutcome(snapshot, intent.plugin, counters, catalog);
  if (intent.type === "replaceInstrument") return replaceInstrumentOutcome(snapshot, intent.node, intent.plugin, catalog);
  if (intent.type === "setEdgeDepth") return depthOutcome(snapshot, intent.edge, intent.depth);
  if (intent.type === "removeEdge") return removeEdgeOutcome(snapshot, intent.edge);
  if (intent.type === "addModulator") return addModulatorOutcome(snapshot, intent.parent, intent.kind, counters);
  if (intent.type === "modulate") return modulateOutcome(snapshot, intent.to, intent.kind, intent.depth, counters);
  if (intent.type === "removeNode") return removeNodeOutcome(snapshot, intent.node);
  if (intent.type === "moveEffect") return moveEffectOutcome(snapshot, intent.node, intent.index);
  if (intent.type === "addModEdge") return addModEdgeOutcome(snapshot, intent.modulator, intent.to, counters);
  // (The master is a node of the graph and not of the model: the controller does not find it.)
  const node = snapshot.graph.nodes.find((n) => n.id === intent.node && n.kind !== "master");
  if (!node) return noNode(intent.node);
  switch (intent.type) {
    case "setParam": {
      // (In the controller's order: a node with no plugin, a lane with none, a plugin without the parameter, a number it cannot hold.)
      if (node.kind !== "instrument" && node.kind !== "effect") return invalid("that node has no plugin");
      if (node.plugin === null) return invalid("that instrument has no plugin");
      const param = node.plugin.params.find((p) => p.index === intent.param);
      if (!param) return invalid(`the plugin has no parameter ${intent.param}`);
      if (!holds(intent.value)) return invalid(`${shown(intent.value)} is not a value for ${param.name}`);
      const clamped = clamp(intent.value, param.min, param.max);
      const value = param.labels === null ? clamped : roundIn32Bits(clamped);
      return { event: { type: "paramSet", node: node.id, param: param.index, value } };
    }
    case "setGain": {
      // (The number is judged before the node it is for.)
      if (!holds(intent.value)) return invalid(`${shown(intent.value)} is not a value for ${variant(intent.gain)}`);
      const fits =
        (intent.gain === "volume" && node.kind === "instrument") ||
        (intent.gain === "mix" && node.kind === "effect") ||
        ((intent.gain === "groupVolume" || intent.gain === "groupPan") && node.kind === "group");
      if (!fits) return invalid(`${variant(intent.gain)} does not apply to that node`);
      const [low, high] = GAIN_RANGES[intent.gain];
      return { event: { type: "gainSet", node: node.id, gain: intent.gain, value: clamp(intent.value, low, high) } };
    }
    case "loadPreset":
      return presetOutcome(snapshot, node, intent.preset);
    case "setRange":
      return rangeOutcome(node, intent.range);
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

/** The number of a channel in the header of its frames. */
const CHANNEL_NUMBERS: Record<TelemetryChannel, number> = { meters: 0, modulation: 1, held: 2 };

/** The bytes of a telemetry frame, as the server writes them (`telemetry_frame`). */
function frameBytes(channel: TelemetryChannel, layoutRev: number, values: readonly number[]): ArrayBuffer {
  const buffer = new ArrayBuffer(8 + 4 * values.length);
  const view = new DataView(buffer);
  view.setUint8(0, CHANNEL_NUMBERS[channel]);
  view.setUint16(2, values.length, true);
  view.setUint32(4, layoutRev, true);
  values.forEach((value, i) => {
    view.setFloat32(8 + 4 * i, value, true);
  });
  return buffer;
}

/** How fast a meter's peak falls, as the engine's does: 40 dB a second. */
const PEAK_FALL_DB_PER_SECOND = 40;

/** What an intent told the clients: the events it made, stamped, and whether it changed the structure (they are given the snapshot then). */
export interface Told {
  events: StampedEvent[];
  restructured: boolean;
}

export class MockServer {
  /** Everything the clients have sent, in order. */
  readonly received: ClientMessage[] = [];
  /** A factory of transports to this server, for a client. */
  readonly transport: TransportFactory = () => new MockConnection(this);

  readonly #options: MockOptions;
  #snapshot: Snapshot;
  #catalog: { entries: readonly CatalogEntry[]; scanning: boolean };
  /** How many times the catalog has been told (it is the `rev` of the message). */
  #catalogRev = 0;
  readonly #counters: Counters;
  readonly #connections = new Set<MockConnection>();
  /** What can be undone and redone, as the controller keeps it (`src/app/history.rs`). */
  readonly #history = new History();
  /** What the clients were last told Undo and Redo would do. */
  #historyTold: HistoryState = { undo: null, redo: null };

  constructor(options: MockOptions = {}) {
    this.#options = options;
    this.#snapshot = options.snapshot ?? tracerSnapshot;
    this.#catalog = options.catalog ?? { entries: builtinCatalog, scanning: false };
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

  /** What Undo and Redo would do now. */
  get history(): HistoryState {
    return this.#history.state();
  }

  /** Another front-end's intent (the TUI's, or another browser's): applied, and every subscribed client is told. What was told. */
  apply(intent: Intent): Told {
    const done = this.#carry(intent, true);
    if ("error" in done) throw new Error(done.error.message);
    // (The real server tells what can be undone at its next refresh, when a front-end applies an intent on its own.)
    this.#tellHistory();
    return done.told;
  }

  /** Change the session without telling anybody: the clients will see a revision missing at the next change. */
  applyQuietly(intent: Intent): void {
    const done = this.#carry(intent, false);
    if ("error" in done) throw new Error(done.error.message);
  }

  /** The server's idea of what the telemetry slots mean changes (a node came or went): the subscribed clients are told. */
  announceLayout(layout: TelemetryLayout): void {
    this.#snapshot = { ...this.#snapshot, telemetry: layout };
    for (const connection of this.#connections) {
      if (connection.subscribed) connection.tell({ type: "layout", layout });
    }
  }

  /** The scan finds more plugins, or ends: the subscribed clients are told the catalog as it is now. */
  announceCatalog(entries: readonly CatalogEntry[], scanning: boolean): void {
    this.#catalog = { entries, scanning };
    for (const connection of this.#connections) {
      if (connection.subscribed) connection.tell(this.#catalogMessage());
    }
  }

  #catalogMessage(): ServerMessage {
    return { type: "catalog", rev: ++this.#catalogRev, scanning: this.#catalog.scanning, entries: [...this.#catalog.entries] };
  }

  #historyMessage(): ServerMessage {
    const { undo, redo } = this.#history.state();
    return { type: "history", undo, redo };
  }

  /** Tell the subscribed clients what Undo and Redo would do, if it is not what they were told. */
  #tellHistory(): void {
    const now = this.#history.state();
    if (now.undo === this.#historyTold.undo && now.redo === this.#historyTold.redo) return;
    this.#historyTold = now;
    for (const connection of this.#connections) {
      if (connection.subscribed) connection.tell(this.#historyMessage());
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
        connection.tell(this.#catalogMessage());
        connection.tell(this.#historyMessage());
        break;
      case "telemetry":
        connection.wantTelemetry(message.channels);
        break;
      case "request": {
        const done = this.#carry(message.intent, true);
        if ("error" in done) {
          connection.tell({ type: "rejected", reqId: message.reqId, error: done.error });
        } else {
          // (The events are told before the answer, and so is the snapshot that tells a change of structure, and what can be undone now.)
          this.#tellHistory();
          connection.tell({ type: "ack", reqId: message.reqId, rev: done.rev });
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
   * Do an intent as the controller does, what it keeps in the history included: the revision it made and what it told, or why it could not
   * be done. Undo and Redo apply what the last step says, each intent of it a revision of its own, and are no steps themselves.
   */
  #carry(intent: Intent, tell: boolean): { rev: number; told: Told } | { error: WireError } {
    if (intent.type === "undo" || intent.type === "redo") return this.#backAndForth(intent.type, tell);
    const plan = planOf(this.#snapshot, intent, this.now(), this.#catalog.entries);
    const before = this.#snapshot;
    this.#counters.begin();
    const outcome = outcomeOf(this.#snapshot, intent, this.#counters, this.#catalog.entries);
    const made = this.#counters.take();
    if ("error" in outcome) return outcome;
    const done = this.#commit(outcome, tell);
    this.#keep(plan, made, before);
    return done;
  }

  /** The intents of the last step that can be undone, or of the last that was undone, carried out one after the other. */
  #backAndForth(which: "undo" | "redo", tell: boolean): { rev: number; told: Told } | { error: WireError } {
    const step = which === "undo" ? this.#history.takeUndo() : this.#history.takeRedo();
    if (!step) return { error: { code: "invalid", message: `there is nothing to ${which}` } };
    let rev = this.#snapshot.rev;
    const told: Told = { events: [], restructured: false };
    // What was taken out is put back as it was: the nodes and the cables of the session, and the layout of its telemetry, are what they were before.
    // (Everything done after was undone first, so that nothing else has changed them.)
    if (which === "undo" && step.taken) {
      const { graph, telemetry } = step.taken;
      const done = this.#commit({ restructured: { ...this.#snapshot, graph, telemetry } }, tell);
      this.#history.keptUndone(step);
      return done;
    }
    // What they make is made under the ids it was made under the first time (what is not made of them is not kept for what is made next).
    this.#counters.reuse(which === "undo" ? step.undoMade : step.redoMade);
    const was = this.#snapshot;
    try {
      // (A step whose intent cannot be done, a node that is not there any more, is dropped, and what was done of it stays.)
      for (const intent of which === "undo" ? step.undo : step.redo) {
        const outcome = outcomeOf(this.#snapshot, intent, this.#counters, this.#catalog.entries);
        if ("error" in outcome) return outcome;
        const done = this.#commit(outcome, tell);
        rev = done.rev;
        told.events.push(...done.told.events);
        told.restructured ||= done.told.restructured;
      }
    } finally {
      this.#counters.dropReused();
    }
    // What a removal that is done again takes is what the next undo puts back (the step's way back is replaced each time, as the controller's is): the session as it was just before,
    // which is not what it was the first time when what was added has been added again, with slots of the telemetry of its own.
    if (which === "redo" && step.taken) step.taken = { graph: was.graph, telemetry: was.telemetry };
    if (which === "undo") this.#history.keptUndone(step);
    else this.#history.keptRedone(step);
    return { rev, told };
  }

  /** An intent has been carried out, and made `made`: its step is kept, or the history is forgotten. */
  #keep(plan: Plan, made: Made, before: Snapshot): void {
    if (plan === "barrier") {
      this.#history.clear();
    } else if (plan !== "nothing") {
      if ("step" in plan) {
        this.#history.record(plan.step);
        return;
      }
      if ("removed" in plan) {
        const { label, intent, at } = plan.removed;
        this.#history.record(removedStep(label, intent, { graph: before.graph, telemetry: before.telemetry }, at));
        return;
      }
      const { label, intent, at } = plan.added;
      const undo = takesOut(this.#snapshot, intent, made);
      // (It made something that cannot be named, or what it made is not there: nothing puts it back.)
      if (undo) this.#history.record({ ...stepOf(label, undo, intent, null, at), redoMade: made });
      else this.#history.clear();
    }
  }

  /**
   * The next revision: apply what the intent did to the model and, if `tell`, tell the subscribed clients. The
   * revision of the intent. (A change of structure is that, and the snapshot that tells it is the revision after,
   * as `Controller::apply` has it.)
   */
  #commit(outcome: Exclude<Outcome, { error: WireError }>, tell: boolean): { rev: number; told: Told } {
    const rev = this.#snapshot.rev + 1;
    if ("restructured" in outcome) {
      this.#snapshot = { ...outcome.restructured, rev: rev + 1, meta: { ...outcome.restructured.meta, dirty: true } };
      if (tell) {
        for (const connection of this.#connections) {
          if (connection.subscribed) connection.tell({ type: "snapshot", snapshot: this.#snapshot });
        }
      }
      return { rev, told: { events: [], restructured: true } };
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
    return { rev, told: { events: [stamped], restructured: false } };
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
    // (The held notes follow no layout: a value for each of the 128 notes, 1 for one that any connection holds.)
    if (channel === "held") {
      const held = this.heldNotes;
      return frameBytes(channel, 0, Array.from({ length: 128 }, (_, note) => (held.has(note) ? 1 : 0)));
    }
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
