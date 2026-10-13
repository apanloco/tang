// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { Client } from "../client/connection";
import { nodeNamed, tracerSnapshot } from "../client/fixtures";
import { MockServer } from "../client/mock";
import type { MockOptions } from "../client/mock";
import type { Notices } from "../client/notices";
import { App } from "./App";
import { ClientProvider, useNotices } from "./ClientProvider";

afterEach(cleanup);

const filter = nodeNamed(tracerSnapshot, "Filter");

function page(options: MockOptions = {}) {
  const server = new MockServer(options);
  const client = new Client({ transport: server.transport, reconnectDelay: () => 5 });
  const view = render(
    <ClientProvider client={client}>
      <App engine="mock" />
    </ClientProvider>,
  );
  return { server, client, view };
}

/** The text of the session as the page shows it, once whoever is looking has asked to see it. */
const shown = (): string => document.querySelector(".tang-session")?.textContent ?? "";

/** Open the session as JSON, which the page keeps closed (and does not write while it is). */
function openJson(): void {
  const details = document.querySelector("details");
  if (!details) throw new Error("the page has no session as JSON");
  details.open = true;
  fireEvent(details, new Event("toggle"));
}

describe("the page", () => {
  test("connects, says so, and shows the session as the server sent it", async () => {
    page();
    expect(screen.getByRole("status").textContent).toBe("Connecting…");
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).toBe("Connected");
    });
    openJson();
    expect(JSON.parse(shown())).toEqual(tracerSnapshot);
    expect(shown()).toContain('\n  "graph"'); // written out for a person to read
    expect(screen.getByText("mock engine")).toBeTruthy();
  });

  test("says when it is talking to a real server", async () => {
    const server = new MockServer();
    const client = new Client({ transport: server.transport });
    render(
      <ClientProvider client={client}>
        <App engine="server" />
      </ClientProvider>,
    );
    await screen.findByText("Connected");
    expect(screen.getByText("server")).toBeTruthy();
    expect(screen.queryByText("mock engine")).toBeNull();
  });

  test("cannot be rendered without a client", () => {
    const quiet = vi.spyOn(console, "error").mockImplementation(() => undefined);
    expect(() => render(<App engine="mock" />)).toThrow(/ClientProvider/);
    quiet.mockRestore();
  });

  test("says what could not be done, where the person can read it and take it away", async () => {
    const server = new MockServer();
    const client = new Client({ transport: server.transport, reconnectDelay: () => 5 });
    let notices: Notices | undefined;
    function Probe() {
      notices = useNotices();
      return null;
    }
    render(
      <ClientProvider client={client}>
        <Probe />
        <App engine="mock" />
      </ClientProvider>,
    );
    await screen.findByText("Connected");
    expect(screen.queryByRole("button", { name: /^Dismiss/ })).toBeNull();
    act(() => {
      notices?.say("Could not add an LFO to Filter: a modulator goes in a lane or in a group");
    });
    expect(screen.getByText(/^Could not add an LFO to Filter/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /^Dismiss: Could not add an LFO/ }));
    expect(screen.queryByText(/^Could not add an LFO to Filter/)).toBeNull();
  });

  test("shows a change somebody else makes", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    openJson();
    expect(shown()).not.toContain("0.123");
    server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.123 });
    await waitFor(() => {
      expect(shown()).toContain("0.123");
    });
  });

  test("goes on showing the session while the connection is lost, and says it is", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    openJson();
    server.drop();
    await screen.findByText("Connection lost, reconnecting…");
    expect(JSON.parse(shown())).toEqual(tracerSnapshot);
    await screen.findByText("Connected");
  });

  test("tells the person why when the server cannot be used", async () => {
    page({ protocol: 9 });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/protocol 1 and the server speaks 9/);
    expect(screen.getByRole("status").textContent).toBe("Disconnected");
    openJson();
    expect(shown()).toBe("");
  });

  test("shows the session as a patch: the nodes of the tracer and the cables between them", async () => {
    page();
    await screen.findByText("Connected");
    const patch = await screen.findByTestId("patch");
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(4);
    });
    expect(Array.from(patch.querySelectorAll(".tang-node__name")).map((name) => name.textContent)).toEqual([
      "Oscillator",
      "Filter",
      "LFO",
      "Master",
    ]);
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__edge")).toHaveLength(3);
    });
  });

  test("keeps the session as JSON closed, and writes it only while it is open", async () => {
    page();
    await screen.findByText("Connected");
    expect(document.querySelector("details")?.open).toBe(false);
    expect(document.querySelector(".tang-session")).toBeNull();
    openJson();
    expect(document.querySelector(".tang-session")).not.toBeNull();
    const details = document.querySelector("details");
    if (details) {
      details.open = false;
      fireEvent(details, new Event("toggle"));
    }
    expect(document.querySelector(".tang-session")).toBeNull();
  });

  test("does not go blank for a number the server could not write: JSON has no infinity, and says null", async () => {
    const quiet = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
    const nodes = tracerSnapshot.graph.nodes.map((node) => (node.id === oscillator.id && node.kind === "instrument" ? { ...node, volume: null as unknown as number } : node));
    page({ snapshot: { ...tracerSnapshot, graph: { ...tracerSnapshot.graph, nodes } } });
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/the patch/);
    // What is not the patch is there: the state of the connection, and the session as the server sent it.
    expect(screen.getByRole("status").textContent).toBe("Connected");
    openJson();
    expect(shown()).toContain('"volume": null');
    quiet.mockRestore();
  });

  test("leaves the server when it is taken off the page", async () => {
    const { server, view } = page();
    await screen.findByText("Connected");
    expect(server.connections).toBe(1);
    view.unmount();
    expect(server.connections).toBe(0);
  });
});
