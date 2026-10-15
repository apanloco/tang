// The controls of a node, bound to the session: each reads its own value from the client's
// store (so a value that changes renders that control and no other), shows what the user
// has set it to until the server has answered (`Drive`), and draws the modulation on it:
// the ring from the cables that end at it and the dot from the telemetry.
import { Handle, Position } from "@xyflow/react";
import { memo, useContext, useId, useMemo, useRef, useState, useSyncExternalStore } from "react";

import { modulationExtent, modulationsOn } from "../../client/modulation";
import type { Modulation } from "../../client/modulation";
import { NOTE_NAMES } from "../../client/notes";
import type { Drivable } from "../../client/reach";
import { slotReader } from "../../client/readers";
import { cableDepthOf, gainOf, groupChoicesOf, isKnob, modulatorFieldOf, paramOf, rangeOf, sameGroupChoices } from "../../client/selectors";
import { DEPTH_RANGE, GAIN_RANGES, MODULATOR_FIELD_DEFAULTS, MODULATOR_FIELD_RANGES, WAVEFORMS } from "../../protocol";
import type { EdgeId, Gain, ModulatorField, NodeId, NoteRange, Param, Port } from "../../protocol";
import { Choice } from "../../ui/Choice";
import { formatValue, Knob } from "../../ui/Knob";
import { afterKey } from "../../ui/knob-geometry";
import type { Scale } from "../../ui/knob-geometry";
import { LiveBar } from "../../ui/LiveBar";
import { MenuButton } from "../../ui/MenuButton";
import type { MenuItem } from "../../ui/MenuButton";
import { useClient, useClientSelection, useClientState, useDrive, useEditor, useFrames } from "../ClientProvider";
import { ConnectingContext, keyOf } from "./connection";
import { seconds } from "./describe";
import { depthHandle, handleOf, paramHandle } from "./toFlow";

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

/**
 * What any control of a parameter of the plugin in `node` reads and does: what the parameter is (`facts`, which do not change
 * as its value does), what it shows (what the user set it to, until the engine has answered, and then the session's value),
 * the control that sets it, and the reader of what the engine makes of it (modulation included), for a view to draw on a frame.
 */
function useParam(node: NodeId, index: number) {
  const client = useClient();
  const control = useDrive().param(node, index);
  const facts = useClientSelection((state) => paramOf(state, node, index), sameParamFacts);
  const stored = useClientState((state) => paramOf(state, node, index)?.value);
  const pending = useSyncExternalStore(control.subscribe, control.getPending);
  const readEffective = useMemo(() => slotReader(client.telemetry, "modulation", node, { type: "param", index }, "effective"), [client, node, index]);
  return { facts, shown: pending ?? stored, control, readEffective };
}

