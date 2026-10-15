// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { Client } from "../client/connection";
import { nodeNamed, tracerSnapshot } from "../client/fixtures";
import { MockServer } from "../client/mock";
import type { Intent } from "../protocol";
import { BusyLight } from "./BusyLight";
import { ClientProvider } from "./ClientProvider";
import { holdable } from "./testing";

afterEach(cleanup);

// What the page says when the server is slow to answer what it asked (a plugin that takes seconds to load): a small "Working…" that is there for as long as
// a request has waited longer than the client was told to wait, and is not there otherwise. The client counts them (`connection.test.ts`); this is what is drawn.

/** The light over a mock server whose answers can be held back, for a client that calls a request slow after 30 ms. */
async function lit() {
  const server = new MockServer();
  const connection = holdable(server);
  const client = new Client({ transport: connection.transport, slowAfter: 30 });
  render(
    <ClientProvider client={client}>
      <BusyLight />
    </ClientProvider>,
  );
  await waitFor(() => {
    expect(client.state.status).toBe("open");
  });
  return { client, connection };
}

const light = () => screen.queryByRole("progressbar", { name: "Working" });
const cutoff = (value: number): Intent => ({ type: "setParam", node: nodeNamed(tracerSnapshot, "Filter").id, param: 0, value });

describe("the light that says that the server is busy", () => {
  test("is not there while nothing is slow, nor after a request that was answered at once", async () => {
    const { client } = await lit();
    expect(light()).toBeNull();
    await client.request(cutoff(0.2));
    await new Promise((resolve) => setTimeout(resolve, 60)); // (longer than the client waits)
    expect(light()).toBeNull();
    expect(document.querySelector(".tang-busy")).toBeNull();
  });

  test("comes when a request has waited longer than the client was told to, and goes when it is answered", async () => {
    const { client, connection } = await lit();
    connection.hold();
    const answered = client.request(cutoff(0.2));
    await waitFor(() => {
      expect(light()).not.toBeNull();
    });
    expect(light()?.textContent).toBe("Working…");
    act(() => {
      connection.answer();
    });
    await answered;
    await waitFor(() => {
      expect(light()).toBeNull();
    });
  });

  test("is one light for any number of slow requests, and stays until the last is answered", async () => {
    const { client, connection } = await lit();
    connection.hold();
    const first = client.request(cutoff(0.2));
    const second = client.request(cutoff(0.3));
    await waitFor(() => {
      expect(screen.queryAllByRole("progressbar", { name: "Working" })).toHaveLength(1);
    });
    act(() => {
      connection.answer(); // what the first made, and its answer
    });
    await first;
    expect(light()).not.toBeNull();
    act(() => {
      connection.answer();
    });
    await second;
    await waitFor(() => {
      expect(light()).toBeNull();
    });
  });

  test("is a progress bar that does not say how far: a screen reader is told that something is going on, and not that the connection has changed", async () => {
    const { client, connection } = await lit();
    connection.hold();
    const answered = client.request(cutoff(0.2));
    const bar = await screen.findByRole("progressbar", { name: "Working" });
    expect(bar.hasAttribute("aria-valuenow")).toBe(false);
    // (The page's one status, the state of the connection, is still alone in its role: the tests of the page find it by it.)
    expect(screen.queryAllByRole("status")).toHaveLength(0);
    act(() => {
      connection.answer();
    });
    await answered;
  });
});
