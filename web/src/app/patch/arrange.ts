// Arranging the nodes of the canvas so that the cables between them are short (docs/web-ui.md step 98: the user's "we need to rethink the arrange into something smart that minimizes the
// length of cables maybe (we want some distance of cables for spacing, but I think we should try to minimize and see how it turns out)").
//
// The layout of `toFlow` is a set of rows: a strip for each lane with its modulators above it, the groups at the right of the longest lane, the master after them. It is steady (a node that is
// added or taken out moves nothing else) and it is long: a lane that has few effects is a long way from the master, a modulator that drives knobs of two lanes is above one of them, a group
// is level with the middle of its members whatever the room is. This starts from it and moves each node, one after the other, to where the cables that are at it are shortest, until no node
// (and no set of nodes that are held to each other by cables that have just their room, which is moved as one) can be moved to a place that is better:
//
//  - a cable is the curve that the page draws between the two places it joins on the boxes of its nodes (a cable of audio leaves the right edge and arrives at the left edge, one of modulation
//    leaves the bottom edge of a modulator and arrives at the top edge of what it drives, in the column of the knob that it drives: the same curve as the page's `getBezierPath`), and it costs
//    its length, and as much again for each pixel by which it is short of the room that it should have in front of its start (`GAP_X` of a cable of audio, `GAP_Y` of one of modulation: the room
//    for its button and its slider and for being seen is the spacing that the user asked for, and it is the one number to turn to have more or less);
//  - a cable that passes over a node that it does not join costs `OVER_RATE` for each pixel of it that is over the node (a cable is drawn over the nodes it passes, and one that crosses a node
//    hides it and says nothing of where it goes);
//  - the nodes that feed one node (two oscillators into one effect) are drawn to be in a column, `ALIGN` for each pixel that they are out of line, as the layers of a drawing of a graph are, and
//    the cables that end at one node on two places of its left edge are not to cross (the source of the cable that ends higher is not lower than the other's: `INVERT` for each pixel);
//  - no node comes nearer to another than `MARGIN`;
//  - a node is a little drawn to where the layout had it (`HOME`), which keeps the nodes that are indifferent where they were, and a node that no cable is at is drawn to the middle of the others
//    (`PACK`), so that it is not left where the rest has moved away from.
//
// Moving one node at a time to its best place ends where no single move is better, which is not always the best there is (a node that is in the way of the one that would let the others in, a modulator
// that is wedged between two nodes that would be one over the other): the work is made again with the nodes looked at in other orders, up to `STARTS` in all while the budget of `TOTAL_WORK` lasts, and
// the best of what they come to is taken.
//
// It is a function of the nodes and the cables and nothing else: the same session is arranged the same way in every browser (no random numbers but a generator with a state of its own that every
// browser runs alike, the same orders of work, a fixed number of steps, integer places on a grid of `GRID` pixels), so that what a person arranges and what another opens are alike, and a test can say
// what it is. It is run when the nodes are asked to be arranged, and not when the session changes.
import type { NodeId } from "../../protocol";

/** A node as the arrangement sees it: where its box is and how big. */
export interface Block {
  id: NodeId;
  x: number;
  y: number;
  width: number;
  height: number;
}

/** A cable as the arrangement sees it: the two nodes and the handles (as `handleOf` names them) that it joins, and how much it matters that it is short. */
export interface Wire {
  from: NodeId;
  fromHandle: string;
  to: NodeId;
  toHandle: string;
  /** How much a pixel of this cable costs: 1 for a cable that is drawn as one, less for the faint links by which a lane hears every source and the ties that keep a modulator by what it hears. */
  weight: number;
  /**
   * Where the cable ends along the top edge of its node, in pixels from the node's left edge, when that is not the middle: the column of the knob that a cable of modulation drives (the cable comes
   * down to the knob's port from above, and a node that is under another has the cable come down beside it, not through it). Only for a cable that ends at the top.
   */
  toX?: number;
}

/** A place. */
export interface Place {
  x: number;
  y: number;
}

/** How far the end of a cable of audio or of notes is to be from its start, along the way the cable goes: room for the cable's button and its slider, and for being seen. */
export const GAP_X = 72;

/** The same for a cable of modulation, which leaves the bottom of its modulator and arrives at the top of what it drives. */
export const GAP_Y = 56;

/** How near a node may be to another: nothing is nearer than this. */
export const MARGIN = 24;

/**
 * How near a node may be to the left edge of a node that a cable goes down beside, to come in at a port on that edge (the cable to the depth of one of the node's cables: the page runs it, round the node,
 * `SIDE_OFFSET` of `cableRoute.ts`, 24 pixels from the edge): more than `MARGIN`, so that the cable is not along the edge of the node next to it, over it.
 */
export const SIDE_ROOM = 32;

/** The grid that the nodes are put on, in pixels. */
export const GRID = 8;

/** How many straight pieces a cable's curve is made of, for its length and for what it passes over. */
const SEGMENTS = 12;

