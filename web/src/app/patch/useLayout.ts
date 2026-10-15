// The layout of the canvas, kept from one change of the session to the next (docs/web-ui.md §9.3): `toFlow` is given where it put the nodes last,
// so that a cable that is drawn, or a node that is added or taken out, does not rearrange the canvas under a person's hands, and it is made from
// nothing, every node where the layout has it, when the person asks for the nodes to be arranged again.
import { useCallback, useState } from "react";

import type { Graph } from "../../protocol";
import { toFlow } from "./toFlow";
import type { Flow } from "./toFlow";

/** What the canvas draws before the session has come. */
const NO_FLOW: Flow = { nodes: [], edges: [], places: new Map(), arranged: true };

interface Laid {
  /** The session that `flow` is the layout of. */
  graph: Graph | null;
  flow: Flow;
  /**
   * The nodes that a person placed are being given back to the layout, one answer of the server after the other: until the last has been, the layout is made from nothing
   * each time (a node that the layout had to move clear of one that was placed would stay where it was moved to, and the nodes would not be arranged when the last is given back).
   */
  arranging: boolean;
}

/** Whether a person has placed any node of the session. */
const anyPlaced = (graph: Graph | null): boolean => graph?.nodes.some((node) => node.ui !== undefined) ?? false;

export interface Layout {
  flow: Flow;
  /** The nodes that a person had placed are being given back to the layout: what is laid out now is not what they will be when the last is (`Laid.arranging`). */
  arranging: boolean;
  /** Lay the nodes out from nothing: every node that is not placed by a person goes where the layout puts it. */
  arrangeAgain: () => void;
}

/** The layout of `graph` (which is the same object while the session's structure is the same: what is laid out is not the values). */
export function useLayout(graph: Graph | null): Layout {
  const [laid, setLaid] = useState<Laid>({ graph: null, flow: NO_FLOW, arranging: false });
  let current = laid;
  // (A layout that is made when the session changes, in the render that sees it: there is no frame that draws the new session with the old layout.)
  if (laid.graph !== graph) {
    const flow = graph ? toFlow(graph, laid.arranging ? undefined : laid.flow.places) : NO_FLOW;
    current = { graph, flow, arranging: laid.arranging && anyPlaced(graph) };
    setLaid(current);
  }
  const arrangeAgain = useCallback(() => {
    setLaid((now) => ({ graph: now.graph, flow: now.graph ? toFlow(now.graph) : NO_FLOW, arranging: anyPlaced(now.graph) }));
  }, []);
  return { flow: current.flow, arranging: current.arranging, arrangeAgain };
}
