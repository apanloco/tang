// The history of what a person turned, and the way back (docs/web-ui.md §5 and D35): the mock engine's, which has to be the controller's
// (`src/app/history.rs`), so that what is built on the mock is built on what the program does; the fixture `history.json` pins the two together.
//
// Every change that can be undone leaves a step: the intents that put it back and the intents that do it again, in the words a button has for
// it. A change that goes on (a knob that is dragged is a change at every frame) is one step: a change of what the last step was of, at most a
// second after the last of it, is merged into it, and the step goes back to what the first change found.
//
// What is added is taken out by undo and put back by redo under the ids it had (`Made`): every step that names the node or the cable goes on naming it.
import { HISTORY_MAX_STEPS, HISTORY_SAME_STEP, WAVEFORMS } from "../protocol";
import type { CatalogEntry, EdgeId, Gain, Intent, ModulatorField, ModulatorKind, Node, NodeId, Snapshot } from "../protocol";
import { cablesIntoLaneFromItsGroup } from "./membership";

/** What a change is of, for a change that goes on to be told from the rest: a string that says which parameter, gain, setting or cable. */
export type Key = string;

/** What the buttons need to say: the words of what Undo and Redo would do, or null for what they would not. */
export interface HistoryState {
  undo: string | null;
  redo: string | null;
}

/** What the buttons say when there is nothing to undo or redo (and what they say before the server has told). */
export const NO_HISTORY: HistoryState = { undo: null, redo: null };

/** The ids that what was done made, each in the order it was handed out: the nodes and the cables. */
export interface Made {
  nodes: NodeId[];
  edges: EdgeId[];
}

/** Nothing made. (A new one each time: what is made is written to.) */
export const noneMade = (): Made => ({ nodes: [], edges: [] });

/** One thing that was done, and the way back. */
export interface Step {
  /** What it was, for a button: "set cutoff". */
  label: string;
  /** The intents that put it back, in the order they are applied. */
  undo: Intent[];
  /** The intents that do it again, in the order they are applied. */
  redo: Intent[];
  /** What it was of, if it is a change that goes on. */
  key: Key | null;
  /** When it was last changed, in seconds. */
  last: number;
  /** The ids that `undo` makes, in the order it makes them (none, for most). */
  undoMade: Made;
  /** The ids that `redo` makes, in the order it makes them: what was added is added again under the ids it had. */
  redoMade: Made;
}

/** A step of one change: `undo` puts back what it found, `redo` is the change. */
export function stepOf(label: string, undo: Intent, redo: Intent, key: Key | null, at: number): Step {
  return { label, undo: [undo], redo: [redo], key, last: at, undoMade: noneMade(), redoMade: noneMade() };
}

/** The steps that can be undone, the last on top, and the steps that were undone and can be done again. */
export class History {
  readonly #undo: Step[] = [];
  readonly #redo: Step[] = [];

  /**
   * Something was done. What could have been redone cannot be any more. If it is another change of what the last step was of, soon after
   * it, the two are one step: it puts back what the first found and does what the last did.
   */
  record(step: Step): void {
    this.#redo.length = 0;
    const top = this.#undo.at(-1);
    if (top && step.key !== null && top.key === step.key && step.last - top.last <= HISTORY_SAME_STEP) {
      top.redo = step.redo;
      top.last = step.last;
      return;
    }
    this.#undo.push(step);
    if (this.#undo.length > HISTORY_MAX_STEPS) this.#undo.shift();
  }

  /** The step to undo, taken off (it is gone if it cannot be put back; `keptUndone` keeps it to be redone if it was). */
  takeUndo(): Step | undefined {
    return this.#undo.pop();
  }

  /** The step that was undone, to be redone. */
  keptUndone(step: Step): void {
    this.#redo.push(step);
  }

  /** The step to redo, taken off. */
  takeRedo(): Step | undefined {
    return this.#redo.pop();
  }

  /** The step that was redone, to be undone again: as it was, and not merged into the one before it. */
  keptRedone(step: Step): void {
    this.#undo.push(step);
  }

  /** Forget everything: something was done that cannot be undone, and what came before cannot be undone past it. True if there was something to forget. */
  clear(): boolean {
    const had = this.#undo.length > 0 || this.#redo.length > 0;
    this.#undo.length = 0;
    this.#redo.length = 0;
    return had;
  }

