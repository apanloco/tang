// The canvas (docs/web-ui.md §9.3): the session as nodes and cables, drawn by React Flow.
// The nodes cannot be moved yet (their places are the layout's own; the places a user chooses are
// kept in the session in a later step); what can be done on the canvas is to turn the controls on
// the nodes, to add a modulator to a lane or a group, to take a modulator or a cable out, and to
// draw a cable by dragging it from a modulator's output to a parameter's port.
import { Background, Controls, ReactFlow } from "@xyflow/react";
import type { Connection, Edge, EdgeTypes, NodeTypes, OnConnectStart } from "@xyflow/react";
import { useCallback, useMemo, useState } from "react";

import type { Graph } from "../../protocol";
import { useClientSelection, useEditor } from "../ClientProvider";
import { cableOf, canDraw, ConnectingContext, drawableFrom } from "./connection";
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

/** How a cable looks while it is dragged: as the cables of modulation do. */
const CONNECTION_LINE = { stroke: "var(--tang-modulation)", strokeWidth: 2, strokeDasharray: "7 5" };

/** Whether two graphs (or the lack of one) are laid out the same: what the canvas depends on, which is not the values. */
export const sameGraphStructure = (a: Graph | null, b: Graph | null): boolean => (a && b ? sameStructure(a, b) : a === b);

export function PatchView() {
  // The canvas is laid out again when something other than a value changed.
  const graph = useClientSelection((state) => state.snapshot?.graph ?? null, sameGraphStructure);
  const flow = useMemo(() => (graph ? toFlow(graph) : { nodes: [], edges: [] }), [graph]);
  const editor = useEditor();
  // The parameters that take the cable being dragged, for their ports to say so.
  const [connecting, setConnecting] = useState<ReadonlySet<string> | null>(null);

  const onConnectStart = useCallback<OnConnectStart>(
    (_event, { nodeId, handleType }) => {
      setConnecting(handleType === "source" && nodeId !== null ? drawableFrom(flow.nodes, Number(nodeId)) : null);
    },
    [flow.nodes],
  );
  const onConnectEnd = useCallback(() => {
    setConnecting(null);
  }, []);
  const isValidConnection = useCallback(
    (connection: Connection | Edge) => {
      const ends = cableOf(connection);
      return ends !== null && canDraw(flow.nodes, ends);
    },
    [flow.nodes],
  );
  const onConnect = useCallback(
    (connection: Connection) => {
      const ends = cableOf(connection);
      // (If it is refused the page says so: `Editor` puts the reason among the notices.)
      if (ends) void editor.addCable(ends.modulator, ends.node, ends.param);
    },
    [editor],
  );

  return (
    <div className="tang-patch" data-testid="patch" data-connecting={connecting ? "" : undefined}>
      <ConnectingContext.Provider value={connecting}>
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
          connectionRadius={40}
          connectionLineStyle={CONNECTION_LINE}
          isValidConnection={isValidConnection}
          onConnectStart={onConnectStart}
          onConnectEnd={onConnectEnd}
          onConnect={onConnect}
          edgesFocusable={false}
          elementsSelectable={false}
          deleteKeyCode={null}
        >
          <Background gap={24} />
          <Controls showInteractive={false} />
        </ReactFlow>
      </ConnectingContext.Provider>
    </div>
  );
}
