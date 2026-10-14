// The nodes of the canvas (docs/web-ui.md §9.3): what each kind of node of the session is
// drawn as. A plugin node is a function of its data and the controls in it read their own
// values, so that a value that changes renders its control and nothing else.
import { Handle, Position } from "@xyflow/react";
import type { NodeProps } from "@xyflow/react";
import { useContext, useMemo } from "react";
import type { ReactNode } from "react";

import { effectItems, instrumentItems } from "../../client/catalog";
import { slotReader } from "../../client/readers";
import { nodeById, paramCountOf, presetsOf } from "../../client/selectors";
import type { ModulatorKind, Node, NodeId, Preset } from "../../protocol";
import { LiveBar } from "../../ui/LiveBar";
import { MenuButton } from "../../ui/MenuButton";
import type { MenuItem } from "../../ui/MenuButton";
import { Meter } from "../../ui/Meter";
import { useClient, useClientSelection, useClientState, useEditor, useFrames } from "../ClientProvider";
import { focusWhenPossible } from "../focus";
import { DrivePicker, GainCell, ModulatorSettingCell, ParamCell, RangeCell } from "./controls";
import { controlDetail, kindOf, outputScale, settingsOf } from "./describe";
import { InspectorContext, paramsButtonId } from "./inspector";
import type { PatchNode } from "./toFlow";

function Header({ name, badge, children }: { name: string; badge?: string | null; children?: ReactNode }) {
  return (
    <header className="tang-node__header">
      <span className="tang-node__name" title={name}>
        {name}
      </span>
      {badge ? <span className="tang-node__badge">{badge}</span> : null}
      {children}
    </header>
  );
}

/** The kinds of modulator a lane or a group can be given, as the menu offers them. */
const MODULATOR_KINDS: readonly MenuItem<ModulatorKind>[] = [
  { id: "lfo", label: "LFO" },
  { id: "envelope", label: "Envelope" },
  { id: "midiLearn", label: "MIDI control" },
];

/** The `id` of the button that adds a modulator to the lane or group `node`, which what takes a modulator out gives the focus to. */
const addModulatorId = (node: NodeId): string => `add-modulator-${String(node)}`;

/** The button of a lane or a group that adds a modulator to it: a menu of the kinds. */
function AddModulatorMenu({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  return (
    <MenuButton
      id={addModulatorId(node)}
      label={`Add a modulator to ${name}`}
      items={MODULATOR_KINDS}
      onChoose={(kind) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addModulator(node, kind);
      }}
    >
      +
    </MenuButton>
  );
}

/** The `id` of the button that adds an effect to the lane or group `node`, which what takes an effect out gives the focus to. */
const addEffectId = (node: NodeId): string => `add-effect-${String(node)}`;

/** The button of a lane or a group that puts an effect at the end of its chain: a menu of what is installed, under the format of each. */
function AddEffectMenu({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const catalog = useClientState((state) => state.catalog);
  const items = useMemo(() => effectItems(catalog?.entries ?? []), [catalog]);
  return (
    <MenuButton
      id={addEffectId(node)}
      label={`Add an effect to ${name}`}
      className="tang-node__fx"
      items={items}
      onChoose={(plugin) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addEffect(node, plugin);
      }}
    >
      fx
    </MenuButton>
  );
}

/** The `id` of the button that changes the instrument of the lane `node`, which the focus stays on (the lane's node stays). */
const changeInstrumentId = (node: NodeId): string => `change-instrument-${String(node)}`;

/**
 * The button of a lane that puts another instrument in it: a menu of what is installed and can be one, under the format of each.
 * An empty lane (the TUI can empty one) is asked to be given one. The lane keeps its effects, modulators, range and volume; the
 * cables that drove the old instrument's parameters go with it.
 */
function ChangeInstrumentMenu({ node, name, empty }: { node: NodeId; name: string; empty: boolean }) {
  const editor = useEditor();
  const catalog = useClientState((state) => state.catalog);
  const items = useMemo(() => instrumentItems(catalog?.entries ?? []), [catalog]);
  return (
    <MenuButton
      id={changeInstrumentId(node)}
      label={empty ? `Choose an instrument for ${name}` : `Change the instrument of ${name}`}
      className="tang-node__instrument"
      items={items}
      onChoose={(plugin) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.replaceInstrument(node, plugin);
      }}
    >
      ♪
    </MenuButton>
  );
}

