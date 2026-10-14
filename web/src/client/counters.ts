// What the mock engine hands out, as the controller does (`NodeIds` of `src/app/model.rs`, `SlotAllocator` of `src/telemetry.rs`): the ids of nodes and of cables, and the
// slots of the telemetry.
import type { EdgeId, NodeId, Snapshot } from "../protocol";
import { noneMade } from "./history";
import type { Made } from "./history";

/**
 * What the controller hands out and does not hand out again, however much is taken out after: the ids of
 * nodes and the telemetry slots. They go on from what the session has. One thing is handed out twice: what undo puts back has the ids it
 * had (`reuse`), so that every step of the history that names it goes on naming it; the ids are written down while a log is kept (`begin`,
 * `take`), for the step that an add leaves.
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
    this.#edge = 1 + Math.max(-1, ...graph.edges.flatMap((e) => (e.kind === "modulation" ? [e.id] : [])));
    this.#slot = 1 + Math.max(-1, ...[...telemetry.meters, ...telemetry.modulation].map((entry) => entry.slot));
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
}
