// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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

  test("has a button in the bar that opens a menu of the instruments that are installed, under the format each is of, and not of the effects", async () => {
    page();
    fireEvent.click(await screen.findByRole("button", { name: "Add an instrument" }));
    const menu = screen.getByRole("menu", { name: "Add an instrument" });
    const groups = within(menu)
      .getAllByRole("group")
      .map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);
    expect(groups).toEqual([["Built-in", ["Oscillator"]]]);
  });

  test("asks for a lane with the instrument that was chosen, and the lane is on the canvas when the server has made it", async () => {
    const { server } = page();
    const patch = await screen.findByTestId("patch");
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(4);
    });
    fireEvent.click(await screen.findByRole("button", { name: "Add an instrument" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
    await waitFor(() => {
      expect(patch.querySelectorAll(".react-flow__node")).toHaveLength(5);
    });
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []))).toEqual([{ type: "addInstrument", plugin: "builtin:osc" }]);
    // The two lanes are told apart by a number, and the new one is a lane of its own: it has all the buttons a lane has.
    expect(await screen.findByRole("button", { name: "Change the instrument of Oscillator 2" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Add an effect to Oscillator 2" })).toBeTruthy();
    // The menu has gone, and the focus is back on the button that opened it.
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an instrument" }));
  });

  test("has nothing to open until the server has said what is installed, or when nothing that is can be an instrument", async () => {
    const effect = { id: "builtin:filter", name: "Filter", format: "Built-in", isInstrument: false, params: 4, presets: 0 };
    for (const catalog of [{ entries: [], scanning: true }, { entries: [effect], scanning: false }]) {
      page({ catalog });
      const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Add an instrument" });
      expect(button.disabled).toBe(true);
      cleanup();
    }
  });

  test("says why a lane could not be added, among the notices", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    fireEvent.click(await screen.findByRole("button", { name: "Add an instrument" }));
    server.drop();
    await waitFor(() => {
      expect(screen.getByRole("status").textContent).not.toBe("Connected");
    });
    // (The menu is still there: what is installed is known, and the connection is what is not.)
    fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
    expect(await screen.findByText("Could not add Oscillator as an instrument: Not connected to the server.")).toBeTruthy();
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

describe("the computer keyboard", () => {
  /** The notes the page has told the server, in order. */
  const notes = (server: MockServer): string[] =>
    server.received.flatMap((message) => {
      if (message.type === "noteOn") return [`on ${String(message.note)} ${String(message.velocity)}`];
      if (message.type === "noteOff") return [`off ${String(message.note)}`];
      return [];
    });

  /** Wait until the server has been told exactly these notes (what the page sends arrives a moment after the key). */
  const toHaveHeard = (server: MockServer, expected: string[]) =>
    waitFor(() => {
      expect(notes(server)).toEqual(expected);
    });

  /**
   * Prove that what came before a probe was not sent: the probe is a key of the piano, and messages arrive in the
   * order they were sent, so when it has come, so has everything before it that would have.
   */
  async function probe(server: MockServer, before: string[]): Promise<void> {
    fireEvent.keyDown(document.body, { code: "KeyV" });
    fireEvent.keyUp(document.body, { code: "KeyV" });
    await toHaveHeard(server, [...before, "on 53 100", "off 53"]);
  }

  test("plays a note while a key is down, and keeps that key from the browser", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    // (`fireEvent` says whether the event was left alone: false is a key the page took.)
    expect(fireEvent.keyDown(document.body, { code: "KeyZ" })).toBe(false);
    await toHaveHeard(server, ["on 48 100"]);
    expect(fireEvent.keyUp(document.body, { code: "KeyZ" })).toBe(false);
    await toHaveHeard(server, ["on 48 100", "off 48"]);
    // A key that is not on the piano is the browser's, and so is one with Control or Command held.
    expect(fireEvent.keyDown(document.body, { code: "KeyA" })).toBe(true);
    expect(fireEvent.keyDown(document.body, { code: "KeyZ", metaKey: true })).toBe(true);
    expect(fireEvent.keyDown(document.body, { code: "KeyZ", ctrlKey: true })).toBe(true);
    await probe(server, ["on 48 100", "off 48"]);
  });

  test("says which octave it plays at, and the bracket keys change it", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    expect(screen.getByText("Keys: octave 4")).toBeTruthy();
    fireEvent.keyDown(document.body, { code: "BracketRight" });
    expect(screen.getByText("Keys: octave 5")).toBeTruthy();
    fireEvent.keyDown(document.body, { code: "KeyZ" });
    await toHaveHeard(server, ["on 60 100"]);
    fireEvent.keyDown(document.body, { code: "BracketLeft" });
    fireEvent.keyDown(document.body, { code: "BracketLeft" });
    expect(screen.getByText("Keys: octave 3")).toBeTruthy();
    // The note that is held ends as it began, at the octave it was played in.
    fireEvent.keyUp(document.body, { code: "KeyZ" });
    await toHaveHeard(server, ["on 60 100", "off 60"]);
  });

  test("leaves the keys to a place that takes text", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    const field = document.createElement("input");
    document.body.append(field);
    expect(fireEvent.keyDown(field, { code: "KeyZ" })).toBe(true);
    expect(fireEvent.keyDown(field, { code: "BracketRight" })).toBe(true);
    expect(screen.getByText("Keys: octave 4")).toBeTruthy();
    field.remove();
    await probe(server, []);
  });

  test("lets go of what it plays when the window loses the focus, and when the page is hidden", async () => {
    const { server } = page();
    await screen.findByText("Connected");
    fireEvent.keyDown(document.body, { code: "KeyZ" });
    fireEvent.blur(window);
    await toHaveHeard(server, ["on 48 100", "off 48"]);
    // (The key coming up afterwards has nothing left to end.)
    fireEvent.keyUp(document.body, { code: "KeyZ" });

    fireEvent.keyDown(document.body, { code: "KeyC" });
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
    fireEvent(document, new Event("visibilitychange"));
    Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
    await toHaveHeard(server, ["on 48 100", "off 48", "on 52 100", "off 52"]);
    // A page that is shown again is not made to let go of what is held then.
    fireEvent.keyDown(document.body, { code: "KeyB" });
    fireEvent(document, new Event("visibilitychange"));
    await toHaveHeard(server, ["on 48 100", "off 48", "on 52 100", "off 52", "on 55 100"]);
  });
});
