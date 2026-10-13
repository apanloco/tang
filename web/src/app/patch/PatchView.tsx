// The canvas (docs/web-ui.md §9.3): the session as nodes and cables, drawn by React Flow.
// The nodes cannot be moved yet (their places are the layout's; the places a user chooses are
// kept in the session in a later step) nor cabled by hand; what can be done on the canvas is
// to turn the controls on the nodes.
import { Background, Controls, ReactFlow } from "@xyflow/react";
import type { EdgeTypes, NodeTypes } from "@xyflow/react";
import { useMemo } from "react";

import type { Graph } from "../../protocol";
import { useClientSelection } from "../ClientProvider";
import { AudioEdge, ModulationEdge } from "./edges";
import { GroupNode, MasterNode, ModulatorNode, PluginNode } from "./nodes";
import { sameStructure, toFlow } from "./toFlow";
import "@xyflow/react/dist/style.css";
import "./patch.css";

// (Outside the component, so that they are the same objects every time.)
const NODE_TYPES: NodeTypes = {
  instrument: PluginNode,
  effect: PluginNode,
  modulator: ModulatorNode,
  group: GroupNode,
  master: MasterNode,
};

const EDGE_TYPES: EdgeTypes = { audio: AudioEdge, modulation: ModulationEdge };

/** Whether two graphs (or the lack of one) are laid out the same: what the canvas depends on, which is not the values. */
export const sameGraphStructure = (a: Graph | null, b: Graph | null): boolean => (a && b ? sameStructure(a, b) : a === b);

export function PatchView() {
  // The canvas is laid out again when something other than a value changed.
  const graph = useClientSelection((state) => state.snapshot?.graph ?? null, sameGraphStructure);
  const flow = useMemo(() => (graph ? toFlow(graph) : { nodes: [], edges: [] }), [graph]);
  return (
    <div className="tang-patch" data-testid="patch">
      <ReactFlow
        nodes={flow.nodes}
        edges={flow.edges}
        nodeTypes={NODE_TYPES}
        edgeTypes={EDGE_TYPES}
        colorMode="dark"
        fitView
        fitViewOptions={{ padding: 0.12, maxZoom: 1 }}
        minZoom={0.2}
        maxZoom={1.5}
        nodesConnectable={false}
        edgesFocusable={false}
        elementsSelectable={false}
        deleteKeyCode={null}
      >
        <Background gap={24} />
        <Controls showInteractive={false} />
      </ReactFlow>
    </div>
  );
}
