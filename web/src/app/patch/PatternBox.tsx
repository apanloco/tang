// The pattern box on the canvas (docs/patch-graph.md §11.6, P4h-C part C; the user's "I want them to be their own boxes, think of a box that stores midi and we can record into them, edit them, send them to different
// targets"): a node that stores notes in beats and sends them to the lanes it has a cable to. Notes come in at its port on the left (from the keys and the devices: a box that has no cable of notes in records them
// all), and go out at its port on the right (to the lanes, along the cables that are drawn from it). Its body is the pattern cell of a lane (record, play, clear and the length; the switches; a line that says what it
// is doing) with a picture of its notes between them: the roll that edits them is slice 2.
//
// What the box is doing and what it holds are read from the store here, and not from the node's data: the canvas keeps the structure of the session and leaves it as it was while a box records (`sameNodeStructure`
// leaves a box's notes, settings and status out on purpose, so that a recording does not lay the canvas out again).
import { memo, useCallback, useState } from "react";
import type { NodeProps } from "@xyflow/react";

import { BOX_FINISH_HINT, boxLine, boxPlayHint, boxRecordHint, boxStopHint, isRecording, notesWords } from "../../client/patternWords";
import type { PatternBoxNode } from "../../client/patternBoxes";
import { patternBoxOf, samePatternBox } from "../../client/selectors";
import { PATTERN_BOX_LENGTH_RANGE } from "../../protocol";
import type { NodeId, PatternBoxField } from "../../protocol";
import { MenuButton } from "../../ui/MenuButton";
import type { MenuItem } from "../../ui/MenuButton";
import { NumberField } from "../../ui/NumberField";
import { useClientSelection, useClientState, useEditor } from "../ClientProvider";
import { focusWhenPossible } from "../focus";
import { NotesPort } from "./controls";
import { Header, NameField, RemoveNodeButton, RenameButton } from "./nodes";
import { NotesFromMenu, NotesToMenu } from "./NotesMenus";
import { PatternRoll } from "./PatternRoll";
import type { PatchNode } from "./toFlow";

/** The `id` of the button in the bar that adds a pattern box, which the focus goes to when one is taken out (`app/AddPattern.tsx`). */
export const ADD_PATTERN_ID = "add-pattern";

/** How much an arrow key turns the length by, in beats, and how much with Shift. */
const STEP = 1;
const BIG_STEP = 4;

/** The `id` of the button that records, for the focus to go back to when the notes that were cleared leave nothing else of the box to have it. */
const recordId = (node: NodeId): string => `pattern-box-record-${String(node)}`;

/** The `id` of the button that turns the name of the box `node` into a field. */
const renameBoxId = (node: NodeId): string => `rename-pattern-${String(node)}`;

/** A switch of a box: what it is called, what it is for a person who has not used it, whether it is on, and what it is set to when it is pressed. */
interface Switch {
  text: string;
  says: (name: string) => string;
  title: string;
  on: (box: PatternBoxNode) => boolean;
  field: (on: boolean) => PatternBoxField;
}

const SWITCHES: readonly Switch[] = [
  {
    text: "Loop",
    says: (name) => `Loop ${name}`,
    title: "The box goes round: its notes play over and over (off: it plays them once and stops)",
    on: (box) => box.looping,
    field: (on) => ({ type: "looping", on }),
  },
  {
    text: "Overdub",
    says: (name) => `Overdub into ${name}`,
    title: "A recording is added to the notes the box has, and goes on with another pass until you finish it (off: a recording replaces them)",
    on: (box) => box.overdub,
    field: (on) => ({ type: "overdub", on }),
  },
  {
    text: "Click",
    says: (name) => `Click while ${name} records`,
    title: "A metronome clicks while the box counts in and records",
    on: (box) => box.click,
    field: (on) => ({ type: "click", on }),
  },
];

/** The switch that sets whose beat the box goes by: shown where the session has a host's tempo, or while the box goes by the host's already (so that it can be put back). */
const HOST: Switch = {
  text: "Host",
  says: (name) => `Follow the host's beat with ${name}`,
  title: "The box goes by the host's beat and not by its own clock: it plays while the host plays, and records from the start of its loop (off: its own clock)",
  on: (box) => box.sync === "host",
  field: (on) => ({ type: "sync", sync: on ? "host" : "own" }),
};

