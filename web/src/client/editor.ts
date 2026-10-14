// What the user does to the structure of the session, as the page asks it: add a modulator to a
// lane or a group, draw a cable from a modulator to a parameter, take either out. Each is an intent
// (docs/web-ui.md §5); the answer is the session's, which arrives as a snapshot, and what this adds
// to a plain request is what a person needs: the node that was made (the intent does not name it:
// a client finds it in the session it is then given), and, when the server refuses, a notice that
// says what could not be done and why, in the names the canvas shows.
import type { EdgeId, Graph, Intent, ModulatorKind, NodeId, NodeUi, NoteRange, Port } from "../protocol";
import type { Client } from "./connection";
import { cableLabel, displayNames, portName } from "./names";
import { rangeLabel } from "./notes";
import type { Notices } from "./notices";
import type { Reveals } from "./reveals";

const KINDS: Record<ModulatorKind, string> = { lfo: "an LFO", envelope: "an envelope", midiLearn: "a MIDI control" };

/** What a person was doing when an intent of each kind was made, for a notice that says it could not be done. */
const DOING: Record<Intent["type"], string> = {
  setParam: "set a parameter",
  setGain: "set a level",
  loadPreset: "load a preset",
  setModulatorField: "change a modulator's setting",
  setEdgeDepth: "change the depth of a cable",
  removeEdge: "take out a cable",
  addModulator: "add a modulator",
  addModEdge: "draw a cable",
  addEffect: "add an effect",
  addInstrument: "add an instrument",
  replaceInstrument: "change the instrument of a lane",
  setRange: "set the keys of a lane",
  setScale: "set the scale of the piano",
  setLocked: "lock or unlock the piano",
  setNodeUi: "move a node",
  removeNode: "remove a node",
  moveEffect: "move an effect along its chain",
  save: "save the session",
};

const whyNot = (error: unknown): string => (error instanceof Error ? error.message : String(error));

/** The notice for an intent that did not get through, when nothing more is known of what it was about (a knob turned: `Drive`). */
export function refusal(intent: Intent, error: unknown): string {
  return `Could not ${DOING[intent.type]}: ${whyNot(error)}`;
}

export class Editor {
  readonly #client: Client;
  readonly #notices: Notices;
  readonly #reveals: Reveals | undefined;

  /** `reveals`, if there is a canvas to show things on, is told which node the page added, for it to show. */
  constructor(client: Client, notices: Notices, reveals?: Reveals) {
    this.#client = client;
    this.#notices = notices;
    this.#reveals = reveals;
  }

