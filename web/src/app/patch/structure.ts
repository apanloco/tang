// The shape of the session as the canvas sees it (docs/web-ui.md §9.3): the graph, as the same object for as long as only values change in it (`sameGraphStructure`: a knob that is turned is no
// change of shape). The canvas holds it and hands it to what needs to know the shape of the session and not its values and is on every node, as the menu that sends a node's audio is: each of them
// comparing the graph with the last at every change of a value would be as many comparisons as there are nodes, a frame at a time while a knob is dragged.
import { createContext } from "react";

import type { Graph } from "../../protocol";

/** The graph with its structure, or null where there is no canvas to hold it or no session yet. */
export const StructureContext = createContext<Graph | null>(null);
