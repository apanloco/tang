// @vitest-environment jsdom
// The level of a lane and of a group, on its card (docs/web-ui.md step 100): a meter along the top edge of the card, from the peak that the engine publishes for it.
import { act, cleanup, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { groupsFixture, MASTER } from "../../client/fixtures";
import { decodeFrame } from "../../client/telemetry";
import type { NodeId } from "../../protocol";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// The session of the groups fixture: three groups (0, 1 and 2), four lanes (3 and 7 in the first group, 6 in the second, 4 in none, which has a filter, 5), an effect in two of them (5 and 8), modulators 9 to 15 and the master.
// The canvas numbers the nodes called alike in the order of the graph: the lanes are Oscillator 1 to 4.
const snapshot = groupsFixture.snapshot;
const LANES = [3, 4, 6, 7];
const GROUPS = [0, 1, 2];
const EFFECTS = [5, 8];

const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);
const levels = (card: HTMLElement): HTMLElement[] => within(card).queryAllByRole("meter", { name: /^Level of / });

describe("the level of a lane and of a group", () => {
  test("is a meter on the card of each lane and each group, named for it and on a scale of decibels from -60 to 0, along the top edge of the card", async () => {
    await rig(<PatchView />, { snapshot });
    for (const id of [...LANES, ...GROUPS]) {
      const card = await nodeOf(id);
      const [level, ...others] = levels(card);
      expect(level, `node ${String(id)} has a level`).toBeDefined();
      expect(others, `and one only`).toHaveLength(0);
      expect(level?.getAttribute("aria-valuemin")).toBe("-60");
      expect(level?.getAttribute("aria-valuemax")).toBe("0");
      expect(level?.closest(".tang-node__level"), "it is the strip that is drawn over the top stripe of the card").not.toBeNull();
    }
    expect(levels(await nodeOf(LANES[0] ?? 0))[0]?.getAttribute("aria-label")).toBe("Level of Oscillator 1");
    expect(levels(await nodeOf(LANES[3] ?? 0))[0]?.getAttribute("aria-label")).toBe("Level of Oscillator 4");
  });

  test("is on no other node: not an effect, a modulator or the master, which has a meter of its own", async () => {
    await rig(<PatchView />, { snapshot });
    const modulators = snapshot.graph.nodes.filter((node) => node.kind === "modulator").map((node) => node.id);
    for (const id of [...EFFECTS, ...modulators, MASTER]) {
      expect(levels(await nodeOf(id)), `node ${String(id)} has none`).toHaveLength(0);
    }
    expect(within(await nodeOf(MASTER)).getByRole("meter", { name: "Master level" })).toBeDefined();
  });

  test("follows the peak that the telemetry has for the node, each its own: a lane that makes 0.25 of full scale reads about -12 dB, a group that makes 0.5 about -6 dB, and silence is the floor", async () => {
    // (The mock makes no telemetry here, its period is long, so that the frames are the test's own.)
    const { client, clock } = await rig(<PatchView />, { snapshot, telemetryPeriod: 3_600_000 });
    const layout = client.state.snapshot?.telemetry;
    if (!layout) throw new Error("no layout");
    const peaks = new Map<NodeId, number>([
      [LANES[0] ?? 0, 0.25],
      [GROUPS[0] ?? 0, 0.5],
    ]);
    const values = layout.meters.map((entry) => (entry.kind === "clips" ? 0 : (peaks.get(entry.node) ?? 0)));
    const buffer = new ArrayBuffer(8 + 4 * values.length);
    const data = new DataView(buffer);
    data.setUint16(2, values.length, true);
    data.setUint32(4, layout.rev, true);
    values.forEach((value, i) => {
      data.setFloat32(8 + 4 * i, value, true);
    });
    const decoded = decodeFrame(buffer);
    if (!decoded) throw new Error("the frame does not decode");
    client.telemetry.accept(decoded);
    act(() => {
      clock.tick(0);
    });
    const reads = async (id: number): Promise<number> => Number(levels(await nodeOf(id))[0]?.getAttribute("aria-valuenow"));
    expect(await reads(LANES[0] ?? 0)).toBeCloseTo(-12.04, 1);
    expect(await reads(GROUPS[0] ?? 0)).toBeCloseTo(-6.02, 1);
    // (The others are silent, each at the floor of the scale: the slot of a lane is not the slot of its neighbour.)
    expect(await reads(LANES[1] ?? 0)).toBe(-60);
    expect(await reads(LANES[3] ?? 0)).toBe(-60);
    expect(await reads(GROUPS[1] ?? 0)).toBe(-60);
  });
});
