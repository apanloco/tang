// Which server the page talks to: the one it came from, or the mock engine that runs in
// the page (`?engine=mock`), which needs nothing but the page itself.
import { Client } from "../client/connection";
import { tracerSnapshot } from "../client/fixtures";
import { MockServer } from "../client/mock";
import type { MockOptions } from "../client/mock";
import { stressSnapshot } from "../client/stress";
import { serverUrl, tokenFromHash, webSocketTransport } from "../client/websocket";

export type EngineKind = "server" | "mock";

/** The MIDI inputs that the mock engine says are open, for a source of notes to be added for (the real program says which inputs it has opened). */
export const MOCK_INPUTS: readonly string[] = ["Mock Keyboard MIDI In", "Mock Pad Controller"];

/**
 * The session the mock engine serves: the tracer, the big one that the performance test runs on, or two lanes with an
 * LFO each (where a cable can be drawn out of reach, which the tracer's one lane has no room for).
 */
export type SceneKind = "tracer" | "stress" | "lanes";

/** The engine a page address asks for: `?engine=mock` for the mock, the server otherwise. */
export function engineFor(search: string): EngineKind {
  return new URLSearchParams(search).get("engine") === "mock" ? "mock" : "server";
}

/** The scene an address asks of the mock engine: `?scene=stress` for the big one, `?scene=lanes` for the two lanes, the tracer otherwise. */
export function sceneFor(search: string): SceneKind {
  const scene = new URLSearchParams(search).get("scene");
  return scene === "stress" || scene === "lanes" ? scene : "tracer";
}

/** The snapshot of a scene, as the program has the session once it has loaded it: every modulator is the session's own, and hears the lane or the group it was made for. */
function snapshotOf(scene: SceneKind): MockOptions {
  switch (scene) {
    case "stress":
      return { snapshot: stressSnapshot() };
    case "lanes":
      return { snapshot: stressSnapshot({ lanes: 2, lfosPerLane: 1, targetsPerLfo: 1 }) };
    case "tracer":
      return { snapshot: tracerSnapshot };
  }
}

export interface Connection {
  client: Client;
  engine: EngineKind;
}

/** A client for the page at `location`, not yet started. */
export function createConnection(location: Pick<Location, "protocol" | "host" | "search" | "hash">): Connection {
  const engine = engineFor(location.search);
  const mock = (): MockServer => new MockServer({ ...snapshotOf(sceneFor(location.search)), inputs: [...MOCK_INPUTS] });
  const transport = engine === "mock" ? mock().transport : webSocketTransport(serverUrl(location));
  // The views draw the levels, what the modulators do and the keys that are held, which the server only makes for those who ask.
  return { client: new Client({ transport, token: tokenFromHash(location.hash), telemetry: ["meters", "modulation", "held"] }), engine };
}
