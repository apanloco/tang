// The session the mock engine serves and the tests read: the snapshot of
// `fixtures/tracer.toml` (a saw oscillator through a low-pass filter whose cutoff an LFO
// sweeps: what `examples/tracer.toml` was, kept as the tests' own since a person who runs the
// program on the example can save over it) exactly as the server makes it. The JSON is generated from the Rust types and
// kept current by a Rust test (`the_web_clients_fixture_of_the_tracer_is_current`), so
// the cast below is as good as the generated types.
import type { CatalogEntry, Event, Intent, Node, NodeId, NoteRange, ParamValue, Plugin, Port, Snapshot, StampedEvent } from "../protocol";
import builtinEffectNodes from "./fixtures/builtins.effects.json";
import builtins from "./fixtures/builtins.catalog.json";
import builtinInstrumentNodes from "./fixtures/builtins.instruments.json";
import fields from "./fixtures/fields.json";
import filenames from "./fixtures/filenames.json";
import groups from "./fixtures/groups.json";
import history from "./fixtures/history.json";
import audible from "./fixtures/audible.json";
import builtinPresetValues from "./fixtures/builtins.presets.json";
import moves from "./fixtures/moves.json";
import noise from "./fixtures/noise.json";
import patterns from "./fixtures/patterns.json";
import pins from "./fixtures/pins.json";
import reach from "./fixtures/reach.json";
import steps from "./fixtures/steps.json";
import sync from "./fixtures/sync.json";
import tracer from "./fixtures/tracer.snapshot.json";

export const tracerSnapshot = tracer as unknown as Snapshot;

/**
 * What the engine makes of every combination of mute and solo of two groups and four lanes (the first two in the first group, the third in the second, the
 * fourth in none), read from a run of the audio graph by `the_web_clients_fixture_of_what_is_heard_is_current`: a case is the state of each node, two bits
 * each (the first muted and the second soloed), the groups first and then the lanes, and a sum whose bits are which lanes reach the master (1, 2, 4 and 8) and
 * which buses are heard (16 and 32).
 */
export const audibleFixture = audible as unknown as { groups: number; lanes: (number | null)[]; cases: [number, number][] };

/**
 * A session with two groups, lanes in them and out of them, an empty lane and a modulator in a few of each,
 * and, for each modulator (by its node id), the plugin nodes the controller let a cable be drawn to: written by
 * the controller (`the_web_clients_fixture_of_what_a_modulator_reaches_is_current`).
 */
export const reachFixture = reach as unknown as { snapshot: Snapshot; reach: Record<string, number[]> };

/**
 * A session with a group of three modulators whose first member has three of its own (an envelope with two cables among them), a
 * second member, a lane in no group and a lane in a second group, each with a modulator, and, for each modulator (by its node id),
 * the settings and cable depths of other modulators that the controller let a cable be drawn to, as a node and the port on it:
 * written by the controller (`the_web_clients_fixture_of_what_a_modulator_reaches_of_the_others_is_current`).
 */
export const fieldsFixture = fields as unknown as { snapshot: Snapshot; fields: Record<string, { node: NodeId; port: Port }[]> };

/**
 * A session with a group whose bus has four effects, a lane in it with four of its own and a lane in no group with three (a modulator in each
 * rack, with a cable to every plugin), and what the controller makes of asking it to move each of its nodes to each of some places: for a move
 * that is done the order of the nodes of the graph afterwards and the links of the audio path in the order of the graph's edges (the node each
 * leaves, the node it goes to, and its kind), for one that is refused its words. Written by the controller
 * (`the_web_clients_fixture_of_what_moving_an_effect_comes_to_is_current`).
 */
export const movesFixture = moves as unknown as {
  snapshot: Snapshot;
  moves: ({ node: NodeId; index: number } & ({ error: string } | { nodes: NodeId[]; audio: [NodeId, NodeId, "audio" | "member"][] }))[];
};

/**
 * A session with a group whose bus has an effect, a lane in it with an instrument and an effect, and an empty lane, and what the controller
 * makes of pinning some parameters to each of its nodes (the master and one that is not there too): for a list that is taken the
 * parameters pinned to the node's plugin afterwards, for one that is refused its words. Written by the controller
 * (`the_web_clients_fixture_of_what_pinning_comes_to_is_current`).
 */