/** (The same empty list every time, for what depends on one.) */
const NO_PRESETS: readonly Preset[] = [];

/** Whether two answers of `presetsOf` are the same list and the same preset: what the menu depends on, and not the values of the plugin. */
const samePresets = (a: ReturnType<typeof presetsOf>, b: ReturnType<typeof presetsOf>): boolean =>
  a === b || (a !== null && b !== null && a.presets === b.presets && a.current === b.current);

/**
 * The button of an instrument, or of an effect of a lane, that loads one of its plugin's presets: a menu of their names, the one that was
 * loaded last marked, and the menu opening on it. Nothing for a plugin that has no presets, and for a bus effect (the audio thread
 * loads a preset by a lane's slot). The parameters take the preset's values when the server says it was loaded.
 */
function PresetMenu({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const found = useClientSelection((state) => presetsOf(state, node), samePresets);
  const presets = found?.presets ?? NO_PRESETS;
  const current = found?.current ?? null;
  const items = useMemo<MenuItem[]>(() => presets.map((preset) => ({ id: preset.id, label: preset.name, current: preset.name === current })), [presets, current]);
  if (items.length === 0) return null;
  return (
    <MenuButton
      id={`presets-${String(node)}`}
      label={`Load a preset into ${name}`}
      className="tang-node__presets"
      items={items}
      onChoose={(preset) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.loadPreset(node, preset);
      }}
    >
      ≡
    </MenuButton>
  );
}

/**
 * The button of an instrument or an effect that puts all the parameters of its plugin in the panel (the node has room for a few
 * knobs): pressed again, it takes the panel away. Nothing for a plugin that has no parameters.
 */
function ParamsButton({ node, name }: { node: NodeId; name: string }) {
  const inspector = useContext(InspectorContext);
  const count = useClientState((state) => paramCountOf(state, node));
  if (count === 0) return null;
  return (
    <button
      type="button"
      id={paramsButtonId(node)}
      className="tang-menu-button tang-node__params nodrag nopan"
      aria-label={`Parameters of ${name}`}
      title={`All the parameters of ${name}`}
      aria-expanded={inspector.inspected === node}
      onClick={() => {
        inspector.toggle(node);
      }}
    >
      ⚙
    </button>
  );
}

/** The button of a modulator or an effect that takes it out, with the cables that drive it; the focus goes to the button that adds one like it to its lane or group. */
function RemoveNodeButton({ node, name, parent, focusTo }: { node: NodeId; name: string; parent: NodeId | null; focusTo: (parent: NodeId) => string }) {
  const editor = useEditor();
  return (
    <button
      type="button"
      className="tang-node__remove nodrag nopan"
      aria-label={`Remove ${name}`}
      title={`Remove ${name}`}
      onClick={() => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.removeNode(node).then((removed) => {
          if (removed && parent !== null) focusWhenPossible(focusTo(parent));
        });
      }}
    >
      ×
    </button>
  );
}

