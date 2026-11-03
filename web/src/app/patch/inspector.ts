// The panel that lists all the parameters of a plugin (docs/web-ui.md §9.3): a node shows a few of them as
// knobs, and the button in its header puts the rest in the panel. The canvas holds which node's parameters
// are in it, and hands the nodes this to ask for theirs.
import { createContext } from "react";

import type { NodeId } from "../../protocol";

export interface Inspector {
  /** The node whose parameters are in the panel, if the panel is open. */
  inspected: NodeId | null;
  /** Put the parameters of `node` in the panel, or take the panel away if they are in it. */
  toggle: (node: NodeId) => void;
}

/** What a node asks the canvas for. (Where there is no canvas to hold the panel, the button of a node does nothing.) */
export const InspectorContext = createContext<Inspector>({ inspected: null, toggle: () => undefined });

/** The `id` of the button of `node` that opens its parameters, which the focus goes back to when the panel is closed. */
export const paramsButtonId = (node: NodeId): string => `params-${String(node)}`;
