// What a cable shows of what goes through it (docs/web-ui.md §9.3): it is as bright as the output of the modulator that
// drives it is high, across the range that output goes over, so that a cable is seen to carry a signal. The brightness is a
// number on the cable (`--signal`, from 0 to 1) that the style of the canvas reads; it is written straight to the DOM on the
// frames the value moves in, as everything that follows telemetry is, and no React renders for it.
import { useEffect } from "react";
import type { RefObject } from "react";

import { slotReader } from "../../client/readers";
import { nodeById } from "../../client/selectors";
import type { NodeId } from "../../protocol";
import { fractionOf } from "../../ui/knob-geometry";
import { useClient, useClientSelection, useFrames } from "../ClientProvider";
import { outputRange } from "./describe";

/** Keep `--signal` on the element `light` where the output of the modulator `modulator` is across its range, on every frame it moves; none while there is no output to know. */
export function useCableLight(light: RefObject<SVGGElement | null>, modulator: NodeId | undefined): void {
  const client = useClient();
  const frames = useFrames();
  // (What kind it is says what range its output goes over; nothing else of it matters here.)
  const kind = useClientSelection(
    (state) => {
      const node = modulator === undefined ? undefined : nodeById(state, modulator);
      return node?.kind === "modulator" ? node.source.type : null;
    },
    (a, b) => a === b,
  );

  useEffect(() => {
    const element = light.current;
    if (!element || modulator === undefined || kind === null) return;
    const read = slotReader(client.telemetry, "modulation", modulator, { type: "modOut" }, "modOutput");
    const { min, max } = outputRange(kind);
    let last: number | undefined;
    return frames.subscribe(() => {
      const value = read();
      if (value === last) return;
      last = value;
      if (value === undefined || !Number.isFinite(value)) element.style.removeProperty("--signal");
      else element.style.setProperty("--signal", String(fractionOf(value, min, max)));
    });
  }, [client, frames, light, modulator, kind]);
}
