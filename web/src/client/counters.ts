// What the mock engine hands out, as the controller does (`NodeIds` of `src/app/model.rs`, `SlotAllocator` of `src/telemetry.rs`): the ids of nodes and of cables, and the
// slots of the telemetry.
import type { EdgeId, NodeId, Snapshot } from "../protocol";
import { noneMade } from "./history";
import type { Made } from "./history";

/** Where the counters were (`Counters#mark`), for them to go back to (`Counters#rewind`). */
export interface Mark {
  readonly node: number;
  readonly edge: number;
  readonly slot: number;
  readonly putBack: readonly NodeId[];
  readonly putBackEdges: readonly EdgeId[];
  /** How much of the log there was, if one was begun. */
  readonly logged: { readonly nodes: number; readonly edges: number } | null;
}

/**
 * What the controller hands out and does not hand out again, however much is taken out after: the ids of
 * nodes and the telemetry slots. They go on from what the session has. One thing is handed out twice: what undo puts back has the ids it
 * had (`reuse`), so that every step of the history that names it goes on naming it; the ids are written down while a log is kept (`begin`,
 * `take`), for the step that an add leaves. What is handed out for something that is then refused is given back (`mark`, `rewind`): a refusal uses nothing.
 */
export class Counters {
  #node: number;
  #edge: number;
  #slot: number;
  /** The ids to hand out before new ones, in order. */
  #putBack: NodeId[] = [];
  #putBackEdges: EdgeId[] = [];
  /** Every id handed out since `begin`, if it was begun. */
  #log: Made | null = null;

  constructor(snapshot: Snapshot) {
    const { graph, telemetry } = snapshot;
    // (The master is a node of the graph and not of the model: its id is not one the model has handed out.)
    this.#node = 1 + Math.max(-1, ...graph.nodes.filter((n) => n.kind !== "master").map((n) => n.id));
    // (A cable of modulation, an audio cable and a cable of notes have ids of one series.)
    this.#edge = 1 + Math.max(-1, ...graph.edges.flatMap((e) => (e.kind === "modulation" || e.kind === "cable" || e.kind === "events" ? [e.id] : [])));
    // (The limiter's reduction is the last cell of the telemetry and is never handed out: slots go on from the others, `HANDED_OUT` of `src/telemetry.rs`.)
    this.#slot = 1 + Math.max(-1, ...[...telemetry.meters, ...telemetry.modulation].filter((entry) => entry.kind !== "reduction").map((entry) => entry.slot));
  }

  node(): NodeId {
    const id = this.#putBack.shift() ?? this.#node++;
    this.#log?.nodes.push(id);
    return id;
  }

  edge(): EdgeId {
    const id = this.#putBackEdges.shift() ?? this.#edge++;
    this.#log?.edges.push(id);
    return id;
  }

  /** The ids that come next, in this order, in place of new ones. */
  reuse(made: Made): void {
    this.#putBack = [...made.nodes];
    this.#putBackEdges = [...made.edges];
  }

  /** What was not used of the ids that were to come next is not to be: new ids go on. */
  dropReused(): void {
    this.#putBack = [];
    this.#putBackEdges = [];
  }

  /** Write down every id that is handed out, from now until `take`. */
  begin(): void {
    this.#log = noneMade();
  }

  /** The ids handed out since `begin`, in order; nothing is written down after this until the next `begin`. */
  take(): Made {
    const log = this.#log ?? noneMade();
    this.#log = null;
    return log;
  }

  slot(): number {
    this.#slot += 1;
    return this.#slot - 1;
  }

  /**
   * Where the counters are now. What is made may turn out to be refused, once what it needs is in the session (a modulator that is made for a cable to
   * be drawn from it, whose reach is read from the graph): going back to the mark (`rewind`) leaves no id and no slot used.
   */
  mark(): Mark {
    const logged = this.#log ? { nodes: this.#log.nodes.length, edges: this.#log.edges.length } : null;
    return { node: this.#node, edge: this.#edge, slot: this.#slot, putBack: [...this.#putBack], putBackEdges: [...this.#putBackEdges], logged };
  }

  /** Go back to where the counters were at `mark`: what was handed out since is handed out again, and is not in the log. */
  rewind(mark: Mark): void {
    this.#node = mark.node;
    this.#edge = mark.edge;
    this.#slot = mark.slot;
    this.#putBack = [...mark.putBack];
    this.#putBackEdges = [...mark.putBackEdges];
    if (this.#log && mark.logged) {
      this.#log.nodes.length = mark.logged.nodes;
      this.#log.edges.length = mark.logged.edges;
    }
  }
}
