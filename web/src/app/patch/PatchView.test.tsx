// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { builtinCatalog, builtinEffects, builtinPresets, fieldsFixture, MASTER, nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import { decodeFrame } from "../../client/telemetry";
import type { Port, Snapshot } from "../../protocol";
import { angleOf, arcPath, pointAt } from "../../ui/knob-geometry";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { callsOf, recordingContext } from "../../ui/testing";
import { rig } from "../testing";
import * as describeModule from "./describe";
import { cableWidth } from "./describe";
import { drivesId } from "./controls";
import { PatchView, sameGraphStructure } from "./PatchView";

// Wrapped, to count how often a node says what kind of modulator it is: once a render.
vi.mock("./describe", async (importOriginal) => {
  const original = await importOriginal<typeof import("./describe")>();
  return { ...original, kindOf: vi.fn(original.kindOf) };
});

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");
const lfo = nodeNamed(tracerSnapshot, "LFO");
if (filter.kind !== "effect") throw new Error("the tracer's filter is not an effect");
const filterPlugin = filter.plugin;

/** The node of the canvas that is the picture of this node of the session. */
const nodeOf = async (id: number): Promise<HTMLElement> => {
  const found = await screen.findByTestId(`rf__node-${String(id)}`);
  return found;
};

/** The button of the list of the modulator `id`, which draws a cable from it. */
const pickerOf = async (id: number): Promise<HTMLButtonElement> => {
  const node = await nodeOf(id);
  return within(node).getByRole<HTMLButtonElement>("button", { name: /^Add a cable from / });
};

/** What is on the list that is open, by the plugin or the modulator it is of (a node of the canvas is a group too). */
const listed = (): [string | null, string[]][] =>
  within(screen.getByRole("menu"))
    .getAllByRole("group")
    .map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);

/** The names of the controls in a node, sliders and choices (not the list that draws a cable, which is a button), in the order they are in. */
const controlsIn = (node: HTMLElement): string[] =>
  Array.from(node.querySelectorAll('[role="slider"], select')).map(
    (control) => control.getAttribute("aria-label") ?? control.closest("label")?.querySelector(".tang-choice__label")?.textContent ?? "?",
  );

