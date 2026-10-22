// The button on a node that sends its audio somewhere (docs/patch-graph.md §11.6): a menu of the places that take a cable from its output, for a keyboard, which cannot drag one, and for
// whoever would rather choose than aim. What takes it is worked out when the structure of the session changes (a cable that is drawn, a node that comes or goes), not on every value: there is
// a button on every node of the canvas, and the answer is for the controller's rule (`cableTakers`). The canvas hands the structure to them (`StructureContext`), so that they do not each look
// for a change of it at every change of a value.
import { useContext, useMemo } from "react";

import { sendItems } from "../../client/sends";
import type { NodeId } from "../../protocol";
import { MenuButton } from "../../ui/MenuButton";
import { useEditor } from "../ClientProvider";
import { StructureContext } from "./structure";

/** The `id` of the button that sends the audio of the node `node`, which what takes a cable out of it gives the focus to. */
export const sendId = (node: NodeId): string => `send-${String(node)}`;

/**
 * The menu that draws an audio cable from the output of the node `node` (a lane's instrument, an effect or a group) to the place that is chosen, an effect, the bus of a group or the mix, at the level
 * a new cable has. A place that has a cable from it already, or that the audio would go round to, is not listed; with none left the button is shut.
 */
export function SendMenu({ node, name }: { node: NodeId; name: string }) {
  const editor = useEditor();
  const graph = useContext(StructureContext);
  const items = useMemo(() => (graph ? sendItems(graph, node) : []), [graph, node]);
  return (
    <MenuButton
      id={sendId(node)}
      label={`Send audio from ${name}`}
      className="tang-node__send"
      items={items}
      onChoose={(to) => {
        // (If it is refused the page says so: `Editor` puts the reason among the notices.)
        void editor.addAudioCable(node, Number(to));
      }}
    >
      ⇢
    </MenuButton>
  );
}
