// What the user does to the structure of the session, as the page asks it: add a modulator to a
// lane or a group, draw a cable from a modulator to a parameter, take either out. Each is an intent
// (docs/web-ui.md §5); the answer is the session's, which arrives as a snapshot, and what this adds
// to a plain request is what a person needs: the node that was made (the intent does not name it:
// a client finds it in the session it is then given), and, when the server refuses, a notice that
// says what could not be done and why, in the names the canvas shows.
import type { EdgeId, Graph, Intent, ModulatorKind, NodeId } from "../protocol";
import type { Client } from "./connection";
import { cableLabel, displayNames } from "./names";
import type { Notices } from "./notices";

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
  removeNode: "remove a node",
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

  constructor(client: Client, notices: Notices) {
    this.#client = client;
    this.#notices = notices;
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
    return added?.at(-1)?.id ?? null;
  }

  /** Draw a cable from the modulator `modulator` to parameter `param` (the plugin's own index) of the plugin in `node`. True if it was drawn. */
  addCable(modulator: NodeId, node: NodeId, param: number): Promise<boolean> {
    const words = cableLabel(this.#name(modulator), this.#name(node), this.#paramName(node, param));
    return this.#attempt(`draw a cable from ${words.replace(" → ", " to ")}`, { type: "addModEdge", modulator, to: { node, port: { type: "param", index: param } } });
  }

  /** Take the modulator `node` out, with its cables. True if it went. */
  removeModulator(node: NodeId): Promise<boolean> {
    return this.#attempt(`remove ${this.#name(node)}`, { type: "removeNode", node });
  }

  /** Take the cable `edge` out. True if it went. */
  removeCable(edge: EdgeId): Promise<boolean> {
    return this.#attempt(`take out ${this.#cable(edge)}`, { type: "removeEdge", edge });
  }

  /** Write the session to its own file. True if it was written (the page says why if it was not). */
  save(): Promise<boolean> {
    return this.#attempt(DOING.save, { type: "save" });
  }

  get #graph(): Graph | undefined {
    return this.#client.state.snapshot?.graph;
  }

  #name(node: NodeId): string {
    const graph = this.#graph;
    return (graph && displayNames(graph).get(node)) ?? "that node";
  }

  #paramName(node: NodeId, index: number): string | null {
    const found = this.#graph?.nodes.find((n) => n.id === node);
    const plugin = found?.kind === "instrument" || found?.kind === "effect" ? found.plugin : null;
    return plugin?.params.find((param) => param.index === index)?.name ?? null;
  }

  /** A cable in words, as the canvas labels it; "that cable" if it is not in the session. */
  #cable(edge: EdgeId): string {
    const graph = this.#graph;
    const found = graph?.edges.find((e) => e.kind === "modulation" && e.id === edge);
    if (!graph || !found) return "that cable";
    const names = displayNames(graph);
    const index = found.to.port.type === "param" ? found.to.port.index : null;
    const param = index === null ? null : this.#paramName(found.to.node, index);
    return `the cable ${cableLabel(names.get(found.from.node) ?? "?", names.get(found.to.node) ?? "?", param)}`;
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
