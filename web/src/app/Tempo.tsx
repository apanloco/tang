// The tempo of the session, in the bar at the top of the page: the beats a minute that patterns are recorded at (`SetBpm`; the terminal's `b`), as a field and its unit
// ("120 BPM": the bar has no room for a name as well, and the field's title says what it is). The field is the page's number field (`ui/NumberField`): Enter, or leaving it,
// asks for the number that was typed, Escape gives it up, and the arrow keys turn the tempo by one beat a minute (ten with Shift). In a plugin whose host says its tempo (step 118: `meta.hostTempo`) the tempo is the host's:
// it is shown, as a number that follows the host, and is not a field, since a tempo that is typed does not last.
import { BPM_RANGE } from "../protocol";
import { NumberField } from "../ui/NumberField";
import { useClientState, useEditor } from "./ClientProvider";

/** How much an arrow key turns the tempo by, and how much with Shift. */
const STEP = 1;
const BIG_STEP = 10;

/** A tempo as it is read: the places that a host's tempo has (133.5, 93.12), and none that it has not. */
const shown = (bpm: number): string => String(Number(bpm.toFixed(2)));

export function Tempo() {
  const editor = useEditor();
  const bpm = useClientState((state) => state.snapshot?.meta.bpm);
  const hostTempo = useClientState((state) => state.snapshot?.meta.hostTempo === true);
  if (bpm === undefined) return null;
  if (hostTempo) {
    return (
      <span className="tang-tempo tang-tempo--host" title="The tempo of the program that Tang is in: a plugin goes by its host's tempo, which is not set here">
        <output className="tang-tempo__value" aria-label="Tempo, the host's">
          {shown(bpm)}
        </output>
        <span className="tang-tempo__unit">BPM</span>
      </span>
    );
  }
  return (
    <label className="tang-tempo" title="The tempo that patterns are recorded at, in beats a minute (the arrow keys turn it by one, by ten with Shift)">
      {/* (If a tempo is refused the page says so: `Editor` puts the reason among the notices.) */}
      <NumberField className="tang-tempo__field" label="Tempo" value={bpm} range={BPM_RANGE} step={STEP} bigStep={BIG_STEP} ask={(value) => editor.setBpm(value)} />
      <span className="tang-tempo__unit">BPM</span>
    </label>
  );
}