  /**
   * Add a modulator of `kind` to the lane or group `parent`. Resolves with the id of the node it
   * made, or with null if it was refused (the page says why). The node is the last modulator of
   * `parent` the session did not have before: two clients adding at once to one parent may be given
   * each other's, which is as good.
   */
  async addModulator(parent: NodeId, kind: ModulatorKind): Promise<NodeId | null> {
    const before = new Set(this.#graph?.nodes.map((node) => node.id));
    if (!(await this.#attempt(`add ${KINDS[kind]} to ${this.#name(parent)}`, { type: "addModulator", parent, kind }))) return null;
    const added = this.#graph?.nodes.filter((node) => node.kind === "modulator" && node.parent === parent && !before.has(node.id));
    return this.#shown(added?.at(-1)?.id);
  }

  /**
   * Put the plugin `plugin` (the `id` of an entry of the catalog the server told) at the end of the effect chain of the lane
   * or group `parent`. Resolves with the id of the node it made, or with null if it was refused (the page says why). As for
   * `addModulator`: the node is the last effect of `parent` the session did not have before.
   */
  async addEffect(parent: NodeId, plugin: string): Promise<NodeId | null> {
    const before = new Set(this.#graph?.nodes.map((node) => node.id));
    if (!(await this.#attempt(`add ${this.#pluginName(plugin)} to ${this.#name(parent)}`, { type: "addEffect", parent, plugin }))) return null;
    const added = this.#graph?.nodes.filter((node) => node.kind === "effect" && node.parent === parent && !before.has(node.id));
    return this.#shown(added?.at(-1)?.id);
  }

  /**
   * Add a lane at the end of the session with the instrument `plugin` (the `id` of an entry of the catalog the server told) in it.
   * Resolves with the id of the lane it made (its instrument's node), or with null if it was refused (the page says why). As for
   * `addModulator`: the node is the last lane the session did not have before.
   */
  async addInstrument(plugin: string): Promise<NodeId | null> {
    const before = new Set(this.#graph?.nodes.map((node) => node.id));
    if (!(await this.#attempt(`add ${this.#pluginName(plugin)} as an instrument`, { type: "addInstrument", plugin }))) return null;
    const added = this.#graph?.nodes.filter((node) => node.kind === "instrument" && !before.has(node.id));
    return this.#shown(added?.at(-1)?.id);
  }

  /**
   * Put the instrument `plugin` in the lane `node` in place of the one it has (an empty lane is filled). The lane keeps its
   * node, its effects, its modulators, its range and its volume; the cables that drove the old instrument's parameters go with it.
   * True if it was done (the page says why if it was not).
   */
  replaceInstrument(node: NodeId, plugin: string): Promise<boolean> {
    return this.#attempt(`change the instrument of ${this.#name(node)} to ${this.#pluginName(plugin)}`, { type: "replaceInstrument", node, plugin });
  }

  /**
   * Load the preset `preset` (its `id`) into the plugin of the instrument or effect `node` (a lane's: a bus effect has no presets to
   * load): the plugin's parameters take its values. True if it was loaded (the page says why if it was not, by the preset's name).
   */
  loadPreset(node: NodeId, preset: string): Promise<boolean> {
    return this.#attempt(`load ${this.#presetName(node, preset)} into ${this.#name(node)}`, { type: "loadPreset", node, preset });
  }

  /**
   * Set the keys the lane `node` takes: `range`, both ends included, or null for every note. True if they were set (the page says
   * why if they were not, with the keys written as the TUI writes them: `C2-C6`).
   */
  setRange(node: NodeId, range: NoteRange | null): Promise<boolean> {
    return this.#attempt(`set the keys of ${this.#name(node)} to ${rangeLabel(range)}`, { type: "setRange", node, range });
  }

  /**
   * Set the scale of the piano: `scale` is a root and a kind as the session file says them (`C major`, `F# dorian`). True if it
   * was set (the page says why if it was not).
   */
  setScale(scale: string): Promise<boolean> {
    return this.#attempt(`set the scale of the piano to ${scale}`, { type: "setScale", scale });
  }

  /** Lock the piano to its scale, so that a note outside it is not played, or let every note through. True if it was done. */
  setLocked(locked: boolean): Promise<boolean> {
    return this.#attempt(locked ? "lock the piano to its scale" : "let the piano play every note", { type: "setLocked", locked });
  }

  /**
   * Say where the node `node` is drawn, `ui` in the pixels of the canvas, or null to give it back to the layout (the master is
   * a node too). The server keeps it to a tenth of a pixel and within the limits of the canvas, and saves it with the session.
   * True if it was kept (the page says why if it was not).
   */
  moveNode(node: NodeId, ui: NodeUi | null): Promise<boolean> {
    const what = ui === null ? `give ${this.#name(node)} back to the layout` : `move ${this.#name(node)}`;
    return this.#attempt(what, { type: "setNodeUi", node, ui });
  }

  /**
   * Give every node that has a place back to the layout (the master too), one after the other. True if they all went; the page says why
   * if one did not, and goes on with the rest. With no node placed there is nothing to ask.
   */
  async resetLayout(): Promise<boolean> {
    const placed = this.#graph?.nodes.filter((node) => node.ui !== undefined) ?? [];
    let all = true;
    for (const node of placed) {
      if (!(await this.moveNode(node.id, null))) all = false;
    }
    return all;
  }

  /**
   * Draw a cable from the modulator `modulator` to `to`, a port of `node`: a parameter of the plugin in it (by the plugin's own
   * index), or a setting of the modulator it is (its rate, a stage of its envelope). True if it was drawn.
   */
  addCable(modulator: NodeId, node: NodeId, to: Port): Promise<boolean> {
    const words = cableLabel(this.#name(modulator), this.#name(node), this.#portName(node, to));
    return this.#attempt(`draw a cable from ${words.replace(" → ", " to ")}`, { type: "addModEdge", modulator, to: { node, port: to } });
  }

  /** Take the modulator or effect `node` out, with the cables that drive it (and, for a modulator, those it drives). True if it went. */
  removeNode(node: NodeId): Promise<boolean> {
    return this.#attempt(`remove ${this.#name(node)}`, { type: "removeNode", node });
  }

  /**
   * Move the effect `node` (a lane's or a bus's) to place `index` of its chain, 0 being the first, the nearest the instrument or the
   * group: the effects between its place and the new one move one place the other way, and the audio goes through them in the new
   * order. The cables that drive them stay on the plugins they drove. True if it was moved (the page says why if it was not, with the place
   * counted from 1, as a person counts).
   */
  moveEffect(node: NodeId, index: number): Promise<boolean> {
    return this.#attempt(`move ${this.#name(node)} to place ${String(index + 1)} of its chain`, { type: "moveEffect", node, index });
  }

  /** Take the cable `edge` out. True if it went. */
  removeCable(edge: EdgeId): Promise<boolean> {
    return this.#attempt(`take out ${this.#cable(edge)}`, { type: "removeEdge", edge });
  }

  /** Write the session to its own file. True if it was written (the page says why if it was not). */
  save(): Promise<boolean> {
    return this.#attempt(DOING.save, { type: "save" });
  }

  /** The node the page added, if it found it, which the canvas is asked to show (it is in the session by now); the node, or null. */
  #shown(node: NodeId | undefined): NodeId | null {
    if (node === undefined) return null;
    this.#reveals?.show(node);
    return node;
  }

  get #graph(): Graph | undefined {
    return this.#client.state.snapshot?.graph;
  }

  /** What a plugin is called, by the catalog the server told, or by its id if the catalog is not known (yet) or does not have it. */
  #pluginName(id: string): string {
    return this.#client.state.catalog?.entries.find((entry) => entry.id === id)?.name ?? id;
  }

  /** What a plugin's preset is called, by the plugin's own list, or by its id if the node or the preset is not known. */
  #presetName(node: NodeId, id: string): string {
    const found = this.#graph?.nodes.find((n) => n.id === node);
    const plugin = found?.kind === "instrument" || found?.kind === "effect" ? found.plugin : null;
    return plugin?.presets.find((preset) => preset.id === id)?.name ?? id;
  }

  #name(node: NodeId): string {
    const graph = this.#graph;
    return (graph && displayNames(graph).get(node)) ?? "that node";
  }

  #portName(node: NodeId, port: Port): string | null {
    const graph = this.#graph;
    return graph ? portName(graph, node, port) : null;
  }

  /** A cable in words, as the canvas labels it; "that cable" if it is not in the session. */
  #cable(edge: EdgeId): string {
    const graph = this.#graph;
    const found = graph?.edges.find((e) => e.kind === "modulation" && e.id === edge);
    if (!graph || !found) return "that cable";
    const names = displayNames(graph);
    return `the cable ${cableLabel(names.get(found.from.node) ?? "?", names.get(found.to.node) ?? "?", portName(graph, found.to.node, found.to.port))}`;
  }

  /** Do the intent; if it is refused, or the connection fails, say so as a notice and return false. */
  async #attempt(what: string, intent: Intent): Promise<boolean> {
    try {
      await this.#client.request(intent);
      return true;
    } catch (error) {
      this.#notices.say(`Could not ${what}: ${whyNot(error)}`);
      return false;
    }
  }
}
