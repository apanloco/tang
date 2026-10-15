// The nodes of the canvas (docs/web-ui.md §9.3): what each kind of node of the session is
// drawn as. A plugin node is a function of its data and the controls in it read their own
// values, so that a value that changes renders its control and nothing else.
import { Handle, Position } from "@xyflow/react";
import type { NodeProps } from "@xyflow/react";
import { useContext, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

import { effectItems, instrumentItems } from "../../client/catalog";
import { slotReader } from "../../client/readers";
import { chainPlaceOf, envelopeStagesOf, nodeById, paramCountOf, presetsOf, removalOf, sameChainPlace, sameRemoval, sameStages } from "../../client/selectors";
import type { Removal } from "../../client/selectors";
import { NAME_LIMIT } from "../../protocol";
import type { ModulatorKind, Node, NodeId, Preset } from "../../protocol";
import { formatValue } from "../../ui/Knob";
import { LiveEnvelope } from "../../ui/LiveEnvelope";
import { LiveTrace } from "../../ui/LiveTrace";
import { MenuButton } from "../../ui/MenuButton";
import type { MenuItem } from "../../ui/MenuButton";
import { Meter } from "../../ui/Meter";
import { useClient, useClientSelection, useClientState, useEditor, useFrames } from "../ClientProvider";
import { focusWhenPossible, keepFocus } from "../focus";
import { DepthPort, DrivePicker, GainCell, GroupCell, ModulatorSettingCell, ParamCell, RangeCell } from "./controls";
import { controlDetail, kindOf, outputScale, seconds, settingsOf } from "./describe";
import { InspectorContext, paramsButtonId } from "./inspector";
import { ModulateMenu } from "./ModulateMenu";
import type { CableRow, PatchNode } from "./toFlow";

/** The header of a node: its name (or, for a node whose name is edited in place, `field`), its badge and its buttons. */
function Header({ name, badge, children, field }: { name: string; badge?: string | null; children?: ReactNode; field?: ReactNode }) {
  return (
    <header className="tang-node__header">
      {field ?? (
        <span className="tang-node__name" title={name}>
          {name}
        </span>
      )}
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

/** The `id` of the button that moves the effect `node` one place earlier or later in its chain, which the focus is kept on. */
const moveEffectId = (node: NodeId, direction: "earlier" | "later"): string => `move-${direction}-${String(node)}`;

/**
 * The two buttons of an effect that move it one place along its chain: earlier (towards the instrument or the group) and later (towards the master).
 * The audio goes through the effects in the order of the chain, a lane's or a bus's alike; the cables that drive the plugins stay on them. The
 * button of a direction the effect cannot go (the first effect has no place earlier, the last none later) says so and does nothing, and keeps the
 * focus where it was, which a button that was taken away or disabled would not. The canvas puts the effect's node in its new place among the
 * others, and a browser lets go of the focus of an element that is moved in the page (of the node that is put after the one it was before, not of
 * the one that stays): a button that was pressed with the focus on it is given it again.
 */
function MoveEffectButtons({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const place = useClientSelection((state) => chainPlaceOf(state, node), sameChainPlace);
  if (!place) return null;
  const buttons = [
    { direction: "earlier", glyph: "‹", step: -1, available: place.index > 0 },
    { direction: "later", glyph: "›", step: 1, available: place.index < place.count - 1 },
  ] as const;
  return (
    <>
      {buttons.map(({ direction, glyph, step, available }) => (
        <button
          key={direction}
          id={moveEffectId(node, direction)}
          type="button"
          className="tang-menu-button tang-node__move nodrag nopan"
          aria-label={`Move ${name} ${direction} in its chain`}
          title={`Move ${name} ${direction} in its chain`}
          aria-disabled={!available}
          onClick={(event) => {
            if (!available) return;
            const focused = document.activeElement === event.currentTarget;
            // (If it is refused the page says so: `Editor` puts the reason among the notices.)
            void editor.moveEffect(node, place.index + step).then((moved) => {
              if (moved && focused) keepFocus(moveEffectId(node, direction));
            });
          }}
        >
          {glyph}
        </button>
      ))}
    </>
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

/** "1 effect", "2 effects". */
const counted = (n: number, word: string): string => `${String(n)} ${word}${n === 1 ? "" : "s"}`;

/** What taking out a lane or a group comes to, in words: what goes with it, and, for a group, what stays. */
function removalWords(removal: Removal): string {
  const goes = [removal.effects > 0 ? counted(removal.effects, "effect") : null, removal.modulators > 0 ? counted(removal.modulators, "modulator") : null].filter((part) => part !== null);
  const stays = removal.lanes > 0 ? `its ${counted(removal.lanes, "lane")} ${removal.lanes === 1 ? "stays" : "stay"}, in no group` : null;
  const parts = [goes.length > 0 ? `its ${goes.join(" and ")} go${goes.length === 1 && removal.effects + removal.modulators === 1 ? "es" : ""} with it` : null, stays].filter((part) => part !== null);
  return parts.length > 0 ? `: ${parts.join(", and ")}` : "";
}

/**
 * The button of a lane or a group that takes it out of the session, for good: there is no undo, so it opens a menu that asks, first for keeping
 * (where the focus starts), and then for removing it with the words of what goes with it and what stays. Nothing for the only lane of a session (a
 * session keeps a lane) and for any other node. The focus goes to the button of the bar that adds a lane or a group, which is what is left to do.
 */
function RemoveLaneOrGroupMenu({ node }: { node: NodeId }) {
  const editor = useEditor();
  const removal = useClientSelection((state) => removalOf(state, node), sameRemoval);
  if (!removal || removal.last) return null;
  const what = removal.kind === "lane" ? `the lane of ${removal.name}` : removal.name;
  const items: MenuItem<"keep" | "remove">[] = [
    { id: "keep", label: `Keep ${removal.name}` },
    { id: "remove", label: `Remove ${what} for good${removalWords(removal)}` },
  ];
  return (
    <MenuButton
      id={`remove-${String(node)}`}
      label={`Remove ${what}`}
      className="tang-node__remove-menu"
      items={items}
      onChoose={(choice) => {
        if (choice !== "remove") return;
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.removeNode(node).then((removed) => {
          if (removed) focusWhenPossible(removal.kind === "lane" ? "add-instrument" : "add-group");
        });
      }}
    >
      ×
    </MenuButton>
  );
}

/** The `id` of the button that turns the name of the group `node` into a field. */
const renameGroupId = (node: NodeId): string => `rename-group-${String(node)}`;

/**
 * The name of a group, in a field, in place of the name in its header: Enter (or leaving the field) asks the server for it, Escape leaves it as it
 * was, and a name that is the one it was asks nothing. The server takes the spaces off, and gives a name of nothing the name the group has by
 * default. `done` is called once, with whether the focus goes back to the button that opened the field (it does, for Enter and Escape: the
 * person is still there).
 */
function GroupNameField({ node, name, done }: { node: NodeId; name: string; done: (focusBack: boolean) => void }) {
  const editor = useEditor();
  const finished = useRef(false);
  const [text, setText] = useState(name);
  const finish = (ask: boolean, focusBack: boolean): void => {
    if (finished.current) return;
    finished.current = true;
    // (If it is refused the page says so: `Editor` puts the reason among the notices.)
    if (ask && text !== name) void editor.renameGroup(node, text);
    done(focusBack);
  };
  return (
    <input
      className="tang-node__name-field nodrag nopan"
      type="text"
      aria-label={`Name of ${name}`}
      maxLength={NAME_LIMIT}
      value={text}
      autoFocus
      onFocus={(event) => {
        event.currentTarget.select();
      }}
      onChange={(event) => {
        setText(event.target.value);
      }}
      onKeyDown={(event) => {
        if (event.key !== "Enter" && event.key !== "Escape") return;
        // The key is taken here: the focus goes to the button at once, and a browser gives an Enter that was left alone to whatever has the
        // focus when it comes to act on it, which would press the button and open the field again.
        event.preventDefault();
        finish(event.key === "Enter", true);
      }}
      onBlur={() => {
        finish(true, false);
      }}
    />
  );
}

/** The button of a group that turns its name into a field. */
function RenameGroupButton({ node, name, onEdit }: { node: NodeId; name: string; onEdit: () => void }) {
  return (
    <button type="button" id={renameGroupId(node)} className="tang-menu-button tang-node__rename nodrag nopan" aria-label={`Rename ${name}`} title={`Rename ${name}`} onClick={onEdit}>
      ✎
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
            <MoveEffectButtons node={data.node} name={data.name} />
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
            <RemoveLaneOrGroupMenu node={data.node} />
          </span>
        )}
      </Header>
      <div className="tang-node__controls nodrag nopan nowheel">
        {data.knobs.map((index) => (
          <ParamCell key={index} node={data.node} index={index} driven={data.modulated.includes(index)} />
        ))}
        <GainCell node={data.node} gain={effect ? "mix" : "volume"} />
        <RangeCell node={data.node} name={data.name} />
        <GroupCell node={data.node} name={data.name} />
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

/**
 * The shape an envelope's four settings make, with a dot where the envelope is while notes are played. It reads the settings itself, so that
 * turning one draws the shape again and nothing else of the node.
 */
function EnvelopeShape({ node, name, read }: { node: NodeId; name: string; read: () => number | undefined }) {
  const frames = useFrames();
  const stages = useClientSelection((state) => envelopeStagesOf(state, node), sameStages);
  if (!stages) return null;
  const words = `attack ${seconds(stages.attack)}, decay ${seconds(stages.decay)}, sustain ${formatValue(stages.sustain)}, release ${seconds(stages.release)}`;
  return <LiveEnvelope label={`${name} shape: ${words}`} stages={stages} read={read} frames={frames} />;
}

/**
 * The cables that start at a modulator, as a list at the foot of its node: a row for each, saying where it goes, with a port at the left edge of the
 * row where a cable from another modulator can end, to drive how deep this one goes, and the button that modulates the depth (`ModulateMenu`: a new modulator
 * for it, or one that is there). Nothing for a modulator that drives nothing.
 */
function CableRows({ node, name, cables }: { node: NodeId; name: string; cables: readonly CableRow[] }) {
  if (cables.length === 0) return null;
  return (
    <ul className="tang-node__cables" aria-label={`Cables of ${name}`}>
      {cables.map((row) => (
        <li key={row.edge} className={`tang-cable-row${row.driven ? " tang-cable-row--driven" : ""}`}>
          <DepthPort node={node} edge={row.edge} driven={row.driven} />
          <span className="tang-cable-row__label" title={row.label}>
            {row.label}
          </span>
          <ModulateMenu node={node} port={{ type: "depth", edge: row.edge }} name={`depth of ${name} → ${row.label}`} className="tang-modulate--cable" />
        </li>
      ))}
    </ul>
  );
}

/** A modulator: what it is, its settings as controls, and what it puts out, as a line that scrolls (an envelope has its shape too). */
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
        {source?.type === "envelope" ? <EnvelopeShape node={data.node} name={data.name} read={read} /> : null}
        <LiveTrace label={`${data.name} output`} read={read} min={scale.min} max={scale.max} centred={scale.centred} frames={frames} />
        <div className="tang-node__drives nodrag nopan nowheel">
          <DrivePicker modulator={data.node} name={data.name} targets={data.targets} />
        </div>
        <CableRows node={data.node} name={data.name} cables={data.cables} />
      </div>
      <Handle type="source" position={Position.Bottom} id="modOut" className="tang-port tang-port--mod" />
    </div>
  );
}

/** A submix group: its volume and its pan, between its members and the master; its name is changed in place, and it can be taken out. */
export function GroupNode({ data }: NodeProps<PatchNode>) {
  const [editing, setEditing] = useState(false);
  return (
    <div className="tang-node tang-node--group">
      <Handle isConnectable={false} type="target" position={Position.Left} id="audioIn" className="tang-port tang-port--audio" />
      <Handle isConnectable={false} type="source" position={Position.Right} id="audioOut" className="tang-port tang-port--audio" />
      <Header
        name={data.name}
        badge="Group"
        field={
          editing ? (
            <GroupNameField
              node={data.node}
              name={data.name}
              done={(focusBack) => {
                setEditing(false);
                if (focusBack) focusWhenPossible(renameGroupId(data.node));
              }}
            />
          ) : undefined
        }
      >
        <span className="tang-node__actions">
          <RenameGroupButton
            node={data.node}
            name={data.name}
            onEdit={() => {
              setEditing(true);
            }}
          />
          <AddEffectMenu node={data.node} name={data.name} />
          <AddModulatorMenu node={data.node} name={data.name} />
          <RemoveLaneOrGroupMenu node={data.node} />
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
