// The cables of the canvas: the audio path, derived and plain, and modulation, which is
// magenta, dashed, and the thicker the deeper it goes.
import { BaseEdge, getBezierPath } from "@xyflow/react";
import type { EdgeProps } from "@xyflow/react";

import { cableDepthOf } from "../../client/selectors";
import { useClientState } from "../ClientProvider";
import { cableWidth } from "./describe";
import type { PatchEdge } from "./toFlow";

export function AudioEdge(props: EdgeProps<PatchEdge>) {
  const [path] = getBezierPath(props);
  return <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--audio" />;
}

export function ModulationEdge(props: EdgeProps<PatchEdge>) {
  const [path] = getBezierPath(props);
  // How deep it goes is a value, read here so that turning it renders this cable and nothing else.
  const cable = props.data?.cable;
  const depth = useClientState((state) => (cable === undefined ? undefined : cableDepthOf(state, cable)));
  return <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--modulation" style={{ strokeWidth: cableWidth(depth ?? 0) }} />;
}
