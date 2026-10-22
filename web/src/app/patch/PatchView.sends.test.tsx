// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { RenderResult } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { audioCablesFixture, MASTER, tracerSnapshot } from "../../client/fixtures";
import { sendItems } from "../../client/sends";
import type { Snapshot } from "../../protocol";
import { rig } from "../testing";
import { sendWidth } from "./describe";
import { PatchView } from "./PatchView";

// Wrapped, to count how often the menus look for the places that take audio: once a node, and again when the structure of the session changes.
vi.mock("../../client/sends", async (importOriginal) => {
  const original = await importOriginal<typeof import("../../client/sends")>();
  return { ...original, sendItems: vi.fn(original.sendItems) };
});

afterEach(cleanup);

// The session of the audio cables fixture: group 0 ("Pad") with Filter 1 (node 1), Reverb (2) and Delay 1 (3) on its bus; lane 4 ("Oscillator 1") in it with Filter 2 (5), Delay 2 (6) and an LFO (7) that drives
// Filter 2's cutoff; lane 8 ("Oscillator 2") with Drive (9); an empty lane (10). The audio cables, by id, with what the canvas calls what they join and their levels.
const { snapshot } = audioCablesFixture;
const CABLES: readonly [number, string, number][] = [
  [1, "Oscillator 2 → Filter 2", 0.5],
  [2, "Filter 2 → Delay 2", 0.75],
  [3, "Pad → Drive", 1.5],
  [4, "Filter 1 → Reverb", 1],
  [5, "Delay 2 → Pad", 1],
  [6, "Reverb → Master", 0.5],
];

const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);
const sendsOf = (view: RenderResult): SVGPathElement[] => Array.from(view.container.querySelectorAll<SVGPathElement>(".tang-cable--send"));
const levelOf = (words: string): Promise<HTMLInputElement> => screen.findByRole<HTMLInputElement>("slider", { name: `Level of ${words}` });
/** The nodes that have a handle of this name, by the id of the node, in order. */
const nodesWith = (view: RenderResult, handle: string): number[] =>
  Array.from(view.container.querySelectorAll(`.react-flow__handle[data-handleid="${handle}"]`), (port) => Number(port.getAttribute("data-nodeid"))).sort((a, b) => a - b);