/** What a pixel of a cable that is over a node that it does not join costs, in pixels of cable. */
export const OVER_RATE = 1.5;

/** How much of a cable must be over a node for it to be counted as passing over it (a cable that grazes a corner is not). */
const OVER_MIN = 6;

/** What a pixel by which a cable is short of its room (its end is behind its start, or not far enough in front of it) costs, over and above its length. */
const BACKWARDS = 2;

/** The pull of the place that the layout gave a node, for each pixel away from it. */
const HOME = 0.03;

/** The pull of the middle of the nodes that have cables on a node that has none, for each pixel away from it. */
const PACK = 0.05;

/** What a pixel by which two nodes that feed one node are out of line costs, so that they are put in a column (the sources of a join are one under the other, as the layers of a drawing of a graph are). */
const ALIGN = 2;

/** What a pixel by which the sources of two cables that end one above the other at a node are the wrong way round costs: the cables cross (the first input of an effect that has two is above the second, and what feeds it is above what feeds the second). */
const INVERT = 1;

/** How many orders of the nodes the work is made with, at the most: the first is the order of the graph, and the best of what they come to is taken (one order can end where another does not: a node in the way of the one that would let the others in). */
const STARTS = 12;

/** How much work the starts make in all, at the most (places priced, each as many times as there are nodes, since a place is priced against them all): a start after the first is made only while there is room left for another of the size of the first. */
export const TOTAL_WORK = 1_500_000;

/** How many times each node is looked at, at the most: the work stops before when nothing can be moved to a better place. */
const PASSES = 12;

/** How many places are priced in all, at the most, so that a session of hundreds of nodes is arranged in a moment (a number and not a time, so that it comes to the same in every browser). */
const BUDGET = 150_000;

/** How far from a node's ideal place another node's edge is looked at as a place to put it against. */
const NEAR = 720;

/** How much larger than a node the box is that a cable must not pass through (a cable that grazes a node is over it). */
const SLACK = 4;

/** The most nodes that move together: the nodes that are held to each other by cables that have just their room are moved as one, up to this many. */
const CHAIN = 12;

type Side = "left" | "right" | "top" | "bottom";

/** Where a handle is on the box of its node, as a fraction of its width and height, and which side of the box it is on (the way that a cable leaves it or arrives). */
export interface Anchor {
  fx: number;
  fy: number;
  side: Side;
  /** Pixels to add to `fx` times the width: where along the edge a knob's port is. */
  ox?: number;
}

const RIGHT: Anchor = { fx: 1, fy: 0.5, side: "right" };
const LEFT: Anchor = { fx: 0, fy: 0.5, side: "left" };
const LEFT_PAIR: Anchor = { fx: 0, fy: 1 / 3, side: "left" };
const LEFT_SIDE: Anchor = { fx: 0, fy: 2 / 3, side: "left" };
const BOTTOM: Anchor = { fx: 0.5, fy: 1, side: "bottom" };
const TOP: Anchor = { fx: 0.5, fy: 0, side: "top" };

/**
 * Where the handle `handle` of a node is on its box: the ends of audio and of notes are in the middle of the right and the left edge (the two inputs of an effect that has two at a third and two
 * thirds of the height, the first as `audioIn:pair`), the output of a modulator is at the middle of its bottom edge, and the place that a cable of modulation ends at (a parameter, a volume, a mix, a
 * pan, a setting of a modulator, the depth of a cable, whose port is on the left edge at the foot of the node but whose cable comes down to it) is taken as the top edge, in the middle unless the
 * cable says which column of the node's grid of knobs it comes down to (`Wire.toX`: the height of the knob in the node is not told, and does not move the node).
 */
export function anchorOf(handle: string): Anchor {
  switch (handle) {
    case "audioOut":
    case "eventsOut":
      return RIGHT;
    case "audioIn":
    case "eventsIn":
      return LEFT;
    case "audioIn:pair":
      return LEFT_PAIR;
    case "audioInSide":
      return LEFT_SIDE;
    case "modOut":
      return BOTTOM;
    default:
      return TOP;
  }
}

/** A box. */
interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
  /** How much more than `MARGIN` is kept free at its left edge: where a cable goes down beside the node (`SIDE_ROOM`). */
  corridor?: number;
}

/** A cable with the places it joins worked out, and the other end's. */
interface Anchored extends Wire {
  start: Anchor;
  end: Anchor;
  /** The index of the node the cable starts at and of the node it ends at, among the blocks. */
  a: number;
  b: number;
}

/** How far a cable's curve is drawn out along the way it leaves (or arrives) by, as the page's `getBezierPath` has it: half of the distance if the other end is ahead, else a curve round it. */
const bow = (distance: number): number => (distance >= 0 ? 0.5 * distance : 0.25 * 25 * Math.sqrt(-distance));

