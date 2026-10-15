import { useSlowRequests } from "./ClientProvider";
import "./BusyLight.css";

/**
 * Says that the server is busy with what the page asked: a request has waited longer than the client was told to wait for its answer (a plugin that takes seconds
 * to load: the server does it on the thread that answers, so every other request waits behind it). It is the page's patience and nothing else: nothing is blocked,
 * and the page goes on. A progress bar that does not say how far, and not a status: the state of the connection is the page's one.
 */
export function BusyLight() {
  const slow = useSlowRequests();
  if (slow === 0) return null;
  return (
    <div className="tang-busy" role="progressbar" aria-label="Working">
      <span className="tang-busy__spinner" aria-hidden="true" />
      Working…
    </div>
  );
}
