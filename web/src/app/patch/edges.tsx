// The cables of the canvas: the links of the audio path that a lane and a group make by themselves, which are cables like the rest (amber, with the button that takes them out at the middle, and no slider: a link has no level);
// the audio cables a person has drawn (the sends), amber too, and the thicker the louder they are, with a slider for the level at the middle; the cables of notes a person has drawn, from a source to a lane, violet, with the
// button that takes them out at the middle, and, faint and dotted in the same colour, the links by which a lane that no cable of notes ends at hears every source; and modulation, which is magenta, dashed, and the thicker the deeper it goes.
import { BaseEdge, EdgeLabelRenderer, getBezierPath } from "@xyflow/react";
import { useRef } from "react";
import type { EdgeProps } from "@xyflow/react";

import { cableDepthOf, cableGainOf } from "../../client/selectors";
import type { EdgeId, NodeId } from "../../protocol";
import { useClientState, useEditor } from "../ClientProvider";
import { focusWhenPossible } from "../focus";
import { useCableLight } from "./cableLight";
import { cableRoute } from "./cableRoute";
import { CableDepthControl, CableLevelControl, drivesId } from "./controls";
import { cableWidth, sendWidth } from "./describe";
import { notesToId } from "./NotesMenus";
import { sendId } from "./SendMenu";
import type { PatchEdge } from "./toFlow";

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

/**
 * The button on a link of the audio path that takes it out, as it takes out any cable (the controller cuts the link: `SetRoute` for the end of a lane's chain or of a group's, which goes to the group's bus or the
 * master, and `SetFeed` for a link into an effect, from the instrument, the effect before it or the group). What is left is a node that sends nowhere by that link, and the cable can be drawn again as any is,
 * from the menu of the node it starts at (the focus goes to it, since the link and this button are gone) or by a drag. If it is refused the page says so, and the focus stays on the button.
 */
function RemoveLinkButton({ owner, fed, source, label }: { owner: NodeId | undefined; fed: NodeId | undefined; source: NodeId; label: string }) {
  const editor = useEditor();
  return (
    <button
      type="button"
      className="tang-depth__remove"
      aria-label={`Remove cable ${label}`}
      title={`Remove cable ${label}`}
      onClick={() => {
        const cut = owner !== undefined ? editor.setRoute(owner, "none") : fed !== undefined ? editor.setFeed(fed, "none") : Promise.resolve(false);
        void cut.then((done) => {
          if (done) focusWhenPossible(sendId(source));
        });
      }}
    >
      ×
    </button>
  );
}

/**
 * A link of the audio path, the way that the audio goes by itself: a cable like the ones a person draws (amber, a line as thick as a send at the level of 1, which is all that a link has) with the button that
 * takes it out at its middle, and no slider. The end of a lane's chain, or of a group's, which goes to the group's bus or the master, is its way out; a link into an effect (an instrument to its first effect, an
 * effect to the next, a group to the first of its bus) is that effect's input; the button cuts either.
 */
export function AudioEdge(props: EdgeProps<PatchEdge>) {
  const [path, labelX, labelY] = getBezierPath(props);
  const owner = props.data?.wayOut;
  const fed = props.data?.feedOf;
  const label = props.data?.label ?? "a link";
  return (
    <>
      <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--audio" />
      {owner === undefined && fed === undefined ? null : (
        // (In the layer over the canvas that the cables' controls are in, at the middle of the link.)
        <EdgeLabelRenderer>
          <div className="tang-depth-anchor nodrag nopan nowheel" style={{ transform: `translate(-50%, -50%) translate(${String(labelX)}px, ${String(labelY)}px)` }}>
            <RemoveLinkButton owner={owner} fed={fed} source={Number(props.source)} label={label} />
          </div>
        </EdgeLabelRenderer>
      )}
    </>
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

/**
 * A cable of notes that has been drawn from a source to a lane: a curve in the colour of notes, with the button that takes it out at the middle (it has no level: it carries notes, which are heard or not).
 * The lane hears the sources that have a cable to it and no other; the source stays, and the focus goes to its menu, which draws the next one.
 */
export function NotesEdge(props: EdgeProps<PatchEdge>) {
  const [path, labelX, labelY] = getBezierPath(props);
  const cable = props.data?.cable;
  const label = props.data?.label ?? "a cable of notes";
  return (
    <>
      <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--notes" />
      {cable === undefined ? null : (
        // (In the layer over the canvas that the cables' controls are in, at the middle of the cable.)
        <EdgeLabelRenderer>
          <div className="tang-depth-anchor nodrag nopan nowheel" style={{ transform: `translate(-50%, -50%) translate(${String(labelX)}px, ${String(labelY)}px)` }}>
            <RemoveCableButton edge={cable} focusTo={notesToId(Number(props.source))} label={label} />
          </div>
        </EdgeLabelRenderer>
      )}
    </>
  );
}

/** The link by which a lane that no cable of notes ends at hears a source (the session has it by itself: it is not drawn or taken out): faint and dotted, in the colour of notes, with nothing on it. */
export function HearsEdge(props: EdgeProps<PatchEdge>) {
  const [path] = getBezierPath(props);
  return <BaseEdge id={props.id} path={path} className="tang-cable tang-cable--hears" />;
}