/** The control point of a cable's curve at the end that is at `(x1, y1)` on the side `side`, with the other end at `(x2, y2)`. */
function control(side: Side, x1: number, y1: number, x2: number, y2: number): [number, number] {
  switch (side) {
    case "left":
      return [x1 - bow(x1 - x2), y1];
    case "right":
      return [x1 + bow(x2 - x1), y1];
    case "top":
      return [x1, y1 - bow(y1 - y2)];
    case "bottom":
      return [x1, y1 + bow(y2 - y1)];
  }
}

/** A cable's curve as straight pieces: where it is, how long it is, and the box that holds it. */
interface Curve {
  points: Float64Array;
  length: number;
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

/** The curve that the page draws for a cable that leaves `(px, py)` by `from` and arrives at `(qx, qy)` by `to`. */
function curveOf(px: number, py: number, from: Side, qx: number, qy: number, to: Side): Curve {
  const [c1x, c1y] = control(from, px, py, qx, qy);
  const [c2x, c2y] = control(to, qx, qy, px, py);
  const points = new Float64Array(2 * (SEGMENTS + 1));
  let length = 0;
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  let lastX = px;
  let lastY = py;
  for (let i = 0; i <= SEGMENTS; i += 1) {
    const t = i / SEGMENTS;
    const u = 1 - t;
    const x = u * u * u * px + 3 * u * u * t * c1x + 3 * u * t * t * c2x + t * t * t * qx;
    const y = u * u * u * py + 3 * u * u * t * c1y + 3 * u * t * t * c2y + t * t * t * qy;
    points[2 * i] = x;
    points[2 * i + 1] = y;
    if (i > 0) length += Math.sqrt((x - lastX) ** 2 + (y - lastY) ** 2);
    lastX = x;
    lastY = y;
    if (x < minX) minX = x;
    if (x > maxX) maxX = x;
    if (y < minY) minY = y;
    if (y > maxY) maxY = y;
  }
  return { points, length, minX, minY, maxX, maxY };
}

/** How much of the straight piece from `(x0, y0)` to `(x1, y1)` is in the box with these edges. */
function clipped(x0: number, y0: number, x1: number, y1: number, left: number, top: number, right: number, bottom: number): number {
  const dx = x1 - x0;
  const dy = y1 - y0;
  let enter = 0;
  let leave = 1;
  if (dx === 0) {
    if (x0 < left || x0 > right) return 0;
  } else {
    const a = (left - x0) / dx;
    const b = (right - x0) / dx;
    enter = Math.max(enter, Math.min(a, b));
    leave = Math.min(leave, Math.max(a, b));
    if (enter > leave) return 0;
  }
  if (dy === 0) {
    if (y0 < top || y0 > bottom) return 0;
  } else {
    const a = (top - y0) / dy;
    const b = (bottom - y0) / dy;
    enter = Math.max(enter, Math.min(a, b));
    leave = Math.min(leave, Math.max(a, b));
    if (enter > leave) return 0;
  }
  return (leave - enter) * Math.sqrt(dx * dx + dy * dy);
}

/** How much of the curve is over `box`, made `SLACK` larger on every side. */
function insideOf(curve: Curve, box: Rect): number {
  const left = box.x - SLACK;
  const right = box.x + box.width + SLACK;
  const top = box.y - SLACK;
  const bottom = box.y + box.height + SLACK;
  if (curve.maxX < left || curve.minX > right || curve.maxY < top || curve.minY > bottom) return 0;
  let total = 0;
  const p = curve.points;
  for (let i = 0; i < SEGMENTS; i += 1) total += clipped(p[2 * i] ?? 0, p[2 * i + 1] ?? 0, p[2 * i + 2] ?? 0, p[2 * i + 3] ?? 0, left, top, right, bottom);
  return total;
}

/** Whether two boxes are nearer to each other than `MARGIN`, and than `SIDE_ROOM` at the left edge of one that a cable goes down beside (`Rect.corridor`). */
const tooNear = (a: Rect, b: Rect): boolean =>
  a.x + a.width + MARGIN + (b.corridor ?? 0) > b.x && b.x + b.width + MARGIN + (a.corridor ?? 0) > a.x && a.y + a.height + MARGIN > b.y && b.y + b.height + MARGIN > a.y;

/** The handle of the port of a row of a modulator's cables, where a cable that drives the depth of one ends (`depthHandle` of `toFlow.ts`): on the left edge, at the foot of the node. */
const DEPTH_HANDLE = "depth-";

/** The extra room that each of `count` nodes keeps free at its left edge: `SIDE_ROOM` less the margin for a node that a cable to the depth of a cable ends at, none for the others. */
function corridors(cables: readonly Anchored[], count: number): number[] {
  const extra = new Array<number>(count).fill(0);
  for (const cable of cables) if (cable.toHandle.startsWith(DEPTH_HANDLE)) extra[cable.b] = SIDE_ROOM - MARGIN;
  return extra;
}

/** What an arrangement is worth, for a person to compare two by and a test to say what it holds. */
export interface Measure {
  /** The length of all the cables, in pixels, as the curves that the page draws between the places they join, each as much as it matters (`Wire.weight`). */
  length: number;
  /** How many pixels the cables are short of their room, all the cables together, each as much as it matters (`GAP_X` of a cable of audio in front of its start, `GAP_Y` of one of modulation under it). */
  short: number;
  /** How many cables pass over a node that they do not join. */
  over: number;
  /** How many pixels of cable there are over the nodes that they do not join, all the cables together. */
  inside: number;
  /** How many pixels the sources of the cables that end one above the other at a node are the wrong way round, all the pairs together (the cables cross). */
  inverted: number;
  /** How many pixels the nodes that feed one node are out of line, all the pairs of them together (a pair feeds one node by cables of audio or notes that matter). */
  stagger: number;
  /** How many cables end behind where their room is (`GAP_X` of a cable of audio in front of its start, `GAP_Y` of one of modulation under it). */
  backwards: number;
  /** How many pairs of nodes are nearer to each other than `MARGIN`, or on each other. */
  crowded: number;
  /** The width and the height of the box that all the nodes are in. */
  width: number;
  height: number;
}

/** Anchor the handles of `wires`, dropping the cables whose nodes are not among the blocks, and the ones that start and end at one node. */
function anchored(blocks: readonly Block[], wires: readonly Wire[]): Anchored[] {
  const index = new Map(blocks.map((block, i) => [block.id, i]));
  return wires.flatMap((wire) => {
    const a = index.get(wire.from);
    const b = index.get(wire.to);
    if (a === undefined || b === undefined || a === b) return [];
    const end = anchorOf(wire.toHandle);
    return [{ ...wire, start: anchorOf(wire.fromHandle), end: wire.toX !== undefined && end.side === "top" ? { ...end, fx: 0, ox: wire.toX } : end, a, b }];
  });
}

/** The x and y of where the anchor `anchor` is on the box `box`. */
const pointX = (box: Rect, anchor: Anchor): number => box.x + anchor.fx * box.width + (anchor.ox ?? 0);
const pointY = (box: Rect, anchor: Anchor): number => box.y + anchor.fy * box.height;

/** The curve of the cable `cable` when its nodes are in the boxes `a` and `b`. */
const curveBetween = (cable: Anchored, a: Rect, b: Rect): Curve => curveOf(pointX(a, cable.start), pointY(a, cable.start), cable.start.side, pointX(b, cable.end), pointY(b, cable.end), cable.end.side);

/** How far the end of a cable is short of the room that its start should have in front of it (`GAP_X` or `GAP_Y`, along the way that it leaves by), in pixels, and 0 if it is not. */
function shortOf(cable: Anchored, a: Rect, b: Rect): number {
  const dx = pointX(b, cable.end) - pointX(a, cable.start);
  const dy = pointY(b, cable.end) - pointY(a, cable.start);
  switch (cable.start.side) {
    case "right":
      return Math.max(0, GAP_X - dx);
    case "left":
      return Math.max(0, GAP_X + dx);
    case "bottom":
      return Math.max(0, GAP_Y - dy);
    case "top":
      return Math.max(0, GAP_Y + dy);
  }
}

/** What a cable costs for its length and for not having its room, as much as it matters. */
const lengthCost = (cable: Anchored, curve: Curve, a: Rect, b: Rect): number => cable.weight * (curve.length + BACKWARDS * shortOf(cable, a, b));

/** The pairs of cables (by their places among `cables`) that end at one node at two places of its left edge, the cable that ends higher first. */
function crossings(cables: readonly Anchored[]): [number, number][] {
  const pairs: [number, number][] = [];
  cables.forEach((upper, i) => {
    cables.forEach((lower, j) => {
      if (i === j || upper.b !== lower.b || upper.weight < 1 || lower.weight < 1) return;
      if (upper.start.side !== "right" || lower.start.side !== "right" || upper.end.side !== "left" || lower.end.side !== "left") return;
      if (upper.end.fy < lower.end.fy) pairs.push([i, j]);
    });
  });
  return pairs;
}

/** How many pixels the cable `upper`, which ends higher, starts lower than the cable `lower` does, when the nodes are in the boxes that `box` says: none if they do not cross. */
const inversion = (upper: Anchored, lower: Anchored, box: (n: number) => Rect): number => Math.max(0, pointY(box(upper.a), upper.start) - pointY(box(lower.a), lower.start));

/** The pairs of nodes (by their places among the blocks) that feed one node by cables of audio or notes that matter: they are put in a column. */
function feeders(cables: readonly Anchored[]): [number, number][] {
  const feeding = new Map<number, number[]>();
  for (const cable of cables) {
    if (cable.weight < 1 || cable.start.side !== "right") continue;
    const sources = feeding.get(cable.b) ?? [];
    if (!sources.includes(cable.a)) sources.push(cable.a);
    feeding.set(cable.b, sources);
  }
  const pairs: [number, number][] = [];
  for (const sources of feeding.values()) {
    for (let i = 0; i < sources.length; i += 1) {
      for (let j = i + 1; j < sources.length; j += 1) pairs.push([got(sources, i), got(sources, j)]);
    }
  }
  return pairs;
}

/** What an arrangement of `blocks` is worth for the cables `wires` (`Measure`): the same for any two arrangements of the same session, whatever made them. */
export function measure(blocks: readonly Block[], wires: readonly Wire[]): Measure {
  let length = 0;
  let short = 0;
  let over = 0;
  let inside = 0;
  let backwards = 0;
  const cables = anchored(blocks, wires);
  for (const wire of cables) {
    const from = blocks[wire.a];
    const to = blocks[wire.b];
    if (!from || !to) continue;
    const curve = curveBetween(wire, from, to);
    length += wire.weight * curve.length;
    const missing = shortOf(wire, from, to);
    short += wire.weight * missing;
    // (Short of its room by more than half a step of the grid: a place on the grid is not the room to the pixel.)
    if (missing > GRID / 2) backwards += 1;
    for (let m = 0; m < blocks.length; m += 1) {
      const other = blocks[m];
      if (m === wire.a || m === wire.b || !other) continue;
      const passed = insideOf(curve, other);
      if (passed > OVER_MIN) over += 1;
      inside += wire.weight * passed;
    }
  }
  let stagger = 0;
  for (const [i, j] of feeders(cables)) stagger += Math.abs(got(blocks, i).x - got(blocks, j).x);
  let inverted = 0;
  for (const [i, j] of crossings(cables)) inverted += inversion(got(cables, i), got(cables, j), (n) => got(blocks, n));
  let crowded = 0;
  const aisles = corridors(cables, blocks.length);
  for (let i = 0; i < blocks.length; i += 1) {
    for (let j = i + 1; j < blocks.length; j += 1) {
      const a = blocks[i];
      const b = blocks[j];
      if (a && b && tooNear({ ...a, corridor: aisles[i] ?? 0 }, { ...b, corridor: aisles[j] ?? 0 })) crowded += 1;
    }
  }
  const left = Math.min(...blocks.map((block) => block.x));
  const top = Math.min(...blocks.map((block) => block.y));
  const right = Math.max(...blocks.map((block) => block.x + block.width));
  const bottom = Math.max(...blocks.map((block) => block.y + block.height));
  return { length, short, over, inside, inverted, stagger, backwards, crowded, width: blocks.length > 0 ? right - left : 0, height: blocks.length > 0 ? bottom - top : 0 };
}

/** The item at `index` of `items`, which has to be there (the places that the arrangement looks up are made from the same lists). */
function got<T>(items: readonly T[], index: number): T {
  const item = items[index];
  if (item === undefined) throw new Error(`the arrangement has no item at ${String(index)}`);
  return item;
}

const snap = (value: number): number => Math.round(value / GRID) * GRID;

/** The middle of `values` (the lower of the two in the middle for an even number: the same for any order of them). */
function middle(values: readonly number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor((sorted.length - 1) / 2)] ?? 0;
}