  /** What Undo and Redo would do. */
  state(): HistoryState {
    return { undo: this.#undo.at(-1)?.label ?? null, redo: this.#redo.at(-1)?.label ?? null };
  }

  /** How many steps can be undone (for tests). */
  get undoable(): number {
    return this.#undo.length;
  }

  /** How many steps can be redone (for tests). */
  get redoable(): number {
    return this.#redo.length;
  }
}

/** An add that is carried out, whose step is made once it is known what it made. */
export interface Added {
  label: string;
  intent: Intent;
  /** When it was done, in seconds. */
  at: number;
}

/**
 * What a change is to the history: a step; or an add, whose step is made when it is carried out; or something that cannot be undone yet (and what came
 * before cannot be undone past it); or no change.
 */
export type Plan = { step: Step } | { added: Added } | "barrier" | "nothing";

/** What a button that adds a modulator says it adds (`ModulatorKind::label`). */
const MODULATOR_LABELS: Record<ModulatorKind, string> = { lfo: "an LFO", envelope: "an envelope", midiLearn: "a MIDI control" };

/**
 * The intent that takes out what `added`, an add that was carried out, made: the one node, or the one cable, as it is in `snapshot`; or null if it
 * made something that cannot be named, or what it made is not there. As `takes_out` of the controller.
 */
export function takesOut(snapshot: Snapshot, added: Intent, made: Made): Intent | null {
  if (added.type === "addModEdge") {
    const [edge, ...more] = made.edges;
    if (edge === undefined || more.length > 0 || made.nodes.length > 0) return null;
    return snapshot.graph.edges.some((e) => e.kind === "modulation" && e.id === edge) ? { type: "removeEdge", edge } : null;
  }
  const [node, ...more] = made.nodes;
  if (node === undefined || more.length > 0 || made.edges.length > 0) return null;
  return snapshot.graph.nodes.some((n) => n.id === node) ? { type: "removeNode", node } : null;
}

const GAIN_LABELS: Record<Gain, string> = { volume: "set volume", mix: "set mix", groupVolume: "set the group's volume", groupPan: "set pan" };

/** The host-side gain `gain` of `node`, if it has it. */
function gainOf(node: Node, gain: Gain): number | undefined {
  if (gain === "volume" && node.kind === "instrument") return node.volume;
  if (gain === "mix" && node.kind === "effect") return node.mix;
  if (gain === "groupVolume" && node.kind === "group") return node.volume;
  if (gain === "groupPan" && node.kind === "group") return node.pan;
  return undefined;
}

/** The value of a setting of a modulator, if its kind has it. */
function fieldOf(node: Node, field: ModulatorField): number | undefined {
  if (node.kind !== "modulator") return undefined;
  const { source } = node;
  if (source.type === "lfo") {
    if (field === "rate") return source.rate;
    if (field === "waveform") return WAVEFORMS.indexOf(source.waveform);
    return undefined;
  }
  if (source.type === "envelope") {
    if (field === "attack") return source.attack;
    if (field === "decay") return source.decay;
    if (field === "sustain") return source.sustain;
    if (field === "release") return source.release;
  }
  return undefined;
}

/**
 * What `intent` is to the history, read from the session before it is applied: the step that puts back what it will change (so it is the value it
 * will overwrite that is read), or that it cannot be undone, or that it is no change. As `plan_history` of the controller.
 */
export function planOf(snapshot: Snapshot, intent: Intent, now: number, catalog: readonly CatalogEntry[]): Plan {
  const nodeOf = (id: number): Node | undefined => snapshot.graph.nodes.find((n) => n.id === id);
  const step = (label: string, undo: Intent, key: Key | null): Plan => ({ step: stepOf(label, undo, intent, key, now) });
  const added = (label: string): Plan => ({ added: { label, intent, at: now } });
  const catalogName = (plugin: string): string | undefined => catalog.find((entry) => entry.id === plugin)?.name;
  switch (intent.type) {
    case "save":
    case "undo":
    case "redo":
      return "nothing";
    case "setParam": {
      const node = nodeOf(intent.node);
      const plugin = node && (node.kind === "instrument" || node.kind === "effect") ? node.plugin : null;
      const row = plugin?.params.find((p) => p.index === intent.param);
      if (!row) return "nothing";
      return step(`set ${row.name}`, { type: "setParam", node: intent.node, param: intent.param, value: row.value }, `param:${String(intent.node)}:${String(intent.param)}`);
    }
    case "setGain": {
      const node = nodeOf(intent.node);
      const old = node ? gainOf(node, intent.gain) : undefined;
      if (old === undefined) return "nothing";
      return step(GAIN_LABELS[intent.gain], { type: "setGain", node: intent.node, gain: intent.gain, value: old }, `gain:${String(intent.node)}:${intent.gain}`);
    }
    case "setModulatorField": {
      const node = nodeOf(intent.node);
      const old = node ? fieldOf(node, intent.field) : undefined;
      if (old === undefined) return "nothing";
      return step(`set ${intent.field}`, { type: "setModulatorField", node: intent.node, field: intent.field, value: old }, `field:${String(intent.node)}:${intent.field}`);
    }
    case "setEdgeDepth": {
      const edge = snapshot.graph.edges.find((e) => e.kind === "modulation" && e.id === intent.edge);
      if (edge?.kind !== "modulation") return "nothing";
      return step("set the depth of a cable", { type: "setEdgeDepth", edge: intent.edge, depth: edge.amount }, `depth:${String(intent.edge)}`);
    }
    case "setRange": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "instrument") return "nothing";
      return step("set the keys", { type: "setRange", node: intent.node, range: node.range }, null);
    }
    case "setNodeUi": {
      const node = nodeOf(intent.node);
      if (!node) return "nothing";
      return step("move a node", { type: "setNodeUi", node: intent.node, ui: node.ui ?? null }, null);
    }
    case "setPinned": {
      const node = nodeOf(intent.node);
      const plugin = node && (node.kind === "instrument" || node.kind === "effect") ? node.plugin : null;
      if (!plugin) return "nothing";
      return step("choose the knobs", { type: "setPinned", node: intent.node, params: [...plugin.pinned] }, null);
    }
    case "renameGroup": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "group") return "nothing";
      // (The graph has the name a group is called by, and not whether it was ever given one: one that is called by its place has none.)
      const place = snapshot.graph.nodes.filter((n) => n.kind === "group").findIndex((n) => n.id === node.id);
      const old = node.name === `Group ${String(place + 1)}` ? "" : node.name;
      return step("rename a group", { type: "renameGroup", node: intent.node, name: old }, null);
    }
    case "setScale":
      return step("set the scale", { type: "setScale", scale: snapshot.meta.piano.scale }, null);
    case "setLocked":
      return step(intent.locked ? "lock the piano to its scale" : "let every note of the piano through", { type: "setLocked", locked: snapshot.meta.piano.locked }, null);
    case "moveEffect": {
      const node = nodeOf(intent.node);
      if (node?.kind !== "effect") return "nothing";
      const chain = snapshot.graph.nodes.filter((n) => n.kind === "effect" && n.parent === node.parent);
      return step("move an effect", { type: "moveEffect", node: intent.node, index: chain.findIndex((n) => n.id === node.id) }, null);
    }
    case "addEffect": {
      // (The catalog names the plugin, or the intent is refused.)
      const name = catalogName(intent.plugin);
      return name === undefined ? "nothing" : added(`add ${name}`);
    }
    case "addInstrument": {
      const name = catalogName(intent.plugin);
      return name === undefined ? "nothing" : added(`add ${name} as an instrument`);
    }
    case "addModulator":
      return added(`add ${MODULATOR_LABELS[intent.kind]}`);
    case "addModEdge":
      return added("draw a cable");
    case "addGroup": {
      // (A member that is no lane is refused with the rest, so it need not be looked at.)
      const leavesAGroup = intent.members.some((id) => {
        const member = nodeOf(id);
        return member?.kind === "instrument" && member.parent !== null;
      });
      // A lane that leaves its group is no longer driven by it, and the cables that did are gone: a step does not bring them back.
      return leavesAGroup ? "barrier" : added("add a group");
    }
    case "setLaneGroup": {
      // (A node that is no lane, and a group that is no group, are refused: no step is kept for them.)
      const lane = nodeOf(intent.node);
      if (lane?.kind !== "instrument") return "nothing";
      if (lane.parent === intent.group) return "nothing";
      // The cables of the group it leaves are taken out, and a step does not bring them back.
      if (cablesIntoLaneFromItsGroup(snapshot.graph, lane.id).length > 0) return "barrier";
      return step(intent.group === null ? "take a lane out of its group" : "put a lane in a group", { type: "setLaneGroup", node: intent.node, group: lane.parent }, null);
    }
    // Nothing puts these back yet.
    case "loadPreset":
    case "removeEdge":
    case "removeNode":
    case "replaceInstrument":
      return "barrier";
  }
}
