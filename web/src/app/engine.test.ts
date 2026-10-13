import { afterEach, describe, expect, test, vi } from "vitest";

import { createConnection, engineFor } from "./engine";

describe("which engine a page asks for", () => {
  test("is the server it came from, unless it says otherwise", () => {
    expect(engineFor("")).toBe("server");
    expect(engineFor("?foo=1")).toBe("server");
    expect(engineFor("?engine=server")).toBe("server");
    expect(engineFor("?engine=whatever")).toBe("server");
  });

  test("is the mock when the address says engine=mock", () => {
    expect(engineFor("?engine=mock")).toBe("mock");
    expect(engineFor("?foo=1&engine=mock")).toBe("mock");
  });

  test("makes a client of the kind", () => {
    const at = { protocol: "http:", host: "localhost:5173", hash: "" };
    expect(createConnection({ ...at, search: "?engine=mock" }).engine).toBe("mock");
    expect(createConnection({ ...at, search: "" }).engine).toBe("server");
    expect(createConnection({ ...at, search: "" }).client.state.status).toBe("idle");
  });
});

/** A WebSocket that does nothing but remember what it was asked to do. */
class FakeSocket {
  static readonly OPEN = 1;
  static instances: FakeSocket[] = [];
  binaryType = "blob";
  readyState = 0;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: unknown }) => void) | null = null;
  constructor(readonly url: string) {
    FakeSocket.instances.push(this);
  }
  send(text: string): void {
    this.sent.push(text);
  }
  close(): void {
    // (A real socket reports that it has closed a moment later; nothing here is waiting for that.)
  }
}

describe("the connection of a page", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    FakeSocket.instances = [];
  });

  test("goes to the WebSocket of the server the page came from, and gives it the token in the address", () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const { client } = createConnection({ protocol: "https:", host: "tang.example:9000", search: "", hash: "#token=0123abcd" });
    client.start();
    const socket = FakeSocket.instances[0];
    expect(socket?.url).toBe("wss://tang.example:9000/ws");
    if (!socket) return;
    socket.readyState = FakeSocket.OPEN;
    socket.onopen?.();
    socket.onmessage?.({ data: JSON.stringify({ type: "hello", protocol: 1, rev: 0, features: [], needsToken: true }) });
    expect(socket.sent.map((text) => JSON.parse(text) as unknown)).toEqual([{ type: "auth", token: "0123abcd" }, { type: "subscribe" }]);
    client.stop();
  });

  test("does not open a socket when the engine is the mock", () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const { client } = createConnection({ protocol: "http:", host: "localhost:5173", search: "?engine=mock", hash: "" });
    client.start();
    expect(FakeSocket.instances).toHaveLength(0);
    client.stop();
  });
});