/** What a whole arrangement is worth in one number, for deciding between two: what the cables cost as the work prices them (their length, `BACKWARDS` times what they are short of their room, `OVER_RATE` times what is over nodes) `ALIGN` times what the nodes that feed one node are out of line and `INVERT` times what the cables that end at one node are crossed. */
export const score = (m: Measure): number => m.length + BACKWARDS * m.short + OVER_RATE * m.inside + ALIGN * m.stagger + INVERT * m.inverted;

/**
 * Where each of `blocks` goes to make the cables `wires` short (see the top of this file), or the places that came in if what comes out is worse for them (a session of nodes that no cable joins is
 * left as it is). A block that is in `fixed` stays where it is, and the others keep clear of it.
 */
export function arrange(blocks: readonly Block[], wires: readonly Wire[], fixed: ReadonlySet<NodeId> = new Set()): Map<NodeId, Place> {
  const start = new Map<NodeId, Place>(blocks.map((block) => [block.id, { x: block.x, y: block.y }]));
  const work = { priced: 0 };
  const first = descend(blocks, wires, fixed, undefined, work);
  if (first === null) return start;
  const worth = (places: ReadonlyMap<NodeId, Place>): number => score(measure(blocks.map((block) => ({ ...block, ...places.get(block.id) })), wires));
  let best = first;
  let bestWorth = worth(first);
  // (The same work with the nodes looked at in other orders, while there is room for another of the size of the first: where a node ends can depend on which was moved before it, and the best of them is taken.)
  const size = work.priced;
  for (let k = 1; k < STARTS && roomForAnother(work.priced, size, blocks.length); k += 1) {
    const places = descend(blocks, wires, fixed, shuffled(blocks.length, k), work);
    if (places === null) break;
    const found = worth(places);
    if (found < bestWorth) {
      best = places;
      bestWorth = found;
    }
  }
  // (What came in is kept if what comes out is worse: the places are put on the grid, which can cost a pixel or two on a layout that had its room to the pixel. One that is as good is taken: it has the nodes that no cable is at drawn in.)
  return bestWorth > score(measure(blocks, wires)) ? start : best;
}

