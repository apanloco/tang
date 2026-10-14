// The history of what a person turned, and the way back (docs/web-ui.md §5 and D35): the mock engine's, which has to be the controller's
// (`src/app/history.rs`), so that what is built on the mock is built on what the program does; the fixture `history.json` pins the two together.
//
// Every change that can be undone leaves a step: the intents that put it back and the intents that do it again, in the words a button has for
// it. A change that goes on (a knob that is dragged is a change at every frame) is one step: a change of what the last step was of, at most a
// second after the last of it, is merged into it, and the step goes back to what the first change found.
import { HISTORY_MAX_STEPS, HISTORY_SAME_STEP, WAVEFORMS } from "../protocol";
import type { Gain, Intent, ModulatorField, Node, Snapshot } from "../protocol";

/** What a change is of, for a change that goes on to be told from the rest: a string that says which parameter, gain, setting or cable. */
export type Key = string;

/** What the buttons need to say: the words of what Undo and Redo would do, or null for what they would not. */
export interface HistoryState {
  undo: string | null;
  redo: string | null;
}

/** What the buttons say when there is nothing to undo or redo (and what they say before the server has told). */
export const NO_HISTORY: HistoryState = { undo: null, redo: null };

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
}

/** A step of one change: `undo` puts back what it found, `redo` is the change. */
export function stepOf(label: string, undo: Intent, redo: Intent, key: Key | null, at: number): Step {
  return { label, undo: [undo], redo: [redo], key, last: at };
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

/** What a change is to the history: a step, or something that cannot be undone yet (and what came before cannot be undone past it), or no change. */
export type Plan = { step: Step } | "barrier" | "nothing";

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
export function planOf(snapshot: Snapshot, intent: Intent, now: number): Plan {
  const nodeOf = (id: number): Node | undefined => snapshot.graph.nodes.find((n) => n.id === id);
  const step = (label: string, undo: Intent, key: Key | null): Plan => ({ step: stepOf(label, undo, intent, key, now) });
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
    // Nothing puts these back yet.
    case "loadPreset":
    case "addGroup":
    case "setLaneGroup":
    case "removeEdge":
    case "addModulator":
    case "addModEdge":
    case "removeNode":
    case "addEffect":
    case "addInstrument":
    case "replaceInstrument":
      return "barrier";
  }
}
