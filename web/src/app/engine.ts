// Which server the page talks to: the one it came from, or the mock engine that runs in
// the page (`?engine=mock`), which needs nothing but the page itself.
import { Client } from "../client/connection";
import { MockServer } from "../client/mock";
import { stressSnapshot } from "../client/stress";
import { serverUrl, tokenFromHash, webSocketTransport } from "../client/websocket";

export type EngineKind = "server" | "mock";

/** The session the mock engine serves: the tracer, or the big one that the performance test runs on. */
export type SceneKind = "tracer" | "stress";

/** The engine a page address asks for: `?engine=mock` for the mock, the server otherwise. */
export function engineFor(search: string): EngineKind {
  return new URLSearchParams(search).get("engine") === "mock" ? "mock" : "server";
}

/** The scene an address asks of the mock engine: `?scene=stress` for the big one, the tracer otherwise. */
export function sceneFor(search: string): SceneKind {
  return new URLSearchParams(search).get("scene") === "stress" ? "stress" : "tracer";
}

export interface Connection {
  client: Client;
  engine: EngineKind;
}

/** A client for the page at `location`, not yet started. */
export function createConnection(location: Pick<Location, "protocol" | "host" | "search" | "hash">): Connection {
  const engine = engineFor(location.search);
  const mock = (): MockServer => new MockServer(sceneFor(location.search) === "stress" ? { snapshot: stressSnapshot() } : {});
  const transport = engine === "mock" ? mock().transport : webSocketTransport(serverUrl(location));
  // The views draw the levels and what the modulators do, which the server only makes for those who ask.
  return { client: new Client({ transport, token: tokenFromHash(location.hash), telemetry: ["meters", "modulation"] }), engine };
}
