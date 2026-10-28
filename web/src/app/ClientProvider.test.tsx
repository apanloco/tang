// @vitest-environment jsdom
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { Client } from "../client/connection";
import type { Control } from "../client/drive";
import type { Editor } from "../client/editor";
import { FrameLoop } from "../client/frameloop";
import { nodeNamed, tracerSnapshot } from "../client/fixtures";
import { MockServer } from "../client/mock";
import type { Notices } from "../client/notices";
import type { Reveals } from "../client/reveals";
import { Clock } from "../client/testing";
import { ClientProvider, useClient, useClientSelection, useClientState, useDrive, useEditor, useFrames, useNotes, useNotices, useReveals } from "./ClientProvider";

afterEach(cleanup);

const filter = nodeNamed(tracerSnapshot, "Filter");

function mount(children: React.ReactNode, frames?: FrameLoop) {
  const server = new MockServer({ telemetryPeriod: 1000 });
  const client = new Client({ transport: server.transport });
  const view = render(
    <ClientProvider client={client} {...(frames ? { frames } : {})}>
      {children}
    </ClientProvider>,
  );
  return { server, client, view };
}

describe("what the page gives its views", () => {
  test("is the client, which is connected for as long as the page has it", async () => {
    let seen: Client | undefined;
    function Probe() {
      seen = useClient();
      return null;
    }
    const { client, server, view } = mount(<Probe />);
    expect(seen).toBe(client);
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    expect(server.connections).toBe(1);
    view.unmount();
    expect(server.connections).toBe(0);
  });

  test("is a drive that sends what a view sets through the client, on the frame loop it was given", async () => {
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    let cutoff: Control | undefined;
    function Probe() {
      cutoff = useDrive().param(filter.id, 0);
      return null;
    }
    const { client, server } = mount(<Probe />, frames);
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    act(() => {
      cutoff?.set(0.25);
    });
    // Not before the frame, and not on any frame but the one of its own loop.
    await new Promise((resolve) => setTimeout(resolve, 60));
    expect(server.received.filter((message) => message.type === "request")).toEqual([]);
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(server.received.filter((message) => message.type === "request")).toHaveLength(1);
    });
    await waitFor(() => {
      const node = client.state.snapshot?.graph.nodes.find((candidate) => candidate.id === filter.id);
      expect(node?.kind === "effect" && node.plugin.params[0]?.value).toBeCloseTo(0.25, 6);
    });
  });

  test("is the same drive for as long as the client is, so that a control is one control", () => {
    const drives: unknown[] = [];
    function Probe() {
      drives.push(useDrive());
      return null;
    }
    const { view, client } = mount(<Probe />);
    view.rerender(
      <ClientProvider client={client}>
        <Probe />
      </ClientProvider>,
    );
    expect(drives.length).toBeGreaterThanOrEqual(2);
    expect(new Set(drives).size).toBe(1);
  });

  test("is a drive of its own for another client, which sends through that one", async () => {
    let drive: ReturnType<typeof useDrive> | undefined;
    function Probe() {
      drive = useDrive();
      return null;
    }
    const clock = new Clock();
    const frames = new FrameLoop(clock);
    const first = mount(<Probe />, frames);
    const firstDrive = drive;
    const secondServer = new MockServer({ telemetryPeriod: 1000 });
    const second = new Client({ transport: secondServer.transport });
    first.view.rerender(
      <ClientProvider client={second} frames={frames}>
        <Probe />
      </ClientProvider>,
    );
    expect(drive).not.toBe(firstDrive);
    await waitFor(() => {
      expect(second.state.status).toBe("open");
    });
    act(() => {
      drive?.param(filter.id, 0).set(0.5);
      clock.tick(16);
    });
    await waitFor(() => {
      expect(secondServer.received.filter((message) => message.type === "request")).toHaveLength(1);
    });
    expect(first.server.received.filter((message) => message.type === "request")).toEqual([]);
  });

  test("is the notes the page plays, through the client, with a count of who holds each: the same for as long as the client is", () => {
    const sent: string[] = [];
    const client = {
      start: () => undefined,
      stop: () => undefined,
      noteOn: (note: number, velocity?: number) => sent.push(`on ${String(note)} ${String(velocity)}`),
      noteOff: (note: number) => sent.push(`off ${String(note)}`),
    } as unknown as Client;
    const seen: unknown[] = [];
    function Probe() {
      seen.push(useNotes());
      return null;
    }
    const view = render(
      <ClientProvider client={client}>
        <Probe />
      </ClientProvider>,
    );
    view.rerender(
      <ClientProvider client={client}>
        <Probe />
      </ClientProvider>,
    );
    expect(new Set(seen).size).toBe(1);
    const notes = seen[0] as ReturnType<typeof useNotes>;
    // (A key and a pointer are two players: the note ends when the second lets go.)
    notes.noteOn(60, 100);
    notes.noteOn(60, 100);
    notes.noteOff(60);
    expect(sent).toEqual(["on 60 100"]);
    notes.noteOff(60);
    expect(sent).toEqual(["on 60 100", "off 60"]);
  });

  test("is the loop of the page unless there is another", () => {
    const loops: FrameLoop[] = [];
    function Probe() {
      loops.push(useFrames());
      return null;
    }
    mount(<Probe />);
    const own = new FrameLoop(new Clock());
    cleanup();
    mount(<Probe />, own);
    expect(loops[0]).not.toBe(own);
    expect(loops.at(-1)).toBe(own);
  });

  test("tells what a drive could not get through, as a notice, and not in the console", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => undefined);
    const clock = new Clock();
    let missing: Control | undefined;
    let notices: Notices | undefined;
    function Probe() {
      missing = useDrive().param(filter.id, 99); // no such parameter
      notices = useNotices();
      return null;
    }
    const { client } = mount(<Probe />, new FrameLoop(clock));
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    act(() => {
      missing?.set(1);
      missing?.commit();
    });
    await waitFor(() => {
      expect(notices?.getItems()).toHaveLength(1);
    });
    expect(notices?.getItems()[0]?.text).toBe("Could not set a parameter: the plugin has no parameter 99");
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  test("is an editor that adds what is asked to the session of the client, and says what it could not through the same notices", async () => {
    let editor: Editor | undefined;
    let notices: Notices | undefined;
    function Probe() {
      editor = useEditor();
      notices = useNotices();
      return null;
    }
    const { client, server } = mount(<Probe />);
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    expect(await editor?.addFreeModulator("lfo", nodeNamed(tracerSnapshot, "Oscillator").id)).toBe(3);
    expect(server.snapshot.graph.nodes.map((node) => node.id)).toContain(3);
    expect(await editor?.addFreeModulator("lfo", filter.id)).toBeNull();
    expect(notices?.getItems().map((notice) => notice.text)).toEqual(["Could not add an LFO that hears Filter: only a lane or a group can be heard"]);
  });

  test("is an editor that has what it adds shown through the reveals the views can listen to", async () => {
    let editor: Editor | undefined;
    let reveals: Reveals | undefined;
    function Probe() {
      editor = useEditor();
      reveals = useReveals();
      return null;
    }
    const { client } = mount(<Probe />);
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    const shown: number[] = [];
    reveals?.subscribe((node) => shown.push(node));
    expect(await editor?.addInstrument("builtin:osc")).toBe(3);
    expect(await editor?.addFreeModulator("lfo", filter.id)).toBeNull(); // refused: nothing to show
    expect(shown).toEqual([3]);
  });

  test("is the same editor and notices for as long as the client is, and others for another client", () => {
    const seen: [unknown, unknown][] = [];
    function Probe() {
      seen.push([useEditor(), useNotices()]);
      return null;
    }
    const { view, client } = mount(<Probe />);
    view.rerender(
      <ClientProvider client={client}>
        <Probe />
      </ClientProvider>,
    );
    expect(new Set(seen.map(([editor]) => editor)).size).toBe(1);
    expect(new Set(seen.map(([, notices]) => notices)).size).toBe(1);
    const other = new Client({ transport: new MockServer({ telemetryPeriod: 1000 }).transport });
    view.rerender(
      <ClientProvider client={other}>
        <Probe />
      </ClientProvider>,
    );
    expect(seen.at(-1)?.[0]).not.toBe(seen[0]?.[0]);
    expect(seen.at(-1)?.[1]).not.toBe(seen[0]?.[1]);
  });

  test("cannot be asked for below no provider", () => {
    const quiet = vi.spyOn(console, "error").mockImplementation(() => undefined);
    for (const hook of [useClient, useDrive, useFrames, useNotices, useEditor, useReveals]) {
      function Probe() {
        hook();
        return null;
      }
      expect(() => render(<Probe />)).toThrow(/ClientProvider/);
    }
    quiet.mockRestore();
  });
});