/** An instrument or an effect: its plugin's name and format, the parameters it shows, and its own gain. */
export function PluginNode({ data, type }: NodeProps<PatchNode>) {
  const effect = type === "effect";
  // (Where an effect is: the lane or group whose chain it is in, which the focus goes back to when it is taken out.)
  const parent = useClientState((state) => (effect ? (nodeById(state, data.node)?.parent ?? null) : null));
  return (
    <div className={`tang-node tang-node--${type}`}>
      {effect ? <Handle isConnectable={false} type="target" position={Position.Left} id="audioIn" className="tang-port tang-port--audio" /> : null}
      <Handle isConnectable={false} type="source" position={Position.Right} id="audioOut" className="tang-port tang-port--audio" />
      <Header name={data.name} badge={data.format}>
        {effect ? (
          <span className="tang-node__actions">
            <PresetMenu node={data.node} name={data.name} />
            <ParamsButton node={data.node} name={data.name} />
            <RemoveNodeButton node={data.node} name={data.name} parent={parent} focusTo={addEffectId} />
          </span>
        ) : (
          <span className="tang-node__actions">
            <ChangeInstrumentMenu node={data.node} name={data.name} empty={data.format === null} />
            <PresetMenu node={data.node} name={data.name} />
            <ParamsButton node={data.node} name={data.name} />
            <AddEffectMenu node={data.node} name={data.name} />
            <AddModulatorMenu node={data.node} name={data.name} />
          </span>
        )}
      </Header>
      <div className="tang-node__controls nodrag nopan nowheel">
        {data.pinned.map((index) => (
          <ParamCell key={index} node={data.node} index={index} driven={data.modulated.includes(index)} />
        ))}
        <GainCell node={data.node} gain={effect ? "mix" : "volume"} />
        <RangeCell node={data.node} name={data.name} />
      </div>
    </div>
  );
}

/**
 * Whether two nodes are the same modulator as far as its node is drawn: of the same kind, and a
 * control bound to the same thing. Its settings are not part of it: each control reads its own,
 * so that turning one renders that control and not the node.
 */
export function sameModulatorShape(a: Node | undefined, b: Node | undefined): boolean {
  if (a === b) return true;
  if (a?.kind !== "modulator" || b?.kind !== "modulator" || a.source.type !== b.source.type) return false;
  return a.source.type !== "midiLearn" || JSON.stringify(a.source) === JSON.stringify(b.source);
}

/** A modulator: what it is, its settings as controls, and what it puts out as it does so. */
export function ModulatorNode({ data }: NodeProps<PatchNode>) {
  const client = useClient();
  const frames = useFrames();
  const node = useClientSelection((state) => nodeById(state, data.node), sameModulatorShape);
  const read = useMemo(() => slotReader(client.telemetry, "modulation", data.node, { type: "modOut" }, "modOutput"), [client, data.node]);
  const source = node?.kind === "modulator" ? node.source : null;
  const scale = source ? outputScale(source) : { min: 0, max: 1, centred: false };
  // (An LFO is called what it is: its kind is not said twice.)
  const kind = source ? kindOf(source) : null;
  const settings = source ? settingsOf(source) : [];
  return (
    <div className="tang-node tang-node--modulator">
      <Header name={data.name} badge={kind === data.name || data.name.startsWith(`${kind ?? ""} `) ? null : kind}>
        <RemoveNodeButton node={data.node} name={data.name} parent={node?.parent ?? null} focusTo={addModulatorId} />
      </Header>
      {settings.length > 0 ? (
        <div className="tang-node__controls nodrag nopan nowheel">
          {settings.map((field) => (
            <ModulatorSettingCell key={field} node={data.node} field={field} driven={data.drivenSettings.some((setting) => setting === field)} />
          ))}
        </div>
      ) : null}
      <div className="tang-node__body">
        {source?.type === "midiLearn" ? <p className="tang-node__detail">{controlDetail(source)}</p> : null}
        <LiveBar label={`${data.name} output`} read={read} min={scale.min} max={scale.max} centred={scale.centred} frames={frames} />
        <div className="tang-node__drives nodrag nopan nowheel">
          <DrivePicker modulator={data.node} name={data.name} targets={data.targets} />
        </div>
      </div>
      <Handle type="source" position={Position.Bottom} id="modOut" className="tang-port tang-port--mod" />
    </div>
  );
}

/** A submix group: its volume and its pan, between its members and the master. */
export function GroupNode({ data }: NodeProps<PatchNode>) {
  return (
    <div className="tang-node tang-node--group">
      <Handle isConnectable={false} type="target" position={Position.Left} id="audioIn" className="tang-port tang-port--audio" />
      <Handle isConnectable={false} type="source" position={Position.Right} id="audioOut" className="tang-port tang-port--audio" />
      <Header name={data.name} badge="Group">
        <span className="tang-node__actions">
          <AddEffectMenu node={data.node} name={data.name} />
          <AddModulatorMenu node={data.node} name={data.name} />
        </span>
      </Header>
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
