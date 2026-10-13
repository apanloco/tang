// The controls of a node, bound to the session: each reads its own value from the client's
// store (so a value that changes renders that control and no other), shows what the user
// has set it to until the server has answered (`Drive`), and draws the modulation on it:
// the ring from the cables that end at it and the dot from the telemetry.
import { Handle, Position } from "@xyflow/react";
import { memo, useContext, useMemo, useSyncExternalStore } from "react";

import { modulationExtent, modulationsOn } from "../../client/modulation";
import type { Modulation } from "../../client/modulation";
import type { Drivable } from "../../client/reach";
import { slotReader } from "../../client/readers";
import { cableDepthOf, gainOf, modulatorFieldOf, paramOf } from "../../client/selectors";
import { DEPTH_RANGE, GAIN_RANGES, MODULATOR_FIELD_DEFAULTS, MODULATOR_FIELD_RANGES, WAVEFORMS } from "../../protocol";
import type { EdgeId, Gain, ModulatorField, NodeId, Param } from "../../protocol";
import { Choice } from "../../ui/Choice";
import { formatValue, Knob } from "../../ui/Knob";
import type { Scale } from "../../ui/knob-geometry";
import { useClient, useClientSelection, useClientState, useDrive, useEditor, useFrames } from "../ClientProvider";
import { ConnectingContext, keyOf } from "./connection";
import { seconds } from "./describe";

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

/**
 * Whether two modulators are the same as far as the ring of a knob goes: the same kind, and for a control the
 * one it is bound to. (An LFO's rate and waveform and an envelope's stages move the dot, not the ring, which is
 * how far the modulator can swing the value: turning them is not for the knobs they drive to draw again.)
 */
function sameKindOfSource(a: Modulation["source"], b: Modulation["source"]): boolean {
  if (a === b) return true;
  if (a.type === "midiLearn" && b.type === "midiLearn") return JSON.stringify(a.control) === JSON.stringify(b.control);
  return a.type === b.type;
}