/** Whether a start of the size of `size` places priced can be made after `done` places have been priced, for `nodes` nodes (a place is priced against them all): the work in all stays within `TOTAL_WORK`. */
export const roomForAnother = (done: number, size: number, nodes: number): boolean => (done + size) * nodes <= TOTAL_WORK;

/** The places 0 to `count - 1` in an order that is the same every time for the same `start` (a shuffle by a generator that has a state of its own: no two browsers differ). */
export function shuffled(count: number, start: number): number[] {
  let seed = (Math.imul(start + 1, 0x9e3779b1) ^ 0x85ebca6b) >>> 0;
  const next = (): number => {
    seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
    return seed / 2 ** 32;
  };
  const order = Array.from({ length: count }, (_, i) => i);
  for (let i = count - 1; i > 0; i -= 1) {
    const j = Math.floor(next() * (i + 1));
    const kept = got(order, i);
    order[i] = got(order, j);
    order[j] = kept;
  }
  return order;
}

/**
 * The work of `arrange` before it is judged: every block starts where it is (on the grid), and the blocks are looked at in turn (in the order of `order`, which is the order of the blocks if it is not
 * given), over and over, each one moved (alone, and with the blocks that are held to it by cables that have just their room) to the place that is best for the cables at it among the places that are
 * tried (against the cables' own ends, against the other boxes' edges, a few steps either way) until none moves. The places are on a grid of `GRID` pixels, and the whole is moved so that its left and top
 * are those of the blocks that came in (not if some are `fixed`). The places that it prices are added to `work.priced`, for `arrange` to see how much of its budget the work has taken. It is none
 * if there is nothing to move (fewer than two blocks, or no cable between two of them). Exported so that a test can see what it comes to.
 */
