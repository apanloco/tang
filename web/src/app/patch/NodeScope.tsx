// The scope of a node (docs/web-ui.md §9.3, D47): the signal that comes out of an instrument, an effect or a group, as a wave or a spectrum, in the node. The master's scope is the same
// drawing (`ScopePanel`), of the output mix, which the server sends to every client that asks; a node's is of a signal that the engine listens to only while somebody watches it,
// so it is asked for while the scope is in the node (`Client.watchProbe`) and let go of when it is taken away.
import { useCallback, useContext, useEffect, useState } from "react";

import type { NodeId } from "../../protocol";
import { Choice } from "../../ui/Choice";
import { LiveScope } from "../../ui/LiveScope";
import type { ScopeView } from "../../ui/LiveScope";
import { useClient, useClientState, useFrames } from "../ClientProvider";
import { ScopesContext, scopeButtonId } from "./scopes";

/** What a scope can show, in the order of its choice. */
const SCOPE_VIEWS: readonly { view: ScopeView; label: string }[] = [
  { view: "wave", label: "Wave" },
  { view: "spectrum", label: "Spectrum" },
];

/** How many samples the scope's wave can show across its width, a time base each (the third is the one it begins with). */
const SCOPE_TIMES: readonly number[] = [256, 512, 1024, 2048];
const SCOPE_TIME_AT_FIRST = 2;

/**
 * A scope with the choices of its view and of its time base, which are its own (the page's, not the session's): the cells of the body of a node, one under the other. `read` and
 * `readFrames` are what `LiveScope` is given (keep them the same from one render to the next).
 */
export function ScopePanel({ label, read, readFrames }: { label: string; read: () => Float32Array | undefined; readFrames: () => number }) {
  const frames = useFrames();
  const sampleRate = useClientState((state) => state.snapshot?.meta.sampleRate ?? 48_000);
  const [view, setView] = useState(0);
  const [time, setTime] = useState(SCOPE_TIME_AT_FIRST);
  const shown = SCOPE_VIEWS[view]?.view ?? "wave";
  return (
    <>
      <LiveScope label={label} read={read} readFrames={readFrames} view={shown} samples={SCOPE_TIMES[time] ?? 1024} sampleRate={sampleRate} frames={frames} />
      <div className="tang-scope-choices nodrag nopan nowheel">
        <Choice label="View" labels={SCOPE_VIEWS.map((choice) => choice.label)} value={view} onChange={setView} />
        {shown === "wave" ? <Choice label="Time" labels={SCOPE_TIMES.map((samples) => `${String(Math.round((samples / sampleRate) * 1000))} ms`)} value={time} onChange={setTime} /> : null}
      </div>
    </>
  );
}

/** The scope of what comes out of the node `node`, below its controls: the server is asked for the signal for as long as it is here. */
export function NodeScope({ node, name }: { node: NodeId; name: string }) {
  const client = useClient();
  useEffect(() => client.watchProbe(node), [client, node]);
  const read = useCallback(() => client.telemetry.probe(node), [client, node]);
  const readFrames = useCallback(() => client.telemetry.probeFrames(node), [client, node]);
  return (
    <div className="tang-node__body">
      <ScopePanel label={`Output of ${name}`} read={read} readFrames={readFrames} />
    </div>
  );
}

/** The button of an instrument, an effect or a group that shows the scope of what comes out of it in its node, or takes it away if it is shown. */
export function ScopeButton({ node, name }: { node: NodeId; name: string }) {
  const scopes = useContext(ScopesContext);
  return (
    <button
      type="button"
      id={scopeButtonId(node)}
      className="tang-menu-button tang-node__scope nodrag nopan"
      aria-label={`Scope of ${name}`}
      title={`What comes out of ${name}, as a wave or a spectrum`}
      aria-expanded={scopes.open.has(node)}
      onClick={() => {
        scopes.toggle(node);
      }}
    >
      ∿
    </button>
  );
}
