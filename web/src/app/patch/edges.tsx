// The cables of the canvas: the audio path, derived and plain, and modulation, which is
// magenta, dashed, and the thicker the deeper it goes.
import { BaseEdge, EdgeLabelRenderer, getBezierPath } from "@xyflow/react";
import { useRef } from "react";
import type { EdgeProps } from "@xyflow/react";

import { cableDepthOf } from "../../client/selectors";
import type { EdgeId, NodeId } from "../../protocol";
import { useClientState, useEditor } from "../ClientProvider";
import { focusWhenPossible } from "../focus";
import { useCableLight } from "./cableLight";
import { cableRoute } from "./cableRoute";
import { CableDepthControl, drivesId } from "./controls";
import { cableWidth } from "./describe";
import type { PatchEdge } from "./toFlow";

export function AudioEdge(props: EdgeProps<PatchEdge>) {
  const [path] = getBezierPath(props);
  return <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--audio" />;
}

/** The button on a cable that takes it out (the modulator stays); the focus goes to the list of the modulator, where the next cable is drawn from. */
function RemoveCableButton({ edge, modulator, label }: { edge: EdgeId; modulator: NodeId | undefined; label: string }) {
  const editor = useEditor();
  return (
    <button
      type="button"
      className="tang-depth__remove"
      aria-label={`Remove cable ${label}`}
      title={`Remove cable ${label}`}
      onClick={() => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices, and the focus stays on the button.)
        void editor.removeCable(edge).then((removed) => {
          if (removed && modulator !== undefined) focusWhenPossible(drivesId(modulator));
        });
      }}
    >
      ×
    </button>
  );
}

export function ModulationEdge(props: EdgeProps<PatchEdge>) {
  const [path, labelX, labelY] = cableRoute(props);
  // How deep it goes is a value, read here so that turning it renders this cable and nothing else.
  const cable = props.data?.cable;
  const depth = useClientState((state) => (cable === undefined ? undefined : cableDepthOf(state, cable)));
  // How bright it is, which follows what its modulator puts out (written to the DOM, not rendered).
  const light = useRef<SVGGElement>(null);
  useCableLight(light, props.data?.modulator);
  return (
    <>
      <g ref={light} className="tang-cable__light">
        <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--modulation" style={{ strokeWidth: cableWidth(depth ?? 0) }} />
      </g>
      {cable === undefined ? null : (
        // (In a layer of its own over the canvas, at the middle of the cable, where the slider is turned.)
        <EdgeLabelRenderer>
          <div className="tang-depth-anchor nodrag nopan nowheel" style={{ transform: `translate(-50%, -50%) translate(${String(labelX)}px, ${String(labelY)}px)` }}>
            <CableDepthControl edge={cable} label={props.data?.label ?? "a cable"} />
            <RemoveCableButton edge={cable} modulator={props.data?.modulator} label={props.data?.label ?? "a cable"} />
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}
