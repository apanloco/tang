import { getBezierPath, getSmoothStepPath, Position } from "@xyflow/react";
import { describe, expect, test } from "vitest";

import { cableRoute, CORNER, SIDE_OFFSET } from "./cableRoute";

// A cable from the foot of a modulator at (1000, 400) to a port at (820, 900): the top of a control, or the left edge of a node (the row of a cable).
const from = { sourceX: 1000, sourceY: 400, sourcePosition: Position.Bottom };
const to = (targetPosition: Position, targetY = 900) => ({ ...from, targetX: 820, targetY, targetPosition });

describe("how a modulation cable is drawn", () => {
  test("is a curve from the output to the top of a control, with its depth turned at the middle, as it was", () => {
    const [path, x, y] = cableRoute(to(Position.Top));
    expect([path, x, y]).toEqual(getBezierPath(to(Position.Top)).slice(0, 3));
  });

  test("goes round the node when it ends at its left edge: down from what drives it, along under it, down beside the node, and in", () => {
    const [path] = cableRoute(to(Position.Left));
    expect(path).toBe(getSmoothStepPath({ ...to(Position.Left), offset: SIDE_OFFSET, borderRadius: CORNER })[0]);
    // It leaves the output and ends at the port. Its first leg is down from the output; its run along is under the output, to a little way to the left of the
    // port, where the node is not; and its second leg is down beside the node, to the port's level.
    const beside = 820 - SIDE_OFFSET;
    const under = 400 + SIDE_OFFSET;
    expect(path.startsWith("M1000 400")).toBe(true);
    expect(path).toContain(`L 1000,${String(under - CORNER)}`);
    expect(path).toContain(`L ${String(beside + CORNER)},${String(under)}`);
    expect(path).toContain(`L ${String(beside)},${String(900 - CORNER)}`);
    expect(path.endsWith("L820 900")).toBe(true);
  });

  test("has the slider of the depth under what drives it, half way along the run to the node, where there is room, and not over the node", () => {
    const [, x, y] = cableRoute(to(Position.Left));
    expect([x, y]).toEqual([(1000 + 820 - SIDE_OFFSET) / 2, 400 + SIDE_OFFSET]);
    // (Between the foot of what drives it and the port, level with the run along under it.)
    expect(y).toBeGreaterThan(400);
    expect(y).toBeLessThan(900);
  });

  test("has the slider where the library puts it when the node is not below what drives it: there is no run under it to put it on", () => {
    // (Level with it, a little below it, and far above it, where the library puts the slider on the run beside the node.)
    for (const targetY of [100, 380, 400, 400 + SIDE_OFFSET]) {
      const [, x, y] = cableRoute(to(Position.Left, targetY));
      const [, centreX, centreY] = getSmoothStepPath({ ...to(Position.Left, targetY), offset: SIDE_OFFSET, borderRadius: CORNER });
      expect([x, y], `a port at ${String(targetY)}`).toEqual([centreX, centreY]);
    }
  });

  test("is the same for every cable that arrives at the top: only the side of a node is gone round", () => {
    for (const position of [Position.Top, Position.Bottom, Position.Right]) {
      expect(cableRoute(to(position))[0], position).toBe(getBezierPath(to(position))[0]);
    }
  });
});
