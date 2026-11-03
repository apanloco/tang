// The level of a lane or a group, on its card (docs/web-ui.md step 100; the user's first try of the microphone, "hmm I cant get the microphone to work yet", and the light for an input that is silent that it was
// offered): the peak of what comes out of it, which the engine publishes for every lane and every group, a lane that has no way out too (the microphone's lane is bare until a cable is drawn from it, and its level is
// what first says that it hears). It is drawn along the top edge of the card, over the stripe of the colour of its kind, which is the card's border (3 px): no card is taller for it.
import { useMemo } from "react";

import { slotReader } from "../../client/readers";
import type { NodeId } from "../../protocol";
import { Meter } from "../../ui/Meter";
import { useClient, useFrames } from "../ClientProvider";

/** The level of the lane or the group `node` (called `name` on the canvas), as a meter that is written to the DOM on every frame, with no React render. */
export function LevelStrip({ node, name }: { node: NodeId; name: string }) {
  const client = useClient();
  const frames = useFrames();
  const readPeak = useMemo(() => slotReader(client.telemetry, "meters", node, { type: "audioOut" }, "peak"), [client, node]);
  return (
    <div className="tang-node__level">
      <Meter label={`Level of ${name}`} readPeak={readPeak} frames={frames} />
    </div>
  );
}