/** The controls of a box, and what it holds and is doing. */
const BoxBody = memo(function BoxBody({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const box = useClientSelection((state) => patternBoxOf(state, node), samePatternBox);
  const hostTempo = useClientState((state) => state.snapshot?.meta.hostTempo === true);
  // (A node that is no box has nothing to draw.)
  if (box === undefined) return null;
  const recording = isRecording(box.status);
  const playing = box.status !== "stopped";
  const words = boxLine(box);
  const switches = hostTempo || box.sync === "host" ? [...SWITCHES, HOST] : SWITCHES;
  const items: MenuItem<"keep" | "clear">[] = [
    { id: "keep", label: "Keep the notes" },
    { id: "clear", label: `Clear the notes of ${name}: ${notesWords(box.notes.length)}` },
  ];
  return (
    <div className="tang-pattern tang-patternbox nodrag nopan nowheel" role="group" aria-label={`Pattern box ${name}`}>
      <div className="tang-pattern__row">
        <button
          type="button"
          id={recordId(node)}
          className={`tang-pattern__button tang-pattern__record${recording ? " tang-pattern__record--going" : ""}`}
          aria-label={recording ? `Finish recording ${name}` : `Record into ${name}`}
          title={recording ? BOX_FINISH_HINT : boxRecordHint(box)}
          onClick={() => {
            // (If it is refused the page says so: `Editor` puts the reason among the notices.)
            void editor.patternBoxTransport(node, recording ? "finishRecording" : "record");
          }}
        >
          {recording ? "■ Finish" : "● Record"}
        </button>
        <button
          type="button"
          className="tang-pattern__button tang-patternbox__play"
          aria-label={playing ? `Stop ${name}` : `Play ${name}`}
          title={playing ? boxStopHint(box.status) : boxPlayHint(box)}
          onClick={() => {
            void editor.patternBoxTransport(node, playing ? "stop" : "play");
          }}
        >
          {playing ? "■ Stop" : "▶ Play"}
        </button>
        {box.notes.length === 0 ? (
          <button type="button" className="tang-pattern__button" disabled aria-label={`Clear the notes of ${name}`}>
            Clear
          </button>
        ) : (
          <MenuButton
            label={`Clear the notes of ${name}`}
            className="tang-pattern__button"
            items={items}
            onChoose={(choice) => {
              if (choice !== "clear") return;
              void editor.clearPatternBox(node).then((cleared) => {
                if (cleared) focusWhenPossible(recordId(node));
              });
            }}
          >
            Clear
          </MenuButton>
        )}
        <label className="tang-pattern__length" title="How long the box's loop is, in beats (the next recording takes this long)">
          <NumberField
            className="tang-pattern__field"
            label={`Length of ${name}`}
            value={box.lengthBeats}
            range={PATTERN_BOX_LENGTH_RANGE}
            step={STEP}
            bigStep={BIG_STEP}
            ask={(beats) => editor.setPatternBoxField(node, { type: "lengthBeats", beats })}
          />
          <span>beats</span>
        </label>
      </div>
      <div className="tang-pattern__row">
        {switches.map(({ text, says, title, on, field }) => (
          <button
            key={text}
            type="button"
            className="tang-pattern__button tang-pattern__switch"
            aria-label={says(name)}
            aria-pressed={on(box)}
            title={title}
            onClick={() => {
              void editor.setPatternBoxField(node, field(!on(box)));
            }}
          >
            {text}
          </button>
        ))}
      </div>
      <PatternRoll notes={box.notes} length={box.lengthBeats} name={name} />
      <p className="tang-pattern__line" aria-live="polite" title={words}>
        {words}
      </p>
    </div>
  );
});

/**
 * A pattern box: notes come in at its port on the left and go out at the one on the right. The menu of its name takes notes in from a source (▸♫) and sends them on to a lane (♫▸), for a keyboard, which cannot drag
 * a cable; its name is changed in place, and its button takes it out with the cables of notes it has.
 */
export function PatternBoxNode({ data }: NodeProps<PatchNode>) {
  const editor = useEditor();
  const [editing, setEditing] = useState(false);
  const rename = useCallback((name: string) => editor.setPatternBoxField(data.node, { type: "name", name }), [editor, data.node]);
  return (
    <div className="tang-node tang-node--pattern nokey">
      <NotesPort node={data.node} direction="in" />
      <NotesPort node={data.node} direction="out" />
      <Header
        name={data.name}
        badge="Pattern box"
        sub={
          <>
            <NotesFromMenu node={data.node} name={data.name} glyph="▸♫" />
            <NotesToMenu node={data.node} name={data.name} glyph="♫▸" />
          </>
        }
        field={
          editing ? (
            <NameField
              name={data.name}
              rename={rename}
              done={(focusBack) => {
                setEditing(false);
                if (focusBack) focusWhenPossible(renameBoxId(data.node));
              }}
            />
          ) : undefined
        }
      >
        <span className="tang-node__actions">
          <RenameButton
            id={renameBoxId(data.node)}
            name={data.name}
            onEdit={() => {
              setEditing(true);
            }}
          />
          <RemoveNodeButton node={data.node} name={data.name} focusTo={ADD_PATTERN_ID} />
        </span>
      </Header>
      <BoxBody node={data.node} name={data.name} />
    </div>
  );
}
