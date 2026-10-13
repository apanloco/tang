// Which server the page talks to: the one it came from, or the mock engine that runs in
// the page (`?engine=mock`), which needs nothing but the page itself.
import { Client } from "../client/connection";
import { MockServer } from "../client/mock";
import { serverUrl, tokenFromHash, webSocketTransport } from "../client/websocket";

export type EngineKind = "server" | "mock";

/** The engine a page address asks for: `?engine=mock` for the mock, the server otherwise. */
export function engineFor(search: string): EngineKind {
  return new URLSearchParams(search).get("engine") === "mock" ? "mock" : "server";
}

export interface Connection {
  client: Client;
  engine: EngineKind;
}

/** A client for the page at `location`, not yet started. */
export function createConnection(location: Pick<Location, "protocol" | "host" | "search" | "hash">): Connection {
  const engine = engineFor(location.search);
  const transport = engine === "mock" ? new MockServer().transport : webSocketTransport(serverUrl(location));
  return { client: new Client({ transport, token: tokenFromHash(location.hash) }), engine };
}