describe("the audio cables of the canvas", () => {
  test("are drawn for each audio cable of the session, each as a cable of its own beside the links of the audio path, which stay as they were", async () => {
    const { view } = await rig(<PatchView />, { snapshot });
    await waitFor(() => {
      expect(sendsOf(view)).toHaveLength(6);
    });
    expect(sendsOf(view).map((path) => path.id).sort()).toEqual(["send:1", "send:2", "send:3", "send:4", "send:5", "send:6"]);
    expect(view.container.querySelectorAll(".react-flow__edge-send")).toHaveLength(6);
    // (The links of the audio path, a lane's chain and a lane's way to its group or the mix, and the cable of modulation.)
    expect(view.container.querySelectorAll(".tang-cable--audio")).toHaveLength(10);
    expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(1);
    for (const path of sendsOf(view)) expect(path.classList.contains("tang-cable--audio"), path.id).toBe(false);
  });

  test("are as thick as they are loud, at the level the session has them", async () => {
    const { view } = await rig(<PatchView />, { snapshot });
    await waitFor(() => {
      expect(sendsOf(view)).toHaveLength(6);
    });
    for (const [id, words, level] of CABLES) {
      expect(view.container.querySelector<SVGPathElement>(`#send\\:${String(id)}`)?.style.strokeWidth, words).toBe(String(sendWidth(level)));
    }
  });

  test("have a slider for their level each, named for what the cable joins, from nothing to twice, and a cable of modulation has none of that sort", async () => {
    await rig(<PatchView />, { snapshot });
    for (const [, words, level] of CABLES) {
      const slider = await levelOf(words);
      expect([slider.min, slider.max, slider.step, slider.value], words).toEqual(["0", "2", "0.01", String(level)]);
      expect(slider.getAttribute("aria-valuetext"), words).toBe(`${String(Math.round(level * 100))}%`);
    }
    expect(screen.getAllByRole("slider", { name: /^Level of / })).toHaveLength(6);
    // (The cable of modulation has its depth, and the audio cables have none.)
    expect(screen.getAllByRole("slider", { name: /^Depth of / })).toHaveLength(1);
    expect(screen.queryByRole("slider", { name: /^Depth of .*Filter 2$/ })).toBeNull();
  });

  test("turn the level on the cable: shown at once, sent on the frame as the level of the cable, and the cable follows", async () => {
    const { clock, requests, view } = await rig(<PatchView />, { snapshot });
    const level = await levelOf("Oscillator 2 → Filter 2");
    fireEvent.change(level, { target: { value: "1.25" } });
    expect(level.value).toBe("1.25");
    expect(level.getAttribute("aria-valuetext")).toBe("125%");
    expect(requests()).toEqual([]); // not before the frame
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setCableGain", edge: 1, gain: 1.25 } }]);
    });
    await waitFor(() => {
      expect(view.container.querySelector<SVGPathElement>("#send\\:1")?.style.strokeWidth).toBe(String(sendWidth(1.25)));
    });
    // (The other cables were not touched.)
    expect(view.container.querySelector<SVGPathElement>("#send\\:2")?.style.strokeWidth).toBe(String(sendWidth(0.75)));
  });

  test("send a level when the gesture that set it ends: the key let go, the pointer let go, the focus gone (one intent a frame)", async () => {
    const { requests, clock } = await rig(<PatchView />, { snapshot });
    const level = await levelOf("Pad → Drive");
    const ends: [string, () => void][] = [
      ["0.5", () => fireEvent.keyUp(level, { key: "ArrowUp" })],
      ["0.6", () => fireEvent.pointerUp(level)],
      ["0.7", () => fireEvent.blur(level)],
    ];
    for (const [value, end] of ends) {
      fireEvent.change(level, { target: { value } });
      end(); // (not waiting for the frame)
      await waitFor(() => {
        expect(requests().at(-1)).toEqual({ intent: { type: "setCableGain", edge: 3, gain: Number(value) } });
      });
      act(() => {
        clock.tick(16 * (requests().length + 1)); // the next frame, which is the one that is allowed the next intent
      });
    }
    expect(requests()).toHaveLength(3);
  });

  test("follow a level that somebody else sets, to a whole percent", async () => {
    const { server } = await rig(<PatchView />, { snapshot });
    const level = await levelOf("Filter 2 → Delay 2");
    act(() => {
      server.apply({ type: "setCableGain", edge: 2, gain: 1.2 });
    });
    await waitFor(() => {
      expect(level.value).toBe("1.2");
    });
    expect(level.getAttribute("aria-valuetext")).toBe("120%");
    // (A whole percent, though 0.29 times 100 is not quite 29.)
    act(() => {
      server.apply({ type: "setCableGain", edge: 2, gain: 0.29 });
    });
    await waitFor(() => {
      expect(level.getAttribute("aria-valuetext")).toBe("29%");
    });
  });

  test("are taken out by the button on the cable, named for what it joins: the server is asked, the cable and its slider go, and Undo draws it again at its level", async () => {
    const { client, requests, server, view } = await rig(<PatchView />, { snapshot });
    fireEvent.click(await screen.findByRole("button", { name: "Remove cable Oscillator 2 → Filter 2" }));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "removeEdge", edge: 1 } }]);
    });
    await waitFor(() => {
      expect(sendsOf(view)).toHaveLength(5);
    });
    expect(screen.queryByRole("slider", { name: "Level of Oscillator 2 → Filter 2" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Remove cable Oscillator 2 → Filter 2" })).toBeNull();
    expect(server.history).toEqual({ undo: "remove an audio cable", redo: null });
    await client.request({ type: "undo" });
    const level = await levelOf("Oscillator 2 → Filter 2");
    expect(level.value).toBe("0.5");
    expect(sendsOf(view)).toHaveLength(6);
  });

  test("give the focus, when one is taken out, to the button that sends audio from the node it started at, which is where the next one is drawn from", async () => {
    await rig(<PatchView />, { snapshot });
    const menu = within(await nodeOf(8)).getByRole("button", { name: "Send audio from Oscillator 2" });
    fireEvent.click(await screen.findByRole("button", { name: "Remove cable Oscillator 2 → Filter 2" }));
    await waitFor(() => {
      expect(document.activeElement).toBe(menu);
    });
  });

  test("keep the modulation cable's own button, which gives the focus to the list of its modulator", async () => {
    await rig(<PatchView />, { snapshot });
    const list = within(await nodeOf(7)).getByRole("button", { name: /^Add a cable from / });
    fireEvent.click(await screen.findByRole("button", { name: "Remove cable LFO → Filter 2: cutoff" }));
    await waitFor(() => {
      expect(document.activeElement).toBe(list);
    });
  });
});

