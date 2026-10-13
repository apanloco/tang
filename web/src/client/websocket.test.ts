import { describe, expect, test } from "vitest";

import type { CloseInfo, TransportEvents } from "./connection";
import { serverUrl, tokenFromHash, webSocketTransport } from "./websocket";

/** Just enough of a WebSocket to see what the transport does with one. */
class FakeSocket {
  static readonly OPEN = 1;
  static instances: FakeSocket[] = [];

  binaryType = "blob";
  readyState = 0;
  sent: string[] = [];
  closed = false;
  onopen: (() => void) | null = null;
  onmessage: ((message: { data: unknown }) => void) | null = null;
  onclose: ((close: { code: number; reason: string }) => void) | null = null;

  constructor(readonly url: string) {
    FakeSocket.instances.push(this);
  }

  send(text: string): void {
    this.sent.push(text);
  }

  close(): void {
    this.closed = true;
  }
}

function transportTo(url: string) {
  FakeSocket.instances = [];
  const transport = webSocketTransport(url, FakeSocket as unknown as typeof WebSocket)();
  const heard: string[] = [];
  const events: TransportEvents = {
    open: () => heard.push("open"),
    text: (data) => heard.push(`text:${data}`),
    binary: (data) => heard.push(`binary:${data.byteLength}`),
    close: (info: CloseInfo) => heard.push(`close:${info.code}:${info.reason}`),
  };
  return { transport, events, heard, socket: () => FakeSocket.instances[0] };
}

describe("the address of the server", () => {
  test("is the page's own, on the WebSocket scheme that goes with its", () => {
    expect(serverUrl({ protocol: "http:", host: "127.0.0.1:8765" })).toBe("ws://127.0.0.1:8765/ws");
    expect(serverUrl({ protocol: "https:", host: "tang.example" })).toBe("wss://tang.example/ws");
    expect(serverUrl({ protocol: "http:", host: "localhost:5173" })).toBe("ws://localhost:5173/ws");
  });
});

describe("the token in the fragment", () => {
  test("is read from the address", () => {
    expect(tokenFromHash("#token=0123abcd")).toBe("0123abcd");
    expect(tokenFromHash("token=0123abcd")).toBe("0123abcd");
    expect(tokenFromHash("#other=1&token=x%20y")).toBe("x y");
  });

  test("is not there when the address has none", () => {
    expect(tokenFromHash("")).toBeUndefined();
    expect(tokenFromHash("#")).toBeUndefined();
    expect(tokenFromHash("#section-2")).toBeUndefined();
    expect(tokenFromHash("#tokens=abc")).toBeUndefined();
  });
});

describe("the WebSocket transport", () => {
  test("opens a socket to the address, for binary frames as array buffers", () => {
    const { transport, events, socket } = transportTo("ws://x/ws");
    expect(FakeSocket.instances).toHaveLength(0); // nothing until it is asked to connect
    transport.connect(events);
    expect(socket()?.url).toBe("ws://x/ws");
    expect(socket()?.binaryType).toBe("arraybuffer");
  });

  test("tells the client what the socket does", () => {
    const { transport, events, heard, socket } = transportTo("ws://x/ws");
    transport.connect(events);
    socket()?.onopen?.();
    socket()?.onmessage?.({ data: '{"type":"hello"}' });
    socket()?.onmessage?.({ data: new ArrayBuffer(12) });
    socket()?.onmessage?.({ data: new Blob(["no"]) }); // not what the server sends: ignored
    socket()?.onclose?.({ code: 1008, reason: "closing" });
    expect(heard).toEqual(["open", 'text:{"type":"hello"}', "binary:12", "close:1008:closing"]);
  });

  test("sends only while the socket is open", () => {
    const { transport, events, socket } = transportTo("ws://x/ws");
    transport.send("before connecting");
    transport.connect(events);
    transport.send("while connecting");
    const open = socket();
    if (open) open.readyState = FakeSocket.OPEN;
    transport.send("hello");
    expect(socket()?.sent).toEqual(["hello"]);
  });

  test("closes the socket", () => {
    const { transport, events, socket } = transportTo("ws://x/ws");
    transport.close(); // nothing to close yet, and no harm
    transport.connect(events);
    transport.close();
    expect(socket()?.closed).toBe(true);
  });
});
