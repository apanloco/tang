// The button in the bar of the page that adds a lane with an instrument in it: a menu of what is
// installed and can be an instrument, under the format of each. The lane is full range and in no
// group, at the end of the session; what it is called, and where it is, is the canvas's business.
import { useMemo } from "react";

import { instrumentItems } from "../client/catalog";
import { MenuButton } from "../ui/MenuButton";
import { useClientState, useEditor } from "./ClientProvider";

export function AddInstrumentMenu() {
  const editor = useEditor();
  const catalog = useClientState((state) => state.catalog);
  const items = useMemo(() => instrumentItems(catalog?.entries ?? []), [catalog]);
  return (
    <MenuButton
      id="add-instrument"
      label="Add an instrument"
      className="tang-app__add-instrument"
      items={items}
      onChoose={(plugin) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addInstrument(plugin);
      }}
    >
      + Instrument
    </MenuButton>
  );
}
