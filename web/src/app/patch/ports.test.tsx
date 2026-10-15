// @vitest-environment jsdom
import { ReactFlow, ReactFlowProvider } from "@xyflow/react";
import type { NodeTypes } from "@xyflow/react";
import { cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { fieldsFixture } from "../../client/fixtures";
import { rig } from "../testing";
import { ConnectingContext } from "./connection";
import { DepthPort, ModulatorSettingCell, ParamCell } from "./controls";

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
