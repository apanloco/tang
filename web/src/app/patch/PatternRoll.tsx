// The roll of a pattern box, under its switches (docs/patch-graph.md §11.6, P4h-C part C): its notes as marks, the beats across and the pitches up, a picture of what the box plays. It is as tall whatever it shows
// (the layout counts on it: `PATTERN_ROLL_HEIGHT`) and takes no pointer: the roll that is edited is slice 2 of the plan.
import { memo, useMemo } from "react";

import { rollOf } from "../../client/patternRoll";
import { beatsWords, notesWords } from "../../client/patternWords";
import type { PatternNote } from "../../protocol";

export const PatternRoll = memo(function PatternRoll({ notes, length, name }: { notes: readonly PatternNote[]; length: number; name: string }) {
  const roll = useMemo(() => rollOf(notes, length), [notes, length]);
  return (
    <svg
      className="tang-roll"
      role="img"
      aria-label={`Notes of ${name}: ${notesWords(notes.length)} in ${beatsWords(length)}`}
      viewBox={`0 0 ${String(roll.beats)} ${String(roll.span)}`}
      preserveAspectRatio="none"
    >
      {roll.lines.map(({ at, bar }) => (
        <line key={at} className={`tang-roll__line${bar ? " tang-roll__line--bar" : ""}`} x1={at} x2={at} y1={0} y2={roll.span} vectorEffect="non-scaling-stroke" />
      ))}
      {roll.notes.map(({ x, y, width, height, opacity }, index) => (
        // (The notes have no names: a mark is as its place in the list, which a recording that comes makes anew.)
        <rect key={index} className="tang-roll__note" x={x} y={y} width={width} height={height} opacity={opacity} />
      ))}
    </svg>
  );
});
