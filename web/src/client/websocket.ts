// The transport of a browser: a WebSocket to the server that served the page.
import type { TransportFactory } from "./connection";

/** The WebSocket of the server a page came from, wherever that is (`ws:` for `http:`, `wss:` for `https:`). */
export function serverUrl(location: Pick<Location, "protocol" | "host">): string {
  return `${location.protocol === "https:" ? "wss:" : "ws:"}//${location.host}/ws`;
}

/**
 * The token in the fragment of a page's address (`#token=…`), if there is one. A
 * fragment is not sent to a server and does not reach a log or a `Referer`, which is
 * why the server puts it there.
 */
export function tokenFromHash(hash: string): string | undefined {
  const token = new URLSearchParams(hash.replace(/^#/, "")).get("token");
  return token ?? undefined;
}

/** A transport that opens a WebSocket to `url`. */
export function webSocketTransport(url: string, SocketClass: typeof WebSocket = WebSocket): TransportFactory {
  return () => {
    let socket: WebSocket | null = null;
    return {
      connect(events) {
        const ws = new SocketClass(url);
        ws.binaryType = "arraybuffer";
        ws.onopen = () => {
          events.open();
        };
        ws.onmessage = (message: MessageEvent<unknown>) => {
          if (typeof message.data === "string") events.text(message.data);
          else if (message.data instanceof ArrayBuffer) events.binary(message.data);
        };
        // (An error is always followed by a close, and that is what the client acts on.)
        ws.onclose = (close) => {
          events.close({ code: close.code, reason: close.reason });
        };
        socket = ws;
      },
      send(text) {
        if (socket?.readyState === SocketClass.OPEN) socket.send(text);
      },
      close() {
        socket?.close();
      },
    };
  };
}
