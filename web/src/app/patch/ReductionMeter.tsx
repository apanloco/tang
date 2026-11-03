// How far the limiter on the output mix has brought the sound down lately, on the master's card (docs/web-ui.md step 102): the engine publishes it in decibels, falling as a meter's peak does (40 decibels a
// second), and 0 while the limiter is out of the sound or has nothing to do. A bar that fills from the left and the number, written to the DOM on every frame with no React render.
import { useEffect, useMemo, useRef } from "react";

import { slotReader } from "../../client/readers";
import type { NodeId } from "../../protocol";
import { LiveBar } from "../../ui/LiveBar";
import { useClient, useFrames } from "../ClientProvider";

/** The reduction at the end of the bar, in decibels: a limiter that brings the sound down by more than this fills it. */
export const REDUCTION_SCALE = 18;

/** A reduction as it is written: `-3.2 dB`, and `0 dB` for none (the engine holds what is under 0.05 to nothing). */
export const reduction = (db: number): string => (db < 0.05 ? "0 dB" : `-${db.toFixed(1)} dB`);

/** The reduction of the limiter at the master `node`, as a bar and a number. */
export function ReductionMeter({ node }: { node: NodeId }) {
  const client = useClient();
  const frames = useFrames();
  const read = useMemo(() => slotReader(client.telemetry, "meters", node, { type: "audioIn" }, "reduction"), [client, node]);
  const value = useRef<HTMLSpanElement>(null);
  useEffect(() => {
    // (Written only when the words change: a write on every frame would be a change of the DOM on every frame, which a test that waits for the page to settle never sees the end of.)
    let last: string | undefined;
    return frames.subscribe(() => {
      const element = value.current;
      if (!element) return;
      const shown = read();
      const text = shown === undefined || !Number.isFinite(shown) ? "" : reduction(shown);
      if (text === last) return;
      last = text;
      element.textContent = text;
    });
  }, [frames, read]);
  return (
    <div className="tang-reduction">
      <span className="tang-reduction__label">Reduction</span>
      <LiveBar label="Limiter reduction" read={read} min={0} max={REDUCTION_SCALE} format={reduction} frames={frames} />
      <span ref={value} className="tang-reduction__value" aria-hidden="true" />
    </div>
  );
}
