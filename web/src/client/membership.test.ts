import { describe, expect, test } from "vitest";

import type { Edge } from "../protocol";
import { groupsFixture } from "./fixtures";
import { cablesIntoLaneFromItsGroup } from "./membership";

// The session of the groups fixture (`fixtures/groups.json`): group 0 has an LFO (13) and an envelope (14), and two lanes, 3 and 10; group 1 has an LFO
// (15) and the lane 9; lane 5 is in no group, and has modulators of its own. The cables: 0 from the LFO of group 0 to a parameter of lane 3, 1 to a
// parameter of lane 10, 2 to the rate of the LFO of lane 10 (12), 3 from the envelope to the depth of cable 1, and 4 from the LFO of group 1 to lane 9.
describe("the cables of a group that go into one of its lanes", () => {
  const { graph } = groupsFixture.snapshot;

  test("are those from its modulators to a plugin of the lane, and to the settings of the lane's modulators", () => {
    expect(cablesIntoLaneFromItsGroup(graph, 3)).toEqual([0]);
    expect(cablesIntoLaneFromItsGroup(graph, 10)).toEqual([1, 2]);
    expect(cablesIntoLaneFromItsGroup(graph, 9)).toEqual([4]);
  });

  test("are none for a lane that is in no group, though its own modulators have cables", () => {
    expect(cablesIntoLaneFromItsGroup(graph, 5)).toEqual([]);
  });

  test("are none for a lane in no group, whatever a modulator that belongs to nothing drives in it", () => {
    // (No session has one: a lane that is in no group has no group to drive it, and the rule says so.)
    const nodes = graph.nodes.map((n) => (n.id === 7 ? { ...n, parent: null } : n));
    const edges: Edge[] = [...graph.edges, { from: { node: 7, port: { type: "modOut" } }, to: { node: 6, port: { type: "param", index: 1 } }, kind: "modulation", id: 99, amount: 0.5 }];
    expect(cablesIntoLaneFromItsGroup({ nodes, edges }, 5)).toEqual([]);
  });

  test("are none for what is no lane, and for what is not there", () => {
    expect(cablesIntoLaneFromItsGroup(graph, 13)).toEqual([]);
    expect(cablesIntoLaneFromItsGroup(graph, 0)).toEqual([]);
    expect(cablesIntoLaneFromItsGroup(graph, 9_999)).toEqual([]);
  });

  test("leave out a cable that drives the depth of one of them, which ends at the group's modulator and not in the lane", () => {
    expect(cablesIntoLaneFromItsGroup(graph, 10)).not.toContain(3);
  });
});