describe("a part of the state, as a view takes it", () => {
  test("makes the view render again only when that part changes", async () => {
    let renders = 0;
    function Probe() {
      renders += 1;
      const status = useClientState((state) => state.status);
      return <span>{status}</span>;
    }
    const { client, view } = mount(<Probe />);
    await waitFor(() => {
      expect(view.container.textContent).toBe("open");
    });
    const before = renders;
    client.store.setState({ reconnects: 5 }); // not what the view takes
    expect(renders).toBe(before);
  });

  test("with an equality of its own: only when it is not the same by that", async () => {
    let renders = 0;
    function Probe() {
      renders += 1;
      // An object made anew each time: the same by what is in it.
      const shown = useClientSelection(
        (state) => ({ rev: state.snapshot?.rev ?? -1, status: state.status }),
        (a, b) => a.status === b.status,
      );
      return <span>{`${shown.status} ${String(shown.rev)}`}</span>;
    }
    const { client, view } = mount(<Probe />);
    await waitFor(() => {
      expect(view.container.textContent).toBe("open 0");
    });
    const before = renders;
    act(() => {
      client.store.setState({ reconnects: 3 });
    });
    expect(renders).toBe(before);
    // The rev changed, which the equality does not look at: the view still shows the one it had.
    act(() => {
      const snapshot = client.state.snapshot;
      if (snapshot) client.store.setState({ snapshot: { ...snapshot, rev: 7 } });
    });
    expect(renders).toBe(before);
    expect(view.container.textContent).toBe("open 0");
    act(() => {
      client.store.setState({ status: "reconnecting" });
    });
    expect(view.container.textContent).toBe("reconnecting 7");
  });
});