export function descend(blocks: readonly Block[], wires: readonly Wire[], fixed: ReadonlySet<NodeId> = new Set(), order?: readonly number[], work?: { priced: number }): Map<NodeId, Place> | null {
  const cables = anchored(blocks, wires);
  if (blocks.length < 2 || cables.length === 0) return null;

  const pinned: boolean[] = blocks.map((block) => fixed.has(block.id));
  const aisles = corridors(cables, blocks.length);
  // (A node that is fixed is where it is, to the pixel; the others start on the grid, so that every move is a number of steps of it.)
  const nodes: Rect[] = blocks.map((block, n) => ({ x: pinned[n] ? block.x : snap(block.x), y: pinned[n] ? block.y : snap(block.y), width: block.width, height: block.height, corridor: aisles[n] ?? 0 }));
  const home: Place[] = nodes.map((node) => ({ x: node.x, y: node.y }));
  /** The cables at each node, by their places in `cables`. */
  const at: number[][] = nodes.map(() => []);
  cables.forEach((cable, wire) => {
    at[cable.a]?.push(wire);
    at[cable.b]?.push(wire);
  });
  /** The pairs of nodes that feed one node (a column is made of them), and the pairs that each node is in. */
  const pairs = feeders(cables);
  const pairsAt: number[][] = nodes.map(() => []);
  pairs.forEach(([i, j], pair) => {
    pairsAt[i]?.push(pair);
    pairsAt[j]?.push(pair);
  });
  /** The pairs of cables that cross if their sources are the wrong way round, and the pairs that each node is a source of (where the node that they end at is does not decide it). */
  const crossed = crossings(cables);
  const crossedAt: number[][] = nodes.map(() => []);
  crossed.forEach(([i, j], pair) => {
    for (const n of new Set([got(cables, i).a, got(cables, j).a])) crossedAt[n]?.push(pair);
  });
  /** Each cable's curve as the nodes are now. */
  const curves: Curve[] = cables.map((cable) => curveBetween(cable, got(nodes, cable.a), got(nodes, cable.b)));
  const refresh = (n: number): void => {
    for (const wire of at[n] ?? []) {
      const cable = got(cables, wire);
      curves[wire] = curveBetween(cable, got(nodes, cable.a), got(nodes, cable.b));
    }
  };
  let priced = 0;

  /** The middle of the nodes that have cables: where a node that has none is drawn to. */
  const centre = (): Place => {
    let x = 0;
    let y = 0;
    let count = 0;
    nodes.forEach((node, n) => {
      if ((at[n]?.length ?? 0) === 0) return;
      x += node.x + node.width / 2;
      y += node.y + node.height / 2;
      count += 1;
    });
    return count === 0 ? { x: 0, y: 0 } : { x: x / count, y: y / count };
  };

  /**
   * What everything that the nodes in `set` have to do with costs while they are where `moved` says (the others where they are): the cables at them, the cables at them that pass over other nodes,
   * the other cables that pass over them, their pull to where they were or, for a node that has no cable, to the middle of the rest, how far out of line they are with the nodes that feed the same node, and how far the cables that end at one node are crossed.
   */
  const costOf = (set: readonly number[], moved: ReadonlyMap<number, Rect>, middleOfAll: Place): number => {
    priced += 1;
    const box = (n: number): Rect => moved.get(n) ?? got(nodes, n);
    const wiresOfSet = new Set<number>();
    for (const n of set) for (const wire of at[n] ?? []) wiresOfSet.add(wire);
    let cost = 0;
    for (const wire of wiresOfSet) {
      const cable = got(cables, wire);
      const a = box(cable.a);
      const b = box(cable.b);
      const curve = curveBetween(cable, a, b);
      cost += lengthCost(cable, curve, a, b);
      for (let m = 0; m < nodes.length; m += 1) {
        if (m === cable.a || m === cable.b) continue;
        cost += OVER_RATE * cable.weight * insideOf(curve, box(m));
      }
    }
    for (let wire = 0; wire < cables.length; wire += 1) {
      if (wiresOfSet.has(wire)) continue;
      const cable = got(cables, wire);
      const curve = got(curves, wire);
      for (const n of set) {
        if (n === cable.a || n === cable.b) continue;
        cost += OVER_RATE * cable.weight * insideOf(curve, box(n));
      }
    }
    for (const n of set) {
      const node = box(n);
      if ((at[n]?.length ?? 0) === 0) {
        cost += PACK * Math.sqrt((node.x + node.width / 2 - middleOfAll.x) ** 2 + (node.y + node.height / 2 - middleOfAll.y) ** 2);
      } else {
        const pull = got(home, n);
        cost += HOME * Math.sqrt((node.x - pull.x) ** 2 + (node.y - pull.y) ** 2);
      }
    }
    // (A pair of which both nodes are in the set is priced once for each of them, which a move of the whole set does not change: the nodes of the pair stay as far from each other as they were.)
    for (const n of set) {
      for (const pair of pairsAt[n] ?? []) {
        const [i, j] = got(pairs, pair);
        cost += ALIGN * Math.abs(box(i).x - box(j).x);
      }
    }
    for (const n of set) {
      for (const pair of crossedAt[n] ?? []) {
        const [i, j] = got(crossed, pair);
        cost += INVERT * inversion(got(cables, i), got(cables, j), box);
      }
    }
    return cost;
  };

  /** Whether the nodes in `set` can be where `moved` says: nearer than `MARGIN` to no node that is not in the set. */
  const clear = (set: readonly number[], moved: ReadonlyMap<number, Rect>): boolean => {
    for (const n of set) {
      const node = moved.get(n) ?? got(nodes, n);
      for (let m = 0; m < nodes.length; m += 1) {
        if (set.includes(m)) continue;
        if (tooNear(node, got(nodes, m))) return false;
      }
    }
    return true;
  };

  /** Where the cables at the node `n` would have it, each cable by itself: the place at which its end of the cable is at the wanted distance in front of the other end, in line with it. */
  const ideals = (n: number): Place[] => {
    const node = nodes[n];
    if (!node) return [];
    const places: Place[] = [];
    for (const wire of at[n] ?? []) {
      const cable = got(cables, wire);
      const mine = cable.a === n ? cable.start : cable.end;
      const theirs = cable.a === n ? cable.end : cable.start;
      const other = nodes[cable.a === n ? cable.b : cable.a];
      if (!other) continue;
      const gap = mine.side === "left" || mine.side === "right" ? GAP_X : GAP_Y;
      // (The anchor is a gap behind the other end, on the side that the cable leaves by or arrives at.)
      const ax = pointX(other, theirs) - (mine.side === "right" ? 1 : mine.side === "left" ? -1 : 0) * gap;
      const ay = pointY(other, theirs) - (mine.side === "bottom" ? 1 : mine.side === "top" ? -1 : 0) * gap;
      places.push({ x: ax - mine.fx * node.width - (mine.ox ?? 0), y: ay - mine.fy * node.height });
    }
    return places;
  };

  /**
   * The nodes that are held to `n` by cables that have just the room that they should (to within a step of the grid): the nodes in a row with it along cables of audio and of notes, the modulators
   * above it and what is under it, and the ones that these are held to in their turn, and so on (at most `CHAIN` in all, `n` first, the nearest first). They are moved with it, so that a row of nodes,
   * or a node with its modulators, is slid as one and not left with a node that cannot move without making a cable short.
   */
  const heldWith = (n: number): number[] => {
    const held = [n];
    const seen = new Set(held);
    for (let i = 0; i < held.length && held.length < CHAIN; i += 1) {
      const from = got(held, i);
      for (const wire of at[from] ?? []) {
        const cable = got(cables, wire);
        const other = cable.a === from ? cable.b : cable.a;
        if (seen.has(other) || pinned[other] || held.length >= CHAIN) continue;
        const side = cable.start.side;
        if (side !== "right" && side !== "bottom") continue;
        const a = got(nodes, cable.a);
        const b = got(nodes, cable.b);
        const along = side === "right" ? pointX(b, cable.end) - pointX(a, cable.start) : pointY(b, cable.end) - pointY(a, cable.start);
        if (Math.abs(along - (side === "right" ? GAP_X : GAP_Y)) > GRID) continue;
        held.push(other);
        seen.add(other);
      }
    }
    return held;
  };

  const visit = order ?? nodes.map((_, n) => n);
  for (let pass = 0; pass < PASSES && priced < BUDGET; pass += 1) {
    let moved = false;
    for (const n of visit) {
      if (priced >= BUDGET) break;
      const node = nodes[n];
      if (!node || pinned[n]) continue;
      const middleOfAll = centre();
      const isolated = (at[n]?.length ?? 0) === 0;
      const wanted = ideals(n);
      const aimed = isolated
        ? { x: middleOfAll.x - node.width / 2, y: middleOfAll.y - node.height / 2 }
        : wanted.length > 0
          ? { x: middle(wanted.map((place) => place.x)), y: middle(wanted.map((place) => place.y)) }
          : { x: node.x, y: node.y };

      const held = isolated ? [n] : heldWith(n);
      const sets: (readonly number[])[] = held.length > 1 ? [[n], held] : [[n]];
      let best: { dx: number; dy: number; gain: number; set: readonly number[] } | null = null;
      for (const set of sets) {
        const before = costOf(set, new Map(), middleOfAll);
        const tried = new Set<string>();
        /** Try moving the nodes of `set` by as much as takes the node `n` to `(x, y)`. */
        const consider = (x: number, y: number): void => {
          const dx = snap(x) - node.x;
          const dy = snap(y) - node.y;
          if (dx === 0 && dy === 0) return;
          const key = `${String(dx)},${String(dy)}`;
          if (tried.has(key)) return;
          tried.add(key);
          const to = new Map<number, Rect>(
            set.map((m) => {
              const rect = got(nodes, m);
              return [m, { ...rect, x: rect.x + dx, y: rect.y + dy }];
            }),
          );
          if (!clear(set, to)) return;
          const gain = costOf(set, to, middleOfAll) - before;
          if (gain < -1e-6 && (best === null || gain < best.gain)) best = { dx, dy, gain, set };
        };
        // Where each cable would have it, where the middle of them all would, and each cable's place along the one that the others agree on.
        consider(aimed.x, aimed.y);
        for (const place of wanted) {
          consider(place.x, place.y);
          consider(place.x, aimed.y);
          consider(aimed.x, place.y);
        }
        // Against the edges of the boxes that are near where it would have it: touching, with the margin, and in line with the middle of the cables.
        for (let m = 0; m < nodes.length; m += 1) {
          const other = nodes[m];
          if (m === n || !other || set.includes(m)) continue;
          if (Math.abs(other.x - aimed.x) > NEAR + other.width || Math.abs(other.y - aimed.y) > NEAR + other.height) continue;
          consider(other.x + other.width + MARGIN + (node.corridor ?? 0), aimed.y);
          consider(other.x - MARGIN - (other.corridor ?? 0) - node.width, aimed.y);
          consider(aimed.x, other.y + other.height + MARGIN);
          consider(aimed.x, other.y - MARGIN - node.height);
          consider(other.x + other.width + MARGIN + (node.corridor ?? 0), other.y);
          consider(other.x + other.width + MARGIN + (node.corridor ?? 0), other.y + other.height - node.height);
          consider(other.x, other.y + other.height + MARGIN);
          consider(other.x + other.width - node.width, other.y + other.height + MARGIN);
          consider(other.x, other.y - MARGIN - node.height);
          consider(other.x + other.width - node.width, other.y - MARGIN - node.height);
        }
        // And a few steps from where it is.
        for (const step of [GRID, GRID * 4, GRID * 16]) {
          consider(node.x + step, node.y);
          consider(node.x - step, node.y);
          consider(node.x, node.y + step);
          consider(node.x, node.y - step);
        }
      }
      const found = best as { dx: number; dy: number; gain: number; set: readonly number[] } | null;
      if (found) {
        for (const m of found.set) {
          const rect = got(nodes, m);
          rect.x += found.dx;
          rect.y += found.dy;
        }
        for (const m of found.set) refresh(m);
        moved = true;
      }
    }
    if (!moved) break;
  }

  if (work) work.priced += priced;
  // The same top and left as the blocks that came in (unless some stay where they are).
  const left = Math.min(...blocks.map((block) => block.x));
  const top = Math.min(...blocks.map((block) => block.y));
  const shiftX = fixed.size > 0 ? 0 : Math.min(...nodes.map((node) => node.x)) - left;
  const shiftY = fixed.size > 0 ? 0 : Math.min(...nodes.map((node) => node.y)) - top;
  return new Map(blocks.map((block, i) => [block.id, { x: (nodes[i]?.x ?? block.x) - shiftX, y: (nodes[i]?.y ?? block.y) - shiftY }]));
}
