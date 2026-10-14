// @vitest-environment jsdom
import { ReactFlow, ReactFlowProvider } from "@xyflow/react";
import type { NodeTypes } from "@xyflow/react";
import { cleanup, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { fieldsFixture } from "../../client/fixtures";
import { rig } from "../testing";
import { ConnectingContext } from "./connection";
import { ModulatorSettingCell, ParamCell } from "./controls";

afterEach(cleanup);

// What a modulator's node and a plugin's put a cable's port in: a cell of a control. A canvas of one node that has some of them, in a session
// that has the nodes they are of (the oscillator 3, the lane's LFO 4 and envelope 5 of the fixture).
const NODE_TYPES: NodeTypes = {
  cells: () => (
    <div>
      <ParamCell node={3} index={1} driven={false} />
      <ParamCell node={3} index={2} driven />
      <ModulatorSettingCell node={4} field="rate" driven={false} />
      <ModulatorSettingCell node={4} field="waveform" driven={false} />
      <ModulatorSettingCell node={5} field="attack" driven />
      <ModulatorSettingCell node={5} field="decay" driven={false} />
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
  test("is a port of a parameter, by the plugin's own index, or of a modulator's setting by its name, and a waveform has none", async () => {
    const { ports } = await cells(null);
    expect(ports.map((port) => port.getAttribute("data-handleid"))).toEqual(["param-1", "param-2", "rate", "attack", "decay"]);
    for (const port of ports) {
      expect(port.getAttribute("data-handlepos"), port.getAttribute("data-handleid") ?? "?").toBe("top");
      expect(port.classList.contains("tang-port--param")).toBe(true);
      expect(port.classList.contains("connectablestart")).toBe(false);
    }
  });

  test("says that a cable ends at it when it is driven, and not otherwise", async () => {
    const { ports } = await cells(null);
    expect(ports.filter((port) => port.classList.contains("tang-port--driven")).map((port) => port.getAttribute("data-handleid"))).toEqual(["param-2", "attack"]);
  });

  test("says that it takes the cable that is dragged when the node and the handle are among those that do, and no other does", async () => {
    const { ports } = await cells(new Set(["3:param-1", "4:rate", "5:decay", "5:release", "6:attack"]));
    expect(ports.filter((port) => port.classList.contains("tang-port--takes")).map((port) => port.getAttribute("data-handleid"))).toEqual(["param-1", "rate", "decay"]);
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
    const { ports } = await cells(new Set(["13:rate", "40:attack", "30:param-1"]));
    expect(ports.some((port) => port.classList.contains("tang-port--takes"))).toBe(false);
  });
});
