// How much of each block's time a plugin takes, and the whole block does (the engine's `telemetry::Loads`): along the bottom edge of each plugin's card, and of the master's for the whole block, with
// the number in its corner. The engine publishes each as a share of the time the block lasts, falling from its peak (to half of it in a quarter of a second); over 1 the sound is late and is heard as clicks and gaps. Written to
// the DOM on every frame with no React render, and not there at all while the master's switch has them hidden (`loadsShown`).
import { useEffect, useMemo, useRef } from "react";
import type { RefObject } from "react";

import type { NodeId } from "../../protocol";
import { LiveBar } from "../../ui/LiveBar";
import { useClient, useFrames } from "../ClientProvider";
import { showLoads, useLoadsShown } from "../loadsShown";

/** A load as it is written: the share of the block, in percent. */
export const percent = (load: number): string => `${Math.round(load * 100)}%`;

/** How a load is coloured: as it is, close to the block's time, or at it or over it. */
export function levelOf(load: number): "low" | "high" | "over" {
  if (load >= 0.8) return "over";
  if (load >= 0.5) return "high";
  return "low";
}

/** Ask for the loads while this is on the screen, and read the load of the plugin at `node`, or of the whole block for none. */
function useLoad(node: NodeId | null): () => number | undefined {
  const client = useClient();
  useEffect(() => client.watchTelemetry("loads"), [client]);
  return useMemo(() => (node === null ? () => client.telemetry.blockLoad() : () => client.telemetry.load(node)), [client, node]);
}

/** Write the load that `read` gives into `value` on every frame, as a percentage, and its level on `levelled`: nothing until it is known. */
function useWritten(read: () => number | undefined, value: RefObject<HTMLSpanElement | null>, levelled: RefObject<HTMLElement | null>): void {
  const frames = useFrames();
  useEffect(() => {
    // (Written only when the words change: a write on every frame would be a change of the DOM on every frame, which a test that waits for the page to settle never sees the end of.)
    let last: string | undefined;
    return frames.subscribe(() => {
      const element = value.current;
      if (!element) return;
      const load = read();
      const known = load !== undefined && Number.isFinite(load);
      const text = known ? percent(load) : "";
      if (text === last) return;
      last = text;
      element.textContent = text;
      const level = known ? levelOf(load) : "";
      if (levelled.current) levelled.current.dataset.level = level;
    });
  }, [frames, read, value, levelled]);
}

/** The load of the plugin at `node`, along the bottom edge of its card and as a number in its corner, while the loads are shown. */
export function PluginLoad({ node, name }: { node: NodeId; name: string }) {
  return useLoadsShown() ? <PluginLoadShown node={node} name={name} /> : null;
}

function PluginLoadShown({ node, name }: { node: NodeId; name: string }) {
  return <LoadStrip read={useLoad(node)} label={`Load of ${name}`} title={`How much of the audio thread's time ${name} takes`} />;
}

/** The load of the whole block, along the bottom edge of the master's card as a plugin's is along its own, while the loads are shown. */
export function BlockLoad() {
  return useLoadsShown() ? <BlockLoadShown /> : null;
}

function BlockLoadShown() {
  return <LoadStrip read={useLoad(null)} label="Audio load" title="How much of the time each block of sound lasts the whole engine takes to make it, the plugins and Tang's own work: at 100% the sound is late" />;
}

/** A load as a bar along the bottom edge of a card, and the number in its bottom right corner. */
function LoadStrip({ read, label, title }: { read: () => number | undefined; label: string; title: string }) {
  const frames = useFrames();
  const strip = useRef<HTMLDivElement>(null);
  const value = useRef<HTMLSpanElement>(null);
  useWritten(read, value, strip);
  return (
    <div ref={strip} className="tang-node__load" title={title}>
      <LiveBar label={label} read={read} min={0} max={1} format={percent} frames={frames} />
      <span ref={value} className="tang-load tang-node__load-value" aria-hidden="true" />
    </div>
  );
}

/** The switch in the master's bottom left corner that shows the loads or hides them, everywhere. */
export function LoadsSwitch() {
  const shown = useLoadsShown();
  return (
    <label className="tang-loads-switch nodrag nopan" title="Show how much of the audio thread's time each plugin, and the whole block, takes">
      <input
        type="checkbox"
        checked={shown}
        onChange={(event) => {
          showLoads(event.currentTarget.checked);
        }}
      />
      Load
    </label>
  );
}
