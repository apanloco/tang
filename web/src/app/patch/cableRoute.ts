// How a modulation cable runs between its two ends (docs/web-ui.md §9.3), and where its depth is turned.
import { getBezierPath, getSmoothStepPath, Position } from "@xyflow/react";
import type { EdgeProps } from "@xyflow/react";

/** How far a cable that ends at the left edge of a node goes from what it leaves, and from the node, before it turns: the room that the layout leaves between the rows of modulators is more (`CABLE_GAP`). */
export const SIDE_OFFSET = 24;

/** How round the corners of such a cable are. */
export const CORNER = 10;

type Ends = Pick<EdgeProps, "sourceX" | "sourceY" | "sourcePosition" | "targetX" | "targetY" | "targetPosition">;

/**
 * The path of a cable, and the place where its depth is turned. A cable that ends at the top of a control (a parameter's, a modulator's setting) is a curve, and
 * its depth is at its middle. One that ends at the left edge of a node, the port of a row of the cables of a modulator (the depth of one), would be a curve over the
 * node it ends at, so it is drawn round it: down from what drives it, along under it, down beside the node and in. Where there is room for the slider, which is
 * not over the node, is on the run along under what drives it.
 */
export function cableRoute(ends: Ends): [path: string, labelX: number, labelY: number] {
  if (ends.targetPosition !== Position.Left) {
    const [path, labelX, labelY] = getBezierPath(ends);
    return [path, labelX, labelY];
  }
  const [path, centreX, centreY] = getSmoothStepPath({ ...ends, offset: SIDE_OFFSET, borderRadius: CORNER });
  // (A node that is not below what drives it has no such run: the cable goes some other way round, and the slider is where the library has it.)
  const below = ends.targetY > ends.sourceY + SIDE_OFFSET;
  return below ? [path, (ends.sourceX + ends.targetX - SIDE_OFFSET) / 2, ends.sourceY + SIDE_OFFSET] : [path, centreX, centreY];
}
