// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { Client } from "../../client/connection";
import { MockServer } from "../../client/mock";
import { NOTE_NAMES } from "../../client/notes";
import { nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import type { NoteRange, Snapshot } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { ClientProvider, useNotices } from "../ClientProvider";
import { holdable, rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

const lowest = (name = "Oscillator") => screen.findByRole<HTMLSelectElement>("combobox", { name: `Lowest key of ${name}` });
const highest = (name = "Oscillator") => screen.findByRole<HTMLSelectElement>("combobox", { name: `Highest key of ${name}` });

/** What the two selects of a lane show: the text of the chosen options. */
const shown = async (name = "Oscillator"): Promise<[string, string]> => {
  const text = (select: HTMLSelectElement): string => select.selectedOptions[0]?.textContent ?? "?";
  return [text(await lowest(name)), text(await highest(name))];
};

/** The tracer with the oscillator's lane taking these keys. */
function keys(range: NoteRange | null): Snapshot {
  const snapshot = structuredClone(tracerSnapshot);
  const node = snapshot.graph.nodes.find((n) => n.id === oscillator.id);
  if (node?.kind !== "instrument") throw new Error("no lane");
  node.range = range;
  return snapshot;
}

describe("the keys of a lane", () => {
  test("are a pair of choices on every lane, an empty one too, and on nothing else", async () => {
    await rig(<PatchView />);
    expect(await lowest()).toBeTruthy();
    expect(screen.getAllByRole("combobox", { name: /^Lowest key of / })).toHaveLength(1);
    expect(screen.getAllByRole("combobox", { name: /^Highest key of / })).toHaveLength(1);
    cleanup();

    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    await screen.findAllByRole("combobox", { name: /^Lowest key of / });
    const lanes = reachFixture.snapshot.graph.nodes.filter((node) => node.kind === "instrument");
    expect(lanes.some((lane) => lane.plugin === null)).toBe(true);
    expect(screen.getAllByRole("combobox", { name: /^Lowest key of / })).toHaveLength(lanes.length);
    expect(screen.getAllByRole("combobox", { name: /^Highest key of / })).toHaveLength(lanes.length);
  });

  test("are in the lane's node, in a cell of its own called Keys, after its volume", async () => {
    await rig(<PatchView />);
    const node = (await lowest()).closest(".tang-node");
    expect(node?.classList.contains("tang-node--instrument")).toBe(true);
    const cells = Array.from(node?.querySelectorAll(".tang-node__controls > .tang-cell") ?? []);
    expect(cells).toHaveLength(10); // eight parameters, the volume, the keys
    const last = cells.at(-1);
    expect(last?.textContent).toContain("Keys");
    expect(within(last as HTMLElement).getAllByRole("combobox")).toHaveLength(2);
  });

  test("are not on an effect, a modulator or the master", async () => {
    await rig(<PatchView />);
    const node = await screen.findByTestId(`rf__node-${String(filter.id)}`);
    expect(within(node).queryByText("Keys")).toBeNull();
    expect(screen.getAllByText("Keys")).toHaveLength(1);
  });

  test("say any at both ends for a lane that takes every note, and the notes for one that does not", async () => {
    await rig(<PatchView />);
    expect(await shown()).toEqual(["any", "any"]);
    cleanup();
    await rig(<PatchView />, { snapshot: keys({ low: 48, high: 71 }) });
    expect(await shown()).toEqual(["C3", "B4"]);
    cleanup();
    // (An end that is the end of the keyboard is no end at all: it says any.)
    await rig(<PatchView />, { snapshot: keys({ low: 0, high: 59 }) });
    expect(await shown()).toEqual(["any", "B3"]);
    cleanup();
    await rig(<PatchView />, { snapshot: keys({ low: 60, high: 127 }) });
    expect(await shown()).toEqual(["C4", "any"]);
    cleanup();
    await rig(<PatchView />, { snapshot: keys({ low: 0, high: 127 }) });
    expect(await shown()).toEqual(["any", "any"]);
  });

  test("offer any, and then the notes of the keyboard by name: not the one that is any already, the lowest for the low end and the highest for the high end", async () => {
    await rig(<PatchView />);
    const low = Array.from((await lowest()).options);
    expect(low.map((option) => option.textContent)).toEqual(["any", ...NOTE_NAMES.slice(1)]);
    expect(low.map((option) => option.value).slice(0, 3)).toEqual(["", "1", "2"]);
    expect(low.at(-1)?.value).toBe("127");
    const high = Array.from((await highest()).options);
    expect(high.map((option) => option.textContent)).toEqual(["any", ...NOTE_NAMES.slice(0, 127)]);
    expect(high.map((option) => option.value).slice(0, 3)).toEqual(["", "0", "1"]);
    expect(high.at(-1)?.value).toBe("126");
  });

  test("ask for the range from a low note to the end of the keyboard when only a low note is chosen, and show what was chosen at once", async () => {
    const { requests } = await rig(<PatchView />);
    fireEvent.change(await lowest(), { target: { value: "60" } });
    expect(await shown()).toEqual(["C4", "any"]);
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setRange", node: oscillator.id, range: { low: 60, high: 127 } } }]);
    });
  });

  test("ask for the range from the bottom of the keyboard to a high note when only a high note is chosen", async () => {
    const { requests } = await rig(<PatchView />);
    fireEvent.change(await highest(), { target: { value: "71" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setRange", node: oscillator.id, range: { low: 0, high: 71 } } }]);
    });
    expect(await shown()).toEqual(["any", "B4"]);
  });

  test("keep the other end of the range as it is", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: keys({ low: 48, high: 71 }) });
    fireEvent.change(await highest(), { target: { value: "84" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setRange", node: oscillator.id, range: { low: 48, high: 84 } } }]);
    });
    fireEvent.change(await lowest(), { target: { value: "36" } });
    await waitFor(() => {
      expect(requests()).toHaveLength(2);
    });
    expect(requests()[1]).toEqual({ intent: { type: "setRange", node: oscillator.id, range: { low: 36, high: 84 } } });
  });

  test("ask for no range, which is every note, when both ends are any again", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: keys({ low: 60, high: 127 }) });
    fireEvent.change(await lowest(), { target: { value: "" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setRange", node: oscillator.id, range: null } }]);
    });
    expect(await shown()).toEqual(["any", "any"]);
  });

  test("ask for the range to the end of the keyboard when the high end is any again, and for none when the low end is any too", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: keys({ low: 60, high: 71 }) });
    fireEvent.change(await highest(), { target: { value: "" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setRange", node: oscillator.id, range: { low: 60, high: 127 } } }]);
    });
    expect(await shown()).toEqual(["C4", "any"]);
    cleanup();

    const { requests: again } = await rig(<PatchView />, { snapshot: keys({ low: 0, high: 71 }) });
    fireEvent.change(await highest(), { target: { value: "" } });
    await waitFor(() => {
      expect(again()).toEqual([{ intent: { type: "setRange", node: oscillator.id, range: null } }]);
    });
    expect(await shown()).toEqual(["any", "any"]);
  });

  test("take the high note up to a low note that is above it, and the low note down to a high note that is below it", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: keys({ low: 48, high: 60 }) });
    fireEvent.change(await lowest(), { target: { value: "72" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setRange", node: oscillator.id, range: { low: 72, high: 72 } } }]);
    });
    expect(await shown()).toEqual(["C5", "C5"]);
    fireEvent.change(await highest(), { target: { value: "36" } });
    await waitFor(() => {
      expect(requests()).toHaveLength(2);
    });
    expect(requests()[1]).toEqual({ intent: { type: "setRange", node: oscillator.id, range: { low: 36, high: 36 } } });
    expect(await shown()).toEqual(["C2", "C2"]);
  });

  test("follow what another front-end sets", async () => {
    const { server } = await rig(<PatchView />);
    expect(await shown()).toEqual(["any", "any"]);
    act(() => {
      server.apply({ type: "setRange", node: oscillator.id, range: { low: 24, high: 48 } });
    });
    await waitFor(async () => {
      expect(await shown()).toEqual(["C1", "C3"]);
    });
    act(() => {
      server.apply({ type: "setRange", node: oscillator.id, range: null });
    });
    await waitFor(async () => {
      expect(await shown()).toEqual(["any", "any"]);
    });
  });

  test("say why when they are refused, among the page's notices, and go back to what the session has", async () => {
    function Board() {
      return <NoticeBoard notices={useNotices()} />;
    }
    const { server, client } = await rig(
      <>
        <PatchView />
        <Board />
      </>,
      { snapshot: keys({ low: 48, high: 71 }) },
    );
    expect(await shown()).toEqual(["C3", "B4"]);
    server.drop();
    await waitFor(() => {
      expect(client.state.status).not.toBe("open");
    });
    fireEvent.change(await lowest(), { target: { value: "60" } });
    expect(await screen.findByText("Could not set the keys of Oscillator to C4-B4: Not connected to the server.")).toBeTruthy();
    await waitFor(async () => {
      expect(await shown()).toEqual(["C3", "B4"]);
    });
  });
});

