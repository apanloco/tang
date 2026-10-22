import { describe, expect, test } from "vitest";

import type { Edge, Endpoint, Graph, NodeId } from "../protocol";
import { MASTER, audioCablesFixture, groupsFixture, reachFixture, tracerSnapshot } from "./fixtures";
import { isLink } from "./cables";
import { cableEnds, cableLoops, cableTakers, cablesOf, effectLoops, goesRound, laneLoops, linksOf, routeLoops, wiringOf } from "./wiring";
import type { Ends } from "./wiring";

// The session of the fixture (`fixtures/audioCables.json`): group 0 ("Pad") has Filter 1, Reverb 2 and Delay 3 on its bus; lane 4 is in it and has Filter 5 and Delay 6 (and an LFO, 7); lane 8, in no group,
// has Drive 9; lane 10 is empty. The cables: 1 from lane 8 to Filter 5, 2 from Filter 5 to Delay 6, 3 from the group's output to Drive 9, 4 from Filter 1 to Reverb 2, 5 from Delay 6 to the group's
// bus and 6 from Reverb 2 to the mix.
const graph: Graph = audioCablesFixture.snapshot.graph;
const out = (node: NodeId): Endpoint => ({ node, port: { type: "audioOut" } });
const into = (node: NodeId): Endpoint => ({ node, port: { type: "audioIn" } });
const ends = (from: Ends["from"]["kind"], start: NodeId, to: Ends["to"]["kind"], end: NodeId): Ends => ({ from: { kind: from, node: start }, to: { kind: to, node: end } });

/** The graph with the cables `drawn` (as ends: an effect, a group's bus or the mix) in place of those it has. */
const withCables = (...drawn: [NodeId, NodeId][]): Graph => ({
  ...graph,
  edges: [
    ...graph.edges.filter((e) => e.kind !== "cable"),
    ...drawn.map(([from, to], i): Edge => ({ kind: "cable", id: 100 + i, gain: 1, from: out(from), to: into(to) })),
  ],
});

/** The graph with the way out of the lanes and groups `cut` cut, and the links of the audio path that go with it taken out (as the controller's graph has them). */
const withWaysOutCut = (base: Graph, ...cut: NodeId[]): Graph => {
  const nodes = base.nodes.map((n) => ((n.kind === "instrument" || n.kind === "group") && cut.includes(n.id) ? { ...n, route: "none" as const } : n));
  return { nodes, edges: [...linksOf(nodes), ...base.edges.filter((e) => !isLink(e))] };
};

describe("the wiring of a graph", () => {
  test("is its lanes, each with the effects of its chain in order and the group it is in, and its groups, each with the effects of its bus", () => {
    expect(wiringOf(graph)).toEqual({
      lanes: [
        { node: 4, effects: [5, 6], group: 0, route: "out" },
        { node: 8, effects: [9], group: null, route: "out" },
        { node: 10, effects: [], group: null, route: "out" },
      ],
      groups: [{ node: 0, effects: [1, 2, 3], route: "out" }],
    });
  });

  test("puts a lane whose group is not in the graph in no group, as the controller does for a group that is gone", () => {
    const lanes = graph.nodes.map((n) => (n.id === 8 ? { ...n, parent: 77 } : n));
    expect(wiringOf({ ...graph, nodes: lanes }).lanes[1]).toEqual({ node: 8, effects: [9], group: null, route: "out" });
  });

  test("says which way out each lane and each group has", () => {
    const cut = wiringOf(withWaysOutCut(graph, 4, 0));
    expect(cut.lanes.map((lane) => lane.route)).toEqual(["none", "out", "out"]);
    expect(cut.groups.map((group) => group.route)).toEqual(["none"]);
  });

  test("has the audio cables of the graph, each by what its start and its end are, in the order of the graph", () => {
    expect(cablesOf(graph)).toEqual([
      ends("instrument", 8, "effect", 5),
      ends("effect", 5, "effect", 6),
      ends("group", 0, "effect", 9),
      ends("effect", 1, "effect", 2),
      ends("effect", 6, "groupBus", 0),
      ends("effect", 2, "master", MASTER),
    ]);
  });

  test("leaves out a cable with a node that is not in the graph, and what is not an audio cable", () => {
    const stray: Edge[] = [
      { kind: "cable", id: 50, gain: 1, from: out(77), to: into(5) },
      { kind: "cable", id: 51, gain: 1, from: out(8), to: into(78) },
      { kind: "cable", id: 52, gain: 1, from: out(7), to: into(5) },
      { kind: "cable", id: 53, gain: 1, from: out(8), to: into(10) },
    ];
    expect(cablesOf({ ...graph, edges: [...graph.edges, ...stray] })).toEqual(cablesOf(graph));
  });
});

