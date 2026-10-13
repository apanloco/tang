// The nodes of the canvas (docs/web-ui.md §9.3): what each kind of node of the session is
// drawn as. A plugin node is a function of its data and the controls in it read their own
// values, so that a value that changes renders its control and nothing else.
import { Handle, Position } from "@xyflow/react";
import type { NodeProps } from "@xyflow/react";
import { useMemo } from "react";

import { slotReader } from "../../client/readers";
import { nodeById } from "../../client/selectors";
import { LiveBar } from "../../ui/LiveBar";
import { Meter } from "../../ui/Meter";
import { useClient, useClientState, useFrames } from "../ClientProvider";
import { GainCell, ParamCell } from "./controls";
import { detailOf, kindOf, outputScale } from "./describe";
import type { PatchNode } from "./toFlow";

function Header({ name, badge }: { name: string; badge?: string | null }) {
  return (
    <header className="tang-node__header">
      <span className="tang-node__name" title={name}>
        {name}
      </span>
      {badge ? <span className="tang-node__badge">{badge}</span> : null}
    </header>
  );
}

/** An instrument or an effect: its plugin's name and format, the parameters it shows, and its own gain. */
export function PluginNode({ data, type }: NodeProps<PatchNode>) {
  const effect = type === "effect";
  return (
    <div className={`tang-node tang-node--${type}`}>
      {effect ? <Handle isConnectable={false} type="target" position={Position.Left} id="audioIn" className="tang-port tang-port--audio" /> : null}
      <Handle isConnectable={false} type="source" position={Position.Right} id="audioOut" className="tang-port tang-port--audio" />
      <Header name={data.name} badge={data.format} />
      <div className="tang-node__controls nodrag nopan nowheel">
        {data.pinned.map((index) => (
          <ParamCell key={index} node={data.node} index={index} driven={data.modulated.includes(index)} />
        ))}
        <GainCell node={data.node} gain={effect ? "mix" : "volume"} />
      </div>
    </div>
  );
}

/** A modulator: what it is, its settings, and what it puts out as it does so. */
export function ModulatorNode({ data }: NodeProps<PatchNode>) {
  const client = useClient();
  const frames = useFrames();
  const node = useClientState((state) => nodeById(state, data.node));
  const read = useMemo(() => slotReader(client.telemetry, "modulation", data.node, { type: "modOut" }, "modOutput"), [client, data.node]);
  const source = node?.kind === "modulator" ? node.source : null;
  const scale = source ? outputScale(source) : { min: 0, max: 1, centred: false };
  // (An LFO is called what it is: its kind is not said twice.)
  const kind = source ? kindOf(source) : null;
  return (
    <div className="tang-node tang-node--modulator">
      <Header name={data.name} badge={kind === data.name ? null : kind} />
      <div className="tang-node__body">
        <p className="tang-node__detail">{source ? detailOf(source) : ""}</p>
        <LiveBar label={`${data.name} output`} read={read} min={scale.min} max={scale.max} centred={scale.centred} frames={frames} />
      </div>
      <Handle isConnectable={false} type="source" position={Position.Bottom} id="modOut" className="tang-port tang-port--mod" />
    </div>
  );
}

/** A submix group: its volume and its pan, between its members and the master. */
export function GroupNode({ data }: NodeProps<PatchNode>) {
  return (
    <div className="tang-node tang-node--group">
      <Handle isConnectable={false} type="target" position={Position.Left} id="audioIn" className="tang-port tang-port--audio" />
      <Handle isConnectable={false} type="source" position={Position.Right} id="audioOut" className="tang-port tang-port--audio" />
      <Header name={data.name} badge="Group" />
      <div className="tang-node__controls nodrag nopan nowheel">
        <GainCell node={data.node} gain="groupVolume" />
        <GainCell node={data.node} gain="groupPan" />
      </div>
    </div>
  );
}

/** The master: where the audio of the session ends, with its level. */
export function MasterNode({ data }: NodeProps<PatchNode>) {
  const client = useClient();
  const frames = useFrames();
  const readPeak = useMemo(() => slotReader(client.telemetry, "meters", data.node, { type: "audioIn" }, "peak"), [client, data.node]);
  const readClips = useMemo(() => slotReader(client.telemetry, "meters", data.node, { type: "audioIn" }, "clips"), [client, data.node]);
  return (
    <div className="tang-node tang-node--master">
      <Handle isConnectable={false} type="target" position={Position.Left} id="audioIn" className="tang-port tang-port--audio" />
      <Header name={data.name} />
      <div className="tang-node__body">
        <Meter label="Master level" readPeak={readPeak} readClips={readClips} frames={frames} />
      </div>
    </div>
  );
}
