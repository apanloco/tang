// The controls of a node, bound to the session: each reads its own value from the client's
// store (so a value that changes renders that control and no other), shows what the user
// has set it to until the server has answered (`Drive`), and draws the modulation on it:
// the ring from the cables that end at it and the dot from the telemetry.
import { Handle, Position } from "@xyflow/react";
import { memo, useMemo, useSyncExternalStore } from "react";

import { modulationExtent, modulationsOn } from "../../client/modulation";
import type { Modulation } from "../../client/modulation";
import { slotReader } from "../../client/readers";
import { gainOf, paramOf } from "../../client/selectors";
import { GAIN_RANGES } from "../../protocol";
import type { Gain, NodeId, Param } from "../../protocol";
import { Choice } from "../../ui/Choice";
import { formatValue, Knob } from "../../ui/Knob";
import { useClient, useClientSelection, useClientState, useDrive, useFrames } from "../ClientProvider";

/** What of a parameter does not change as its value does: the same by this, it is drawn the same. */
export function sameParamFacts(a: Param | undefined, b: Param | undefined): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return (
    a.index === b.index &&
    a.name === b.name &&
    a.min === b.min &&
    a.max === b.max &&
    a.default === b.default &&
    a.unit === b.unit &&
    a.labels === b.labels
  );
}

/** Whether two lists of modulators are the same ones, at the same depths. */
export function sameModulations(a: readonly Modulation[], b: readonly Modulation[]): boolean {
  return a.length === b.length && a.every((modulation, i) => modulation.source === b[i]?.source && modulation.amount === b[i].amount);
}

/** A value with its unit, if the parameter has one. */
function formatterFor(unit: string | null): (value: number) => string {
  return unit ? (value) => `${formatValue(value)} ${unit}` : formatValue;
}

/** A parameter of the plugin in a node: a knob, or a choice if its values have names. */
export const ParamControl = memo(function ParamControl({ node, index }: { node: NodeId; index: number }) {
  const client = useClient();
  const frames = useFrames();
  const control = useDrive().param(node, index);
  const facts = useClientSelection((state) => paramOf(state, node, index), sameParamFacts);
  const stored = useClientState((state) => paramOf(state, node, index)?.value);
  const modulations = useClientSelection((state) => (state.snapshot ? modulationsOn(state.snapshot.graph, node, index) : []), sameModulations);
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const readEffective = useMemo(() => slotReader(client.telemetry, "modulation", node, { type: "param", index }, "effective"), [client, node, index]);
  const format = useMemo(() => formatterFor(facts?.unit ?? null), [facts?.unit]);

  const shown = pending ?? stored;
  const ring = useMemo(
    () => (facts && shown !== undefined ? modulationExtent(shown, facts.min, facts.max, modulations) : undefined),
    [facts, shown, modulations],
  );
  if (!facts || shown === undefined) return null;

  if (facts.labels) {
    return (
      <Choice
        label={facts.name}
        labels={facts.labels}
        value={shown}
        onChange={(chosen) => {
          control.set(chosen);
          control.commit();
        }}
      />
    );
  }
  return (
    <Knob
      label={facts.name}
      value={shown}
      min={facts.min}
      max={facts.max}
      defaultValue={facts.default}
      format={format}
      modulation={ring}
      onChange={(value) => {
        control.set(value);
      }}
      onCommit={() => {
        control.commit();
      }}
      readEffective={readEffective}
      frames={frames}
    />
  );
});

/** What a host-side gain is called, and where a double click puts it; how far it goes is the controller's (`GAIN_RANGES`). */
const GAINS: Record<Gain, { label: string; reset: number }> = {
  volume: { label: "Volume", reset: 1 },
  mix: { label: "Mix", reset: 1 },
  groupVolume: { label: "Volume", reset: 1 },
  groupPan: { label: "Pan", reset: 0 },
};

/** A host-side gain of a node: an instrument's volume, an effect's mix, a group's volume or pan. */
export const GainControl = memo(function GainControl({ node, gain }: { node: NodeId; gain: Gain }) {
  const frames = useFrames();
  const control = useDrive().gain(node, gain);
  const stored = useClientState((state) => gainOf(state, node, gain));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const shown = pending ?? stored;
  if (shown === undefined) return null;
  const { label, reset } = GAINS[gain];
  const [min, max] = GAIN_RANGES[gain];
  return (
    <Knob
      label={label}
      value={shown}
      min={min}
      max={max}
      defaultValue={reset}
      onChange={(value) => {
        control.set(value);
      }}
      onCommit={() => {
        control.commit();
      }}
      frames={frames}
    />
  );
});

/**
 * A parameter in a node with a place on its top edge where a cable from a modulator ends.
 * (That the control is there when a cable ends at it is for `toFlow` to see to: a
 * parameter that is driven is always among those a node shows.)
 */
export const ParamCell = memo(function ParamCell({ node, index, driven }: { node: NodeId; index: number; driven: boolean }) {
  return (
    <div className="tang-cell">
      <Handle
        isConnectable={false}
        type="target"
        position={Position.Top}
        id={`param-${String(index)}`}
        className={driven ? "tang-port tang-port--param tang-port--driven" : "tang-port tang-port--param"}
      />
      <ParamControl node={node} index={index} />
    </div>
  );
});

export const GainCell = memo(function GainCell({ node, gain }: { node: NodeId; gain: Gain }) {
  return (
    <div className="tang-cell">
      <GainControl node={node} gain={gain} />
    </div>
  );
});
