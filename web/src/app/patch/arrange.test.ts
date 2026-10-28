import { describe, expect, test } from "vitest";

import { GAP_X, GAP_Y, GRID, MARGIN, OVER_RATE, anchorOf, arrange, measure } from "./arrange";
import type { Block, Wire } from "./arrange";

const WIDTH = 304;

const block = (id: number, x: number, y: number, height = 120): Block => ({ id, x, y, width: WIDTH, height });
const audio = (from: number, to: number): Wire => ({ from, fromHandle: "audioOut", to, toHandle: "audioIn", weight: 1 });
const modulation = (from: number, to: number, handle = "param-3"): Wire => ({ from, fromHandle: "modOut", to, toHandle: handle, weight: 1 });
const notes = (from: number, to: number): Wire => ({ from, fromHandle: "eventsOut", to, toHandle: "eventsIn", weight: 1 });

/** The blocks of `placed` where `places` puts them. */
const moved = (blocks: readonly Block[], places: ReadonlyMap<number, { x: number; y: number }>): Block[] => blocks.map((b) => ({ ...b, ...(places.get(b.id) ?? {}) }));
const at = (blocks: readonly Block[], id: number): Block => {
  const found = blocks.find((b) => b.id === id);
  if (!found) throw new Error(`no block ${String(id)}`);
  return found;
};
const middleY = (b: Block): number => b.y + b.height / 2;
const centreX = (b: Block): number => b.x + b.width / 2;

describe("where a cable is joined to a node", () => {
  test("audio and notes leave by the middle of the right edge and arrive by the middle of the left edge, and the second input of an effect is lower", () => {
    expect(anchorOf("audioOut")).toEqual({ fx: 1, fy: 0.5, side: "right" });
    expect(anchorOf("eventsOut")).toEqual({ fx: 1, fy: 0.5, side: "right" });
    expect(anchorOf("audioIn")).toEqual({ fx: 0, fy: 0.5, side: "left" });
    expect(anchorOf("eventsIn")).toEqual({ fx: 0, fy: 0.5, side: "left" });
    expect(anchorOf("audioInSide")).toEqual({ fx: 0, fy: 2 / 3, side: "left" });
    // (The first of an effect's two inputs is at a third of the height: `wiresOf` says which an effect has.)
    expect(anchorOf("audioIn:pair")).toEqual({ fx: 0, fy: 1 / 3, side: "left" });
  });

  test("a modulator's output is at the middle of its bottom edge, and what a cable of modulation ends at is the middle of the top edge, whichever knob it is", () => {
    expect(anchorOf("modOut")).toEqual({ fx: 0.5, fy: 1, side: "bottom" });
    for (const handle of ["param-0", "param-17", "volume", "mix", "pan", "rate", "attack", "release", "depth-4"]) expect(anchorOf(handle), handle).toEqual({ fx: 0.5, fy: 0, side: "top" });
  });
});

