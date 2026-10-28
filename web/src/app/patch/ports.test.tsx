// @vitest-environment jsdom
import { ReactFlow, ReactFlowProvider } from "@xyflow/react";
import type { NodeProps, NodeTypes } from "@xyflow/react";
import { cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { fieldsFixture } from "../../client/fixtures";
import { rig } from "../testing";
import { ConnectingContext } from "./connection";
import { AudioPort, DepthPort, ModulatorSettingCell, NotesPort, ParamCell } from "./controls";

afterEach(cleanup);

// What a modulator's node and a plugin's put a cable's port in: a cell of a control, or a row of the cables of a modulator. A canvas of one node that has
// some of them, in a session that has the nodes they are of (the oscillator 3, the lane's LFO 4 and envelope 5 of the fixture).
const NODE_TYPES: NodeTypes = {
  cells: () => (
    <div>
      <ParamCell node={3} index={1} driven={false} />
      <ParamCell node={3} index={2} driven />
      <ModulatorSettingCell node={4} field="rate" driven={false} />
      <ModulatorSettingCell node={4} field="waveform" driven={false} />
      <ModulatorSettingCell node={5} field="attack" driven />
      <ModulatorSettingCell node={5} field="decay" driven={false} />
      <DepthPort node={4} edge={0} driven={false} />
      <DepthPort node={5} edge={1} driven />
    </div>
  ),
};

async function cells(connecting: ReadonlySet<string> | null) {
  const { view } = await rig(
    <ReactFlowProvider>
      <ConnectingContext.Provider value={connecting}>
        <div style={{ width: 800, height: 600 }}>
          <ReactFlow nodes={[{ id: "1", type: "cells", position: { x: 0, y: 0 }, data: {} }]} edges={[]} nodeTypes={NODE_TYPES} />
        </div>
      </ConnectingContext.Provider>
    </ReactFlowProvider>,
    { snapshot: fieldsFixture.snapshot },
  );
  await screen.findByTestId("rf__node-1");
  const ports = Array.from(view.container.querySelectorAll(".react-flow__handle"));
  const named = (id: string) => ports.find((port) => port.getAttribute("data-handleid") === id);
  return { ports, named };
}

describe("the port of a control", () => {
  test("is a port of a parameter, by the plugin's own index, or of a modulator's setting by its name, or of a cable's depth by the id of the cable, and a waveform has none", async () => {
    const { ports } = await cells(null);
    expect(ports.map((port) => port.getAttribute("data-handleid"))).toEqual(["param-1", "param-2", "rate", "attack", "decay", "depth-0", "depth-1"]);
    for (const port of ports) {
      const id = port.getAttribute("data-handleid") ?? "?";
      // (The ports of controls are on their top edges; those of the rows of a modulator's cables are on the node's left edge.)
      expect(port.getAttribute("data-handlepos"), id).toBe(id.startsWith("depth-") ? "left" : "top");
      expect(port.classList.contains(id.startsWith("depth-") ? "tang-port--cable" : "tang-port--param"), id).toBe(true);
      // A cable ends there, and is not started there.
      expect(port.classList.contains("connectablestart"), id).toBe(false);
      expect(port.classList.contains("connectableend"), id).toBe(true);
    }
  });

  test("says that a cable ends at it when it is driven, and not otherwise", async () => {
    const { ports } = await cells(null);
    expect(ports.filter((port) => port.classList.contains("tang-port--driven")).map((port) => port.getAttribute("data-handleid"))).toEqual(["param-2", "attack", "depth-1"]);
  });

  test("says that it takes the cable that is dragged when the node and the handle are among those that do, and no other does", async () => {
    const { ports } = await cells(new Set(["3:param-1", "4:rate", "5:decay", "5:release", "6:attack", "4:depth-0", "4:depth-1", "5:depth-0"]));
    expect(ports.filter((port) => port.classList.contains("tang-port--takes")).map((port) => port.getAttribute("data-handleid"))).toEqual(["param-1", "rate", "decay", "depth-0"]);
  });

  test("says nothing of the sort while no cable is dragged", async () => {
    const { ports } = await cells(null);
    expect(ports.some((port) => port.classList.contains("tang-port--takes"))).toBe(false);
  });

  test("says nothing of the sort when a cable is dragged that nothing takes", async () => {
    const { ports } = await cells(new Set());
    expect(ports.some((port) => port.classList.contains("tang-port--takes"))).toBe(false);
  });

  test("does not take for a node what is another node's: the key is of the node and of the handle", async () => {
    const { ports } = await cells(new Set(["13:rate", "40:attack", "30:param-1", "14:depth-0", "50:depth-1"]));
    expect(ports.some((port) => port.classList.contains("tang-port--takes"))).toBe(false);
  });
});

describe("the audio ports of a node", () => {
  // Two nodes, each with the input and the output of audio that a node of the canvas has (the session is the one a node reads its own values from, and these read none).
  const AUDIO_TYPES: NodeTypes = {
    audio: ({ data }: NodeProps) => (
      <div>
        <AudioPort node={data.node as number} direction="in" />
        <AudioPort node={data.node as number} direction="out" />
      </div>
    ),
  };

  async function audioPorts(connecting: ReadonlySet<string> | null) {
    const { view } = await rig(
      <ReactFlowProvider>
        <ConnectingContext.Provider value={connecting}>
          <div style={{ width: 800, height: 600 }}>
            <ReactFlow
              nodes={[
                { id: "3", type: "audio", position: { x: 0, y: 0 }, data: { node: 3 } },
                { id: "4", type: "audio", position: { x: 300, y: 0 }, data: { node: 4 } },
              ]}
              edges={[]}
              nodeTypes={AUDIO_TYPES}
            />
          </div>
        </ConnectingContext.Provider>
      </ReactFlowProvider>,
      { snapshot: fieldsFixture.snapshot },
    );
    await screen.findByTestId("rf__node-4");
    const ports = Array.from(view.container.querySelectorAll(".react-flow__handle"));
    const named = (node: number, handle: string) => ports.find((port) => port.getAttribute("data-nodeid") === String(node) && port.getAttribute("data-handleid") === handle);
    return { ports, named };
  }

  test("is an output where a cable is started and is not where one ends, on the right edge, and an input where one ends and is not where one starts, on the left", async () => {
    const { ports, named } = await audioPorts(null);
    expect(ports.map((port) => `${port.getAttribute("data-nodeid") ?? "?"}:${port.getAttribute("data-handleid") ?? "?"}`)).toEqual(["3:audioIn", "3:audioOut", "4:audioIn", "4:audioOut"]);
    for (const node of [3, 4]) {
      const output = named(node, "audioOut");
      expect([output?.getAttribute("data-handlepos"), output?.classList.contains("source"), output?.classList.contains("connectablestart"), output?.classList.contains("connectableend")], `output of ${String(node)}`).toEqual(["right", true, true, false]);
      const input = named(node, "audioIn");
      expect([input?.getAttribute("data-handlepos"), input?.classList.contains("target"), input?.classList.contains("connectablestart"), input?.classList.contains("connectableend")], `input of ${String(node)}`).toEqual(["left", true, false, true]);
      // (The stylesheet has them by the direction: the output says so to the pointer, and the input that takes a cable grows to a strip.)
      expect([output?.classList.contains("tang-port--out"), output?.classList.contains("tang-port--in")], `output of ${String(node)}`).toEqual([true, false]);
      expect([input?.classList.contains("tang-port--in"), input?.classList.contains("tang-port--out")], `input of ${String(node)}`).toEqual([true, false]);
    }
    for (const port of ports) expect(port.classList.contains("tang-port--audio"), port.getAttribute("data-handleid") ?? "?").toBe(true);
  });

  test("says that it takes the cable that is dragged when its node is among those that do, an input only, and no other does", async () => {
    // (Node 4 has the key of its output in the set, which is no place that audio ends: a cable that is dragged ends at inputs, so that it is not the input of node 4 that takes it, nor node 3's output.)
    const { ports, named } = await audioPorts(new Set(["3:audioIn", "4:param-1", "4:audioOut"]));
    expect(ports.filter((port) => port.classList.contains("tang-port--takes")).map((port) => `${port.getAttribute("data-nodeid") ?? "?"}:${port.getAttribute("data-handleid") ?? "?"}`)).toEqual(["3:audioIn"]);
    expect(named(3, "audioOut")?.classList.contains("tang-port--takes")).toBe(false);
    expect(named(4, "audioIn")?.classList.contains("tang-port--takes")).toBe(false);
    expect(named(4, "audioOut")?.classList.contains("tang-port--takes")).toBe(false);
  });

  test("says nothing of the sort while no cable is dragged, or one that nothing takes, or for the key of another node's input", async () => {
    for (const connecting of [null, new Set<string>(), new Set(["30:audioIn", "13:audioIn", "5:audioIn"])]) {
      const { ports } = await audioPorts(connecting);
      expect(ports.some((port) => port.classList.contains("tang-port--takes"))).toBe(false);
      cleanup();
    }
  });
});

describe("the two audio inputs of an effect that has a second one", () => {
  // A node with the two inputs, named as the Math's plugin names them, and its output; and a node with the one input that every other effect has.
  const PAIR_TYPES: NodeTypes = {
    pair: ({ data }: NodeProps) => (
      <div>
        <AudioPort node={data.node as number} direction="in" pair={{ side: false, label: "A" }} />
        <AudioPort node={data.node as number} direction="in" pair={{ side: true, label: "B" }} />
        <AudioPort node={data.node as number} direction="out" />
      </div>
    ),
    single: ({ data }: NodeProps) => (
      <div>
        <AudioPort node={data.node as number} direction="in" />
        <AudioPort node={data.node as number} direction="out" />
      </div>
    ),
  };

  async function pairPorts(connecting: ReadonlySet<string> | null) {
    const { view } = await rig(
      <ReactFlowProvider>
        <ConnectingContext.Provider value={connecting}>
          <div style={{ width: 800, height: 600 }}>
            <ReactFlow
              nodes={[
                { id: "3", type: "pair", position: { x: 0, y: 0 }, data: { node: 3 } },
                { id: "4", type: "single", position: { x: 300, y: 0 }, data: { node: 4 } },
              ]}
              edges={[]}
              nodeTypes={PAIR_TYPES}
            />
          </div>
        </ConnectingContext.Provider>
      </ReactFlowProvider>,
      { snapshot: fieldsFixture.snapshot },
    );
    await screen.findByTestId("rf__node-4");
    const ports = Array.from(view.container.querySelectorAll(".react-flow__handle"));
    const named = (node: number, handle: string) => ports.find((port) => port.getAttribute("data-nodeid") === String(node) && port.getAttribute("data-handleid") === handle);
    return { ports, named, view };
  }

  test("are two inputs on the left edge, the first named as an input is and the second after it, each a place where a cable ends and none starts, one above the other by the stylesheet", async () => {
    const { ports, named } = await pairPorts(null);
    expect(ports.map((port) => `${port.getAttribute("data-nodeid") ?? "?"}:${port.getAttribute("data-handleid") ?? "?"}`)).toEqual(["3:audioIn", "3:audioInSide", "3:audioOut", "4:audioIn", "4:audioOut"]);
    for (const handle of ["audioIn", "audioInSide"]) {
      const input = named(3, handle);
      expect([input?.getAttribute("data-handlepos"), input?.classList.contains("target"), input?.classList.contains("connectablestart"), input?.classList.contains("connectableend")], handle).toEqual(["left", true, false, true]);
      expect(input?.classList.contains("tang-port--audio") && input.classList.contains("tang-port--in"), handle).toBe(true);
    }
    expect(named(3, "audioIn")?.classList.contains("tang-port--pair-main")).toBe(true);
    expect(named(3, "audioIn")?.classList.contains("tang-port--pair-side")).toBe(false);
    expect(named(3, "audioInSide")?.classList.contains("tang-port--pair-side")).toBe(true);
    expect(named(3, "audioInSide")?.classList.contains("tang-port--pair-main")).toBe(false);
    // (An effect that has one input has the place that it had, with no word beside it.)
    expect(named(4, "audioIn")?.classList.contains("tang-port--pair-main")).toBe(false);
    expect(named(4, "audioIn")?.classList.contains("tang-port--pair-side")).toBe(false);
  });

  test("have their names written beside them, for the eye and not for a screen reader (the handle is what has the cable), and the effect with one input has none", async () => {
    const { view } = await pairPorts(null);
    const labels = Array.from(view.container.querySelectorAll(".tang-port-label"));
    expect(labels.map((label) => [label.textContent, label.classList.contains("tang-port-label--main"), label.classList.contains("tang-port-label--side"), label.getAttribute("aria-hidden")])).toEqual([
      ["A", true, false, "true"],
      ["B", false, true, "true"],
    ]);
  });

  test("say which of them takes the cable that is dragged: the key of the node and of the handle, and the one input does not take what is for the other", async () => {
    const second = await pairPorts(new Set(["3:audioInSide", "4:audioOut"]));
    expect(second.ports.filter((port) => port.classList.contains("tang-port--takes")).map((port) => port.getAttribute("data-handleid"))).toEqual(["audioInSide"]);
    cleanup();
    const first = await pairPorts(new Set(["3:audioIn", "4:audioInSide"]));
    expect(first.ports.filter((port) => port.classList.contains("tang-port--takes")).map((port) => `${port.getAttribute("data-nodeid") ?? "?"}:${port.getAttribute("data-handleid") ?? "?"}`)).toEqual(["3:audioIn"]);
    cleanup();
    const neither = await pairPorts(new Set(["4:audioIn", "30:audioInSide"]));
    expect(neither.named(3, "audioIn")?.classList.contains("tang-port--takes")).toBe(false);
    expect(neither.named(3, "audioInSide")?.classList.contains("tang-port--takes")).toBe(false);
    expect(neither.named(4, "audioIn")?.classList.contains("tang-port--takes")).toBe(true);
  });
});

describe("the notes ports of a node", () => {
  // Two nodes, each with the input and the output of events that a lane and a source of notes have between them (these read no values of the session).
  const NOTES_TYPES: NodeTypes = {
    notes: ({ data }: NodeProps) => (
      <div>
        <NotesPort node={data.node as number} direction="in" />
        <NotesPort node={data.node as number} direction="out" />
      </div>
    ),
  };

  async function notesPorts(connecting: ReadonlySet<string> | null) {
    const { view } = await rig(
      <ReactFlowProvider>
        <ConnectingContext.Provider value={connecting}>
          <div style={{ width: 800, height: 600 }}>
            <ReactFlow
              nodes={[
                { id: "3", type: "notes", position: { x: 0, y: 0 }, data: { node: 3 } },
                { id: "4", type: "notes", position: { x: 300, y: 0 }, data: { node: 4 } },
              ]}
              edges={[]}
              nodeTypes={NOTES_TYPES}
            />
          </div>
        </ConnectingContext.Provider>
      </ReactFlowProvider>,
      { snapshot: fieldsFixture.snapshot },
    );
    await screen.findByTestId("rf__node-4");
    const ports = Array.from(view.container.querySelectorAll(".react-flow__handle"));
    const named = (node: number, handle: string) => ports.find((port) => port.getAttribute("data-nodeid") === String(node) && port.getAttribute("data-handleid") === handle);
    return { ports, named };
  }

  test("is an output where a cable is started and is not where one ends, on the right edge, and an input where one ends and is not where one starts, on the left", async () => {
    const { ports, named } = await notesPorts(null);
    expect(ports.map((port) => `${port.getAttribute("data-nodeid") ?? "?"}:${port.getAttribute("data-handleid") ?? "?"}`)).toEqual(["3:eventsIn", "3:eventsOut", "4:eventsIn", "4:eventsOut"]);
    for (const node of [3, 4]) {
      const output = named(node, "eventsOut");
      expect([output?.getAttribute("data-handlepos"), output?.classList.contains("source"), output?.classList.contains("connectablestart"), output?.classList.contains("connectableend")], `output of ${String(node)}`).toEqual(["right", true, true, false]);
      const input = named(node, "eventsIn");
      expect([input?.getAttribute("data-handlepos"), input?.classList.contains("target"), input?.classList.contains("connectablestart"), input?.classList.contains("connectableend")], `input of ${String(node)}`).toEqual(["left", true, false, true]);
      // (The stylesheet has them by the direction: the output says so to the pointer, and the input that takes a cable grows to a strip.)
      expect([output?.classList.contains("tang-port--out"), output?.classList.contains("tang-port--in")], `output of ${String(node)}`).toEqual([true, false]);
      expect([input?.classList.contains("tang-port--in"), input?.classList.contains("tang-port--out")], `input of ${String(node)}`).toEqual([true, false]);
    }
    for (const port of ports) {
      expect(port.classList.contains("tang-port--notes"), port.getAttribute("data-handleid") ?? "?").toBe(true);
      expect(port.classList.contains("tang-port--audio"), port.getAttribute("data-handleid") ?? "?").toBe(false);
    }
  });

  test("says that it takes the cable that is dragged when its node is among those that do, an input only, and no other does", async () => {
    // (Node 4 has the key of its output in the set, which is no place that notes end: a cable that is dragged ends at inputs, so that it is not the input of node 4 that takes it, nor node 3's output; and the
    // key of an input of audio is not the key of an input of events.)
    const { ports, named } = await notesPorts(new Set(["3:eventsIn", "4:param-1", "4:eventsOut", "4:audioIn"]));
    expect(ports.filter((port) => port.classList.contains("tang-port--takes")).map((port) => `${port.getAttribute("data-nodeid") ?? "?"}:${port.getAttribute("data-handleid") ?? "?"}`)).toEqual(["3:eventsIn"]);
    expect(named(3, "eventsOut")?.classList.contains("tang-port--takes")).toBe(false);
    expect(named(4, "eventsIn")?.classList.contains("tang-port--takes")).toBe(false);
    expect(named(4, "eventsOut")?.classList.contains("tang-port--takes")).toBe(false);
  });

  test("says nothing of the sort while no cable is dragged, or one that nothing takes, or for the key of another node's input", async () => {
    for (const connecting of [null, new Set<string>(), new Set(["30:eventsIn", "13:eventsIn", "5:eventsIn"])]) {
      const { ports } = await notesPorts(connecting);
      expect(ports.some((port) => port.classList.contains("tang-port--takes"))).toBe(false);
      cleanup();
    }
  });
});
