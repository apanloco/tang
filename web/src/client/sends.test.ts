import { describe, expect, test } from "vitest";

import type { Graph } from "../protocol";
import { MASTER, audioCablesFixture, tracerSnapshot } from "./fixtures";
import { sendItems } from "./sends";

// The session of the audio cables fixture: group 0 ("Pad") with Filter 1 (node 1), Reverb (2) and Delay 1 (3) on its bus; lane 4 ("Oscillator 1") in it with Filter 2 (5) and Delay 2 (6) and an LFO (7);
// lane 8 ("Oscillator 2") with Drive (9); an empty lane (10). The audio cables: 8 to Filter 2, Filter 2 to Delay 2, the group to Drive, Filter 1 to Reverb, Delay 2 to the group's bus, Reverb to the mix.
const { graph } = audioCablesFixture.snapshot;
const words = (items: ReturnType<typeof sendItems>): [string | undefined, string][] => items.map((item) => [item.group, item.label]);

describe("the places that a cable of audio can be sent to, as the items of a menu", () => {
  test("are the effects, then the buses of the groups, then the mix, each under what it is, by the names the canvas gives them and in the order the canvas has them", () => {
    // From the second lane: not Filter 2, where a cable of its own goes already, and not a lane, which has no input.
    expect(words(sendItems(graph, 8))).toEqual([
      ["Effects", "Filter 1"],
      ["Effects", "Reverb"],
      ["Effects", "Delay 1"],
      ["Effects", "Delay 2"],
      ["Effects", "Drive"],
      ["Group buses", "Pad"],
      ["Mix", "Master"],
    ]);
  });

  test("are named by the id of the node, as text, which is what the menu says was chosen", () => {
    expect(sendItems(graph, 8).map((item) => item.id)).toEqual(["1", "2", "3", "6", "9", "0", String(MASTER)]);
  });

  test("leave out where the audio would go round, and where a cable is drawn already: the group's output can go to the mix and nowhere else", () => {
    expect(words(sendItems(graph, 0))).toEqual([["Mix", "Master"]]);
    // (Filter 1's cable goes to the reverb already, and its way into the delay after it is the chain's own: a cable beside the link is allowed. The group's bus, the effects before it and
    // those of the lane that is in the group are where the audio would come back to it.)
    expect(words(sendItems(graph, 1)).map(([, label]) => label)).toEqual(["Delay 1", "Drive", "Master"]);
  });

  test("are for the nodes that have an output of audio and nobody else: not the mix, a modulator, a node that is not there", () => {
    expect(sendItems(graph, MASTER)).toEqual([]);
    expect(sendItems(graph, 7)).toEqual([]);
    expect(sendItems(graph, 77)).toEqual([]);
  });

  test("are the filter and the mix for the tracer's oscillator, and the mix alone for its filter", () => {
    expect(words(sendItems(tracerSnapshot.graph, 0))).toEqual([
      ["Effects", "Filter"],
      ["Mix", "Master"],
    ]);
    expect(words(sendItems(tracerSnapshot.graph, 1))).toEqual([["Mix", "Master"]]);
  });

  test("are none for a node that every place of is taken", () => {
    const full: Graph = {
      ...tracerSnapshot.graph,
      edges: [...tracerSnapshot.graph.edges, { kind: "cable", id: 31, gain: 1, from: { node: 1, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } } }],
    };
    expect(sendItems(full, 1)).toEqual([]);
  });
});