describe("the audio ports of the canvas", () => {
  test("are an output on each node that has audio to send (a lane's instrument, an effect, a group, an empty lane too) and an input on each that takes it (an effect, a group, the master)", async () => {
    const { view } = await rig(<PatchView />, { snapshot });
    await nodeOf(MASTER);
    expect(nodesWith(view, "audioOut")).toEqual([0, 1, 2, 3, 4, 5, 6, 8, 9, 10]);
    expect(nodesWith(view, "audioIn")).toEqual([0, 1, 2, 3, 5, 6, 9, MASTER]);
  });

  test("start a cable at an output and end it at an input, and not the other way round", async () => {
    const { view } = await rig(<PatchView />, { snapshot });
    await nodeOf(MASTER);
    for (const port of Array.from(view.container.querySelectorAll(".react-flow__handle.tang-port--audio"))) {
      const output = port.getAttribute("data-handleid") === "audioOut";
      const where = `${port.getAttribute("data-nodeid") ?? "?"}:${port.getAttribute("data-handleid") ?? "?"}`;
      expect([port.classList.contains("connectablestart"), port.classList.contains("connectableend")], where).toEqual(output ? [true, false] : [false, true]);
    }
  });
});

describe("the menu that sends the audio of a node", () => {
  const buttonIn = async (id: number, name: string): Promise<HTMLButtonElement> => within(await nodeOf(id)).getByRole<HTMLButtonElement>("button", { name: `Send audio from ${name}` });
  const listed = (): [string | null, string[]][] =>
    within(screen.getByRole("menu"))
      .getAllByRole("group")
      .map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);

  test("is a button on each lane that has an instrument, each effect and each group, and on no other node: not the master, a modulator or a lane with no instrument", async () => {
    const { view } = await rig(<PatchView />, { snapshot });
    await nodeOf(MASTER);
    const buttons = Array.from(view.container.querySelectorAll('button[aria-label^="Send audio from "]'), (button) => button.getAttribute("aria-label"));
    expect(buttons.sort()).toEqual(
      ["Delay 1", "Delay 2", "Drive", "Filter 1", "Filter 2", "Oscillator 1", "Oscillator 2", "Pad", "Reverb"].map((name) => `Send audio from ${name}`).sort(),
    );
    for (const id of [7, 10, MASTER]) {
      expect(within(await nodeOf(id)).queryByRole("button", { name: /^Send audio from / }), String(id)).toBeNull();
    }
  });

  test("has an id of its own among the ids of the page: the number of the node after `send-`, which what takes a cable out gives the focus to", async () => {
    await rig(<PatchView />, { snapshot });
    expect((await buttonIn(8, "Oscillator 2")).id).toBe("send-8");
    expect((await buttonIn(0, "Pad")).id).toBe("send-0");
    expect((await buttonIn(5, "Filter 2")).id).toBe("send-5");
  });

  test("lists, when it is opened, the places that take audio from the node, by the names the canvas gives them, under what each is", async () => {
    await rig(<PatchView />, { snapshot });
    fireEvent.click(await buttonIn(8, "Oscillator 2"));
    expect(listed()).toEqual([
      ["Effects", ["Filter 1", "Reverb", "Delay 1", "Delay 2", "Drive"]],
      ["Group buses", ["Pad"]],
      ["Mix", ["Master"]],
    ]);
  });

  test("lists the mix alone for a group whose audio can go nowhere else", async () => {
    await rig(<PatchView />, { snapshot });
    fireEvent.click(await buttonIn(0, "Pad"));
    expect(listed()).toEqual([["Mix", ["Master"]]]);
  });

  test("draws the cable that is chosen, at the level a new cable has, and the place is not listed again", async () => {
    const { requests, view } = await rig(<PatchView />, { snapshot });
    const menu = await buttonIn(8, "Oscillator 2");
    fireEvent.click(menu);
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Reverb" }));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "addCable", from: { node: 8, port: { type: "audioOut" } }, to: { node: 2, port: { type: "audioIn" } }, gain: null } }]);
    });
    const level = await levelOf("Oscillator 2 → Reverb");
    expect([level.value, level.getAttribute("aria-valuetext")]).toEqual(["1", "100%"]);
    expect(sendsOf(view)).toHaveLength(7);
    // (The focus is on the button that opened the menu, so that the next one is a key away.)
    expect(document.activeElement).toBe(menu);
    fireEvent.click(menu);
    expect(listed()[0]).toEqual(["Effects", ["Filter 1", "Delay 1", "Delay 2", "Drive"]]);
  });

  test("follows the session: a cable that is drawn by somebody else takes its end off the list", async () => {
    const { server } = await rig(<PatchView />, { snapshot });
    const menu = await buttonIn(8, "Oscillator 2");
    act(() => {
      server.apply({ type: "addCable", from: { node: 8, port: { type: "audioOut" } }, to: { node: 3, port: { type: "audioIn" } }, gain: null });
    });
    await levelOf("Oscillator 2 → Delay 1");
    fireEvent.click(menu);
    expect(listed()[0]).toEqual(["Effects", ["Filter 1", "Reverb", "Delay 2", "Drive"]]);
  });

  test("looks for the places again when the structure of the session changes, and not when a value is turned: a knob, a level", async () => {
    const { server } = await rig(<PatchView />, { snapshot });
    await levelOf("Oscillator 2 → Filter 2");
    const looked = (): number => vi.mocked(sendItems).mock.calls.length;
    // (A menu on each of the nine nodes that send, found once.)
    await waitFor(() => {
      expect(looked()).toBeGreaterThanOrEqual(9);
    });
    const settled = looked();
    act(() => {
      server.apply({ type: "setCableGain", edge: 1, gain: 1.2 });
      server.apply({ type: "setParam", node: 5, param: 1, value: 6 });
      server.apply({ type: "setGain", node: 8, gain: "volume", value: 0.5 });
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLInputElement>("slider", { name: "Level of Oscillator 2 → Filter 2" }).value).toBe("1.2");
    });
    expect(looked()).toBe(settled);
    // A cable that is drawn is a change of the structure: every menu looks again (the one that has the cable's end has one place fewer).
    act(() => {
      server.apply({ type: "addCable", from: { node: 8, port: { type: "audioOut" } }, to: { node: 2, port: { type: "audioIn" } }, gain: null });
    });
    await levelOf("Oscillator 2 → Reverb");
    expect(looked()).toBeGreaterThan(settled);
  });

  test("is shut when there is nowhere to send: the audio of the tracer's filter can go to the mix and, once a cable is there, nowhere", async () => {
    const full: Snapshot = structuredClone(tracerSnapshot);
    full.graph.edges.push({ kind: "cable", id: 31, gain: 1, from: { node: 1, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } } });
    await rig(<PatchView />, { snapshot: full });
    expect((await buttonIn(1, "Filter")).disabled).toBe(true);
    expect((await buttonIn(0, "Oscillator")).disabled).toBe(false);
  });
});
