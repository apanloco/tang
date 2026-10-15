// The scopes in the nodes (docs/web-ui.md §9.3, D47): an instrument, an effect and a group can show a scope of what comes out of it, below its controls. Which of them do is the
// viewer's own, like which node's parameters are in the panel: the canvas holds it (it lays the nodes out with room for the scopes) and hands the nodes this to ask for theirs.
import { createContext } from "react";

import type { NodeId } from "../../protocol";

export interface Scopes {
  /** The nodes that show their scope. */
  open: ReadonlySet<NodeId>;
  /** Show the scope of `node`, or take it away if it is shown. */
  toggle: (node: NodeId) => void;
}

/** What a node asks the canvas for. (Where there is no canvas to hold them, the button of a node does nothing.) */
export const ScopesContext = createContext<Scopes>({ open: new Set(), toggle: () => undefined });

/** The `id` of the button of `node` that shows its scope. */
export const scopeButtonId = (node: NodeId): string => `scope-${String(node)}`;
