// What the tests of what is drawn on a canvas have in common (only tests import this): a context that is not a
// canvas's, which remembers what it was asked to do, in order, so that a test can say what was drawn.

/** What a call of a recording context was: the name of the method (or of the property that was set) and what it was given. */
export type Call = [name: string, ...args: unknown[]];

export interface RecordingContext {
  calls: Call[];
  setTransform: (...args: number[]) => void;
  clearRect: (...args: number[]) => void;
  beginPath: () => void;
  moveTo: (...args: number[]) => void;
  lineTo: (...args: number[]) => void;
  stroke: () => void;
  arc: (...args: number[]) => void;
  fill: () => void;
  strokeStyle: unknown;
  fillStyle: unknown;
  lineWidth: unknown;
  lineJoin: unknown;
  lineCap: unknown;
}

const METHODS = ["setTransform", "clearRect", "beginPath", "moveTo", "lineTo", "stroke", "arc", "fill"] as const;
const PROPERTIES = ["strokeStyle", "fillStyle", "lineWidth", "lineJoin", "lineCap"] as const;

/** A context that does nothing but remember: each method and each property that is set is a call in `calls`. */
export function recordingContext(): RecordingContext {
  const calls: Call[] = [];
  const context: Record<string, unknown> = { calls };
  for (const name of METHODS) {
    context[name] = (...args: unknown[]) => {
      calls.push([name, ...args]);
    };
  }
  for (const name of PROPERTIES) {
    Object.defineProperty(context, name, {
      set(value: unknown) {
        calls.push([name, value]);
      },
      get: () => undefined,
    });
  }
  return context as unknown as RecordingContext;
}

/** The calls of a kind, in order, as what each was given. */
export function callsOf(context: RecordingContext, name: string): unknown[][] {
  return context.calls.filter((call) => call[0] === name).map(([, ...args]) => args);
}