describe("what an arrangement is worth", () => {
  test("a cable of audio that has its room in front of its start is as long as that room, and is not behind", () => {
    const blocks = [block(1, 0, 0), block(2, WIDTH + GAP_X, 0)];
    const worth = measure(blocks, [audio(1, 2)]);
    expect(worth.length).toBeCloseTo(GAP_X, 6);
    expect({ ...worth, length: 0 }).toEqual({ length: 0, short: 0, over: 0, inside: 0, backwards: 0, crowded: 0, width: 2 * WIDTH + GAP_X, height: 120 });
  });

  test("a cable whose end is behind its start, or has less room than it should, is counted; one that is level is as long as the line, and one that goes up or down is longer, as the curve that the page draws is", () => {
    // The end is level with the start and 20 pixels short of the room.
    const short = measure([block(1, 0, 0), block(2, WIDTH + GAP_X - 20, 0)], [audio(1, 2)]);
    expect(short.backwards).toBe(1);
    expect(short.length).toBeCloseTo(GAP_X - 20, 6);
    // The end is to the left of the start: the curve goes out and comes back, which is longer than the line between the two places.
    const behind = measure([block(1, 400, 0), block(2, 0, 300)], [audio(1, 2)]);
    expect(behind.backwards).toBe(1);
    expect(behind.length).toBeGreaterThan(Math.sqrt((400 + WIDTH) ** 2 + 300 ** 2));
    // The end is GAP_X in front and 100 under: an S, a little longer than the line and shorter than going along and down.
    const sloped = measure([block(1, 0, 0), block(2, WIDTH + GAP_X, 100)], [audio(1, 2)]).length;
    expect(sloped).toBeGreaterThan(Math.sqrt(GAP_X ** 2 + 100 ** 2));
    expect(sloped).toBeLessThan(GAP_X + 100);
  });

  test("a cable of modulation wants its end GAP_Y under the bottom of its modulator, in line with its middle", () => {
    const fine = measure([block(1, 0, 0, 100), block(2, 0, 100 + GAP_Y)], [modulation(1, 2)]);
    expect(fine.length).toBeCloseTo(GAP_Y, 6);
    expect(fine.backwards).toBe(0);
    // Above it, the cable has to go back up.
    expect(measure([block(1, 0, 400, 100), block(2, 0, 0)], [modulation(1, 2)]).backwards).toBe(1);
  });

  test("what the cables are short of their room is counted in pixels, each as much as it matters, and a cable that has its room is short of none", () => {
    expect(measure([block(1, 0, 0), block(2, WIDTH + GAP_X, 0)], [audio(1, 2)]).short).toBe(0);
    const blocks = [block(1, 0, 0), block(2, WIDTH + GAP_X - 20, 0)];
    expect(measure(blocks, [audio(1, 2)]).short).toBeCloseTo(20, 6);
    expect(measure(blocks, [{ ...audio(1, 2), weight: 0.25 }]).short).toBeCloseTo(5, 6);
  });

  test("a cable that comes into the slack round the box of a node is over it by what it comes in, as much as it matters, and passes over it only when that is more than a graze", () => {
    // The cable from 1 to 2 is level and ends 5 pixels inside the box of 3, which is made 4 pixels larger on every side.
    const blocks = [block(1, 0, 0), block(2, WIDTH + GAP_X, 0), block(3, WIDTH + GAP_X - 1, 20, 100)];
    const grazed = measure(blocks, [audio(1, 2)]);
    expect(grazed.inside).toBeCloseTo(5, 6);
    expect(grazed.over).toBe(0);
    expect(measure(blocks, [{ ...audio(1, 2), weight: 0.25 }]).inside).toBeCloseTo(1.25, 6);
    // A few pixels more and it is over the node.
    expect(measure([...blocks.slice(0, 2), block(3, WIDTH + GAP_X - 6, 20, 100)], [audio(1, 2)])).toMatchObject({ over: 1 });
  });

  test("a cable that passes over a node that it does not join is counted, and one that goes round it is not", () => {
    const blocks = [block(1, 0, 0), block(2, 2 * (WIDTH + GAP_X), 0), block(3, WIDTH + GAP_X, 0)];
    expect(measure(blocks, [audio(1, 2)]).over).toBe(1);
    expect(measure([...blocks.slice(0, 2), block(3, WIDTH + GAP_X, 400)], [audio(1, 2)]).over).toBe(0);
    // The nodes at its own ends are not over.
    expect(measure(blocks.slice(0, 2), [audio(1, 2)]).over).toBe(0);
  });

  test("two nodes nearer to each other than the margin are crowded, and two that are the margin apart are not", () => {
    expect(measure([block(1, 0, 0), block(2, WIDTH + MARGIN - 1, 0)], []).crowded).toBe(1);
    expect(measure([block(1, 0, 0), block(2, WIDTH + MARGIN, 0)], []).crowded).toBe(0);
    expect(measure([block(1, 0, 0), block(2, 100, 50)], []).crowded).toBe(1);
  });

  test("the box that holds the nodes is said by its width and its height, and nothing of nothing", () => {
    expect(measure([block(1, 10, 20, 50), block(2, 400, 100, 60)], [])).toMatchObject({ width: 400 + WIDTH - 10, height: 100 + 60 - 20 });
    expect(measure([], [])).toMatchObject({ length: 0, width: 0, height: 0 });
  });

  test("a cable to a node that is not there, and one from a node to itself, are not counted", () => {
    expect(measure([block(1, 0, 0), block(2, 500, 0)], [audio(1, 9), audio(2, 2)]).length).toBe(0);
  });
});

