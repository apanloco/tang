import type { Status } from "../client/connection";
import "./ConnectionStatus.css";

/** What a person reads for each state of the connection, and the tone of its dot. */
const STATES: Record<Status, { label: string; tone: "ok" | "busy" | "bad" | "idle" }> = {
  idle: { label: "Not connected", tone: "idle" },
  connecting: { label: "Connecting…", tone: "busy" },
  syncing: { label: "Loading the session…", tone: "busy" },
  open: { label: "Connected", tone: "ok" },
  reconnecting: { label: "Connection lost, reconnecting…", tone: "busy" },
  stopped: { label: "Disconnected", tone: "bad" },
};

export interface ConnectionStatusProps {
  status: Status;
  /** Why it is not going on, when it is not. */
  problem?: string | null;
}

export function ConnectionStatus({ status, problem = null }: ConnectionStatusProps) {
  const { label, tone } = STATES[status];
  return (
    <span className="tang-status" data-status={status} data-tone={tone} role="status">
      <span className="tang-status__dot" aria-hidden="true" />
      <span>{problem ? `${label} ${problem}` : label}</span>
    </span>
  );
}
