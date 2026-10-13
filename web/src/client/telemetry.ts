// Telemetry on the client: decoding the binary frames, knowing which value is which,
// and holding the latest of each channel where a drawing loop can read it without
// going through any framework's state (docs/web-ui.md §6 and §9.4).
import { FRAME_HEADER_LEN } from "../protocol";
import type { NodeId, Port, SlotKind, TelemetryChannel, TelemetryLayout } from "../protocol";

/** The channel numbers of a frame's header, in order. */
const CHANNELS: readonly TelemetryChannel[] = ["meters", "modulation"];

/** One telemetry frame: the current values of one channel. */
export interface Frame {
  channel: TelemetryChannel;
  /** The `rev` of the layout the values follow. */
  layoutRev: number;
  /** Value *i* belongs to entry *i* of the channel's list in that layout. */
  values: Float32Array;
}

/**
 * Decode a binary frame (`u8 channel | u8 flags | u16 n | u32 layout_rev | n × f32`,
 * little endian). Null for what is not one: too short, a channel this client does not
 * know, or a length that is not the header's count.
 */
export function decodeFrame(data: ArrayBuffer | ArrayBufferView): Frame | null {
  const view = ArrayBuffer.isView(data) ? new DataView(data.buffer, data.byteOffset, data.byteLength) : new DataView(data);
  if (view.byteLength < FRAME_HEADER_LEN) return null;
  const channel = CHANNELS[view.getUint8(0)];
  if (channel === undefined) return null;
  const count = view.getUint16(2, true);
  if (view.byteLength !== FRAME_HEADER_LEN + 4 * count) return null;
  const values = new Float32Array(count);
  for (let i = 0; i < count; i++) values[i] = view.getFloat32(FRAME_HEADER_LEN + 4 * i, true);
  return { channel, layoutRev: view.getUint32(4, true), values };
}

/** A port as one string, so that it can be part of a key. */
export function portKey(port: Port): string {
  switch (port.type) {
    case "param":
      return `param:${port.index}`;
    case "depth":
      return `depth:${port.edge}`;
    default:
      return port.type;
  }
}

function entryKey(channel: TelemetryChannel, node: NodeId, port: string, kind: SlotKind): string {
  return `${channel}/${node}/${port}/${kind}`;
}

/** Where in a channel's frames the value of a node's port is, by what it is. */
export class TelemetryIndex {
  readonly #positions = new Map<string, number>();

  constructor(readonly layout: TelemetryLayout) {
    for (const channel of CHANNELS) {
      layout[channel].forEach((entry, position) => {
        this.#positions.set(entryKey(channel, entry.node, portKey(entry.port), entry.kind), position);
      });
    }
  }

  /** The position of the value of `kind` at `port` of `node`, if the layout has it. */
  find(channel: TelemetryChannel, node: NodeId, port: Port, kind: SlotKind): number | undefined {
    return this.#positions.get(entryKey(channel, node, portKey(port), kind));
  }
}

/**
 * The latest values of each channel. A channel's array is made when the layout is set
 * and written in place by every frame after it, so a loop that has read it once
 * always sees the newest values.
 */
export class TelemetryBuffers {
  #index: TelemetryIndex | null = null;
  readonly #latest = new Map<TelemetryChannel, Float32Array>();
  /** The channels that have had a frame of the layout that is set. */
  readonly #heard = new Set<TelemetryChannel>();
  #frames = 0;

  /** What the slots mean now; the values are reset (they are of another layout, or none yet). */
  setLayout(layout: TelemetryLayout): void {
    if (this.#index?.layout.rev === layout.rev && this.#sameLengths(layout)) return;
    this.#index = new TelemetryIndex(layout);
    this.#latest.clear();
    this.#heard.clear();
    for (const channel of CHANNELS) this.#latest.set(channel, new Float32Array(layout[channel].length));
  }

  /** The `rev` of the layout that is set: what a reader that has found a slot goes by to know the slot may have moved. */
  get layoutRev(): number | undefined {
    return this.#index?.layout.rev;
  }

  /** Whether a frame of the layout that is set has come for `channel`: before one has, its values are only zeros. */
  has(channel: TelemetryChannel): boolean {
    return this.#heard.has(channel);
  }

  /**
   * Take a frame. False, and nothing changes, if it does not follow the layout that is
   * set (the server has made a new one that this client has not been told of, or this one
   * is not set yet): the next frames will.
   */
  accept(frame: Frame): boolean {
    const latest = this.#latest.get(frame.channel);
    if (!this.#index || this.#index.layout.rev !== frame.layoutRev || latest?.length !== frame.values.length) return false;
    latest.set(frame.values);
    this.#heard.add(frame.channel);
    this.#frames += 1;
    return true;
  }

  /** The latest values of a channel, in the order of the layout's list; undefined before a layout is set. */
  values(channel: TelemetryChannel): Float32Array | undefined {
    return this.#latest.get(channel);
  }

  /** Where the value of `kind` at `port` of `node` is in the channel's values. */
  find(channel: TelemetryChannel, node: NodeId, port: Port, kind: SlotKind): number | undefined {
    return this.#index?.find(channel, node, port, kind);
  }

  /** The latest value of `kind` at `port` of `node`, if the layout has it. */
  value(channel: TelemetryChannel, node: NodeId, port: Port, kind: SlotKind): number | undefined {
    const at = this.find(channel, node, port, kind);
    return at === undefined ? undefined : this.#latest.get(channel)?.[at];
  }

  /** How many frames have been taken: a loop can tell whether there is anything new. */
  get frames(): number {
    return this.#frames;
  }

  #sameLengths(layout: TelemetryLayout): boolean {
    return CHANNELS.every((channel) => this.#latest.get(channel)?.length === layout[channel].length);
  }
}
