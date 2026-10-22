// The cables of the canvas: the audio path, derived and plain; the audio cables a person has drawn (the sends), amber, and the thicker the louder they are, with a slider for
// the level at the middle; and modulation, which is magenta, dashed, and the thicker the deeper it goes.
import { BaseEdge, EdgeLabelRenderer, getBezierPath } from "@xyflow/react";
import { useRef } from "react";
import type { EdgeProps } from "@xyflow/react";

import { cableDepthOf, cableGainOf } from "../../client/selectors";
import type { EdgeId } from "../../protocol";
import { useClientState, useEditor } from "../ClientProvider";
import { focusWhenPossible } from "../focus";
import { useCableLight } from "./cableLight";
import { cableRoute } from "./cableRoute";
import { CableDepthControl, CableLevelControl, drivesId } from "./controls";
import { cableWidth, sendWidth } from "./describe";
import { sendId } from "./SendMenu";
import type { PatchEdge } from "./toFlow";

export function AudioEdge(props: EdgeProps<PatchEdge>) {
  const [path] = getBezierPath(props);
  return <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--audio" />;
}

/**
 * The button on a cable that takes it out (the node it starts at stays); the focus goes to `focusTo`, the button that draws the next cable from that node: the list of a modulator, or the
 * menu that sends the audio of a node.
 */
function RemoveCableButton({ edge, focusTo, label }: { edge: EdgeId; focusTo: string | undefined; label: string }) {
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
          if (removed && focusTo !== undefined) focusWhenPossible(focusTo);
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
            <RemoveCableButton edge={cable} focusTo={props.data?.modulator === undefined ? undefined : drivesId(props.data.modulator)} label={props.data?.label ?? "a cable"} />
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

/**
 * An audio cable that has been drawn between two places: a curve in the colour of audio that is sent, as thick as it is loud, with its level as a slider at the middle (a level is read
 * in a glance and set in a stroke, as a depth is) and the button that takes it out beside it. It reads its level itself, so that turning it renders this cable and nothing else.
 */
export function SendEdge(props: EdgeProps<PatchEdge>) {
  const [path, labelX, labelY] = getBezierPath(props);
  const cable = props.data?.cable;
  const level = useClientState((state) => (cable === undefined ? undefined : cableGainOf(state, cable)));
  const label = props.data?.label ?? "an audio cable";
  return (
    <>
      <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--send" style={{ strokeWidth: sendWidth(level ?? 1) }} />
      {cable === undefined ? null : (
        // (In the layer over the canvas that the cables' controls are in, at the middle of the cable, where the slider is turned.)
        <EdgeLabelRenderer>
          <div className="tang-depth-anchor nodrag nopan nowheel" style={{ transform: `translate(-50%, -50%) translate(${String(labelX)}px, ${String(labelY)}px)` }}>
            <CableLevelControl edge={cable} label={label} />
            <RemoveCableButton edge={cable} focusTo={sendId(Number(props.source))} label={label} />
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}
