// The session the mock engine serves and the tests read: the snapshot of
// `fixtures/tracer.toml` (a saw oscillator through a low-pass filter whose cutoff an LFO
// sweeps: what `examples/tracer.toml` was, kept as the tests' own since a person who runs the
// program on the example can save over it) exactly as the server makes it. The JSON is generated from the Rust types and
// kept current by a Rust test (`the_web_clients_fixture_of_the_tracer_is_current`), so
// the cast below is as good as the generated types.
import type { CatalogEntry, Intent, Node, NodeId, NoteRange, ParamValue, Plugin, Port, Snapshot } from "../protocol";
import builtinEffectNodes from "./fixtures/builtins.effects.json";
import builtins from "./fixtures/builtins.catalog.json";
import builtinInstrumentNodes from "./fixtures/builtins.instruments.json";
import fields from "./fixtures/fields.json";
import groups from "./fixtures/groups.json";
import builtinPresetValues from "./fixtures/builtins.presets.json";
import moves from "./fixtures/moves.json";
import pins from "./fixtures/pins.json";
import reach from "./fixtures/reach.json";
import tracer from "./fixtures/tracer.snapshot.json";

export const tracerSnapshot = tracer as unknown as Snapshot;

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
 * A session with two groups, each with a modulator whose cables go to its members (a parameter, and the rate of a member's own modulator;
 * the first group has an envelope too, whose cable drives the depth of one of them), four lanes (two in the first group, one in the second, one
 * in none), and what the controller makes of the intents that add a group, put a
 * lane in one, name one and take a lane or a group out: for one that is refused its words, for a name the name the group has, and for the others what the session comes
 * to: its nodes (the id, the parent and the name of each), the audio path, the modulation cables and the two lists of the layout of the
 * telemetry, a line of text for each entry, made by the rules of `shapeOf` in `groups.test.ts`; and, for the tracer's one lane, what the
 * controller says to taking it out, since a session keeps a lane. Written by the controller
 * (`the_web_clients_fixture_of_what_the_intents_of_groups_come_to_is_current`).
 */
export const groupsFixture = groups as unknown as {
  snapshot: Snapshot;
  rows: ({ intent: Intent } & ({ error: string } | { name: string } | { after: Shape }))[];
  lastLane: { node: NodeId; error: string };
};

/** What a session comes to, for the fixture of the group intents: a line of text for each entry of each list. */
export interface Shape {
  nodes: string[];
  audio: string[];
  cables: string[];
  meters: string[];
  modulation: string[];
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
