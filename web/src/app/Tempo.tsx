// The tempo of the session, in the bar at the top of the page: the beats a minute that patterns are recorded at (`SetBpm`; the terminal's `b`), as a field and its unit
// ("120 BPM": the bar has no room for a name as well, and the field's title says what it is). The field is the page's number field (`ui/NumberField`): Enter, or leaving it,
// asks for the number that was typed, Escape gives it up, and the arrow keys turn the tempo by one beat a minute (ten with Shift).
import { BPM_RANGE } from "../protocol";
import { NumberField } from "../ui/NumberField";
import { useClientState, useEditor } from "./ClientProvider";

/** How much an arrow key turns the tempo by, and how much with Shift. */
const STEP = 1;
const BIG_STEP = 10;

export function Tempo() {
  const editor = useEditor();
  const bpm = useClientState((state) => state.snapshot?.meta.bpm);
  if (bpm === undefined) return null;
  return (
    <label className="tang-tempo" title="The tempo that patterns are recorded at, in beats a minute (the arrow keys turn it by one, by ten with Shift)">
      {/* (If a tempo is refused the page says so: `Editor` puts the reason among the notices.) */}
      <NumberField className="tang-tempo__field" label="Tempo" value={bpm} range={BPM_RANGE} step={STEP} bigStep={BIG_STEP} ask={(value) => editor.setBpm(value)} />
      <span className="tang-tempo__unit">BPM</span>
    </label>
  );
}
