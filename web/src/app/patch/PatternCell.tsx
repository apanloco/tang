// The pattern of a lane, under its controls (docs/web-ui.md §12, step 51; the terminal's `r`, `d` and the rows of its pattern pane): a button that records one (it is counted in, and
// then the notes that are played for as long as the pattern is are the pattern: `SetRecording`), a menu that clears it, for good, after asking (`ClearPattern`), its length in beats, the
// three switches that say what a held key does with it (`SetPatternField`), and a line that says what it is, or what a recording that is going on is doing. The block is as tall whatever
// it shows (the layout counts on it: `PATTERN_HEIGHT`): what a lane that has none has no use for is dimmed and not left out.
import { memo } from "react";

import { beatsWords, notesWords, patternLine, recordHint } from "../../client/patternWords";
import { patternOf } from "../../client/selectors";
import { PATTERN_DEFAULT_LENGTH, PATTERN_FIELD_RANGES } from "../../protocol";
import type { NodeId, PatternField, PatternInfo } from "../../protocol";
import { MenuButton } from "../../ui/MenuButton";
import type { MenuItem } from "../../ui/MenuButton";
import { NumberField } from "../../ui/NumberField";
import { useClientState, useEditor } from "../ClientProvider";
import { focusWhenPossible } from "../focus";

/** How much an arrow key turns the length by, in beats, and how much with Shift. */
const STEP = 1;
const BIG_STEP = 4;

/** What the button says it does while a recording goes on. */
const STOP_HINT = "Stop recording: what has been played since the count-in is the pattern, and a recording that is stopped in the count-in leaves the pattern as it was";

/** The `id` of the button that records, for the focus to go back to when the pattern that was cleared leaves nothing else of the cell to have it. */
const recordId = (node: NodeId): string => `pattern-record-${String(node)}`;

/** The switches of a pattern: what each is called, what it is for a person who has not used it, and whether it is on. */
const SWITCHES: readonly { field: PatternField; text: string; says: (name: string) => string; title: string; on: (pattern: PatternInfo) => boolean }[] = [
  { field: "enabled", text: "Play", says: (name) => `Play the pattern of ${name}`, title: "A held key plays the pattern", on: (pattern) => pattern.enabled },
  { field: "looping", text: "Loop", says: (name) => `Loop the pattern of ${name}`, title: "The pattern goes round while the key is held (off: it plays once)", on: (pattern) => pattern.looping },
  {
    field: "transpose",
    text: "In key",
    says: (name) => `Transpose the pattern of ${name} in key`,
    title: "The pattern is moved by the degrees of the piano's scale (off: by semitones)",
    on: (pattern) => pattern.inKey,
  },
];

export const PatternCell = memo(function PatternCell({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const pattern = useClientState((state) => patternOf(state, node));
  const bpm = useClientState((state) => state.snapshot?.meta.bpm);
  // (A node that is no lane has no pattern to draw.)
  if (pattern === undefined || bpm === undefined) return null;
  const recording = pattern?.recording === true;
  const beats = pattern?.lengthBeats ?? PATTERN_DEFAULT_LENGTH;
  const words = patternLine(pattern, bpm);
  const items: MenuItem<"keep" | "clear">[] =
    pattern === null ? [] : [{ id: "keep", label: "Keep the pattern" }, { id: "clear", label: `Clear the pattern of ${name} for good: ${beatsWords(pattern.lengthBeats)}, ${notesWords(pattern.notes)}` }];
  return (
    <div className="tang-pattern nodrag nopan nowheel" role="group" aria-label={`Pattern of ${name}`}>
      <div className="tang-pattern__row">
        <button
          type="button"
          id={recordId(node)}
          className={`tang-pattern__button tang-pattern__record${recording ? " tang-pattern__record--going" : ""}`}
          aria-label={recording ? `Stop recording ${name}` : `Record a pattern on ${name}`}
          title={recording ? STOP_HINT : recordHint(beats, bpm)}
          onClick={() => {
            // (If it is refused the page says so: `Editor` puts the reason among the notices.)
            void editor.setRecording(node, !recording);
          }}
        >
          {recording ? "■ Stop" : "● Record"}
        </button>
        {pattern === null ? (
          <button type="button" className="tang-pattern__button" disabled aria-label={`Clear the pattern of ${name}`}>
            Clear
          </button>
        ) : (
          <MenuButton
            id={`pattern-clear-${String(node)}`}
            label={`Clear the pattern of ${name}`}
            className="tang-pattern__button"
            items={items}
            onChoose={(choice) => {
              if (choice !== "clear") return;
              void editor.clearPattern(node).then((cleared) => {
                if (cleared) focusWhenPossible(recordId(node));
              });
            }}
          >
            Clear
          </MenuButton>
        )}
        <label className="tang-pattern__length" title="How long the pattern is, in beats (the next recording takes this long)">
          <NumberField
            className="tang-pattern__field"
            label={`Length of the pattern of ${name}`}
            value={beats}
            range={PATTERN_FIELD_RANGES.length}
            step={STEP}
            bigStep={BIG_STEP}
            ask={(value) => editor.setPatternField(node, "length", value)}
          />
          <span>beats</span>
        </label>
      </div>
      <div className="tang-pattern__row">
        {SWITCHES.map(({ field, text, says, title, on }) => (
          <button
            key={field}
            type="button"
            className="tang-pattern__button tang-pattern__switch"
            aria-label={says(name)}
            aria-pressed={pattern !== null && on(pattern)}
            disabled={pattern === null}
            title={title}
            onClick={() => {
              if (pattern !== null) void editor.setPatternField(node, field, on(pattern) ? 0 : 1);
            }}
          >
            {text}
          </button>
        ))}
      </div>
      <p className="tang-pattern__line" aria-live="polite" title={words}>
        {words}
      </p>
    </div>
  );
});