describe("the ends of a cable", () => {
  test("are an instrument's, an effect's or a group's output and an effect's, a group's bus or the mix's input, whichever of a lane's or a bus's the effect is in", () => {
    expect(cableEnds(graph, out(4), into(5))).toEqual(ends("instrument", 4, "effect", 5));
    expect(cableEnds(graph, out(5), into(1))).toEqual(ends("effect", 5, "effect", 1));
    expect(cableEnds(graph, out(3), into(0))).toEqual(ends("effect", 3, "groupBus", 0));
    expect(cableEnds(graph, out(0), into(MASTER))).toEqual(ends("group", 0, "master", MASTER));
    expect(cableEnds(graph, out(10), into(9))).toEqual(ends("instrument", 10, "effect", 9));
  });

  test("are refused in the controller's words and in its order: the ports, the mix as a start, a start, a modulator, an end", () => {
    const refused = (from: Endpoint, to: Endpoint): string => {
      const result = cableEnds(graph, from, to);
      return "error" in result ? `${result.error.code}: ${result.error.message}` : "taken";
    };
    expect(refused({ node: 4, port: { type: "audioIn" } }, into(5))).toBe("invalid: an audio cable starts at an output of audio");
    expect(refused(out(4), { node: 5, port: { type: "audioOut" } })).toBe("invalid: an audio cable ends at an input of audio");
    // (Both ports are wrong: the start is looked at first.)
    expect(refused({ node: 4, port: { type: "modOut" } }, { node: 5, port: { type: "rate" } })).toBe("invalid: an audio cable starts at an output of audio");
    expect(refused(out(MASTER), into(5))).toBe("invalid: the mix has no output of audio");
    expect(refused(out(77), into(5))).toBe("notFound: there is no node 77");
    expect(refused(out(7), into(5))).toBe("invalid: a modulator has no output of audio");
    expect(refused(out(4), into(78))).toBe("notFound: there is no node 78");
    expect(refused(out(4), into(10))).toBe("invalid: a lane has no input of audio: a cable ends at an effect, a group or the mix");
    expect(refused(out(4), into(7))).toBe("invalid: a modulator has no input of audio");
    // (The start is judged all the way before the end: a start that is not there is not found, whatever the end.)
    expect(refused(out(77), into(78))).toBe("notFound: there is no node 77");
    expect(refused(out(7), into(10))).toBe("invalid: a modulator has no output of audio");
  });
});

describe("whether the audio goes round", () => {
  test("is no in the wiring of the session with the cables it has", () => {
    expect(goesRound(wiringOf(graph), cablesOf(graph))).toBe(false);
    expect(goesRound(wiringOf(graph), [])).toBe(false);
  });

  test("is yes for a cable back along a chain, into the effect it starts at, and round through two cables that are none alone", () => {
    expect(cableLoops(graph, ends("effect", 6, "effect", 5))).toBe(true);
    expect(cableLoops(graph, ends("effect", 5, "effect", 5))).toBe(true);
    // (A bus effect back to the one before it; the cable of Filter 1 to Reverb 2 is there already, so the other way round closes the circle.)
    expect(cableLoops(graph, ends("effect", 2, "effect", 1))).toBe(true);
    // Drive 9 to Filter 5 goes round by cable 3 (the group's output into Drive 9) and the lane's way into its group, and the chain of the group.
    expect(cableLoops(graph, ends("effect", 9, "effect", 5))).toBe(true);
    // Two cables that are none alone: Filter 5 to Drive 9, and Drive 9 to Filter 5, with no other cable in the session.
    expect(cableLoops(withCables(), ends("effect", 5, "effect", 9))).toBe(false);
    expect(cableLoops(withCables(), ends("effect", 9, "effect", 5))).toBe(false);
    expect(cableLoops(withCables([5, 9]), ends("effect", 9, "effect", 5))).toBe(true);
  });

  test("tells the head of a group from its tail: its output cannot feed its own bus, nor a bus effect its own bus's head, and a lane that is not in it may send to its bus", () => {
    expect(cableLoops(graph, ends("group", 0, "effect", 1))).toBe(true);
    expect(cableLoops(graph, ends("group", 0, "effect", 3))).toBe(true);
    expect(cableLoops(graph, ends("group", 0, "groupBus", 0))).toBe(true);
    expect(cableLoops(graph, ends("effect", 3, "groupBus", 0))).toBe(true);
    expect(cableLoops(graph, ends("effect", 1, "groupBus", 0))).toBe(true);
    // (The head of a lane that is not in the group, and the head of one that is, may send into the bus: it is where they go anyway.)
    expect(cableLoops(graph, ends("instrument", 8, "groupBus", 0))).toBe(false);
    expect(cableLoops(graph, ends("instrument", 4, "groupBus", 0))).toBe(false);
    expect(cableLoops(graph, ends("instrument", 4, "effect", 2))).toBe(false);
    // The group's output into a lane that is in the group feeds itself; into a lane that is not, it does not.
    expect(cableLoops(graph, ends("group", 0, "effect", 5))).toBe(true);
    expect(cableLoops(graph, ends("group", 0, "master", MASTER))).toBe(false);
  });

  test("is no for a cable from the mix's side of the wiring: whatever ends at the mix does not come back", () => {
    for (const start of [4, 8, 10]) expect(cableLoops(graph, ends("instrument", start, "master", MASTER)), `lane ${String(start)}`).toBe(false);
    expect(cableLoops(graph, ends("effect", 9, "master", MASTER))).toBe(false);
  });
});

