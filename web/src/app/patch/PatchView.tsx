// The canvas (docs/web-ui.md §9.3): the session as nodes and cables, drawn by React Flow.
// What can be done on the canvas is to turn the controls on the nodes, to add a modulator to a
// lane or a group, to take a modulator or a cable out, to draw a cable by dragging it from a
// modulator's output to a parameter's port, and to move a node by dragging it (by what is not a
// control): where it is put down goes to the server, which keeps it with the session (§4.4), and
// every browser draws the node there. The nodes that are not moved are where the layout puts them, and stay there when the session
// changes round them (a cable drawn, a node added or taken out: `toFlow` is given where it put them last) until the person asks for
// the nodes to be arranged again.
import { Background, Controls, Panel, ReactFlow, useReactFlow, useStore } from "@xyflow/react";
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
import { ScopesContext } from "./scopes";
import type { Scopes } from "./scopes";
import { sameStructure } from "./toFlow";
import type { Flow } from "./toFlow";
import { useLayout } from "./useLayout";
import { useMovableNodes } from "./useMovableNodes";
import { rectOf, viewToShow } from "./view";
import type { View } from "./view";
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

/** The room that the canvas leaves round the nodes when it fits them in the view (a share of the view). */
const FIT_PADDING = 0.12;

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
  // Where the canvas is on its way to, until it has had the time to get there: where it looks meanwhile is a point on the way, and a
  // node that is all in the view there may be out of the view the canvas ends in (a node that moved while it was going).
  const going = useRef<{ view: View; until: number } | null>(null);

  const show = useCallback(() => {
    const want = wanted.current;
    if (!want) return;
    const now = Date.now();
    if (now > want.until) {
      wanted.current = null;
      return;
    }
    const { graph: current, flow: laidOut } = latest.current;
    const rect = current ? rectOf(current, laidOut, want.node) : null;
    // (Not laid out yet: when the next layout is.)
    if (!rect) return;
    wanted.current = null;
    const moving = going.current;
    const looking = moving && now < moving.until ? moving.view : canvas.getViewport();
    const next = viewToShow(rect, looking, { width, height }, minZoom);
    if (!next) return;
    going.current = { view: next, until: now + MOVE_MS };
    void canvas.setViewport(next, { duration: MOVE_MS });
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

/**
 * Moves the canvas to show every node when the nodes have been arranged again (a layout puts them where they were not, and the strips are taller than what was in the view), once
 * the nodes are where they will be: not while the places that were given are being given back. `requests` counts the times the nodes were asked to be arranged. Draws nothing; it is in
 * the canvas for the instance of it (`useReactFlow`).
 */
function FitWhenArranged({ requests, arranging }: { requests: number; arranging: boolean }) {
  const canvas = useReactFlow();
  const shown = useRef(0);
  useEffect(() => {
    if (arranging || requests === shown.current) return;
    shown.current = requests;
    void canvas.fitView({ padding: FIT_PADDING, maxZoom: 1, duration: MOVE_MS });
  }, [canvas, requests, arranging]);
  return null;
}

export function PatchView() {
  // The canvas is laid out again when something other than a value changed.
  const graph = useClientSelection((state) => state.snapshot?.graph ?? null, sameGraphStructure);
  // The nodes that show a scope of what comes out of them: the viewer's own, which the layout makes room for (a node that cannot show one is not drawn with it, and one that is not there is nothing).
  const [shown, setShown] = useState<ReadonlySet<NodeId>>(() => new Set());
  const scopes = useMemo<Scopes>(
    () => ({
      open: shown,
      toggle: (node) => {
        setShown((now) => {
          const next = new Set(now);
          if (!next.delete(node)) next.add(node);
          return next;
        });
      },
    }),
    [shown],
  );
  const { flow, arranging, arrangeAgain } = useLayout(graph, shown);
  // How many times the nodes have been asked to be arranged again, for the canvas to be moved to show them.
  const [arrangements, setArrangements] = useState(0);
  const editor = useEditor();
  // Whether there is anything to arrange again: a node that has been moved, or one that is not where the layout would have it.
  const anyMoved = graph?.nodes.some((node) => node.ui !== undefined) ?? false;
  const arrange = useCallback(() => {
    arrangeAgain();
    setArrangements((count) => count + 1);
    // (If one is refused the page says so: `Editor` puts the reason among the notices.)
    void editor.resetLayout();
  }, [arrangeAgain, editor]);
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
      if (ends) void editor.addCable(ends.modulator, ends.node, ends.port);
    },
    [editor],
  );

  return (
    <div className="tang-patch" data-testid="patch" data-connecting={connecting ? "" : undefined}>
      <InspectorContext.Provider value={inspector}>
        <ScopesContext.Provider value={scopes}>
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
                fitViewOptions={{ padding: FIT_PADDING, maxZoom: 1 }}
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
                <Panel position="top-right">
                  <button
                    type="button"
                    className="tang-patch__arrange"
                    title="Put every node where the layout has it, and give back the places that were moved"
                    aria-label="Arrange the nodes again"
                    disabled={!anyMoved && flow.arranged}
                    onClick={arrange}
                  >
                    Arrange
                  </button>
                </Panel>
                <ShowWhatWasAdded graph={graph} flow={flow} />
                <FitWhenArranged requests={arrangements} arranging={arranging} />
              </ReactFlow>
            </ConnectingContext.Provider>
          </div>
        </ScopesContext.Provider>
        {inspected !== null ? <ParamPanel key={inspected} node={inspected} onClose={closePanel} /> : null}
      </InspectorContext.Provider>
    </div>
  );
}
