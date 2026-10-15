// Telemetry on the client: decoding the binary frames, knowing which value is which,
// and holding the latest of each channel where a drawing loop can read it without
// going through any framework's state (docs/web-ui.md §6 and §9.4).
import { FRAME_HEADER_LEN, SCOPE_FRAME_LENGTH } from "../protocol";
import type { NodeId, Port, SlotKind, TelemetryChannel, TelemetryLayout } from "../protocol";

/** The channel numbers of a frame's header, in order. */
const CHANNELS: readonly TelemetryChannel[] = ["meters", "modulation", "held", "scope"];

/** The channels whose values are the entries of a list in the layout (the held notes and the scope follow none). */
export type LayoutChannel = Exclude<TelemetryChannel, "held" | "scope">;
const LAYOUT_CHANNELS: readonly LayoutChannel[] = ["meters", "modulation"];

/** How many notes there are: a frame of the held notes has a value for each of them, at the place of its number. */
export const NOTES = 128;

/** One telemetry frame: the current values of one channel. */
export interface Frame {
  channel: TelemetryChannel;
  /** The `rev` of the layout the values follow (0 for the held notes and the scope, which follow none). */
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

function entryKey(channel: LayoutChannel, node: NodeId, port: string, kind: SlotKind): string {
  return `${channel}/${node}/${port}/${kind}`;
}

/** Where in a channel's frames the value of a node's port is, by what it is. */
export class TelemetryIndex {
  readonly #positions = new Map<string, number>();

  constructor(readonly layout: TelemetryLayout) {
    for (const channel of LAYOUT_CHANNELS) {
      layout[channel].forEach((entry, position) => {
        this.#positions.set(entryKey(channel, entry.node, portKey(entry.port), entry.kind), position);
      });
    }
  }

  /** The position of the value of `kind` at `port` of `node`, if the layout has it. */
  find(channel: LayoutChannel, node: NodeId, port: Port, kind: SlotKind): number | undefined {
    return this.#positions.get(entryKey(channel, node, portKey(port), kind));
  }
}

/**
 * The latest values of each channel. A channel's array is made when the layout is set
 * and written in place by every frame after it, so a loop that has read it once
 * always sees the newest values. (The held notes and the scope follow no layout: their arrays are there
 * from the start, a value for each note and the frames of the two sides of the output, and are not reset by a layout.)
 */
export class TelemetryBuffers {
  #index: TelemetryIndex | null = null;
  readonly #latest = new Map<TelemetryChannel, Float32Array>([
    ["held", new Float32Array(NOTES)],
    ["scope", new Float32Array(2 * SCOPE_FRAME_LENGTH)],
  ]);
  /** How many frames each channel has had taken. */
  readonly #counts = new Map<TelemetryChannel, number>();
  /** The channels that have had a frame of the layout that is set (the held notes and the scope: a frame at all). */
  readonly #heard = new Set<TelemetryChannel>();
  #frames = 0;

  /** What the slots mean now; the values are reset (they are of another layout, or none yet). */
  setLayout(layout: TelemetryLayout): void {
    if (this.#index?.layout.rev === layout.rev && this.#sameLengths(layout)) return;
    this.#index = new TelemetryIndex(layout);
    for (const channel of LAYOUT_CHANNELS) {
      this.#latest.set(channel, new Float32Array(layout[channel].length));
      this.#heard.delete(channel);
    }
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
    // (The held notes and the scope follow no layout: a frame of them is taken at any time, if it is the length it should be.)
    const fits = frame.channel === "held" || frame.channel === "scope" ? true : this.#index?.layout.rev === frame.layoutRev;
    if (!fits || latest?.length !== frame.values.length) return false;
    latest.set(frame.values);
    this.#heard.add(frame.channel);
    this.#counts.set(frame.channel, (this.#counts.get(frame.channel) ?? 0) + 1);
    this.#frames += 1;
    return true;
  }

  /** The latest values of a channel, in the order of the layout's list; undefined before a layout is set. */
  values(channel: TelemetryChannel): Float32Array | undefined {
    return this.#latest.get(channel);
  }

  /** Where the value of `kind` at `port` of `node` is in the channel's values. */
  find(channel: LayoutChannel, node: NodeId, port: Port, kind: SlotKind): number | undefined {
    return this.#index?.find(channel, node, port, kind);
  }

  /** The latest value of `kind` at `port` of `node`, if the layout has it. */
  value(channel: LayoutChannel, node: NodeId, port: Port, kind: SlotKind): number | undefined {
    const at = this.find(channel, node, port, kind);
    return at === undefined ? undefined : this.#latest.get(channel)?.[at];
  }

  /** Whether note number `note` is held, by whichever source holds it, as of the latest frame: none is before one has come. */
  isHeld(note: number): boolean {
    return (this.#latest.get("held")?.[note] ?? 0) > 0;
  }

  /**
   * Forget which notes are held, in the array a reader already has: the connection that told is gone, and which notes the
   * server holds is not known until a frame says (a note that is held when the page comes back is told then).
   */
  forgetHeld(): void {
    this.#latest.get("held")?.fill(0);
  }

  /**
   * Forget what the scope had, in the array a reader already holds: the connection that told is gone, and what the output is now is not known until a frame says
   * (a picture that stays on the screen is of a sound that is not there).
   */
  forgetScope(): void {
    this.#latest.get("scope")?.fill(0);
  }

  /** How many frames have been taken: a loop can tell whether there is anything new. */
  get frames(): number {
    return this.#frames;
  }

  /** How many frames of `channel` have been taken: a drawing of one channel can tell whether there is anything new for it. */
  framesOf(channel: TelemetryChannel): number {
    return this.#counts.get(channel) ?? 0;
  }

  #sameLengths(layout: TelemetryLayout): boolean {
    return LAYOUT_CHANNELS.every((channel) => this.#latest.get(channel)?.length === layout[channel].length);
  }
}
