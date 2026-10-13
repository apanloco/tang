// The session the mock engine serves and the tests read: the snapshot of
// `examples/tracer.toml` (a saw oscillator through a low-pass filter whose cutoff an LFO
// sweeps) exactly as the server makes it. The JSON is generated from the Rust types and
// kept current by a Rust test (`the_web_clients_fixture_of_the_tracer_is_current`), so
// the cast below is as good as the generated types.
import type { Node, NodeId, Snapshot } from "../protocol";
import reach from "./fixtures/reach.json";
import tracer from "./fixtures/tracer.snapshot.json";

export const tracerSnapshot = tracer as unknown as Snapshot;

/**
 * A session with two groups, lanes in them and out of them, an empty lane and a modulator in a few of each,
 * and, for each modulator (by its node id), the plugin nodes the controller let a cable be drawn to: written by
 * the controller (`the_web_clients_fixture_of_what_a_modulator_reaches_is_current`).
 */
export const reachFixture = reach as unknown as { snapshot: Snapshot; reach: Record<string, number[]> };

/** The master's node id, as the server numbers it (`NodeId::MASTER`). */
export const MASTER: NodeId = 4294967295;

/** The node of a snapshot called `name`; throws if there is none, since a fixture that lacks it is wrong. */
export function nodeNamed(snapshot: Snapshot, name: string): Node {
  const node = snapshot.graph.nodes.find((n) => n.name.startsWith(name));
  if (!node) throw new Error(`the snapshot has no node called ${name}`);
  return node;
}