/** Whether two lists of modulators are the same ones, of the same kinds, at the same depths. */
export function sameModulations(a: readonly Modulation[], b: readonly Modulation[]): boolean {
  return a.length === b.length && a.every((modulation, i) => modulation.amount === b[i]?.amount && sameKindOfSource(modulation.source, b[i].source));
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
 * What a number of a modulator is called and written like, and how its knob is spread: a rate
 * and the times of an envelope run over decades, a level does not. How far each goes, and what a
 * double click puts it back to, are the controller's (`MODULATOR_FIELD_RANGES`, `_DEFAULTS`).
 */
const NUMBERS: Record<Exclude<ModulatorField, "waveform">, { label: string; scale: Scale; format: (value: number) => string }> = {
  rate: { label: "Rate", scale: "log", format: (value) => `${formatValue(value)} Hz` },
  attack: { label: "Attack", scale: "log", format: seconds },
  decay: { label: "Decay", scale: "log", format: seconds },
  sustain: { label: "Sustain", scale: "linear", format: formatValue },
  release: { label: "Release", scale: "log", format: seconds },
};

/** A number of a modulator: an LFO's rate, a stage of an envelope. */
export const ModulatorFieldControl = memo(function ModulatorFieldControl({ node, field }: { node: NodeId; field: Exclude<ModulatorField, "waveform"> }) {
  const frames = useFrames();
  const control = useDrive().modulatorField(node, field);
  const stored = useClientState((state) => modulatorFieldOf(state, node, field));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const shown = pending ?? stored;
  if (shown === undefined) return null;
  const { label, scale, format } = NUMBERS[field];
  const [min, max] = MODULATOR_FIELD_RANGES[field];
  return (
    <Knob
      label={label}
      value={shown}
      min={min}
      max={max}
      scale={scale}
      defaultValue={MODULATOR_FIELD_DEFAULTS[field]}
      format={format}
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

/** The waveform of an LFO: one of the four, chosen. */
export const WaveformControl = memo(function WaveformControl({ node }: { node: NodeId }) {
  const control = useDrive().modulatorField(node, "waveform");
  const stored = useClientState((state) => modulatorFieldOf(state, node, "waveform"));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const shown = pending ?? stored;
  if (shown === undefined) return null;
  return (
    <Choice
      label="Wave"
      labels={WAVEFORMS}
      value={shown}
      onChange={(chosen) => {
        control.set(chosen);
        control.commit();
      }}
    />
  );
});

/**
 * A parameter in a node with a place on its top edge where a cable from a modulator ends, drawn by dragging
 * one to it (a port does not start a cable). (That the control is there when a cable ends at it is for
 * `toFlow` to see to: a parameter that is driven is always among those a node shows.)
 */
export const ParamCell = memo(function ParamCell({ node, index, driven }: { node: NodeId; index: number; driven: boolean }) {
  // While a cable is dragged, the ports that take it say so.
  const takes = useContext(ConnectingContext)?.has(keyOf(node, index)) ?? false;
  const classes = ["tang-port", "tang-port--param", ...(driven ? ["tang-port--driven"] : []), ...(takes ? ["tang-port--takes"] : [])];
  return (
    <div className="tang-cell">
      <Handle isConnectableStart={false} type="target" position={Position.Top} id={`param-${String(index)}`} className={classes.join(" ")} />
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

/** A setting of a modulator, in a cell of the node's grid: a choice for the waveform, a knob for the rest. */
export const ModulatorSettingCell = memo(function ModulatorSettingCell({ node, field }: { node: NodeId; field: ModulatorField }) {
  return <div className="tang-cell">{field === "waveform" ? <WaveformControl node={node} /> : <ModulatorFieldControl node={node} field={field} />}</div>;
});

/** A depth as a person says it: a percentage, which goes below nothing when the cable pulls the other way. */
export const percent = (depth: number): string => `${String(Math.round(depth * 100))}%`;

/**
 * How deep a modulation cable pulls, as a slider on the cable (a depth is read in a glance
 * and set in a stroke: a knob is more than the cable has room for). `label` says what the
 * cable joins. It is turned like any control: what the user has set is shown until the engine
 * has answered, at most one intent goes in a frame, and one that ends a gesture goes at once.
 */
export const CableDepthControl = memo(function CableDepthControl({ edge, label }: { edge: EdgeId; label: string }) {
  const control = useDrive().cableDepth(edge);
  const stored = useClientState((state) => cableDepthOf(state, edge));
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const shown = pending ?? stored;
  if (shown === undefined) return null;
  const [min, max] = DEPTH_RANGE;
  const commit = (): void => {
    control.commit();
  };
  return (
    <label className="tang-depth">
      <input
        type="range"
        className="tang-depth__slider"
        aria-label={`Depth of ${label}`}
        aria-valuetext={percent(shown)}
        min={min}
        max={max}
        step={0.01}
        value={shown}
        onChange={(event) => {
          control.set(event.target.valueAsNumber);
        }}
        onPointerUp={commit}
        onKeyUp={commit}
        onBlur={commit}
      />
      {/* (Not an `<output>`, which is a status to a screen reader: the slider has said it.) */}
      <span className="tang-depth__value" aria-hidden="true">
        {percent(shown)}
      </span>
    </label>
  );
});

/**
 * The list that draws a cable from a modulator to any parameter it can still drive, plugin by plugin: for a
 * keyboard (which cannot drag a cable), and for the parameters a node does not show as controls. Choosing
 * one is the cable; the list goes back to its first line (the cable's parameter is not on it any more).
 */
export const DrivePicker = memo(function DrivePicker({ modulator, name, targets }: { modulator: NodeId; name: string; targets: readonly Drivable[] }) {
  const editor = useEditor();
  const nothing = targets.length === 0;
  return (
    <select
      className="tang-drives"
      aria-label={`Cable from ${name} to`}
      value=""
      disabled={nothing}
      onChange={(event) => {
        const [node, param] = event.target.value.split(":").map(Number);
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        if (node !== undefined && param !== undefined) void editor.addCable(modulator, node, param);
      }}
    >
      <option value="">{nothing ? "Nothing to drive" : "Add a cable to…"}</option>
      {targets.map((group) => (
        <optgroup key={group.node} label={group.name}>
          {group.params.map((param) => (
            <option key={param.index} value={`${String(group.node)}:${String(param.index)}`}>
              {param.name}
            </option>
          ))}
        </optgroup>
      ))}
    </select>
  );
});
