// What crosses the socket, as the rest of the app names it. The types themselves are
// generated from the Rust ones (docs/web-ui.md §7) into `generated/`, which is
// rewritten as a whole, so the list of what this app uses is kept here.
export type { CatalogEntry } from "./generated/CatalogEntry";
export type { ClientMessage } from "./generated/ClientMessage";
export type { Edge } from "./generated/Edge";
export type { EdgeId } from "./generated/EdgeId";
export type { Endpoint } from "./generated/Endpoint";
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
export type { NodeUi } from "./generated/NodeUi";
export type { NoteRange } from "./generated/NoteRange";
export type { Param } from "./generated/Param";
export type { ParamValue } from "./generated/ParamValue";
export type { PatternField } from "./generated/PatternField";
export type { PatternInfo } from "./generated/PatternInfo";
export type { PianoMeta } from "./generated/PianoMeta";
export type { Plugin } from "./generated/Plugin";
export type { Port } from "./generated/Port";
export type { Preset } from "./generated/Preset";
export type { ServerMessage } from "./generated/ServerMessage";
export type { SlotKind } from "./generated/SlotKind";
export type { Snapshot } from "./generated/Snapshot";
export type { StampedEvent } from "./generated/StampedEvent";
export type { TelemetryChannel } from "./generated/TelemetryChannel";
export type { TelemetryLayout } from "./generated/TelemetryLayout";
export type { Waveform } from "./generated/Waveform";
export type { WireError } from "./generated/WireError";
export {
  BPM_RANGE,
  DEPTH_RANGE,
  FRAME_HEADER_LEN,
  GAIN_RANGES,
  HISTORY_MAX_STEPS,
  HISTORY_SAME_STEP,
  MODULATOR_FIELD_DEFAULTS,
  MODULATOR_FIELD_RANGES,
  MODULATOR_FIELD_SCALES,
  NAME_LIMIT,
  NODE_UI_LIMIT,
  NODE_UI_PARTS,
  PATTERN_COUNT_IN_BEATS,
  PATTERN_DEFAULT_LENGTH,
  PATTERN_FIELD_RANGES,
  PINNED_LIMIT,
  PROTOCOL,
  SCALE_KINDS,
  SCALE_ROOTS,
  SCOPE_FRAME_LENGTH,
  WAVEFORMS,
} from "./generated/constants";