describe("the patch", () => {
  test("has a node for each node of the session, of its kind, with its name", async () => {
    await rig(<PatchView />);
    for (const [node, kind] of [
      [oscillator, "instrument"],
      [filter, "effect"],
      [lfo, "modulator"],
    ] as const) {
      const placed = await nodeOf(node.id);
      expect(placed.classList.contains(`react-flow__node-${kind}`)).toBe(true);
      expect(placed.querySelector(".tang-node")?.classList.contains(`tang-node--${kind}`)).toBe(true);
      expect(within(placed).getByText(node.name, { selector: ".tang-node__name" })).toBeTruthy();
    }
    expect(within(await nodeOf(oscillator.id)).getByText("Built-in")).toBeTruthy();
    expect(within(await nodeOf(4294967295)).getByText("Master")).toBeTruthy();
  });

  test("shows each plugin node's parameters as controls, and its own gain after them (and a lane's keys after that)", async () => {
    await rig(<PatchView />);
    expect(controlsIn(await nodeOf(oscillator.id))).toEqual(["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "Volume", "Lowest key of Oscillator", "Highest key of Oscillator"]);
    expect(controlsIn(await nodeOf(filter.id))).toEqual(["cutoff", "resonance", "type", "drive", "Mix"]);
  });

  test("has a knob for what has a scale and a choice for what has names", async () => {
    await rig(<PatchView />);
    const osc = await nodeOf(oscillator.id);
    expect(within(osc).getByRole("combobox", { name: "waveform" })).toBeTruthy();
    expect(within(osc).getAllByRole("slider")).toHaveLength(8);
    const fil = await nodeOf(filter.id);
    expect(within(fil).getByRole("combobox", { name: "type" })).toBeTruthy();
    expect(within(fil).getAllByRole("slider")).toHaveLength(4);
  });

  test("draws the cables: the audio between the nodes, and the modulation, which is thicker the deeper it is", async () => {
    const { view } = await rig(<PatchView />);
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(3);
    });
    expect(view.container.querySelectorAll(".tang-cable--audio")).toHaveLength(2);
    expect(Array.from(view.container.querySelectorAll(".tang-cable")).map((path) => path.id).sort()).toEqual(["audio:0>1", `audio:1>${String(MASTER)}`, "mod:0"]);
    const modulation = view.container.querySelectorAll<SVGPathElement>(".tang-cable--modulation");
    expect(modulation).toHaveLength(1);
    expect(modulation[0]?.style.strokeWidth).toBe(String(cableWidth(0.4)));
  });

  test("draws a cable as thick as it is deep as it is turned, without laying the canvas out again", async () => {
    const { view, server } = await rig(<PatchView />);
    const width = () => view.container.querySelector<SVGPathElement>(".tang-cable--modulation")?.style.strokeWidth;
    await waitFor(() => {
      expect(width()).toBe(String(cableWidth(0.4)));
    });
    act(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: -1 });
    });
    await waitFor(() => {
      expect(width()).toBe(String(cableWidth(-1)));
    });
    expect(cableWidth(-1)).not.toBe(cableWidth(0.4));
  });

  test("puts a slider for its depth on each modulation cable, named for what the cable joins", async () => {
    await rig(<PatchView />);
    const depth = await screen.findByRole<HTMLInputElement>("slider", { name: "Depth of LFO → Filter: cutoff" });
    expect([depth.min, depth.max, depth.step, depth.value]).toEqual(["-1", "1", "0.01", "0.4"]);
    expect(depth.getAttribute("aria-valuetext")).toBe("40%");
    expect(screen.getAllByRole("slider", { name: /^Depth of / })).toHaveLength(1); // none on the audio cables
  });

  test("turns the depth of a cable on the cable: shown at once, sent on the frame as a setting of the cable, and the cable follows", async () => {
    const { clock, requests, view } = await rig(<PatchView />);
    const depth = await screen.findByRole<HTMLInputElement>("slider", { name: "Depth of LFO → Filter: cutoff" });
    fireEvent.change(depth, { target: { value: "-0.75" } });
    expect(depth.value).toBe("-0.75");
    expect(depth.getAttribute("aria-valuetext")).toBe("-75%");
    expect(requests()).toEqual([]); // not before the frame
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setEdgeDepth", edge: 0, depth: -0.75 } }]);
    });
    await waitFor(() => {
      expect(view.container.querySelector<SVGPathElement>(".tang-cable--modulation")?.style.strokeWidth).toBe(String(cableWidth(-0.75)));
    });
  });

  test("sends a depth when the gesture that set it ends: the key let go, the pointer let go, the focus gone (one intent a frame)", async () => {
    const { requests, clock } = await rig(<PatchView />);
    const depth = await screen.findByRole<HTMLInputElement>("slider", { name: "Depth of LFO → Filter: cutoff" });
    const ends: [string, () => void][] = [
      ["0.5", () => fireEvent.keyUp(depth, { key: "ArrowUp" })],
      ["0.6", () => fireEvent.pointerUp(depth)],
      ["0.7", () => fireEvent.blur(depth)],
    ];
    for (const [value, end] of ends) {
      fireEvent.change(depth, { target: { value } });
      end(); // (not waiting for the frame)
      await waitFor(() => {
        expect(requests().at(-1)).toEqual({ intent: { type: "setEdgeDepth", edge: 0, depth: Number(value) } });
      });
      act(() => {
        clock.tick(16 * (requests().length + 1)); // the next frame, which is the one that is allowed the next intent
      });
    }
    expect(requests()).toHaveLength(3);
  });

  test("follows a depth that somebody else sets", async () => {
    const { server } = await rig(<PatchView />);
    const depth = await screen.findByRole<HTMLInputElement>("slider", { name: "Depth of LFO → Filter: cutoff" });
    act(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: -0.2 });
    });
    await waitFor(() => {
      expect(depth.value).toBe("-0.2");
    });
    expect(depth.getAttribute("aria-valuetext")).toBe("-20%");
    // (A whole percent, though 0.29 times 100 is not quite 29.)
    act(() => {
      server.apply({ type: "setEdgeDepth", edge: 0, depth: 0.29 });
    });
    await waitFor(() => {
      expect(depth.getAttribute("aria-valuetext")).toBe("29%");
    });
  });

  test("marks the port of the control that a cable ends at, and no other", async () => {
    await rig(<PatchView />);
    const fil = await nodeOf(filter.id);
    const ports = Array.from(fil.querySelectorAll(".tang-port--param"));
    expect(ports).toHaveLength(4); // one for each of the filter's parameters
    expect(fil.querySelectorAll(".tang-port--driven")).toHaveLength(1);
    expect(fil.querySelector(".tang-port--driven")?.getAttribute("data-handleid")).toBe("param-0");
    expect((await nodeOf(oscillator.id)).querySelectorAll(".tang-port--driven")).toHaveLength(0);
  });

  test("shows the modulator for what it is, with a trace of what it puts out", async () => {
    await rig(<PatchView />);
    const mod = await nodeOf(lfo.id);
    // (An LFO is called what it is, so its kind is not said twice.)
    expect(within(mod).getByText("LFO", { selector: ".tang-node__name" })).toBeTruthy();
    expect(mod.querySelector(".tang-node__badge")).toBeNull();
    // Its settings are controls, not words: a knob for the rate and a choice for the waveform.
    expect(controlsIn(mod)).toEqual(["Rate", "Wave"]);
    expect(within(mod).getByRole("slider", { name: "Rate" }).getAttribute("aria-valuetext")).toBe("0.5 Hz");
    expect(within(mod).getByRole<HTMLSelectElement>("combobox", { name: "Wave" }).value).toBe("0");
    expect(mod.querySelector(".tang-node__detail")).toBeNull();
    const bar = within(mod).getByRole("meter", { name: `${lfo.name} output` });
    expect(bar.getAttribute("aria-valuemin")).toBe("-1");
    expect(bar.getAttribute("aria-valuemax")).toBe("1");
    expect(bar.hasAttribute("data-centred")).toBe(true);
  });

  test("says what kind of modulator an envelope and a MIDI control are, which their names do not", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.push(
      { id: 90, parent: 1, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 } },
      { id: 91, parent: 1, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } },
    );
    await rig(<PatchView />, { snapshot });
    expect(within(await nodeOf(90)).getByText("Envelope", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(within(await nodeOf(90)).getByText("ADSR", { selector: ".tang-node__name" })).toBeTruthy();
    expect(within(await nodeOf(91)).getByText("MIDI", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(within(await nodeOf(91)).getByText("CC 74", { selector: ".tang-node__name" })).toBeTruthy();
  });

  test("tells two modulators of one name apart: each node is a group of its own name, and so are the controls in it and the cables", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    // A second LFO on the same lane, driving the same cutoff.
    snapshot.graph.nodes.push({ id: 90, parent: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "square", rate: 7 } });
    snapshot.graph.edges.push({ kind: "modulation", id: 1, amount: 0.3, from: { node: 90, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 0 } } });
    await rig(<PatchView />, { snapshot });
    const first = await screen.findByRole("group", { name: "LFO 1" });
    const second = await screen.findByRole("group", { name: "LFO 2" });
    expect(first.getAttribute("data-id")).toBe(String(lfo.id));
    expect(second.getAttribute("data-id")).toBe("90");
    // Each has its own Rate, which is told by the node it is in.
    expect(within(first).getByRole("slider", { name: "Rate" }).getAttribute("aria-valuenow")).toBe("0.5");
    expect(within(second).getByRole("slider", { name: "Rate" }).getAttribute("aria-valuenow")).toBe("7");
    expect(within(second).getByText("LFO 2", { selector: ".tang-node__name" })).toBeTruthy();
    expect(within(second).queryByText("LFO", { selector: ".tang-node__badge" })).toBeNull(); // (it is said by the name)
    // And each cable has a slider that says whose it is.
    expect(screen.getByRole("slider", { name: "Depth of LFO 1 → Filter: cutoff" })).toBeTruthy();
    expect(screen.getByRole("slider", { name: "Depth of LFO 2 → Filter: cutoff" })).toBeTruthy();
    expect(screen.getByRole("meter", { name: "LFO 2 output" })).toBeTruthy();
  });

  test("gives an envelope the four stages as knobs, and a MIDI control no settings but what it is bound to", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.push(
      { id: 90, parent: 1, name: "ADSR", kind: "modulator", source: { type: "envelope", attack: 0.01, decay: 0.3, sustain: 0.7, release: 2 } },
      { id: 91, parent: 1, name: "CC 74", kind: "modulator", source: { type: "midiLearn", control: { type: "cc", number: 74 }, learning: false } },
    );
    await rig(<PatchView />, { snapshot });
    const envelope = await nodeOf(90);
    expect(controlsIn(envelope)).toEqual(["Attack", "Decay", "Sustain", "Release"]);
    expect(Array.from(envelope.querySelectorAll('[role="slider"]')).map((knob) => knob.getAttribute("aria-valuetext"))).toEqual(["10 ms", "300 ms", "0.7", "2 s"]);
    const control = await nodeOf(91);
    expect(controlsIn(control)).toEqual([]);
    expect(control.querySelector(".tang-node__controls")).toBeNull(); // (not an empty row of them)
    expect(within(control).getByText("CC 74", { selector: ".tang-node__detail" })).toBeTruthy();
  });

  test("says what a control is bound to as soon as it is bound, though the settings of a modulator do not render its node", async () => {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.push({ id: 91, parent: 1, name: "MIDI (learning…)", kind: "modulator", source: { type: "midiLearn", control: null, learning: true } });
    const { client } = await rig(<PatchView />, { snapshot });
    const control = await nodeOf(91);
    expect(within(control).getByText("waiting for a control…", { selector: ".tang-node__detail" })).toBeTruthy();
    const bound = structuredClone(snapshot);
    const node = bound.graph.nodes.find((candidate) => candidate.id === 91);
    if (node?.kind === "modulator") node.source = { type: "midiLearn", control: { type: "cc", number: 21 }, learning: false };
    act(() => {
      client.store.setState({ snapshot: { ...bound, rev: 5 } });
    });
    await waitFor(() => {
      expect(within(control).getByText("CC 21", { selector: ".tang-node__detail" })).toBeTruthy();
    });
  });

  test("turns a modulator's rate on its node, which sends it as a setting and shows it", async () => {
    const { clock, requests } = await rig(<PatchView />);
    const rate = within(await nodeOf(lfo.id)).getByRole("slider", { name: "Rate" });
    fireEvent.keyDown(rate, { key: "ArrowUp" });
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toMatchObject({ type: "setModulatorField", node: lfo.id, field: "rate" });
    await waitFor(() => {
      expect(Number(rate.getAttribute("aria-valuenow"))).toBeCloseTo(0.5 * 5000 ** 0.01, 6);
    });
  });

  test("does not render a modulator's node again for a change of its settings, only the control", async () => {
    const { server, client } = await rig(<PatchView />);
    const node = await nodeOf(lfo.id);
    // A node renders from its own selection of the store, what it is made of and not the values of its settings, and says what kind it is each time it does.
    const renders = () => vi.mocked(describeModule.kindOf).mock.calls.length;
    const settled = renders();
    expect(settled).toBeGreaterThan(0);
    const rev = client.state.snapshot?.rev ?? 0;
    act(() => {
      server.apply({ type: "setModulatorField", node: lfo.id, field: "rate", value: 9 });
      server.apply({ type: "setModulatorField", node: lfo.id, field: "waveform", value: 1 });
    });
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBe(rev + 2);
    });
    await waitFor(() => {
      expect(within(node).getByRole("slider", { name: "Rate" }).getAttribute("aria-valuenow")).toBe("9");
    });
    expect(within(node).getByRole<HTMLSelectElement>("combobox", { name: "Wave" }).value).toBe("1");
    expect(renders()).toBe(settled);
  });

  test("gives each node the ports it has: an effect takes audio in and puts it out, an instrument only puts it out, a modulator puts out modulation, the master takes audio in", async () => {
    await rig(<PatchView />);
    const handles = async (id: number) =>
      Array.from((await nodeOf(id)).querySelectorAll(".react-flow__handle:not(.tang-port--param)")).map(
        (handle) => `${handle.getAttribute("data-handleid") ?? "?"}:${handle.classList.contains("source") ? "source" : "target"}:${handle.getAttribute("data-handlepos") ?? "?"}`,
      );
    // (Audio goes in on the left and out on the right; a modulator's output is at its bottom, over what it drives.)
    expect(await handles(oscillator.id)).toEqual(["audioOut:source:right"]);
    expect(await handles(filter.id)).toEqual(["audioIn:target:left", "audioOut:source:right"]);
    expect(await handles(lfo.id)).toEqual(["modOut:source:bottom"]);
    expect(await handles(MASTER)).toEqual(["audioIn:target:left"]);
    // And the ports of the controls are on their top edges.
    const ports = Array.from((await nodeOf(filter.id)).querySelectorAll(".tang-port--param")).map((handle) => handle.getAttribute("data-handlepos"));
    expect(new Set(ports)).toEqual(new Set(["top"]));
  });

  test("can be moved but not selected, and takes a cable only at a modulator's output (to start one) and at a parameter's or a setting's port (to end one)", async () => {
    const { view } = await rig(<PatchView />);
    await nodeOf(filter.id);
    for (const node of Array.from(view.container.querySelectorAll(".react-flow__node"))) {
      expect(node.classList.contains("draggable")).toBe(true);
      expect(node.classList.contains("selectable")).toBe(false);
    }
    const connectable = Array.from(view.container.querySelectorAll(".react-flow__handle.connectable"));
    const ports = connectable.map((handle) => handle.getAttribute("data-handleid") ?? "?");
    expect(ports.filter((port) => port === "modOut")).toHaveLength(1);
    expect(ports.filter((port) => port.startsWith("param-"))).toHaveLength(12); // the oscillator's eight and the filter's four
    expect(ports.filter((port) => port === "rate")).toHaveLength(1); // the LFO's rate, which another modulator's cable can end at
    expect(ports).toHaveLength(14); // and no audio port: the audio path is derived, there is nothing to draw
    // A cable is started at a modulator's output and ends at a parameter, never the other way.
    const output = view.container.querySelector(".react-flow__handle[data-handleid='modOut']");
    expect(output?.classList.contains("connectablestart")).toBe(true);
    const endsCable = (handle: Element): boolean => {
      const id = handle.getAttribute("data-handleid") ?? "";
      return id.startsWith("param-") || id === "rate";
    };
    for (const handle of connectable.filter(endsCable)) {
      expect(handle.classList.contains("connectablestart")).toBe(false);
      expect(handle.classList.contains("connectableend")).toBe(true);
    }
  });

  describe("adding a modulator", () => {
    const addButtons = async () => (await screen.findAllByRole("button", { name: /^Add a modulator to / })).map((button) => button.getAttribute("aria-label"));

    test("is offered on every lane and group, by a button that names whose it is, and on nothing else", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      const names = (await addButtons()).sort();
      expect(names).toEqual(
        [
          "Add a modulator to (empty)",
          "Add a modulator to Group 1",
          "Add a modulator to Group 2",
          "Add a modulator to Oscillator 1",
          "Add a modulator to Oscillator 2",
          "Add a modulator to Oscillator 3",
          "Add a modulator to Oscillator 4",
        ].sort(),
      );
      // None on an effect, a modulator or the master.
      for (const id of [5, 6, 13, MASTER]) expect(within(await nodeOf(id)).queryByRole("button", { name: /^Add a modulator/ })).toBeNull();
    });

    test("opens a menu of the kinds of modulator", async () => {
      await rig(<PatchView />);
      fireEvent.click(await screen.findByRole("button", { name: "Add a modulator to Oscillator" }));
      const menu = screen.getByRole("menu", { name: "Add a modulator to Oscillator" });
      expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["LFO", "Envelope", "MIDI control"]);
    });

    test("asks the server to add the kind that was chosen to that lane, and the node is on the canvas with its controls when it has", async () => {
      const { requests } = await rig(<PatchView />);
      expect(screen.queryByRole("group", { name: "ADSR" })).toBeNull();
      fireEvent.click(await screen.findByRole("button", { name: "Add a modulator to Oscillator" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "Envelope" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addModulator", parent: oscillator.id, kind: "envelope" }]);
      });
      const added = await screen.findByRole("group", { name: "ADSR" });
      expect(controlsIn(added)).toEqual(["Attack", "Decay", "Sustain", "Release"]);
      // The menu has gone, and the focus is back on the button that opened it.
      expect(screen.queryByRole("menu")).toBeNull();
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add a modulator to Oscillator" }));
      });
    });

    test("to a group asks for one in the group, whichever kind", async () => {
      const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      for (const [item, kind] of [["LFO", "lfo"], ["MIDI control", "midiLearn"]] as const) {
        fireEvent.click(await screen.findByRole("button", { name: "Add a modulator to Group 2" }));
        fireEvent.click(screen.getByRole("menuitem", { name: item }));
        await waitFor(() => {
          expect(requests().at(-1)?.intent).toEqual({ type: "addModulator", parent: 2, kind });
        });
      }
      await waitFor(() => {
        expect(requests()).toHaveLength(2);
      });
    });

    test("says why when it is refused, on the page's notices, and the canvas is as it was", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client, view } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      const before = view.container.querySelectorAll(".react-flow__node").length;
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      fireEvent.click(await screen.findByRole("button", { name: "Add a modulator to Oscillator" }));
      fireEvent.click(screen.getByRole("menuitem", { name: "LFO" }));
      expect(await screen.findByText("Could not add an LFO to Oscillator: Not connected to the server.")).toBeTruthy();
      expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(before);
    });
  });

  describe("adding an effect", () => {
    const addButtons = async () => (await screen.findAllByRole("button", { name: /^Add an effect to / })).map((button) => button.getAttribute("aria-label"));
    const opened = async (name: string): Promise<HTMLElement> => {
      fireEvent.click(await screen.findByRole("button", { name: `Add an effect to ${name}` }));
      return screen.getByRole("menu", { name: `Add an effect to ${name}` });
    };

    test("is offered on every lane and group, by a button that names whose it is, and on nothing else", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      expect((await addButtons()).sort()).toEqual(
        [
          "Add an effect to (empty)",
          "Add an effect to Group 1",
          "Add an effect to Group 2",
          "Add an effect to Oscillator 1",
          "Add an effect to Oscillator 2",
          "Add an effect to Oscillator 3",
          "Add an effect to Oscillator 4",
        ].sort(),
      );
      for (const id of [5, 6, 13, MASTER]) expect(within(await nodeOf(id)).queryByRole("button", { name: /^Add an effect/ })).toBeNull();
    });

    test("opens a menu of the effects that are installed, under the format each is of, and not of the instruments", async () => {
      await rig(<PatchView />);
      const menu = await opened("Oscillator");
      const groups = within(menu).getAllByRole("group").map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);
      expect(groups).toEqual([["Built-in", ["Filter", "Reverb"]]]);
    });

    test("has what the scan finds as it finds it, while it is open", async () => {
      const { server } = await rig(<PatchView />, { catalog: { entries: builtinCatalog, scanning: true } });
      const menu = await opened("Oscillator");
      expect(within(menu).getAllByRole("menuitem")).toHaveLength(2);
      act(() => {
        server.announceCatalog(
          [
            ...builtinCatalog,
            { id: "vst3:Acme Verb", name: "Acme Verb", format: "VST3", isInstrument: false, params: 4, presets: 0 },
            { id: "clap:com.acme.chorus", name: "Chorus", format: "CLAP", isInstrument: false, params: 3, presets: 0 },
            { id: "vst3:Acme Synth", name: "Acme Synth", format: "VST3", isInstrument: true, params: 40, presets: 0 },
          ],
          false,
        );
      });
      await waitFor(() => {
        expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Filter", "Reverb", "Chorus", "Acme Verb"]);
      });
      expect(within(screen.getByRole("menu")).getAllByRole("group").map((group) => group.getAttribute("aria-label"))).toEqual(["Built-in", "CLAP", "VST3"]);
    });

    test("asks the server to put the one that was chosen in that lane's chain, and the node is on the canvas with its controls when it has", async () => {
      const { requests } = await rig(<PatchView />);
      expect(screen.queryByRole("group", { name: "Reverb" })).toBeNull();
      await opened("Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Reverb" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addEffect", parent: oscillator.id, plugin: "builtin:reverb" }]);
      });
      const added = await screen.findByRole("group", { name: "Reverb" });
      const names = (builtinEffects["builtin:reverb"]?.plugin.params ?? []).slice(0, 8).map((param) => param.name);
      expect(controlsIn(added)).toEqual([...names, "Mix"]);
      // The menu has gone, and the focus is back on the button that opened it.
      expect(screen.queryByRole("menu")).toBeNull();
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an effect to Oscillator" }));
      });
    });

    test("takes an audio cable to the master from the new node, and the lane's own goes into it", async () => {
      const { view } = await rig(<PatchView />);
      await waitFor(() => {
        expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(3);
      });
      await opened("Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Reverb" }));
      await waitFor(() => {
        expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(4);
      });
      const audio = Array.from(view.container.querySelectorAll(".react-flow__edge")).filter((edge) => edge.querySelector(".tang-cable--audio"));
      expect(audio).toHaveLength(3);
    });

    test("to a group asks for one on its bus", async () => {
      const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      await opened("Group 2");
      fireEvent.click(screen.getByRole("menuitem", { name: "Filter" }));
      await waitFor(() => {
        expect(requests().at(-1)?.intent).toEqual({ type: "addEffect", parent: 2, plugin: "builtin:filter" });
      });
    });

    test("has nothing to open when nothing that is installed can be an effect, or the server has not said what is", async () => {
      const instrument = { id: "builtin:osc", name: "Oscillator", format: "Built-in", isInstrument: true, params: 8, presets: 0 };
      for (const catalog of [{ entries: [], scanning: true }, { entries: [instrument], scanning: false }]) {
        await rig(<PatchView />, { catalog });
        const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Add an effect to Oscillator" });
        expect(button.disabled).toBe(true);
        cleanup();
      }
    });

    test("says why when it is refused, on the page's notices, by the name the catalog gives it", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client, view } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      const before = view.container.querySelectorAll(".react-flow__node").length;
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      await opened("Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Reverb" }));
      expect(await screen.findByText("Could not add Reverb to Oscillator: Not connected to the server.")).toBeTruthy();
      expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(before);
    });
  });

  describe("changing an instrument", () => {
    const label = /^(Change the instrument of|Choose an instrument for) /;
    const buttons = async () => (await screen.findAllByRole("button", { name: label })).map((button) => button.getAttribute("aria-label"));
    const opened = async (name: string): Promise<HTMLElement> => {
      fireEvent.click(await screen.findByRole("button", { name }));
      return screen.getByRole("menu", { name });
    };
    const waveform = async (id: number): Promise<string> => within(await nodeOf(id)).getByRole<HTMLSelectElement>("combobox", { name: "waveform" }).value;

    test("is offered on every lane, by a button that names whose it is (an empty lane is asked to be given one), and on nothing else", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      expect((await buttons()).sort()).toEqual(
        [
          "Change the instrument of Oscillator 1",
          "Change the instrument of Oscillator 2",
          "Change the instrument of Oscillator 3",
          "Change the instrument of Oscillator 4",
          "Choose an instrument for (empty)",
        ].sort(),
      );
      for (const id of [0, 1, 2, 3, 5, 6, 13, MASTER]) expect(within(await nodeOf(id)).queryByRole("button", { name: label })).toBeNull();
    });

    test("opens a menu of the instruments that are installed, under the format each is of, and not of the effects", async () => {
      await rig(<PatchView />);
      const menu = await opened("Change the instrument of Oscillator");
      const groups = within(menu).getAllByRole("group").map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);
      expect(groups).toEqual([["Built-in", ["Oscillator"]]]);
    });

    test("has what the scan finds as it finds it, while it is open", async () => {
      const { server } = await rig(<PatchView />, { catalog: { entries: builtinCatalog, scanning: true } });
      const menu = await opened("Change the instrument of Oscillator");
      expect(within(menu).getAllByRole("menuitem")).toHaveLength(1);
      act(() => {
        server.announceCatalog(
          [
            ...builtinCatalog,
            { id: "vst3:Acme Synth", name: "Acme Synth", format: "VST3", isInstrument: true, params: 40, presets: 0 },
            { id: "vst3:Acme Verb", name: "Acme Verb", format: "VST3", isInstrument: false, params: 4, presets: 0 },
          ],
          false,
        );
      });
      await waitFor(() => {
        expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Oscillator", "Acme Synth"]);
      });
    });

    test("asks the server to put the one that was chosen in that lane, which keeps its node and has the new instrument's parameters", async () => {
      const { requests } = await rig(<PatchView />);
      expect(await waveform(oscillator.id)).toBe("3"); // the tracer's is a saw
      await opened("Change the instrument of Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "replaceInstrument", node: oscillator.id, plugin: "builtin:osc" }]);
      });
      await waitFor(async () => {
        expect(await waveform(oscillator.id)).toBe("0"); // a new instrument starts as a sine
      });
      // The menu has gone, and the focus is back on the button that opened it.
      expect(screen.queryByRole("menu")).toBeNull();
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Change the instrument of Oscillator" }));
      });
    });

    test("fills an empty lane, which is called by its instrument then and has a button that changes it", async () => {
      const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      await opened("Choose an instrument for (empty)");
      fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
      await waitFor(() => {
        expect(requests().at(-1)?.intent).toEqual({ type: "replaceInstrument", node: 14, plugin: "builtin:osc" });
      });
      expect(await screen.findByRole("button", { name: "Change the instrument of Oscillator 5" })).toBeTruthy();
      expect(screen.queryByRole("button", { name: "Choose an instrument for (empty)" })).toBeNull();
      expect(controlsIn(await nodeOf(14))).toEqual(["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "Volume", "Lowest key of Oscillator 5", "Highest key of Oscillator 5"]);
    });

    test("takes the cables that drove the old instrument off the canvas, and leaves the others", async () => {
      const { server, view } = await rig(<PatchView />);
      server.apply({ type: "addModEdge", modulator: lfo.id, to: { node: oscillator.id, port: { type: "param", index: 1 } } });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(4);
      });
      await opened("Change the instrument of Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
      await waitFor(() => {
        expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(3);
      });
    });

    test("has nothing to open when nothing that is installed can be an instrument, or the server has not said what is", async () => {
      const effect = { id: "builtin:filter", name: "Filter", format: "Built-in", isInstrument: false, params: 4, presets: 0 };
      for (const catalog of [{ entries: [], scanning: true }, { entries: [effect], scanning: false }]) {
        await rig(<PatchView />, { catalog });
        const button = await screen.findByRole<HTMLButtonElement>("button", { name: "Change the instrument of Oscillator" });
        expect(button.disabled).toBe(true);
        cleanup();
      }
    });

    test("says why when it is refused, on the page's notices, by the names the catalog gives", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      await opened("Change the instrument of Oscillator");
      fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator" }));
      expect(await screen.findByText("Could not change the instrument of Oscillator to Oscillator: Not connected to the server.")).toBeTruthy();
    });
  });

  describe("loading a preset", () => {
    const label = /^Load a preset into /;
    const buttons = async () => (await screen.findAllByRole("button", { name: label })).map((button) => button.getAttribute("aria-label"));
    const opened = async (name: string): Promise<HTMLElement> => {
      fireEvent.click(await screen.findByRole("button", { name }));
      return screen.getByRole("menu", { name });
    };
    /** What a knob of a node says it is at. */
    const knob = async (node: number, name: string): Promise<string | null> => within(await nodeOf(node)).getByRole("slider", { name }).getAttribute("aria-valuenow");
    const PRESETS = ["Arcadia Dream Hall", "Cathedral", "Vintage Plate", "Small Room", "Ambient Wash"];

    test("is offered on every instrument and effect of a lane that has presets, by a button that names whose it is, and on nothing else", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      // (Of the two reverbs, the one on a group's bus has none to load into: the audio thread addresses a preset by a lane's slot.)
      expect(await buttons()).toEqual(["Load a preset into Reverb 2"]);
      for (const id of [0, 1, 2, 3, 4, 5, 6, 13, MASTER]) expect(within(await nodeOf(id)).queryByRole("button", { name: label })).toBeNull();
    });

    test("is offered on an instrument that has presets too, and not on one that has none", async () => {
      const snapshot = structuredClone(tracerSnapshot);
      const osc = snapshot.graph.nodes.find((n) => n.id === oscillator.id);
      if (osc?.kind !== "instrument" || !osc.plugin) throw new Error("no instrument");
      osc.plugin.presets = [
        { id: "pad", name: "Pad" },
        { id: "lead", name: "Lead" },
      ];
      await rig(<PatchView />, { snapshot });
      expect(await buttons()).toEqual(["Load a preset into Oscillator"]);
      expect(within(await nodeOf(filter.id)).queryByRole("button", { name: label })).toBeNull();
    });

    test("opens a menu of the preset names, in the plugin's order, none marked while none is loaded", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      const menu = await opened("Load a preset into Reverb 2");
      const entries = within(menu).getAllByRole("menuitem");
      expect(entries.map((entry) => entry.textContent)).toEqual(PRESETS);
      expect(entries.map((entry) => entry.getAttribute("aria-current"))).toEqual([null, null, null, null, null]);
    });

    test("asks the server to load the one that was chosen, and the knobs of the node show what it sets", async () => {
      const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      const before = await knob(10, "decay");
      await opened("Load a preset into Reverb 2");
      fireEvent.click(screen.getByRole("menuitem", { name: "Cathedral" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "loadPreset", node: 10, preset: "cathedral" }]);
      });
      // (The reverb's decay is the first of a preset's values: a long one in Cathedral.)
      const decay = (builtinPresets["builtin:reverb"]?.cathedral ?? []).find((v) => v.param === 0)?.value;
      await waitFor(async () => {
        expect(Number(await knob(10, "decay"))).toBeCloseTo(decay ?? Number.NaN, 5);
      });
      expect(Number(before)).not.toBeCloseTo(decay ?? 0, 5);
      // The menu has gone, and the focus is back on the button that opened it.
      expect(screen.queryByRole("menu")).toBeNull();
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Load a preset into Reverb 2" }));
      });
    });

    test("has the preset that was loaded marked, and the menu opens on it", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      await opened("Load a preset into Reverb 2");
      fireEvent.click(screen.getByRole("menuitem", { name: "Vintage Plate" }));
      await waitFor(() => {
        expect(screen.queryByRole("menu")).toBeNull();
      });
      const menu = await opened("Load a preset into Reverb 2");
      const marked = within(menu).getAllByRole("menuitem").filter((entry) => entry.getAttribute("aria-current") === "true");
      expect(marked.map((entry) => entry.textContent)).toEqual(["Vintage Plate"]);
      expect(document.activeElement).toBe(marked[0]);
    });

    test("is told of a preset that another front-end loads: the mark moves, and so do the knobs", async () => {
      const { server } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      await screen.findByRole("button", { name: "Load a preset into Reverb 2" });
      act(() => {
        server.apply({ type: "loadPreset", node: 10, preset: "room" });
      });
      const room = (builtinPresets["builtin:reverb"]?.room ?? []).find((v) => v.param === 0)?.value;
      await waitFor(async () => {
        expect(Number(await knob(10, "decay"))).toBeCloseTo(room ?? Number.NaN, 5);
      });
      const menu = await opened("Load a preset into Reverb 2");
      expect(within(menu).getAllByRole("menuitem").filter((entry) => entry.getAttribute("aria-current") === "true").map((entry) => entry.textContent)).toEqual(["Small Room"]);
    });

    test("says why when it is refused, on the page's notices, by the preset's name", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
        { snapshot: reachFixture.snapshot },
      );
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      await opened("Load a preset into Reverb 2");
      fireEvent.click(screen.getByRole("menuitem", { name: "Cathedral" }));
      expect(await screen.findByText("Could not load Cathedral into Reverb 2: Not connected to the server.")).toBeTruthy();
    });
  });

  describe("drawing a cable to a parameter, from a list", () => {
    test("every modulator has a list of what it can be drawn to: its plugins' parameters, by plugin, but what it drives already", async () => {
      await rig(<PatchView />);
      const picker = await pickerOf(lfo.id);
      expect(picker.getAttribute("aria-label")).toBe("Add a cable from LFO");
      expect(picker.textContent).toBe("Add a cable…");
      expect(picker.hasAttribute("disabled")).toBe(false);
      // (A menu, which does nothing until a parameter is chosen: not a native list, whose arrow keys change it on some platforms.)
      expect(picker.getAttribute("aria-haspopup")).toBe("menu");
      fireEvent.click(picker);
      expect(screen.getByRole("menu", { name: "Add a cable from LFO" })).toBeTruthy();
      expect(listed()).toEqual([
        ["Oscillator", ["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan"]],
        ["Filter", ["resonance", "type", "drive"]],
      ]);
    });

    test("draws nothing by being opened, moved through or left: only choosing a parameter is a cable", async () => {
      const { requests } = await rig(<PatchView />);
      const picker = await pickerOf(lfo.id);
      fireEvent.keyDown(picker, { key: "ArrowDown" });
      for (const key of ["ArrowDown", "ArrowDown", "End", "ArrowUp", "d"]) fireEvent.keyDown(document.activeElement ?? document.body, { key });
      fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
      expect(screen.queryByRole("menu")).toBeNull();
      expect(requests()).toEqual([]);
    });

    test("asks the server for the cable that was picked, and it is drawn, and the parameter is off the list", async () => {
      const { requests, view } = await rig(<PatchView />);
      const detune = oscillator.kind === "instrument" ? oscillator.plugin?.params.find((param) => param.name === "detune")?.index : undefined;
      const picker = await pickerOf(lfo.id);
      fireEvent.click(picker);
      fireEvent.click(screen.getByRole("menuitem", { name: "detune" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addModEdge", modulator: lfo.id, to: { node: oscillator.id, port: { type: "param", index: detune } } }]);
      });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(2);
      });
      // The cable has its depth to turn and its button to remove; the menu is closed, with the focus on its button, and the parameter is not offered again.
      expect(screen.getByRole("slider", { name: "Depth of LFO → Oscillator: detune" })).toBeTruthy();
      expect(screen.queryByRole("menu")).toBeNull();
      expect(document.activeElement).toBe(picker);
      fireEvent.click(picker);
      expect(listed()[0]).toEqual(["Oscillator", ["waveform", "volume", "attack", "decay", "sustain", "release", "pan"]]);
      // And the oscillator's detune knob has its port driven now.
      const knob = within(await nodeOf(oscillator.id)).getByRole("slider", { name: "detune" });
      expect(knob.closest(".tang-cell")?.querySelector(".tang-port--driven")).toBeTruthy();
    });

    test("says why when the cable was not drawn, and the list is as it was", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      const picker = await pickerOf(lfo.id);
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      fireEvent.click(picker);
      fireEvent.click(screen.getByRole("menuitem", { name: "detune" }));
      expect(await screen.findByText("Could not draw a cable from LFO to Oscillator: detune: Not connected to the server.")).toBeTruthy();
      expect(document.activeElement).toBe(picker);
      fireEvent.click(picker);
      expect(screen.getByRole("menuitem", { name: "detune" })).toBeTruthy();
    });

    test("is for a group's modulator the parameters of its members' plugins and of its own bus effects, then the settings of its members' modulators", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      fireEvent.click(await pickerOf(15));
      // (In the order of the graph: the group's own bus effect, then the lanes that are its members; then their modulators, which have settings.)
      expect(listed().map(([plugin]) => plugin)).toEqual(["Filter 1", "Oscillator 1", "Filter 2", "Oscillator 2", "LFO 2", "ADSR 2"]);
    });

    test("is disabled for a modulator with nothing to be drawn to, and says so", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      const picker = await pickerOf(17); // the lane with nothing in it
      expect(picker.hasAttribute("disabled")).toBe(true);
      expect(picker.textContent).toBe("Nothing to drive");
      fireEvent.click(picker);
      expect(screen.queryByRole("menu")).toBeNull();
    });

    test("tells two modulators of one name apart", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      // (The group's comes first in the graph, so it is the first.)
      expect((await pickerOf(15)).getAttribute("aria-label")).toBe("Add a cable from LFO 1");
      expect((await pickerOf(6)).getAttribute("aria-label")).toBe("Add a cable from LFO 2");
    });

    test("has an id the focus can be given back to, one for each modulator", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      for (const id of [15, 6]) expect((await pickerOf(id)).id).toBe(drivesId(id));
      expect(drivesId(15)).not.toBe(drivesId(6));
    });
  });

  describe("drawing a cable to a setting of another modulator", () => {
    // A group with an LFO ("LFO 1"), an envelope ("ADSR 1") and a MIDI control ("CC 74") of its own; its first member lane has an LFO ("LFO 2"),
    // an envelope ("ADSR 2") and a MIDI control ("CC 1"), and its second an LFO ("LFO 3"); a lane in no group has an LFO, and one of another group.
    const { snapshot } = fieldsFixture;
    const [groupLfo, laneLfo, laneEnvelope, laneControl] = [13, 4, 5, 6];
    const cables = snapshot.graph.edges.filter((edge) => edge.kind === "modulation").length;
    /** The session with a cable (the 50th) from a modulator to a setting of another. */
    const withCable = (from: number, to: number, port: Port): Snapshot => ({
      ...snapshot,
      graph: { ...snapshot.graph, edges: [...snapshot.graph.edges, { kind: "modulation", id: 50, amount: 0.5, from: { node: from, port: { type: "modOut" } }, to: { node: to, port } }] },
    });
    const portsOf = async (id: number): Promise<(string | null)[]> => Array.from((await nodeOf(id)).querySelectorAll(".tang-port--param")).map((port) => port.getAttribute("data-handleid"));

    test("has a port at the top of each setting that takes a cable: an LFO's rate and an envelope's stages, but not an LFO's waveform nor a MIDI control", async () => {
      await rig(<PatchView />, { snapshot });
      expect(await portsOf(laneLfo)).toEqual(["rate"]);
      expect(await portsOf(laneEnvelope)).toEqual(["attack", "decay", "sustain", "release"]);
      expect(await portsOf(laneControl)).toEqual([]);
      // At the top edge of the control it is for, in its cell, as a parameter's is; a cable ends there, and is not started there.
      const rate = (await nodeOf(laneLfo)).querySelector(".tang-port--param");
      expect(rate?.getAttribute("data-handlepos")).toBe("top");
      expect(rate?.closest(".tang-cell")?.querySelector('[role="slider"]')?.getAttribute("aria-label")).toBe("Rate");
      expect(rate?.classList.contains("connectablestart")).toBe(false);
      expect(rate?.classList.contains("connectableend")).toBe(true);
    });

    test("marks the port of a setting that a cable ends at, and no other, and draws the cable from the modulator's output with a slider for its depth", async () => {
      const { view } = await rig(<PatchView />, { snapshot: withCable(laneEnvelope, laneLfo, { type: "rate" }) });
      const driven = async (id: number) => Array.from((await nodeOf(id)).querySelectorAll(".tang-port--driven")).map((port) => port.getAttribute("data-handleid"));
      expect(await driven(laneLfo)).toEqual(["rate"]);
      expect(await driven(laneEnvelope)).toEqual([]);
      expect(await driven(laneControl)).toEqual([]);
      expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(cables + 1);
      expect(screen.getByRole("slider", { name: "Depth of ADSR 2 → LFO 2: rate" })).toBeTruthy();
      expect(screen.getByRole("button", { name: "Remove cable ADSR 2 → LFO 2: rate" })).toBeTruthy();
    });

    test("is not drawn to the depth of a cable, which the canvas has no port for", async () => {
      const { view } = await rig(<PatchView />, { snapshot: withCable(laneLfo, laneEnvelope, { type: "depth", edge: 1 }) });
      await nodeOf(laneLfo);
      expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(cables);
      expect(screen.queryByRole("slider", { name: /^Depth of LFO 2 → ADSR 2/ })).toBeNull();
    });

    test("is on the list of a modulator, after the parameters, modulator by modulator: the settings of the others of its lane, but what it drives already", async () => {
      await rig(<PatchView />, { snapshot: withCable(laneControl, laneEnvelope, { type: "decay" }) });
      fireEvent.click(await pickerOf(laneControl));
      const groups = listed();
      // (The oscillator's parameters, then the LFO's one setting and the envelope's three, for the control drives its decay.)
      expect(groups.map(([name]) => name)).toEqual(["Oscillator 1", "LFO 2", "ADSR 2"]);
      expect(groups.slice(1)).toEqual([
        ["LFO 2", ["rate"]],
        ["ADSR 2", ["attack", "sustain", "release"]],
      ]);
    });

    test("is on the list of an LFO as the envelope's four stages, and not for a MIDI control, which has none", async () => {
      await rig(<PatchView />, { snapshot });
      fireEvent.click(await pickerOf(laneLfo));
      expect(listed().map(([name]) => name)).toEqual(["Oscillator 1", "ADSR 2"]);
      expect(listed()[1]).toEqual(["ADSR 2", ["attack", "decay", "sustain", "release"]]);
    });

    test("is for a group's modulator the settings of its members' modulators, and of the others of its own rack", async () => {
      await rig(<PatchView />, { snapshot });
      fireEvent.click(await pickerOf(groupLfo));
      // (The group's bus effect and the members' plugins, in the order of the graph; then the members' modulators, member by member, then its own rack's.)
      expect(listed().map(([name]) => name)).toEqual(["Reverb", "Oscillator 1", "Oscillator 2", "LFO 2", "ADSR 2", "LFO 3", "ADSR 1"]);
    });

    test("asks the server for the cable that was picked, and it is drawn, and the setting is off the list", async () => {
      const { requests, view } = await rig(<PatchView />, { snapshot });
      const picker = await pickerOf(laneEnvelope);
      fireEvent.click(picker);
      fireEvent.click(within(within(screen.getByRole("menu")).getByRole("group", { name: "LFO 2" })).getByRole("menuitem", { name: "rate" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "addModEdge", modulator: laneEnvelope, to: { node: laneLfo, port: { type: "rate" } } }]);
      });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(cables + 1);
      });
      expect(screen.getByRole("slider", { name: "Depth of ADSR 2 → LFO 2: rate" })).toBeTruthy();
      // The menu is closed, with the focus on its button; the LFO has nothing more to take, and its port is marked as driven.
      expect(screen.queryByRole("menu")).toBeNull();
      expect(document.activeElement).toBe(picker);
      fireEvent.click(picker);
      expect(listed().map(([name]) => name)).not.toContain("LFO 2");
      expect((await nodeOf(laneLfo)).querySelector(".tang-port--driven")?.getAttribute("data-handleid")).toBe("rate");
    });

    test("says why when the cable was not drawn, in the names of the modulators and the setting, and the list is as it was", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
        { snapshot },
      );
      const picker = await pickerOf(laneEnvelope);
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      fireEvent.click(picker);
      fireEvent.click(within(within(screen.getByRole("menu")).getByRole("group", { name: "LFO 2" })).getByRole("menuitem", { name: "rate" }));
      expect(await screen.findByText("Could not draw a cable from ADSR 2 to LFO 2: rate: Not connected to the server.")).toBeTruthy();
      expect(document.activeElement).toBe(picker);
      fireEvent.click(picker);
      expect(listed().map(([name]) => name)).toContain("LFO 2");
    });

    test("takes the cable out with its button, and the focus goes to the list of the modulator that drove it", async () => {
      const { requests, view } = await rig(<PatchView />, { snapshot: withCable(laneEnvelope, laneLfo, { type: "rate" }) });
      await nodeOf(laneLfo);
      fireEvent.click(screen.getByRole("button", { name: "Remove cable ADSR 2 → LFO 2: rate" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "removeEdge", edge: 50 }]);
      });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(cables);
      });
      expect((await nodeOf(laneLfo)).querySelectorAll(".tang-port--driven")).toHaveLength(0);
      await waitFor(() => {
        expect(document.activeElement?.id).toBe(drivesId(laneEnvelope));
      });
    });
  });

  describe("taking a modulator or a cable out", () => {
    test("is offered on every modulator and every effect, by a button that names it, and on no other node", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      const names = (await screen.findAllByRole("button", { name: /^Remove / })).map((button) => button.getAttribute("aria-label")).filter((name) => !name?.startsWith("Remove cable"));
      // The fixture's modulators: two LFOs of lanes and one of a group (numbered), two envelopes, a control; and its effects: three filters (one on a bus), two reverbs (one on a bus).
      expect(names.sort()).toEqual(
        ["Remove ADSR 1", "Remove ADSR 2", "Remove LFO 1", "Remove LFO 2", "Remove LFO 3", "Remove MIDI (learning…)", "Remove Filter 1", "Remove Filter 2", "Remove Filter 3", "Remove Reverb 1", "Remove Reverb 2"].sort(),
      );
      // None on a lane, a group or the master: they cannot be taken out from here yet.
      for (const id of [0, 2, 4, 7, 14, MASTER]) expect(within(await nodeOf(id)).queryByRole("button", { name: /^Remove / })).toBeNull();
    });

    test("takes an effect out when its button is pressed, with the cable that drives it, and the focus goes to the button that adds one to its lane", async () => {
      const { requests, view } = await rig(<PatchView />);
      fireEvent.click(within(await nodeOf(filter.id)).getByRole("button", { name: "Remove Filter" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "removeNode", node: filter.id }]);
      });
      await waitFor(() => {
        expect(view.container.querySelector(`[data-id="${String(filter.id)}"]`)).toBeNull();
      });
      // The LFO has nothing to drive now; the audio goes from the oscillator to the master.
      expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(0);
      expect(view.container.querySelectorAll(".tang-cable--audio")).toHaveLength(1);
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an effect to Oscillator" }));
      });
    });

    test("takes a bus effect out, and the focus goes to the button that adds one to its group", async () => {
      const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      fireEvent.click(within(await nodeOf(3)).getByRole("button", { name: "Remove Reverb 1" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "removeNode", node: 3 }]);
      });
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an effect to Group 2" }));
      });
    });

    test("says why when an effect could not be taken out, and it stays", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      fireEvent.click(within(await nodeOf(filter.id)).getByRole("button", { name: "Remove Filter" }));
      expect(await screen.findByText("Could not remove Filter: Not connected to the server.")).toBeTruthy();
      expect(await nodeOf(filter.id)).toBeTruthy();
    });

    test("takes a modulator out when its button is pressed, with its cable, and the focus goes to the button that adds one to its lane", async () => {
      const { requests, view } = await rig(<PatchView />);
      fireEvent.click(within(await nodeOf(lfo.id)).getByRole("button", { name: "Remove LFO" }));
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "removeNode", node: lfo.id }]);
      });
      await waitFor(() => {
        expect(view.container.querySelector(`[data-id="${String(lfo.id)}"]`)).toBeNull();
      });
      expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(0);
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add a modulator to Oscillator" }));
      });
    });

    test("says why when the modulator could not be taken out, and it stays", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      fireEvent.click(within(await nodeOf(lfo.id)).getByRole("button", { name: "Remove LFO" }));
      expect(await screen.findByText("Could not remove LFO: Not connected to the server.")).toBeTruthy();
      expect(await nodeOf(lfo.id)).toBeTruthy();
    });

    test("has a button on each cable that takes it out, which names the cable", async () => {
      const { requests, view } = await rig(<PatchView />);
      const button = await screen.findByRole("button", { name: "Remove cable LFO → Filter: cutoff" });
      fireEvent.click(button);
      await waitFor(() => {
        expect(requests().map((request) => request.intent)).toEqual([{ type: "removeEdge", edge: 0 }]);
      });
      await waitFor(() => {
        expect(view.container.querySelectorAll(".tang-cable--modulation")).toHaveLength(0);
      });
      // The modulator stays, and there is no depth to turn.
      expect(await nodeOf(lfo.id)).toBeTruthy();
      expect(screen.queryByRole("slider", { name: /^Depth of/ })).toBeNull();
    });

    test("gives the focus to the list of the modulator that drove the cable, where the next cable is drawn from, and not to the page", async () => {
      await rig(<PatchView />);
      const button = await screen.findByRole("button", { name: "Remove cable LFO → Filter: cutoff" });
      button.focus();
      fireEvent.click(button);
      await waitFor(() => {
        expect(screen.queryByRole("button", { name: /^Remove cable/ })).toBeNull();
      });
      await waitFor(() => {
        expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add a cable from LFO" }));
      });
    });

    test("gives it to the list of the modulator of that cable, whichever modulators there are", async () => {
      const snapshot: Snapshot = structuredClone(tracerSnapshot);
      snapshot.graph.nodes.push({ id: 90, parent: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "square", rate: 7 } });
      snapshot.graph.edges.push({ kind: "modulation", id: 1, amount: 0.3, from: { node: 90, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 0 } } });
      // (A second cable of the second modulator, so that it still has one cable when the first is taken out.)
      snapshot.graph.edges.push({ kind: "modulation", id: 2, amount: 0.3, from: { node: 90, port: { type: "modOut" } }, to: { node: oscillator.id, port: { type: "param", index: 1 } } });
      await rig(<PatchView />, { snapshot });
      for (const [cable, picker] of [
        ["Remove cable LFO 2 → Filter: cutoff", "Add a cable from LFO 2"],
        ["Remove cable LFO 1 → Filter: cutoff", "Add a cable from LFO 1"],
      ] as const) {
        fireEvent.click(await screen.findByRole("button", { name: cable }));
        await waitFor(() => {
          expect(document.activeElement).toBe(screen.getByRole("button", { name: picker }));
        });
      }
    });

    test("leaves the focus where it was when the cable could not be taken out, and says why", async () => {
      function Board() {
        return <NoticeBoard notices={useNotices()} />;
      }
      const { server, client } = await rig(
        <>
          <PatchView />
          <Board />
        </>,
      );
      const button = await screen.findByRole("button", { name: "Remove cable LFO → Filter: cutoff" });
      server.drop();
      await waitFor(() => {
        expect(client.state.status).not.toBe("open");
      });
      button.focus();
      fireEvent.click(button);
      expect(await screen.findByText(/^Could not take out the cable LFO → Filter: cutoff: /)).toBeTruthy();
      expect(document.activeElement).toBe(button);
    });

    test("names two cables of two modulators of one name apart", async () => {
      const snapshot: Snapshot = structuredClone(tracerSnapshot);
      snapshot.graph.nodes.push({ id: 90, parent: 0, name: "LFO", kind: "modulator", source: { type: "lfo", waveform: "square", rate: 7 } });
      snapshot.graph.edges.push({ kind: "modulation", id: 1, amount: 0.3, from: { node: 90, port: { type: "modOut" } }, to: { node: filter.id, port: { type: "param", index: 0 } } });
      await rig(<PatchView />, { snapshot });
      expect(await screen.findByRole("button", { name: "Remove cable LFO 1 → Filter: cutoff" })).toBeTruthy();
      expect(screen.getByRole("button", { name: "Remove cable LFO 2 → Filter: cutoff" })).toBeTruthy();
      expect(screen.getByRole("button", { name: "Remove LFO 2" })).toBeTruthy();
    });
  });

  test("gives the master a level meter with a clip light", async () => {
    await rig(<PatchView />);
    const master = await nodeOf(4294967295);
    expect(within(master).getByRole("meter", { name: "Master level" }).getAttribute("aria-valuemin")).toBe("-60");
    expect(within(master).getByText("CLIP")).toBeTruthy();
  });
});

