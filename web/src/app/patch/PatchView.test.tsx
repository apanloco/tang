// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { MASTER, nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import { decodeFrame } from "../../client/telemetry";
import type { Snapshot } from "../../protocol";
import { angleOf, arcPath, pointAt } from "../../ui/knob-geometry";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
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

  test("shows each plugin node's parameters as controls, and its own gain after them", async () => {
    await rig(<PatchView />);
    expect(controlsIn(await nodeOf(oscillator.id))).toEqual(["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "Volume"]);
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

  test("shows the modulator for what it is, with a bar for what it puts out", async () => {
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

  test("cannot be rearranged or selected, and takes a cable only at a modulator's output (to start one) and at a parameter's port (to end one)", async () => {
    const { view } = await rig(<PatchView />);
    await nodeOf(filter.id);
    for (const node of Array.from(view.container.querySelectorAll(".react-flow__node"))) {
      expect(node.classList.contains("draggable")).toBe(false);
      expect(node.classList.contains("selectable")).toBe(false);
    }
    const connectable = Array.from(view.container.querySelectorAll(".react-flow__handle.connectable"));
    const ports = connectable.map((handle) => handle.getAttribute("data-handleid") ?? "?");
    expect(ports.filter((port) => port === "modOut")).toHaveLength(1);
    expect(ports.filter((port) => port.startsWith("param-"))).toHaveLength(12); // the oscillator's eight and the filter's four
    expect(ports).toHaveLength(13); // and no audio port: the audio path is derived, there is nothing to draw
    // A cable is started at a modulator's output and ends at a parameter, never the other way.
    const output = view.container.querySelector(".react-flow__handle[data-handleid='modOut']");
    expect(output?.classList.contains("connectablestart")).toBe(true);
    for (const handle of connectable.filter((h) => h.getAttribute("data-handleid")?.startsWith("param-"))) {
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

  describe("drawing a cable to a parameter, from a list", () => {
    /** The button of the list of the modulator `id`. */
    const pickerOf = async (id: number): Promise<HTMLButtonElement> => {
      const node = await nodeOf(id);
      return within(node).getByRole<HTMLButtonElement>("button", { name: /^Add a cable from / });
    };

    /** The parameters on the list that is open, by the plugin they are of (a node of the canvas is a group too). */
    const listed = (): [string | null, string[]][] =>
      within(screen.getByRole("menu"))
        .getAllByRole("group")
        .map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);

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

    test("is for a group's modulator the parameters of its members' plugins and of its own bus effects", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      fireEvent.click(await pickerOf(15));
      // (In the order of the graph: the group's own bus effect, then the lanes that are its members.)
      expect(listed().map(([plugin]) => plugin)).toEqual(["Filter 1", "Oscillator 1", "Filter 2", "Oscillator 2"]);
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

  describe("taking a modulator or a cable out", () => {
    test("is offered on every modulator, by a button that names it, and on no other node", async () => {
      await rig(<PatchView />, { snapshot: reachFixture.snapshot });
      const names = (await screen.findAllByRole("button", { name: /^Remove / })).map((button) => button.getAttribute("aria-label")).filter((name) => !name?.startsWith("Remove cable"));
      // The fixture's modulators: two LFOs of lanes and one of a group (numbered), two envelopes, a control.
      expect(names.sort()).toEqual(["Remove ADSR 1", "Remove ADSR 2", "Remove LFO 1", "Remove LFO 2", "Remove LFO 3", "Remove MIDI (learning…)"].sort());
      for (const id of [0, 1, 4, 14, MASTER]) expect(within(await nodeOf(id)).queryByRole("button", { name: /^Remove / })).toBeNull();
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
    expect([fill(bar)?.getPropertyValue("--from"), fill(bar)?.getPropertyValue("--to")]).toEqual(["0.5", "1"]);
    expect(Number(fill(level)?.getPropertyValue("--to"))).toBeGreaterThan(0.7); // 0.35 of full scale is about -9 dB
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