export const pinsFixture = pins as unknown as {
  snapshot: Snapshot;
  pins: ({ node: NodeId; params: number[] } & ({ error: string } | { pinned: number[] }))[];
};

/**
 * A session with a group (a bus effect and an LFO in it), a member of it with a reverb and an LFO of its own, a lane with a pattern that was recorded at 90 beats a minute and a
 * lane with no instrument, at 100, and what the controller makes of intents about patterns: scripts, each a list of intents asked of one session in turn, with the event each is
 * answered with or the words it is refused with. They are every kind of intent asked of every node (the master and one that is not there too), with values that are held to their
 * range, rounded and taken as they are, and runs on each lane that go on from one intent to the next. Written by the controller
 * (`the_web_clients_fixture_of_what_a_pattern_comes_to_is_current`).
 */
export const patternsFixture = patterns as unknown as {
  snapshot: Snapshot;
  scripts: ({ intent: Intent } & ({ error: string } | { event: Event }))[][];
};

/**
 * A session with two groups, each with a modulator whose cables go to its members (a parameter, and the rate of a member's own modulator;
 * the first group has an envelope too, whose cable drives the depth of one of them), four lanes (two in the first group, one in the second, one
 * in none), and what the controller makes of the intents that add a group, put a
 * lane in one, name one and take a lane or a group out: for one that is refused its words, for a name the name the group has, and for the others what the session comes
 * to: its nodes (the id, the parent and the name of each), the audio path, the modulation cables and the two lists of the layout of the
 * telemetry, a line of text for each entry, made by the rules of `shapeOf` (below); and, for the tracer's one lane, what the
 * controller says to taking it out, since a session keeps a lane. Written by the controller
 * (`the_web_clients_fixture_of_what_the_intents_of_groups_come_to_is_current`).
 */
export const groupsFixture = groups as unknown as {
  snapshot: Snapshot;
  rows: ({ intent: Intent } & ({ error: string } | { name: string } | { after: Shape }))[];
  lastLane: { node: NodeId; error: string };
};

/**
 * One op of a script of the history fixture: time going by (in milliseconds), or an intent and what the controller made of it: its words if it
 * was refused, else the events it made, whether it changed the structure and, if it did, what the session comes to then (`shapeOf`).
 */
export type HistoryRow =
  | { wait: number }
  | ({ intent: Intent; history: { undo: string | null; redo: string | null } } & (
      | { error: string }
      | { events: StampedEvent[]; restructured: false }
      | { events: StampedEvent[]; restructured: true; shape: Shape }
    ));

/**
 * A session (a group with a bus filter, another with nothing and no name, a lane with a reverb, a filter, an LFO and an envelope with a cable
 * each, and a lane of its own with an LFO) and what the controller makes of scripts of intents and of time going by (a change is a step of its own when a second has gone by
 * since the last of its kind): for each intent the events it made, stamped, whether it changed the structure, what Undo and Redo would do after it,
 * or the words of its refusal. Written by the controller (`the_web_clients_fixture_of_what_a_history_comes_to_is_current`). A script that needs
 * what that session has not has a session of its own (`snapshot`).
 */
export const historyFixture = history as unknown as { snapshot: Snapshot; scripts: { name: string; snapshot?: Snapshot; rows: HistoryRow[] }[] };

/**
 * What each of some names comes to when the session is saved as it: the file it is saved as, or the controller's words for what is wrong with it. Written by
 * the controller (`the_web_clients_fixture_of_file_names_is_current`, `session::plain_file_name`).
 */
export const filenamesFixture = filenames;

/**
 * The rate, in cycles or steps a second, that the engine goes at for each of the note lengths that a modulator can follow the tempo by, at five tempos (`plugin::chain::rate_at_tempo`): a row
 * is the place in the list, its name, the tempo and the rate, each as the 64-bit number that the engine's 32-bit one widens to. Written by the model (`the_web_clients_fixture_of_what_a_note_length_comes_to_is_current`).
 */
export const syncFixture = sync;

/**
 * The numbers that the engine's `noise` makes for a spread of seeds and cycles (the random numbers of a random LFO, `plugin::chain::noise`), the ends of the 32 bits among them: each as the
 * 64-bit number that the engine's 32-bit one widens to. Written by the engine (`the_web_clients_fixture_of_the_noise_is_current`).
 */
