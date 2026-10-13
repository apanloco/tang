// What crosses the socket, as the rest of the app names it. The types themselves are
// generated from the Rust ones (docs/web-ui.md §7) into `generated/`, which is
// rewritten as a whole, so the list of what this app uses is kept here.
export type { ClientMessage } from "./generated/ClientMessage";
export type { Edge } from "./generated/Edge";
export type { ErrorCode } from "./generated/ErrorCode";
export type { Event } from "./generated/Event";
export type { Gain } from "./generated/Gain";
export type { Graph } from "./generated/Graph";
export type { Intent } from "./generated/Intent";
export type { LayoutEntry } from "./generated/LayoutEntry";
export type { Node } from "./generated/Node";
export type { NodeId } from "./generated/NodeId";
export type { Param } from "./generated/Param";
export type { ParamValue } from "./generated/ParamValue";
export type { Plugin } from "./generated/Plugin";
export type { Port } from "./generated/Port";
export type { ServerMessage } from "./generated/ServerMessage";
export type { SlotKind } from "./generated/SlotKind";
export type { Snapshot } from "./generated/Snapshot";
export type { StampedEvent } from "./generated/StampedEvent";
export type { TelemetryChannel } from "./generated/TelemetryChannel";
export type { TelemetryLayout } from "./generated/TelemetryLayout";
export type { WireError } from "./generated/WireError";
export { FRAME_HEADER_LEN, PROTOCOL } from "./generated/constants";
