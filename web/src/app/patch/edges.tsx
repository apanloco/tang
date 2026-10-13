// The cables of the canvas: the audio path, derived and plain, and modulation, which is
// magenta, dashed, and the thicker the deeper it goes.
import { BaseEdge, getBezierPath } from "@xyflow/react";
import type { EdgeProps } from "@xyflow/react";

import { cableWidth } from "./describe";
import type { PatchEdge } from "./toFlow";

export function AudioEdge(props: EdgeProps<PatchEdge>) {
  const [path] = getBezierPath(props);
  return <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--audio" />;
}

export function ModulationEdge(props: EdgeProps<PatchEdge>) {
  const [path] = getBezierPath(props);
  return <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--modulation" style={{ strokeWidth: cableWidth(props.data?.amount ?? 0) }} />;
}