describe("whether the audio would go round if a lane were put in a group or an effect moved", () => {
  test("a lane put in a group whose output a cable takes into the lane would feed itself: lane 8, into Drive 9, and not the others", () => {
    expect(laneLoops(graph, 8, 0)).toBe(true);
    expect(laneLoops(graph, 10, 0)).toBe(false);
    expect(laneLoops(graph, 4, 0)).toBe(false);
    // Out of its group, or put in no group, a lane cannot make the audio go round, with these cables (and one that is in the group already is as it is).
    expect(laneLoops(graph, 4, null)).toBe(false);
    expect(laneLoops(graph, 8, null)).toBe(false);
  });

  test("an effect moved along its chain loops when a cable beside the chain's link would run the other way: Filter 5 to the end, Delay 6 to the start, and the same on the bus", () => {
    expect(effectLoops(graph, 5, 1)).toBe(true);
    expect(effectLoops(graph, 6, 0)).toBe(true);
    expect(effectLoops(graph, 1, 1)).toBe(true);
    expect(effectLoops(graph, 1, 2)).toBe(true);
    expect(effectLoops(graph, 2, 0)).toBe(true);
    // (Delay 3 to the front keeps Filter 1 before Reverb 2, and Reverb 2 to the end keeps it too.)
    expect(effectLoops(graph, 3, 0)).toBe(false);
    expect(effectLoops(graph, 3, 1)).toBe(false);
    expect(effectLoops(graph, 2, 2)).toBe(false);
  });

  test("an effect that is not moved, a place the chain has not and a node that is no effect do not loop", () => {
    expect(effectLoops(graph, 5, 0)).toBe(false);
    expect(effectLoops(graph, 9, 0)).toBe(false);
    expect(effectLoops(graph, 5, 2)).toBe(false);
    expect(effectLoops(graph, 5, -1)).toBe(false);
    expect(effectLoops(graph, 4, 0)).toBe(false);
    expect(effectLoops(graph, 77, 0)).toBe(false);
  });
});

describe("the links of the audio path that the nodes of a graph have", () => {
  test("are those that the controller lists, in its order, for every session of the fixtures: the groups' chains, then the lanes', each ending in the mix or in its group", () => {
    for (const [name, snapshot] of Object.entries({ audioCables: audioCablesFixture.snapshot, tracer: tracerSnapshot, groups: groupsFixture.snapshot, reach: reachFixture.snapshot })) {
      expect(linksOf(snapshot.graph.nodes), name).toEqual(snapshot.graph.edges.filter(isLink));
    }
  });

  test("end a chain whose way out is cut at its last node: no link leaves the tail of a lane or a group that goes nowhere", () => {
    const links = (cut: NodeId[]): string[] => linksOf(withWaysOutCut(graph, ...cut).nodes).map((e) => `${String(e.from.node)}>${String(e.to.node)} ${e.kind}`);
    const all = links([]);
    // The fixture: group 0 has Filter 1, Reverb 2 and Delay 3 and goes to the mix; lane 4 (in it) has Filter 5 and Delay 6 and goes to the group; lane 8 has Drive 9 and lane 10 has nothing, and go to the mix.
    expect(all).toEqual(["0>1 audio", "1>2 audio", "2>3 audio", "3>4294967295 audio", "4>5 audio", "5>6 audio", "6>0 member", "8>9 audio", "9>4294967295 audio", "10>4294967295 audio"]);
    expect(links([0])).toEqual(all.filter((link) => link !== "3>4294967295 audio"));
    expect(links([4])).toEqual(all.filter((link) => link !== "6>0 member"));
    expect(links([8])).toEqual(all.filter((link) => link !== "9>4294967295 audio"));
    expect(links([10])).toEqual(all.filter((link) => link !== "10>4294967295 audio"));
    expect(links([0, 4, 8, 10])).toEqual(["0>1 audio", "1>2 audio", "2>3 audio", "4>5 audio", "5>6 audio", "8>9 audio"]);
  });

  test("leave out the links that would go to a master that the nodes do not have", () => {
    const nodes = graph.nodes.filter((n) => n.kind !== "master");
    expect(linksOf(nodes).some((e) => e.to.node === MASTER)).toBe(false);
    expect(linksOf(nodes).some((e) => e.kind === "member")).toBe(true);
  });
});