describe("a patch with a group", () => {
  /** The tracer's lane in a group, with the group between the filter and the master. */
  function grouped(): Snapshot {
    const snapshot: Snapshot = structuredClone(tracerSnapshot);
    snapshot.graph.nodes.unshift({ id: 77, parent: null, name: "Pad", kind: "group", volume: 0.8, pan: -0.25 });
    for (const node of snapshot.graph.nodes) if (node.id === oscillator.id) node.parent = 77;
    snapshot.graph.edges = snapshot.graph.edges.flatMap((edge) => {
      if (edge.kind === "audio" && edge.to.node === MASTER) {
        return [
          { kind: "member" as const, from: edge.from, to: { node: 77, port: { type: "audioIn" as const } } },
          { kind: "audio" as const, from: { node: 77, port: { type: "audioOut" as const } }, to: edge.to },
        ];
      }
      return [edge];
    });
    return snapshot;
  }

  test("has the group's volume and pan on its node, with ports for the lanes that feed it and the master", async () => {
    const { view } = await rig(<PatchView />, { snapshot: grouped() });
    const group = await nodeOf(77);
    expect(group.classList.contains("react-flow__node-group")).toBe(true);
    expect(within(group).getByText("Pad")).toBeTruthy();
    expect(within(group).getByText("Group", { selector: ".tang-node__badge" })).toBeTruthy();
    expect(within(group).getByRole("slider", { name: "Volume" }).getAttribute("aria-valuenow")).toBe("0.8");
    expect(within(group).getByRole("slider", { name: "Pan" }).getAttribute("aria-valuenow")).toBe("-0.25");
    expect(Array.from(group.querySelectorAll(".react-flow__handle")).map((handle) => handle.getAttribute("data-handleid"))).toEqual(["audioIn", "audioOut"]);
    // oscillator → filter, filter → group, group → master, and the modulation.
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__edge")).toHaveLength(4);
    });
  });
});