describe("the keys of a lane, chosen again before the first choice is answered", () => {
  test("go on showing the last choice while the earlier ones are answered, and then what the session has", async () => {
    const server = new MockServer();
    const connection = holdable(server);
    const client = new Client({ transport: connection.transport });
    render(
      <ClientProvider client={client}>
        <PatchView />
      </ClientProvider>,
    );
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    connection.hold();
    // Two choices a moment apart, as a key held down on a choice makes them: the answers to both are on their way.
    fireEvent.change(await lowest(), { target: { value: "60" } });
    fireEvent.change(await highest(), { target: { value: "71" } });
    await waitFor(() => {
      expect(connection.waiting()).toBe(4); // an event and an answer for each
    });
    // The first is answered: the session has the keys from C4 to the end of the keyboard, but the last choice is what is shown.
    act(() => {
      connection.release(2);
    });
    await waitFor(() => {
      const node = client.state.snapshot?.graph.nodes.find((n) => n.id === oscillator.id);
      expect(node?.kind === "instrument" && node.range).toEqual({ low: 60, high: 127 });
    });
    expect(await shown()).toEqual(["C4", "B4"]);
    // The second is answered: what the session has is what was chosen.
    act(() => {
      connection.release(2);
    });
    await waitFor(() => {
      const node = client.state.snapshot?.graph.nodes.find((n) => n.id === oscillator.id);
      expect(node?.kind === "instrument" && node.range).toEqual({ low: 60, high: 71 });
    });
    expect(await shown()).toEqual(["C4", "B4"]);
    client.stop();
  });
});
