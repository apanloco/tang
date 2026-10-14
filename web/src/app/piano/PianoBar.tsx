// The choice of the piano's scale and the lock that drops the notes outside it. Both are the session's (the TUI sets them
// too): a choice shows at once and until the answer is in, and after it what the session has, which is what was asked for
// unless it was refused (and the page says why).
import { useRef, useState } from "react";

import { SCALE_KINDS, SCALE_ROOTS } from "../../protocol";
import type { PianoMeta } from "../../protocol";
import { useEditor } from "../ClientProvider";

/** A root and a kind of scale: what the two choices of the picker say, and what is asked for when one of them is changed. */
interface Choice {
  root: number;
  kind: string;
}

/** The root and the kind of the piano's scale, each a choice. */
function ScalePicker({ piano }: { piano: PianoMeta }) {
  const editor = useEditor();
  const [pending, setPending] = useState<Choice | undefined>(undefined);
  const latest = useRef(0);
  const shown: Choice = pending ?? { root: piano.root, kind: piano.kind };
  const choose = (next: Choice): void => {
    const mine = (latest.current += 1);
    setPending(next);
    void editor.setScale(`${SCALE_ROOTS[next.root] ?? "C"} ${next.kind}`).finally(() => {
      if (latest.current === mine) setPending(undefined);
    });
  };
  return (
    <div className="tang-piano__scale">
      <span className="tang-piano__label">Scale</span>
      <select
        className="tang-piano__select"
        aria-label="Scale root"
        title="Root of the scale"
        value={String(shown.root)}
        onChange={(event) => {
          choose({ ...shown, root: Number(event.target.value) });
        }}
      >
        {SCALE_ROOTS.map((name, root) => (
          <option key={name} value={String(root)}>
            {name}
          </option>
        ))}
      </select>
      <select
        className="tang-piano__select"
        aria-label="Scale kind"
        title="Kind of scale"
        value={shown.kind}
        onChange={(event) => {
          choose({ ...shown, kind: event.target.value });
        }}
      >
        {SCALE_KINDS.map(({ name, short }) => (
          <option key={short} value={short}>
            {name}
          </option>
        ))}
      </select>
    </div>
  );
}

/** Whether the piano drops the notes that are not in the scale, which the keys of the page, a MIDI keyboard and the keys of the computer all play through. */
function LockButton({ locked }: { locked: boolean }) {
  const editor = useEditor();
  const [pending, setPending] = useState<boolean | undefined>(undefined);
  const latest = useRef(0);
  const shown = pending ?? locked;
  return (
    <button
      type="button"
      className="tang-piano__lock"
      aria-pressed={shown}
      title="Locked, a note that is not in the scale is not played, from any keyboard. Not locked, every note is played, and the ones outside the scale are only coloured."
      onClick={() => {
        const mine = (latest.current += 1);
        setPending(!shown);
        void editor.setLocked(!shown).finally(() => {
          if (latest.current === mine) setPending(undefined);
        });
      }}
    >
      Lock to scale
    </button>
  );
}

export function PianoBar({ piano }: { piano: PianoMeta }) {
  return (
    <div className="tang-piano__bar">
      <ScalePicker piano={piano} />
      <LockButton locked={piano.locked} />
    </div>
  );
}