describe("the master's meter", () => {
  test("lights the clip light when the count of clipped buffers goes up, from the telemetry", async () => {
    // The mock makes no telemetry here (its period is long), so that the frames are the test's own.
    const { client, clock, view } = await rig(<PatchView />, { telemetryPeriod: 3_600_000 });
    const { layout } = { layout: client.state.snapshot?.telemetry };
    if (!layout) throw new Error("no layout");
    const frame = (clips: number): ArrayBuffer => {
      const values = layout.meters.map((entry) => (entry.kind === "clips" ? clips : 0.25));
      const buffer = new ArrayBuffer(8 + 4 * values.length);
      const data = new DataView(buffer);
      data.setUint16(2, values.length, true);
      data.setUint32(4, layout.rev, true);
      values.forEach((value, i) => {
        data.setFloat32(8 + 4 * i, value, true);
      });
      return buffer;
    };
    const accept = (clips: number) => {
      const decoded = decodeFrame(frame(clips));
      if (decoded) client.telemetry.accept(decoded);
    };
    const light = view.container.querySelector(".tang-clip");
    accept(0);
    act(() => {
      clock.tick(0);
    });
    expect(light?.hasAttribute("data-lit")).toBe(false);
    accept(1);
    act(() => {
      clock.tick(100);
    });
    expect(light?.hasAttribute("data-lit")).toBe(true);
    act(() => {
      clock.tick(2200);
    });
    expect(light?.hasAttribute("data-lit")).toBe(false);
    // And the level meter shows the 0.25 of full scale the frames carried: about -12 dB.
    expect(Number(within(await nodeOf(MASTER)).getByRole("meter").getAttribute("aria-valuenow"))).toBeCloseTo(-12.04, 1);
  });
});