/** A parameter of the plugin in a node: a knob, or a choice if its values have names. */
export const ParamControl = memo(function ParamControl({ node, index }: { node: NodeId; index: number }) {
  const frames = useFrames();
  const { facts, shown, control, readEffective } = useParam(node, index);
  const modulations = useClientSelection((state) => (state.snapshot ? modulationsOn(state.snapshot.graph, node, index) : []), sameModulations);
  const format = useMemo(() => formatterFor(facts?.unit ?? null), [facts?.unit]);

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

/**
 * The pin of a row of the panel: pressed when the parameter is one of the knobs its node shows, and the way to make it one or not
 * (`Editor.pinParam`: a node that has nothing pinned shows the first eight, and pinning one more keeps them).
 */
function PinButton({ node, index, name }: { node: NodeId; index: number; name: string }) {
  const editor = useEditor();
  const pinned = useClientState((state) => isKnob(state, node, index));
  return (
    <button
      type="button"
      className="tang-row__pin"
      aria-pressed={pinned}
      aria-label={`Show ${name} on the node`}
      title="Show as a knob on the node"
      onClick={() => {
        void editor.pinParam(node, index, !pinned);
      }}
    >
      {pinned ? "●" : "○"}
    </button>
  );
}

/**
 * A parameter of the plugin in `node` as a row of a list: its whole name, a slider, its value; a choice if its values have
 * names; and, while a cable drives it, a bar of what the engine makes of it. For a list to look through (a plugin can have
 * thousands of parameters, and a knob has room for a few letters). It is turned like a knob: shown as set until the engine
 * has answered, one intent a frame, a gesture's end at once; the keys are a knob's (a hundredth of the range, a thousandth
 * with shift, a tenth on the page keys, the ends on Home and End), kept from the browser, whose own step on a slider
 * that has none is the whole range; a double click goes back to the default.
 */
export const ParamRow = memo(function ParamRow({ node, index }: { node: NodeId; index: number }) {
  const frames = useFrames();
  const id = useId();
  const { facts, shown, control, readEffective } = useParam(node, index);
  const driven = useClientState((state) => (state.snapshot ? modulationsOn(state.snapshot.graph, node, index).length > 0 : false));
  const format = useMemo(() => formatterFor(facts?.unit ?? null), [facts?.unit]);
  if (!facts || shown === undefined) return null;

  const name = (
    <>
      <label className="tang-row__name" htmlFor={id} title={facts.name}>
        {facts.name}
      </label>
      <PinButton node={node} index={index} name={facts.name} />
    </>
  );
  if (facts.labels) {
    return (
      <div className="tang-row">
        {name}
        <select
          id={id}
          className="tang-row__select"
          value={String(Math.round(shown))}
          onChange={(event) => {
            control.set(Number(event.target.value));
            control.commit();
          }}
        >
          {facts.labels.map((label, position) => (
            <option key={`${String(position)}:${label}`} value={String(position)}>
              {label}
            </option>
          ))}
        </select>
      </div>
    );
  }
  const { min, max } = facts;
  return (
    <div className="tang-row">
      {name}
      <input
        id={id}
        type="range"
        className="tang-row__slider"
        min={min}
        max={max}
        step="any"
        value={shown}
        aria-valuetext={format(shown)}
        onChange={(event) => {
          control.set(event.target.valueAsNumber);
        }}
        onPointerUp={() => {
          control.commit();
        }}
        onBlur={() => {
          control.commit();
        }}
        onKeyDown={(event) => {
          const next = afterKey(event.key, event.shiftKey, shown, min, max);
          if (next === undefined) return;
          event.preventDefault();
          if (next === shown) return;
          control.set(next);
          control.commit();
        }}
        onDoubleClick={() => {
          if (facts.default === shown) return;
          control.set(facts.default);
          control.commit();
        }}
      />
      <span className="tang-row__value">{format(shown)}</span>
      {driven ? <LiveBar label={`${facts.name}, as modulated`} read={readEffective} min={min} max={max} format={format} frames={frames} /> : null}
    </div>
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
 * The keys a lane takes: where its notes begin and where they end, each a choice of `any` (no end) or a note, in a cell of the
 * node's grid. A change asks for the range it makes (`SetRange`): the other end stays as it is, an end that is chosen past the
 * other takes it along (one note is a range), and `any` at both is every note, which is no range. What was chosen shows at once
 * and until the answer is in; after it, what the session has, which is what was asked for unless it was refused (and the page
 * says why). An end of the keyboard is `any`: C-1 as the low end, and G9 as the high end, are not in the lists. A node that is no
 * lane has no keys, and nothing is drawn for it.
 */
export const RangeCell = memo(function RangeCell({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const stored = useClientState((state) => rangeOf(state, node));
  const [pending, setPending] = useState<NoteRange | null | undefined>(undefined);
  const latest = useRef(0);
  if (stored === undefined) return null;
  const range = pending === undefined ? stored : pending;
  const low = range?.low ?? 0;
  const high = range?.high ?? 127;
  const choose = (next: NoteRange): void => {
    // (The whole keyboard is no range.)
    const asked = next.low === 0 && next.high === 127 ? null : next;
    const mine = (latest.current += 1);
    setPending(asked);
    void editor.setRange(node, asked).finally(() => {
      if (latest.current === mine) setPending(undefined);
    });
  };
  return (
    <div className="tang-cell">
      <div className="tang-keys">
        <span className="tang-keys__label">Keys</span>
        <select
          className="tang-keys__select"
          aria-label={`Lowest key of ${name}`}
          title="Lowest key"
          value={low === 0 ? "" : String(low)}
          onChange={(event) => {
            const note = event.target.value === "" ? 0 : Number(event.target.value);
            choose({ low: note, high: Math.max(high, note) });
          }}
        >
          <option value="">any</option>
          {NOTE_NAMES.slice(1).map((label, place) => (
            <option key={label} value={String(place + 1)}>
              {label}
            </option>
          ))}
        </select>
        <select
          className="tang-keys__select"
          aria-label={`Highest key of ${name}`}
          title="Highest key"
          value={high === 127 ? "" : String(high)}
          onChange={(event) => {
            const note = event.target.value === "" ? 127 : Number(event.target.value);
            choose({ low: Math.min(low, note), high: note });
          }}
        >
          <option value="">any</option>
          {NOTE_NAMES.slice(0, 127).map((label, place) => (
            <option key={label} value={String(place)}>
              {label}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
});

/**
 * The group a lane is in, as a choice in a cell of its node: none, a group of the session by the name it has, or a new group with the lane in
 * it (the controller's `AddGroup` with the lane as its member). A group it leaves lets go of it: its cables to the lane go, and what they drove
 * has the value that was set. What is chosen is shown until the server has answered; if it was refused (the page says why) the group the session
 * has comes back. A node that is no lane has no group, and nothing is drawn for it.
 */
export const GroupCell = memo(function GroupCell({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const stored = useClientSelection((state) => groupChoicesOf(state, node), sameGroupChoices);
  const [pending, setPending] = useState<string | undefined>(undefined);
  const latest = useRef(0);
  if (stored === undefined) return null;
  const value = pending ?? (stored.current === null ? "" : String(stored.current));
  // (The choice is narrow and cuts a long name: the tooltip has all of it.)
  const inGroup = value === "new" ? "a new group" : (stored.groups.find((group) => String(group.id) === value)?.name ?? "no group");
  const choose = (next: string): void => {
    const mine = (latest.current += 1);
    setPending(next);
    const done = next === "new" ? editor.addGroup([node]) : editor.setLaneGroup(node, next === "" ? null : Number(next));
    void done.finally(() => {
      if (latest.current === mine) setPending(undefined);
    });
  };
  return (
    <div className="tang-cell">
      <div className="tang-keys">
        <span className="tang-keys__label">Group</span>
        <select
          className="tang-keys__select"
          aria-label={`Group of ${name}`}
          title={`${name} is in ${inGroup}`}
          value={value}
          onChange={(event) => {
            choose(event.target.value);
          }}
        >
          <option value="">none</option>
          {stored.groups.map((group) => (
            <option key={group.id} value={String(group.id)}>
              {group.name}
            </option>
          ))}
          <option value="new">new group</option>
        </select>
      </div>
    </div>
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
 * The place on the top edge of a control where a cable from a modulator ends, drawn by dragging one to it (a port does
 * not start a cable): a handle of the node named `handle` (`handleOf`), quiet until a cable ends at it (`driven`), and,
 * while a cable is dragged, saying whether it takes that one.
 */
function CablePort({ node, handle, driven }: { node: NodeId; handle: string; driven: boolean }) {
  const takes = useContext(ConnectingContext)?.has(keyOf(node, handle)) ?? false;
  const classes = ["tang-port", "tang-port--param", ...(driven ? ["tang-port--driven"] : []), ...(takes ? ["tang-port--takes"] : [])];
  return <Handle isConnectableStart={false} type="target" position={Position.Top} id={handle} className={classes.join(" ")} />;
}

/**
 * The place on the left edge of a row of a modulator's node where a cable from another modulator ends, to drive how deep the cable of that row
 * goes (`depth` of the cable `edge`, which starts at the modulator `node`): a handle named after the cable (`depthHandle`), quiet until a cable ends
 * at it (`driven`), and, while a cable is dragged, saying whether it takes that one. A cable cannot be dropped on it yet.
 */
export function DepthPort({ node, edge, driven }: { node: NodeId; edge: EdgeId; driven: boolean }) {
  const handle = depthHandle(edge);
  const takes = useContext(ConnectingContext)?.has(keyOf(node, handle)) ?? false;
  const classes = ["tang-port", "tang-port--cable", ...(driven ? ["tang-port--driven"] : []), ...(takes ? ["tang-port--takes"] : [])];
  return <Handle isConnectable={false} type="target" position={Position.Left} id={handle} className={classes.join(" ")} />;
}

/**
 * A parameter in a node with a place on its top edge where a cable from a modulator ends. (That the control is
 * there when a cable ends at it is for `toFlow` to see to: a parameter that is driven is always among those a node shows.)
 */
export const ParamCell = memo(function ParamCell({ node, index, driven }: { node: NodeId; index: number; driven: boolean }) {
  return (
    <div className="tang-cell">
      <CablePort node={node} handle={paramHandle(index)} driven={driven} />
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

/**
 * A setting of a modulator, in a cell of the node's grid: a choice for the waveform, which no cable ends at, and for the rest a
 * knob with a place on its top edge where a cable from another modulator ends (`driven`: one does).
 */
export const ModulatorSettingCell = memo(function ModulatorSettingCell({ node, field, driven }: { node: NodeId; field: ModulatorField; driven: boolean }) {
  if (field === "waveform") {
    return (
      <div className="tang-cell">
        <WaveformControl node={node} />
      </div>
    );
  }
  return (
    <div className="tang-cell">
      <CablePort node={node} handle={field} driven={driven} />
      <ModulatorFieldControl node={node} field={field} />
    </div>
  );
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

/** The `id` of the button of the list that draws a cable from the modulator `node`, which what takes a cable out gives the focus to. */
export const drivesId = (node: NodeId): string => `drives-${String(node)}`;

/**
 * The list that draws a cable from a modulator to any parameter it can still drive, plugin by plugin, and to any setting
 * of another modulator, modulator by modulator: for a keyboard (which cannot drag a cable), and for the parameters a node
 * does not show as controls. It is a menu and not a native list: the arrow keys of a closed one change its value on some
 * platforms, which here is a cable on each key. Choosing a parameter or a setting is the cable; the menu closes and the
 * focus is on its button.
 */
export const DrivePicker = memo(function DrivePicker({ modulator, name, targets }: { modulator: NodeId; name: string; targets: readonly Drivable[] }) {
  const editor = useEditor();
  // What each item is the cable to: a node and a port of it, by the key of the port (`keyOf`).
  const { items, ends } = useMemo(() => {
    const items: MenuItem[] = [];
    const ends = new Map<string, { node: NodeId; port: Port }>();
    const add = (node: NodeId, port: Port, label: string, group: string): void => {
      const handle = handleOf(port);
      if (handle === null) return;
      const id = keyOf(node, handle);
      items.push({ id, label, group });
      ends.set(id, { node, port });
    };
    for (const group of targets) {
      for (const param of group.params) add(group.node, { type: "param", index: param.index }, param.name, group.name);
      for (const field of group.fields) add(group.node, field.port, field.name, group.name);
    }
    return { items, ends };
  }, [targets]);
  return (
    <MenuButton
      id={drivesId(modulator)}
      className="tang-drives"
      label={`Add a cable from ${name}`}
      items={items}
      onChoose={(choice) => {
        const end = ends.get(choice);
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        if (end) void editor.addCable(modulator, end.node, end.port);
      }}
    >
      {items.length === 0 ? "Nothing to drive" : "Add a cable…"}
    </MenuButton>
  );
});
