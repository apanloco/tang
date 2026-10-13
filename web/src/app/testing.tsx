// What a test of a view needs: a client talking to a mock engine, a frame loop the test
// turns by hand, and the view in a provider. (Only tests import this.)
import { render, waitFor } from "@testing-library/react";
import type { RenderResult } from "@testing-library/react";
import type { ReactNode } from "react";
import { expect } from "vitest";

import { Client } from "../client/connection";
import { FrameLoop } from "../client/frameloop";
import { MockServer } from "../client/mock";
import type { MockOptions } from "../client/mock";
import { Clock } from "../client/testing";
import { ClientProvider } from "./ClientProvider";

export interface Rig {
  server: MockServer;
  client: Client;
  /** The frames of the views: turn them with `clock.tick(time)`. */
  clock: Clock;
  frames: FrameLoop;
  view: RenderResult;
  /** The time (in seconds) the mock engine's modulators follow: set it to put the LFO where you want it. */
  time: { seconds: number };
  /** The requests the clients have sent, in order. */
  requests: () => { intent: unknown }[];
}

/**
 * Render `ui` below a provider, over a mock engine that makes telemetry on request (every
 * 5 ms, at the time `rig.time.seconds` says), and wait until the client has the session.
 * A connection the test drops stays down: the client would otherwise be back in a quarter of
 * a second, and a test that is slow to get to what it does with the lost connection (a busy
 * machine, a canvas that takes long to draw) would find it up again.
 */
export async function rig(ui: ReactNode, options: MockOptions = {}): Promise<Rig> {
  const time = { seconds: 0 };
  const server = new MockServer({ telemetryPeriod: 5, clock: () => time.seconds, ...options });
  const client = new Client({ transport: server.transport, telemetry: ["meters", "modulation", "held"], reconnectDelay: () => 60_000 });
  const clock = new Clock();
  const frames = new FrameLoop(clock);
  const view = render(
    <ClientProvider client={client} frames={frames}>
      {ui}
    </ClientProvider>,
  );
  await waitFor(() => {
    expect(client.state.status).toBe("open");
  });
  return {
    server,
    client,
    clock,
    frames,
    view,
    time,
    requests: () => server.received.flatMap((message) => (message.type === "request" ? [{ intent: message.intent }] : [])),
  };
}