describe("whether the canvas is the same", () => {
  test("it is when both have no session, or when what differs is only values", () => {
    expect(sameGraphStructure(null, null)).toBe(true);
    const changed = structuredClone(tracerSnapshot.graph);
    for (const node of changed.nodes) if (node.kind === "instrument") node.volume = 0.5;
    expect(sameGraphStructure(tracerSnapshot.graph, changed)).toBe(true);
  });

  test("it is not when one has a session and the other has not, or they differ in structure", () => {
    expect(sameGraphStructure(null, tracerSnapshot.graph)).toBe(false);
    expect(sameGraphStructure(tracerSnapshot.graph, null)).toBe(false);
    expect(sameGraphStructure(tracerSnapshot.graph, { nodes: tracerSnapshot.graph.nodes.slice(1), edges: tracerSnapshot.graph.edges })).toBe(false);
  });
});

describe("the patch while it plays", () => {
  test("has the cutoff's dot and ring where the LFO takes it, the LFO's bar where it is, and the master's level", async () => {
    const { client, clock, time } = await rig(<PatchView />);
    const cutoff = within(await nodeOf(filter.id)).getByRole("slider", { name: "cutoff" }).closest(".tang-knob");
    const bar = within(await nodeOf(lfo.id)).getByRole("meter");
    const level = within(await nodeOf(4294967295)).getByRole("meter");
    expect(cutoff?.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.1, 0.9));
    time.seconds = 0.5; // the LFO is at its peak
    client.noteOn(60); // and a note is held
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      const dot = cutoff?.querySelector(".tang-knob__dot");
      expect(dot?.getAttribute("visibility")).toBe("visible");
      expect(Number(dot?.getAttribute("cx"))).toBeCloseTo(pointAt(24, 18, angleOf(0.9)).x, 3);
      expect(bar.getAttribute("aria-valuenow")).toBe("1");
      expect(level.getAttribute("aria-valuenow")).not.toBeNull();
    });
    const fill = (element: Element) => element.querySelector<HTMLElement>(".tang-livebar__fill")?.style;
    expect(Number(fill(level)?.getPropertyValue("--to"))).toBeGreaterThan(0.7); // 0.35 of full scale is about -9 dB
  });

  test("draws what the LFO puts out as a trace on its node's canvas, at the frame, and a dot at the newest value", async () => {
    const context = recordingContext();
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation((() => context) as never);
    const { clock, time } = await rig(<PatchView />);
    const node = await nodeOf(lfo.id);
    const canvas = node.querySelector("canvas");
    if (!canvas) throw new Error("the LFO's node has no canvas");
    canvas.style.width = "280px";
    canvas.style.height = "44px";
    time.seconds = 0.5; // the LFO is at its peak
    await waitFor(() => {
      context.calls.length = 0;
      act(() => {
        clock.tick(performance.now());
      });
      // (The newest value is at the top of the 44 px less the room above it, once the engine's frame with the LFO at its peak is in.)
      expect(callsOf(context, "arc")[0]?.[1]).toBe(3);
    });
    // And at the right of the 280 px, with the mark across the middle drawn.
    expect(callsOf(context, "arc")[0]?.[0]).toBe(277);
    expect(callsOf(context, "moveTo")[0]).toEqual([0, 22.5]);
  });

  test("follows a parameter somebody else changes without making the canvas again", async () => {
    const { server } = await rig(<PatchView />);
    const fil = await nodeOf(filter.id);
    const nodes = Array.from(document.querySelectorAll(".react-flow__node"));
    const cutoff = within(fil).getByRole("slider", { name: "cutoff" });
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.8 });
    });
    await waitFor(() => {
      expect(cutoff.getAttribute("aria-valuenow")).toBe("0.8");
    });
    // The same elements, not new ones: nothing was laid out again.
    expect(Array.from(document.querySelectorAll(".react-flow__node"))).toEqual(nodes);
  });

  test("is laid out again when the session changes in structure: a node comes", async () => {
    const { client, view } = await rig(<PatchView />);
    await screen.findByTestId(`rf__node-${String(filter.id)}`);
    expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(4);
    const bigger: Snapshot = structuredClone(tracerSnapshot);
    bigger.rev = 1;
    bigger.graph.nodes.splice(2, 0, { id: 88, parent: 0, name: "Second filter", kind: "effect", plugin: structuredClone(filterPlugin), mix: 1 });
    act(() => {
      client.store.setState({ snapshot: bigger });
    });
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(5);
    });
    expect(within(await nodeOf(88)).getByText("Second filter")).toBeTruthy();
  });

  test("has nothing on it before there is a session", async () => {
    const { view, client } = await rig(<PatchView />);
    act(() => {
      client.store.setState({ snapshot: null });
    });
    await waitFor(() => {
      expect(view.container.querySelectorAll(".react-flow__node")).toHaveLength(0);
    });
  });
});
