// Reading telemetry for a drawing loop (docs/web-ui.md §9.4). A drawing reads its value on
// every frame, so the work of finding the value's slot is done once for each layout and
// not once for each frame: the reader keeps the position it found until the layout changes.
import type { NodeId, Port, SlotKind, TelemetryChannel } from "../protocol";
import type { TelemetryBuffers } from "./telemetry";

/**
 * A function that gives the latest value of `kind` at `port` of `node` in `channel`, or
 * `undefined` when there is none: there is no layout yet, the layout has no such slot (a
 * parameter that nothing modulates has no effective value), or no frame of this layout has
 * come, so that what the buffer holds is only the zero it started with.
 */
export function slotReader(
  telemetry: TelemetryBuffers,
  channel: TelemetryChannel,
  node: NodeId,
  port: Port,
  kind: SlotKind,
): () => number | undefined {
  let layoutRev: number | undefined;
  let position: number | undefined;
  return () => {
    const current = telemetry.layoutRev;
    if (current !== layoutRev) {
      layoutRev = current;
      position = telemetry.find(channel, node, port, kind);
    }
    if (position === undefined || !telemetry.has(channel)) return undefined;
    return telemetry.values(channel)?.[position];
  };
}
