// The button in the bar of the page that adds a modulator that is in no lane and no group: a menu of the kinds. The modulator is a module of its own, drawn in a row above the lanes, that a cable can be
// drawn from to a parameter of any plugin of the session (one LFO can move a knob of two lanes) and to the settings of the other modulators like it; what the canvas calls it, and where it is, is the
// canvas's business.
import { MenuButton } from "../ui/MenuButton";
import { useEditor } from "./ClientProvider";
import { ADD_FREE_MODULATOR_ID, MODULATOR_KINDS } from "./patch/nodes";

export function AddFreeModulatorMenu() {
  const editor = useEditor();
  return (
    <MenuButton
      id={ADD_FREE_MODULATOR_ID}
      label="Add a modulator"
      className="tang-app__add-instrument"
      items={MODULATOR_KINDS}
      onChoose={(kind) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addFreeModulator(kind);
      }}
    >
      + Modulator
    </MenuButton>
  );
}
