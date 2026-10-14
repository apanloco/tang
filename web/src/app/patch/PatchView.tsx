// The canvas (docs/web-ui.md §9.3): the session as nodes and cables, drawn by React Flow.
// What can be done on the canvas is to turn the controls on the nodes, to add a modulator to a
// lane or a group, to take a modulator or a cable out, to draw a cable by dragging it from a
// modulator's output to a parameter's port, and to move a node by dragging it (by what is not a
// control): where it is put down goes to the server, which keeps it with the session (§4.4), and
// every browser draws the node there. The nodes that are not moved are where the layout puts them.
import { Background, ControlButton, Controls, ReactFlow, useReactFlow, useStore } from "@xyflow/react";
import type { Connection, Edge, EdgeTypes, NodeTypes, OnConnectStart } from "@xyflow/react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { Graph, NodeId, NodeUi } from "../../protocol";
import { useClientSelection, useEditor, useReveals } from "../ClientProvider";
import { focusWhenPossible } from "../focus";
import { cableOf, canDraw, ConnectingContext, drawableFrom } from "./connection";
import { AudioEdge, ModulationEdge } from "./edges";
import { InspectorContext, paramsButtonId } from "./inspector";
import type { Inspector } from "./inspector";
import { GroupNode, MasterNode, ModulatorNode, PluginNode } from "./nodes";
import { ParamPanel } from "./ParamPanel";
import { sameStructure, toFlow } from "./toFlow";
import type { Flow } from "./toFlow";
import { useMovableNodes } from "./useMovableNodes";
import { rectOf, viewToShow } from "./view";
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

/** How long a node the page added is waited for: the layout that has it comes with the snapshot, which is as good as at once. */
const SHOW_WITHIN_MS = 3000;

/** How long the canvas takes to move to a node. */
const MOVE_MS = 250;

/** How far (in pixels) the pointer has to go with a node before it is a drag: a click on a node's header is not a move. */
const DRAG_THRESHOLD = 3;

/**
 * Moves the canvas to a node the page has just added, if it is not all in the view (a lane goes below the others, an effect
 * after the last of its row): the page asks for it through `Reveals` once the session has the node, and the layout may come a
 * moment later. It is only for what the page itself added: a node somebody else adds is not worth the canvas moving under a
 * person who is turning a knob. Draws nothing; it is in the canvas for the instance of it (`useReactFlow`).
 */
function ShowWhatWasAdded({ graph, flow }: { graph: Graph | null; flow: Flow }) {
  const reveals = useReveals();
  const canvas = useReactFlow();
  const width = useStore((state) => state.width);
  const height = useStore((state) => state.height);
  const minZoom = useStore((state) => state.minZoom);
  const latest = useRef({ graph, flow });
  const wanted = useRef<{ node: NodeId; until: number } | null>(null);

  const show = useCallback(() => {
    const want = wanted.current;
    if (!want) return;
    if (Date.now() > want.until) {
      wanted.current = null;
      return;
    }
    const { graph: now, flow: laidOut } = latest.current;
    const rect = now ? rectOf(now, laidOut, want.node) : null;
    // (Not laid out yet: when the next layout is.)
    if (!rect) return;
    wanted.current = null;
    const next = viewToShow(rect, canvas.getViewport(), { width, height }, minZoom);
    if (next) void canvas.setViewport(next, { duration: MOVE_MS });
  }, [canvas, width, height, minZoom]);

  useEffect(() => {
    latest.current = { graph, flow };
    show();
  }, [graph, flow, show]);

  useEffect(
    () =>
      reveals.subscribe((node) => {
        wanted.current = { node, until: Date.now() + SHOW_WITHIN_MS };
        show();
      }),
    [reveals, show],
  );
  return null;
}

export function PatchView() {
  // The canvas is laid out again when something other than a value changed.
  const graph = useClientSelection((state) => state.snapshot?.graph ?? null, sameGraphStructure);
  const flow = useMemo(() => (graph ? toFlow(graph) : { nodes: [], edges: [] }), [graph]);
  const editor = useEditor();
  // Whether any node has been moved: what there is to arrange again.
  const anyMoved = graph?.nodes.some((node) => node.ui !== undefined) ?? false;
  // The nodes as they are on the canvas: the layout's, but where a person is dragging one it is where the pointer has taken it.
  const moveNode = useCallback((node: NodeId, ui: NodeUi) => editor.moveNode(node, ui), [editor]);
  const { nodes, onNodesChange, onNodeDragStart, onNodeDragStop } = useMovableNodes(flow.nodes, moveNode);
  // The parameters that take the cable being dragged, for their ports to say so.
  const [connecting, setConnecting] = useState<ReadonlySet<string> | null>(null);
  // The plugin whose parameters are in the panel, if the panel is open.
  const [inspected, setInspected] = useState<NodeId | null>(null);
  const inspector = useMemo<Inspector>(
    () => ({
      inspected,
      toggle: (node) => {
        setInspected((now) => (now === node ? null : node));
      },
    }),
    [inspected],
  );
  const closePanel = useCallback(
    (focusBack: boolean) => {
      setInspected(null);
      if (focusBack && inspected !== null) focusWhenPossible(paramsButtonId(inspected));
    },
    [inspected],
  );

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
      <InspectorContext.Provider value={inspector}>
        <div className="tang-patch__canvas">
          <ConnectingContext.Provider value={connecting}>
            <ReactFlow
              nodes={nodes}
              edges={flow.edges}
              onNodesChange={onNodesChange}
              onNodeDragStart={onNodeDragStart}
              onNodeDragStop={onNodeDragStop}
              nodeDragThreshold={DRAG_THRESHOLD}
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
              <Controls showInteractive={false}>
                <ControlButton
                  title="Arrange the nodes again"
                  aria-label="Arrange the nodes again"
                  disabled={!anyMoved}
                  onClick={() => {
                    // (If one is refused the page says so: `Editor` puts the reason among the notices.)
                    void editor.resetLayout();
                  }}
                >
                  <span aria-hidden="true">↺</span>
                </ControlButton>
              </Controls>
              <ShowWhatWasAdded graph={graph} flow={flow} />
            </ReactFlow>
          </ConnectingContext.Provider>
        </div>
        {inspected !== null ? <ParamPanel key={inspected} node={inspected} onClose={closePanel} /> : null}
      </InspectorContext.Provider>
    </div>
  );
}