describe("whether the audio would go round if a way out were given back", () => {
  // The group's output into Filter 5, a lane that is in the group: the lane feeds the group, which feeds the lane, unless the lane's way out is cut.
  const looped = (...cut: NodeId[]): Graph => withWaysOutCut(withCables([0, 5]), ...cut);

  test("a lane whose way out is cut may be given it back unless a cable would then feed it from what it feeds", () => {
    expect(routeLoops(looped(4), 4, "out")).toBe(true);
    expect(routeLoops(looped(4), 4, "none")).toBe(false);
    // (Lane 8 is in no group: its way out goes to the mix, which feeds nothing.)
    expect(routeLoops(looped(4, 8), 8, "out")).toBe(false);
    expect(routeLoops(looped(4, 10), 10, "out")).toBe(false);
  });

  test("a group's way out can not close a loop: it goes to the mix, which has no output", () => {
    // (Lane 4 is cut too, so that the cable into it makes no loop of the session as it is.)
    expect(routeLoops(looped(0, 4), 0, "out")).toBe(false);
    expect(routeLoops(withWaysOutCut(graph, 0), 0, "out")).toBe(false);
  });

  test("a node that is no lane or group makes no loop, and the session as it is does not go round either", () => {
    expect(routeLoops(looped(4), 5, "out")).toBe(false);
    expect(routeLoops(graph, 77, "out")).toBe(false);
    expect(routeLoops(graph, MASTER, "out")).toBe(false);
  });

  test("the audio goes round in the wiring of a lane that feeds its group, through the cable, and not when the lane has no way out", () => {
    expect(goesRound(wiringOf(withWaysOutCut(withCables([0, 5]))), cablesOf(withCables([0, 5])))).toBe(true);
    expect(goesRound(wiringOf(withWaysOutCut(withCables([0, 5]), 4)), cablesOf(withCables([0, 5])))).toBe(false);
  });
});

describe("the nodes that take a cable from an output", () => {
  test("are the effects, the buses and the mix that the controller would draw it to: not where a cable between the two is already, and not where the audio would go round", () => {
    // Lane 8: the cable to Filter 5 is drawn already, and every other end is free.
    expect(cableTakers(graph, 8)).toEqual([0, 1, 2, 3, 6, 9, MASTER]);
    // The group's output feeds Drive 9 already, and what it comes from it cannot feed: only the mix is left.
    expect(cableTakers(graph, 0)).toEqual([MASTER]);
    // A lane's instrument may send beside its chain's own link, to the effects of the bus and to the group, and to the mix.
    expect(cableTakers(graph, 4)).toEqual([0, 1, 2, 3, 5, 6, 9, MASTER]);
  });

  test("are those of the controller's AddCable, for every start of the fixture: the ends that it takes with no level of its own are the takers", () => {
    const starts = graph.nodes.filter((n) => n.kind === "instrument" || n.kind === "effect" || n.kind === "group").map((n) => n.id);
    const taken = (from: NodeId): NodeId[] =>
      audioCablesFixture.rows.flatMap((row) =>
        row.intent.type === "addCable" && row.intent.gain === null && row.intent.from.node === from && row.intent.from.port.type === "audioOut" && row.intent.to.port.type === "audioIn" && !("error" in row)
          ? [row.intent.to.node]
          : [],
      );
    const byId = (a: NodeId, b: NodeId): number => a - b;
    for (const start of starts) {
      expect([...cableTakers(graph, start)].sort(byId), `from ${String(start)}`).toEqual(taken(start).sort(byId));
    }
  });

  test("are none for what has no output of audio: a modulator, the mix and a node that is not there", () => {
    expect(cableTakers(graph, 7)).toEqual([]);
    expect(cableTakers(graph, MASTER)).toEqual([]);
    expect(cableTakers(graph, 77)).toEqual([]);
  });
});