describe("arranging the nodes", () => {
  test("a session of fewer than two nodes, or of nodes that no cable joins, is left as it is", () => {
    const one = [block(1, 40, 56)];
    expect([...arrange(one, [])]).toEqual([[1, { x: 40, y: 56 }]]);
    const apart = [block(1, 0, 0), block(2, 900, 700)];
    expect([...arrange(apart, [])]).toEqual([
      [1, { x: 0, y: 0 }],
      [2, { x: 900, y: 700 }],
    ]);
    expect(arrange([], [])).toEqual(new Map());
  });

  test("two nodes that a cable of audio joins come to be in a line with the room that the cable has: the end GAP_X after the start's edge, the middles level", () => {
    const blocks = [block(1, 0, 0, 200), block(2, 1500, 900, 120)];
    const arranged = moved(blocks, arrange(blocks, [audio(1, 2)]));
    expect(at(arranged, 2).x - (at(arranged, 1).x + WIDTH)).toBe(GAP_X);
    expect(Math.abs(middleY(at(arranged, 2)) - middleY(at(arranged, 1)))).toBeLessThanOrEqual(GRID / 2);
    expect(measure(arranged, [audio(1, 2)]).length).toBeLessThan(measure(blocks, [audio(1, 2)]).length / 5);
  });

  test("a modulator comes to be above what it drives, GAP_Y over its top, in line with its middle", () => {
    const blocks = [block(1, 0, 0, 200), block(2, 1200, 800, 100)];
    const arranged = moved(blocks, arrange(blocks, [modulation(2, 1)]));
    // (The places are on a grid: the room is GAP_Y to within half a step of it.)
    expect(Math.abs(at(arranged, 1).y - (at(arranged, 2).y + 100) - GAP_Y)).toBeLessThanOrEqual(GRID / 2);
    expect(Math.abs(centreX(at(arranged, 1)) - centreX(at(arranged, 2)))).toBeLessThanOrEqual(GRID / 2);
  });

  test("a chain of effects is a row with the room of a cable between each, and a source of notes is before the lane that it feeds", () => {
    const blocks = [block(1, 0, 0), block(2, 0, 504), block(3, 600, 1200), block(4, 96, 2000), block(5, 1800, 40)];
    const wires = [notes(5, 1), audio(1, 2), audio(2, 3), audio(3, 4)];
    const arranged = moved(blocks, arrange(blocks, wires));
    for (const [from, to] of [
      [1, 2],
      [2, 3],
      [3, 4],
    ] as const) expect(at(arranged, to).x - (at(arranged, from).x + WIDTH), `${String(from)} to ${String(to)}`).toBe(GAP_X);
    expect(at(arranged, 1).x - (at(arranged, 5).x + WIDTH)).toBe(GAP_X);
    expect(measure(arranged, wires)).toMatchObject({ backwards: 0, over: 0, crowded: 0 });
  });

  test("the ends of lanes of different lengths come to the master together: the shorter lane is put nearer to it, and not left at the start", () => {
    // A lane of three nodes and a lane of one, level at the left, and a master at the right of the longer.
    const blocks = [block(1, 0, 0), block(2, 376, 0), block(3, 752, 0), block(4, 0, 300), block(5, 1128, 100, 228)];
    const wires = [audio(1, 2), audio(2, 3), audio(3, 5), audio(4, 5)];
    const arranged = moved(blocks, arrange(blocks, wires));
    // Both ends are GAP_X in front of the master's edge, level with its middle or nearly (the lane of three is in a line with the master).
    expect(at(arranged, 5).x - (at(arranged, 3).x + WIDTH)).toBe(GAP_X);
    expect(at(arranged, 5).x - (at(arranged, 4).x + WIDTH)).toBe(GAP_X);
    expect(measure(arranged, wires).length).toBeLessThan(measure(blocks, wires).length);
    expect(measure(arranged, wires).crowded).toBe(0);
  });

  test("a modulator that drives knobs of two lanes comes to be between them in a column, over one and not over a node of the other", () => {
    const blocks = [block(1, 0, 0, 200), block(2, 0, 700, 200), block(3, 900, 340, 100)];
    const wires = [modulation(3, 1), modulation(3, 2)];
    const arranged = moved(blocks, arrange(blocks, wires));
    const before = measure(blocks, wires);
    const after = measure(arranged, wires);
    expect(after.length).toBeLessThanOrEqual(before.length);
    expect(after).toMatchObject({ over: 0, crowded: 0 });
  });

  test("a node is not left on a cable that it has nothing to do with: it is put aside, and the cable is straight", () => {
    // 1 feeds 3 and 2 stands between them, level, with nothing joined to it but a faint link to 4.
    const blocks = [block(1, 0, 0), block(2, WIDTH + GAP_X, 0), block(3, 2 * (WIDTH + GAP_X), 0), block(4, 3000, 3000)];
    const wires = [audio(1, 3), { ...audio(4, 2), weight: 0.25 }];
    const arranged = moved(blocks, arrange(blocks, wires));
    expect(measure(arranged, wires).over).toBe(0);
    expect(measure(arranged, wires).crowded).toBe(0);
  });

  test("the nodes that are fixed stay where they are, and the others are arranged round them", () => {
    const blocks = [block(1, 0, 0), block(2, 1500, 900), block(3, 700, 300)];
    const wires = [audio(1, 2), audio(2, 3)];
    const arranged = moved(blocks, arrange(blocks, wires, new Set([1, 3])));
    expect(at(arranged, 1)).toEqual(at(blocks, 1));
    expect(at(arranged, 3)).toEqual(at(blocks, 3));
    expect(measure(arranged, wires).crowded).toBe(0);
  });

  test("the nodes that are fixed are not moved by the whole being brought back to the corner that the nodes came in at: the others are arranged round them, wherever that is", () => {
    const blocks = [block(1, 1000, 304), block(2, 1500, 0)];
    const arranged = moved(blocks, arrange(blocks, [audio(2, 1)], new Set([1])));
    expect(at(arranged, 1)).toEqual(at(blocks, 1));
    // (The other is before it, with the room of a cable, and level: at the left of where the nodes came in.)
    expect(at(arranged, 2)).toMatchObject({ x: 1000 - GAP_X - WIDTH, y: 304 });
  });

  test("a cable that matters less gives way to one that matters more: a node between a cable of weight 1 and one of weight 0.02 is put at the room of the first, though that is far from the second", () => {
    const blocks = [block(1, 0, 0), block(2, 800, 0), block(3, 1500, 0)];
    const wires = [audio(1, 2), { ...audio(2, 3), weight: 0.02 }];
    const arranged = moved(blocks, arrange(blocks, wires, new Set([1, 3])));
    expect(at(arranged, 2)).toMatchObject({ x: WIDTH + GAP_X, y: 0 });
  });

  test("a node that a cable of little weight joins is not moved far for it: it is drawn to where it was by more than the cable draws it, and a cable that matters draws it to its room", () => {
    const blocks = [block(1, 0, 0), block(2, 600, 0)];
    expect(at(moved(blocks, arrange(blocks, [{ ...audio(1, 2), weight: 0.02 }], new Set([1]))), 2)).toMatchObject({ x: 600, y: 0 });
    expect(at(moved(blocks, arrange(blocks, [audio(1, 2)], new Set([1]))), 2)).toMatchObject({ x: WIDTH + GAP_X, y: 0 });
  });

  test("a node that no cable is at is drawn in even when the cables are as short as they can be already", () => {
    const blocks = [block(1, 0, 0), block(2, WIDTH + GAP_X, 0), block(3, 3000, 2000)];
    const wires = [audio(1, 2)];
    const arranged = moved(blocks, arrange(blocks, wires));
    expect(at(arranged, 1)).toEqual(at(blocks, 1));
    expect(at(arranged, 2)).toEqual(at(blocks, 2));
    expect(measure(arranged, wires)).toMatchObject({ crowded: 0, over: 0 });
    expect(measure(arranged, wires).width).toBeLessThan(measure(blocks, wires).width / 2);
  });

  test("a node that no cable is at is drawn to the middle of the others and is not left far from them, and one that has a cable is not drawn to it", () => {
    // Two nodes that a cable joins, and a third that nothing joins, far away.
    const blocks = [block(1, 0, 0), block(2, 1000, 600), block(3, 4000, 3000)];
    const wires = [audio(1, 2)];
    const arranged = moved(blocks, arrange(blocks, wires));
    const before = measure(blocks, wires);
    const after = measure(arranged, wires);
    expect(after.crowded).toBe(0);
    expect(after.width).toBeLessThan(before.width / 2);
    expect(after.height).toBeLessThan(before.height / 2);
    // (It is next to them, with the margin, and not on them.)
    const gap = Math.min(...[1, 2].map((id) => Math.hypot(at(arranged, 3).x - at(arranged, id).x, at(arranged, 3).y - at(arranged, id).y)));
    expect(gap).toBeLessThan(2 * WIDTH);
  });

  test("a node is moved with the modulator that is held to it by a cable that has just its room, so that the two are not pulled apart by the cable of the node's audio", () => {
    // A modulator exactly GAP_Y above a lane, whose audio goes to a master far below: the lane has to go down, and the modulator with it.
    const blocks = [block(1, 0, 0, 200), block(2, 0, -(100 + GAP_Y), 100), block(3, 1200, 900, 100)];
    const wires = [audio(1, 3), modulation(2, 1)];
    // (The master stays where it is, so that the lane is what moves to it: alone it could not, and with the modulator it can.)
    const arranged = moved(blocks, arrange(blocks, wires, new Set([3])));
    expect(Math.abs(middleY(at(arranged, 1)) - middleY(at(arranged, 3)))).toBeLessThanOrEqual(GRID);
    expect(Math.abs(at(arranged, 1).y - (at(arranged, 2).y + 100) - GAP_Y)).toBeLessThanOrEqual(GRID / 2);
    expect(measure(arranged, wires)).toMatchObject({ backwards: 0, over: 0, crowded: 0 });
  });

  test("a cable that would pass over a node is taken round it, though that is longer: crossing a node costs for every pixel of it", () => {
    // 1 feeds 3 in a line, with 2 between them, and room above for 2 to be put out of the way.
    const blocks = [block(1, 0, 0), block(2, WIDTH + GAP_X, 0, 120), block(3, 2 * (WIDTH + GAP_X), 0)];
    const wires = [audio(1, 3), { ...audio(1, 2), weight: 0.05 }, { ...audio(2, 3), weight: 0.05 }];
    const arranged = moved(blocks, arrange(blocks, wires));
    expect(measure(arranged, wires).over).toBe(0);
  });

  test("a node that is across a cable is put aside though that makes the cables at it longer, and the arrangement is taken for it: crossing a node costs more than the pixels that it takes to go round", () => {
    // 1 feeds 3 across 2, and 2 has the room of a cable on each side (fed by 1 and feeding 3): it is where its own cables have it, on the cable from 1 to 3.
    const blocks = [block(1, 0, 0), block(2, WIDTH + GAP_X, 0), block(3, 2 * (WIDTH + GAP_X), 0)];
    const wires = [audio(1, 3), audio(1, 2), audio(2, 3)];
    expect(measure(blocks, wires).over).toBe(1);
    const arranged = moved(blocks, arrange(blocks, wires, new Set([1, 3])));
    expect(measure(arranged, wires).over).toBe(0);
    expect(measure(arranged, wires).length).toBeGreaterThan(measure(blocks, wires).length);
  });

  test("the cable of a node that would pass over a node it does not join costs for each pixel of it: a node that is held to a place beyond by another cable is put clear of the node, and not left across it", () => {
    // 1 feeds 2 and 2 feeds 3, a long way apart, so that 2 can be anywhere along the line for the length of the cables; a thin node 4 that stays where it is lies on the line between 1 and 2.
    const blocks = [block(1, 0, 0), block(2, 1300, 0), block(3, 2000, 0), block(4, 700, 20, 80)];
    const wires = [audio(1, 2), audio(2, 3)];
    expect(measure(blocks, wires).over).toBe(1);
    const arranged = moved(blocks, arrange(blocks, wires, new Set([1, 3, 4])));
    expect(measure(arranged, wires)).toMatchObject({ over: 0, crowded: 0 });
  });

  test("a cable that is short of its room is given it, though that makes the cable longer: the room is worth more than the pixels", () => {
    const blocks = [block(1, 0, 0), block(2, WIDTH + GAP_X - 24, 0)];
    const wires = [audio(1, 2)];
    const arranged = moved(blocks, arrange(blocks, wires));
    expect(at(arranged, 2)).toMatchObject({ x: WIDTH + GAP_X, y: 0 });
    expect(measure(arranged, wires)).toMatchObject({ backwards: 0, short: 0 });
    expect(measure(arranged, wires).length).toBeGreaterThan(measure(blocks, wires).length);
  });

  test("the cables of a node that has moved are priced where they are now by the nodes that are looked at after it: a node that no cable is at is not put on one", () => {
    // 1 stays and has cables to 2 and 4, which are far from where they will be; 3, which no cable is at, is looked at after them.
    const blocks = [block(1, 920, 880, 96), block(2, 880, 400, 192), block(3, 1640, 720, 96), block(4, 1960, -200, 192)];
    const wires = [audio(1, 2), audio(1, 4)];
    const arranged = moved(blocks, arrange(blocks, wires, new Set([1])));
    expect(measure(arranged, wires)).toMatchObject({ over: 0, crowded: 0 });
    expect(measure(arranged, wires).length).toBeLessThan(340);
  });

  test("what came in is kept when what comes out is worse for the cables: the places are put on a grid, which a layout that had its room to the pixel is not on", () => {
    // Nodes 301 wide: the second is 72 pixels after the edge of the first at 373, which is not on the grid, and the nearest place that is is 3 pixels too far.
    const blocks: Block[] = [
      { id: 1, x: 0, y: 0, width: 301, height: 120 },
      { id: 2, x: 373, y: 0, width: 301, height: 120 },
    ];
    expect([...arrange(blocks, [audio(1, 2)])]).toEqual([
      [1, { x: 0, y: 0 }],
      [2, { x: 373, y: 0 }],
    ]);
  });

  test("it is the same for the same nodes and cables, run again", () => {
    const blocks = [block(1, 0, 0), block(2, 376, 200), block(3, 752, 0, 228), block(4, 0, 500), block(5, 400, 560, 90)];
    const wires = [audio(1, 2), audio(2, 3), audio(4, 3), modulation(5, 2), notes(1, 4)];
    const once = arrange(blocks, wires);
    expect(arrange(blocks, wires)).toEqual(once);
    expect(arrange(blocks.map((b) => ({ ...b })), wires.map((w) => ({ ...w })))).toEqual(once);
  });

  test("the places are on the grid but for the whole being brought back to the corner that the nodes came in at", () => {
    const blocks = [block(1, 16, 24), block(2, 1000, 800), block(3, 296, 904)];
    const places = arrange(blocks, [audio(1, 2), audio(3, 2)]);
    const xs = [...places.values()].map((place) => place.x);
    const ys = [...places.values()].map((place) => place.y);
    expect(Math.min(...xs)).toBe(16);
    expect(Math.min(...ys)).toBe(24);
    for (const place of places.values()) {
      expect((place.x - 16) % GRID).toBe(0);
      expect((place.y - 24) % GRID).toBe(0);
    }
  });

  test("cables whose nodes are not there, cables from a node to itself and cables that go round in a circle do not stop it", () => {
    const blocks = [block(1, 0, 0), block(2, 900, 100), block(3, 100, 700)];
    const wires = [audio(1, 2), audio(2, 3), audio(3, 1), audio(1, 1), audio(9, 1), modulation(2, 7)];
    const arranged = moved(blocks, arrange(blocks, wires));
    expect(arranged).toHaveLength(3);
    expect(measure(arranged, wires).crowded).toBe(0);
  });

  test("a result that is no better than what came in is what came in: an arrangement that has no room to make is left alone", () => {
    // Two nodes that are already as the cable would have them, level and GAP_X apart.
    const blocks = [block(1, 0, 0), block(2, WIDTH + GAP_X, 0)];
    expect(arrange(blocks, [audio(1, 2)])).toEqual(
      new Map([
        [1, { x: 0, y: 0 }],
        [2, { x: WIDTH + GAP_X, y: 0 }],
      ]),
    );
  });

  test("it never makes a session worse and never puts a node nearer to another than the margin, for sessions drawn at random", () => {
    // A small generator (the same numbers on every platform).
    let seed = 20261028;
    const next = (): number => {
      seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
      return seed / 2 ** 32;
    };
    for (let run = 0; run < 40; run += 1) {
      const count = 3 + Math.floor(next() * 14);
      // Nodes on a grid of cells with room round each (so that the start is not crowded).
      const blocks: Block[] = Array.from({ length: count }, (_, i) => block(i + 1, (i % 5) * (WIDTH + 2 * MARGIN + 100), Math.floor(i / 5) * 420, 100 + Math.floor(next() * 5) * 24));
      const wires: Wire[] = [];
      for (let w = 0; w < count + 3; w += 1) {
        const from = 1 + Math.floor(next() * count);
        const to = 1 + Math.floor(next() * count);
        const kind = next();
        if (kind < 0.5) wires.push(audio(from, to));
        else if (kind < 0.85) wires.push(modulation(from, to, next() < 0.5 ? "param-1" : "mix"));
        else wires.push(notes(from, to));
      }
      const before = measure(blocks, wires);
      const arranged = moved(blocks, arrange(blocks, wires));
      const after = measure(arranged, wires);
      expect(after.crowded, `run ${String(run)}: crowded`).toBe(0);
      expect(after.length + OVER_RATE * after.inside, `run ${String(run)}: worse`).toBeLessThanOrEqual(before.length + OVER_RATE * before.inside + 1e-6);
    }
  });

  test("a large session is arranged in a moment (100 nodes and about as many cables)", () => {
    const blocks: Block[] = Array.from({ length: 100 }, (_, i) => block(i + 1, (i % 10) * (WIDTH + 100), Math.floor(i / 10) * 300, 120 + (i % 3) * 40));
    const wires: Wire[] = [];
    for (let i = 1; i < 100; i += 1) wires.push(i % 10 === 0 ? modulation(i, i + 1) : audio(i, i + 1));
    for (let i = 1; i < 90; i += 7) wires.push(modulation(i, i + 10));
    const started = performance.now();
    const arranged = moved(blocks, arrange(blocks, wires));
    expect(performance.now() - started).toBeLessThan(4000);
    expect(measure(arranged, wires).crowded).toBe(0);
    expect(measure(arranged, wires).length).toBeLessThan(measure(blocks, wires).length);
  });
});