export const noiseFixture = noise;

/**
 * What the engine's `steps_output` makes of a spread of patterns (three lists of sixteen steps), lengths, places and glides, written by `the_web_clients_fixture_of_what_a_step_sequencer_puts_out_is_current`:
 * the page's law of a step sequencer (`stepsOutput`) is pinned to it.
 */
export const stepsFixture = steps;

/** What a session comes to, for the fixture of the group intents: a line of text for each entry of each list. */
export interface Shape {
  nodes: string[];
  audio: string[];
  cables: string[];
  meters: string[];
  modulation: string[];
}

const portKey = (port: Port): string => `${port.type}${"index" in port ? String(port.index) : ""}${"edge" in port ? `#${String(port.edge)}` : ""}`;

/**
 * What a session comes to, as the fixtures write it (`shape_of` in `app/testutil.rs`, by the same rules): a line for each node (its id, its
 * parent, its name), each link of the audio path (the node it leaves, the node it goes to, its kind), each cable (its id, the node it leaves
 * and the one it goes to, the port, its depth) and each entry of the two lists of the layout of the telemetry (the slot, the node, the port,
 * the kind); the fingerprint of the layout is not part of it.
 */
export function shapeOf(snapshot: Snapshot): Shape {
  const { nodes, edges } = snapshot.graph;
  return {
    nodes: nodes.map((n) => `${String(n.id)} ${n.parent === null ? "-" : String(n.parent)} ${n.name}`),
    audio: edges.flatMap((e) => (e.kind === "modulation" ? [] : [`${String(e.from.node)}>${String(e.to.node)} ${e.kind}`])),
    cables: edges.flatMap((e) => (e.kind === "modulation" ? [`#${String(e.id)} ${String(e.from.node)}>${String(e.to.node)} ${portKey(e.to.port)} @${String(e.amount)}`] : [])),
    meters: snapshot.telemetry.meters.map((m) => `${String(m.slot)} ${String(m.node)} ${portKey(m.port)} ${m.kind}`),
    modulation: snapshot.telemetry.modulation.map((m) => `${String(m.slot)} ${String(m.node)} ${portKey(m.port)} ${m.kind}`),
  };
}

/** The built-in plugins as the server lists them in its catalog, which the mock engine serves: written by Rust (`the_web_clients_fixture_of_the_builtins_is_current`). */
export const builtinCatalog = builtins as CatalogEntry[];

/**
 * What the controller makes of each built-in effect that is added to a lane or a group, by the plugin's id: the node's name,
 * the plugin with the parameters it has when it is new, and the mix it starts at; the ids are the mock's to hand out. Written
 * by Rust (`the_web_clients_fixture_of_the_builtin_effects_is_current`).
 */
export const builtinEffects = builtinEffectNodes as unknown as Record<string, { name: string; kind: "effect"; plugin: Plugin; mix: number }>;

/**
 * What the controller makes of each built-in instrument that is added in a lane of its own, by the plugin's id: the node's
 * name, the plugin with the parameters it has when it is new, and the lane's range, transposition and volume (full range,
 * none, 1); the ids are the mock's to hand out. Written by Rust (`the_web_clients_fixture_of_the_builtin_instruments_is_current`).
 */
export const builtinInstruments = builtinInstrumentNodes as unknown as Record<
  string,
  { name: string; kind: "instrument"; plugin: Plugin; range: NoteRange | null; transpose: number; volume: number }
>;

/**
 * What the controller says each preset of each built-in plugin does (`PresetLoaded`'s values: what the plugin has for its
 * parameters after it), by the plugin's id and the preset's id. Written by Rust (`the_web_clients_fixture_of_the_builtin_presets_is_current`).
 */
export const builtinPresets = builtinPresetValues as unknown as Record<string, Record<string, ParamValue[]>>;

/** The master's node id, as the server numbers it (`NodeId::MASTER`). */
export const MASTER: NodeId = 4294967295;

/** The node of a snapshot called `name`; throws if there is none, since a fixture that lacks it is wrong. */
export function nodeNamed(snapshot: Snapshot, name: string): Node {
  const node = snapshot.graph.nodes.find((n) => n.name.startsWith(name));
  if (!node) throw new Error(`the snapshot has no node called ${name}`);
  return node;
}
