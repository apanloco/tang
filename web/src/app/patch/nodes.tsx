// The nodes of the canvas (docs/web-ui.md §9.3): what each kind of node of the session is
// drawn as. A plugin node is a function of its data and the controls in it read their own
// values, so that a value that changes renders its control and nothing else.
import { Handle, Position } from "@xyflow/react";
import type { NodeProps } from "@xyflow/react";
import { useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";

import { effectItems, instrumentItems } from "../../client/catalog";
import { inputsStoodFor } from "../../client/inputs";
import { slotReader } from "../../client/readers";
import { audibleAt, chainPlaceOf, envelopeStagesOf, nodeById, paramCountOf, presetsOf, removalOf, sameChainPlace, sameRemoval, sameStages } from "../../client/selectors";
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
import { AudioPort, DepthPort, DrivePicker, FeedButton, GainCell, GroupCell, ModulatorSettingCell, MuteSoloCell, NotesPort, ParamCell, RangeCell, StepsControl, SyncControl } from "./controls";
import { controlDetail, kindOf, outputScale, seconds, settingsOf } from "./describe";
import { InspectorContext, paramsButtonId } from "./inspector";
import { ModulateMenu } from "./ModulateMenu";
import { NodeScope, ScopeButton, ScopePanel } from "./NodeScope";
import { NotesFromMenu, NotesToMenu } from "./NotesMenus";
import { PatternCell } from "./PatternCell";
import { SendMenu } from "./SendMenu";
import type { CableRow, PatchNode } from "./toFlow";

/**
 * The header of a node: its name (or, for a node whose name is edited in place, `field`), its badge and its buttons. The badge, what kind of plugin it is, is under the name, in small type: beside it,
 * it left the name 40 pixels in a lane that has a number in its name and a "×" ("Os…"), and 30 in a lane of Pianoteq 9 ("Pian…"). `sub` is what goes after the badge on that line (the button that
 * sends the audio of the node: a lane's header has no room in its row of buttons for another).
 */
function Header({ name, badge, sub, children, field }: { name: string; badge?: string | null; sub?: ReactNode; children?: ReactNode; field?: ReactNode }) {
  const badged = badge ? <span className="tang-node__badge">{badge}</span> : null;
  return (
    <header className="tang-node__header">
      {field ? (
        <>
          {field}
          {badged}
        </>
      ) : (
        <span className="tang-node__title">
          <span className="tang-node__name" title={name}>
            {name}
          </span>
          {badged}
          {sub}
        </span>
      )}
      {children}
    </header>
  );
}

/** The kinds of modulator a lane or a group can be given, as the menu offers them. */
const MODULATOR_KINDS: readonly MenuItem<ModulatorKind>[] = [
  { id: "lfo", label: "LFO" },
  { id: "envelope", label: "Envelope" },
  { id: "macro", label: "Macro" },
  { id: "midiLearn", label: "MIDI control" },
  { id: "velocity", label: "Velocity" },
  { id: "key", label: "Key" },
  { id: "steps", label: "Step sequencer" },
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

/**
 * The button of a modulator, an effect or a source that takes it out, with the cables that drive it (or, for a source, the cables of notes from it); the focus goes to `focusTo`, the button that adds one
 * like it (to its lane or group, or to the session), if there is one to go to.
 */
function RemoveNodeButton({ node, name, focusTo }: { node: NodeId; name: string; focusTo: string | null }) {
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
          if (removed && focusTo !== null) focusWhenPossible(focusTo);
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

/** What stays in a lane when its instrument is taken out of it, in words: its effects and its modulators, or that it is left empty. */
function keptWords(removal: Removal): string {
  const stays = [removal.effects > 0 ? counted(removal.effects, "effect") : null, removal.modulators > 0 ? counted(removal.modulators, "modulator") : null].filter((part) => part !== null);
  return stays.length > 0 ? `: its ${stays.join(" and ")} stay${removal.effects + removal.modulators === 1 ? "s" : ""} in the lane` : ": the lane stays, empty";
}

/**
 * The button of a lane or a group that takes it out of the session: a menu that asks, first for keeping (where the focus starts), and then for removing it with the words of what goes with it
 * and what stays. A lane that has an instrument is asked for twice over: for taking the instrument out alone, which leaves the lane with its effects, its modulators and the rest (an empty lane,
 * which the focus goes to the button that puts an instrument in), and for taking the lane out, for good, with all of it, which is what goes to the button of the bar that adds a lane. The only lane
 * of a session cannot be taken out (a session keeps a lane), but its instrument can; and nothing for any other node.
 */
function RemoveLaneOrGroupMenu({ node }: { node: NodeId }) {
  const editor = useEditor();
  const removal = useClientSelection((state) => removalOf(state, node), sameRemoval);
  if (!removal || (removal.last && !removal.instrument)) return null;
  const what = removal.kind === "lane" ? `the lane of ${removal.name}` : removal.name;
  const items: MenuItem<"keep" | "instrument" | "remove">[] = [
    { id: "keep", label: `Keep ${removal.name}` },
    ...(removal.instrument ? [{ id: "instrument" as const, label: `Remove ${removal.name} only${keptWords(removal)}` }] : []),
    ...(removal.last ? [] : [{ id: "remove" as const, label: `Remove ${what} for good${removalWords(removal)}` }]),
  ];
  return (
    <MenuButton
      id={`remove-${String(node)}`}
      label={`Remove ${what}`}
      className="tang-node__remove-menu"
      items={items}
      onChoose={(choice) => {
        if (choice === "keep") return;
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        if (choice === "instrument") {
          void editor.clearInstrument(node).then((cleared) => {
            if (cleared) focusWhenPossible(changeInstrumentId(node));
          });
          return;
        }
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

/**
 * The class of a node that cannot be heard at the master (`audibleAt`): a lane or a group that is muted, or not soloed while something is, and the effects in the chain of
 * one. It is dimmed, and still works.
 */
function useUnheard(node: NodeId): string {
  return useClientState((state) => audibleAt(state, node) === false) ? " tang-node--unheard" : "";
}

/** An instrument or an effect: its plugin's name and format, the parameters it shows, and its own gain; a lane's keys, group, mute and solo as well. */
export function PluginNode({ data, type }: NodeProps<PatchNode>) {
  const effect = type === "effect";
  // (A lane that has no instrument has no sound to look at.)
  const silent = !effect && data.format === null;
  // (Where an effect is: the lane or group whose chain it is in, which the focus goes back to when it is taken out.)
  const parent = useClientState((state) => (effect ? (nodeById(state, data.node)?.parent ?? null) : null));
  const unheard = useUnheard(data.node);
  return (
    <div className={`tang-node tang-node--${type}${unheard}`}>
      {/* (An effect takes audio in; a lane takes notes in, from the sources.) */}
      {effect ? <AudioPort node={data.node} direction="in" /> : <NotesPort node={data.node} direction="in" />}
      <AudioPort node={data.node} direction="out" />
      <Header
        name={data.name}
        badge={data.format}
        sub={
          <>
            {silent ? null : <SendMenu node={data.node} name={data.name} />}
            {effect ? <FeedButton node={data.node} name={data.name} /> : <NotesFromMenu node={data.node} name={data.name} />}
          </>
        }
      >
        {effect ? (
          <span className="tang-node__actions">
            <MoveEffectButtons node={data.node} name={data.name} />
            <PresetMenu node={data.node} name={data.name} />
            <ParamsButton node={data.node} name={data.name} />
            <ScopeButton node={data.node} name={data.name} />
            <RemoveNodeButton node={data.node} name={data.name} focusTo={parent !== null ? addEffectId(parent) : null} />
          </span>
        ) : (
          <span className="tang-node__actions">
            <ChangeInstrumentMenu node={data.node} name={data.name} empty={data.format === null} />
            <PresetMenu node={data.node} name={data.name} />
            <ParamsButton node={data.node} name={data.name} />
            {silent ? null : <ScopeButton node={data.node} name={data.name} />}
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
        <MuteSoloCell node={data.node} name={data.name} />
      </div>
      {/* (A node that is no lane has no pattern: the cell draws nothing for it.) */}
      <PatternCell node={data.node} name={data.name} />
      {data.scope ? <NodeScope node={data.node} name={data.name} /> : null}
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
        {/* (The note length that an LFO or a step sequencer follows the tempo by: no other kind has one, and the control is nothing for it.) */}
        <SyncControl node={data.node} />
        <RemoveNodeButton node={data.node} name={data.name} focusTo={node?.parent != null ? addModulatorId(node.parent) : null} />
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
        {source?.type === "steps" ? <StepsControl node={data.node} name={data.name} /> : null}
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

/** A submix group: its volume and its pan, between its members and the master, and its mute and solo; its name is changed in place, and it can be taken out. */
export function GroupNode({ data }: NodeProps<PatchNode>) {
  const [editing, setEditing] = useState(false);
  const unheard = useUnheard(data.node);
  return (
    <div className={`tang-node tang-node--group${unheard}`}>
      <AudioPort node={data.node} direction="in" />
      <AudioPort node={data.node} direction="out" />
      <Header
        name={data.name}
        badge="Group"
        sub={<SendMenu node={data.node} name={data.name} />}
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
          <ScopeButton node={data.node} name={data.name} />
          <AddEffectMenu node={data.node} name={data.name} />
          <AddModulatorMenu node={data.node} name={data.name} />
          <RemoveLaneOrGroupMenu node={data.node} />
        </span>
      </Header>
      <div className="tang-node__controls nodrag nopan nowheel">
        <GainCell node={data.node} gain="groupVolume" />
        <GainCell node={data.node} gain="groupPan" />
        <MuteSoloCell node={data.node} name={data.name} />
      </div>
      {data.scope ? <NodeScope node={data.node} name={data.name} /> : null}
    </div>
  );
}

/** The master: where the audio of the session ends, with its level and a scope of what comes out (a wave, or a spectrum), which the server is asked for while the master is here. */
export function MasterNode({ data }: NodeProps<PatchNode>) {
  const client = useClient();
  const frames = useFrames();
  useEffect(() => client.watchTelemetry("scope"), [client]);
  const readPeak = useMemo(() => slotReader(client.telemetry, "meters", data.node, { type: "audioIn" }, "peak"), [client, data.node]);
  const readClips = useMemo(() => slotReader(client.telemetry, "meters", data.node, { type: "audioIn" }, "clips"), [client, data.node]);
  const readScope = useCallback(() => client.telemetry.values("scope"), [client]);
  const readScopeFrames = useCallback(() => client.telemetry.framesOf("scope"), [client]);
  return (
    <div className="tang-node tang-node--master">
      <AudioPort node={data.node} direction="in" />
      <Header name={data.name} />
      <div className="tang-node__body">
        <Meter label="Master level" readPeak={readPeak} readClips={readClips} frames={frames} />
        <ScopePanel label="Output scope" read={readScope} readFrames={readScopeFrames} />
      </div>
    </div>
  );
}

/**
 * A source of notes: the keys (the keys of the computer and the page's piano) or a device, which the session calls by the name of a MIDI input, or by as much of it as tells it. It says what it is and, for a
 * device, which of the inputs that are open it stands for, or that none is. Notes leave it at its output of events: to the lanes that no cable of notes ends at, which hear every source, and along the cables
 * that are drawn from it; its menu draws one to a lane, and its button takes it out, with the cables of notes it has.
 */
export function SourceNode({ data }: NodeProps<PatchNode>) {
  const inputs = useClientState((state) => state.inputs);
  const device = data.source?.type === "device" ? data.source : null;
  const stands = device ? inputsStoodFor(device.name, inputs) : [];
  const detail = device ? (stands.length > 0 ? `Plugged in: ${stands.join(", ")}` : "Not plugged in") : "Computer keys and the piano";
  return (
    <div className="tang-node tang-node--source">
      <NotesPort node={data.node} direction="out" />
      <Header name={data.name} badge={device ? "MIDI input" : "Keyboard"} sub={<NotesToMenu node={data.node} name={data.name} />}>
        <span className="tang-node__actions">
          <RemoveNodeButton node={data.node} name={data.name} focusTo="add-source" />
        </span>
      </Header>
      <div className="tang-node__body">
        <p className="tang-node__detail tang-node__detail--source" title={stands.join(", ")}>
          {detail}
        </p>
      </div>
    </div>
  );
}
