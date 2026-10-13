// What crosses the socket, as the rest of the app names it. The types themselves are
// generated from the Rust ones (docs/web-ui.md §7) into `generated/`, which is
// rewritten as a whole, so the list of what this app uses is kept here.
export type { ClientMessage } from "./generated/ClientMessage";
export type { Edge } from "./generated/Edge";
export type { EdgeId } from "./generated/EdgeId";
export type { ErrorCode } from "./generated/ErrorCode";
export type { Event } from "./generated/Event";
export type { Gain } from "./generated/Gain";
export type { Graph } from "./generated/Graph";
export type { Intent } from "./generated/Intent";
export type { LayoutEntry } from "./generated/LayoutEntry";
export type { MidiControl } from "./generated/MidiControl";
export type { ModulatorField } from "./generated/ModulatorField";
export type { ModulatorKind } from "./generated/ModulatorKind";
export type { ModulatorSource } from "./generated/ModulatorSource";
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
export type { Waveform } from "./generated/Waveform";
export type { WireError } from "./generated/WireError";
export {
  DEPTH_RANGE,
  FRAME_HEADER_LEN,
  GAIN_RANGES,
  MODULATOR_FIELD_DEFAULTS,
  MODULATOR_FIELD_RANGES,
  PROTOCOL,
  WAVEFORMS,
} from "./generated/constants";
