// What the browser tests of the tracer scene share: a spy on the page's socket (when each
// request left and was answered, how many telemetry frames came), a sampler of a knob's dot and
// of a modulator's bar taken inside the page (on its frames, with no round trip to the test
// between two samples), and a second client of the server.
import type { Locator, Page } from "@playwright/test";

import { TANG } from "../playwright.config";

/** What the spy in the page has seen. Times are the page's `performance.now()`, in milliseconds. */
export interface Wire {
  /** `frame` is how many animation frames the page had started when the request left (see `spyOnTheWire`). */
  sends: { reqId: number; at: number; frame: number; intent: { type: string; value: number } }[];
  acks: { reqId: number; at: number }[];
  /** Pointer moves seen by the page, when each was. */
  moves: number[];
  /** Binary telemetry frames received. */
  binary: number;
}

declare global {
  interface Window {
    __wire?: Wire;
    /** The browser's own WebSocket, which the spy replaces for the page: what a test uses for a client of its own. */
    __RealWebSocket?: typeof WebSocket;
  }
}

/** Put a spy in the page's WebSocket and on its pointer, before any script of the page runs. */
export async function spyOnTheWire(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const Original = window.WebSocket;
    window.__RealWebSocket = Original;
    const wire: NonNullable<Window["__wire"]> = { sends: [], acks: [], moves: [], binary: 0 };
    window.__wire = wire;
    // Count the animation frames the page has started: every callback of a frame is given the
    // same time, so the count goes up at the first of them, before any of the page's runs. Two
    // requests with the same count left in one frame, however far apart in milliseconds (a
    // browser that stalls runs two frames a few milliseconds apart, which is not the same frame).
    let frames = 0;
    let lastFrameTime = Number.NaN;
    const request = window.requestAnimationFrame.bind(window);
    window.requestAnimationFrame = (callback) =>
      request((time) => {
        if (time !== lastFrameTime) {
          lastFrameTime = time;
          frames += 1;
        }
        callback(time);
      });
    class Spy extends Original {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        this.addEventListener("message", (event: MessageEvent<unknown>) => {
          if (typeof event.data !== "string") {
            wire.binary += 1;
            return;
          }
          if (!event.data.includes('"ack"')) return;
          const message = JSON.parse(event.data) as { type: string; reqId: number };
          if (message.type === "ack") wire.acks.push({ reqId: message.reqId, at: performance.now() });
        });
      }
      override send(data: string | BufferSource | Blob): void {
        if (typeof data === "string" && data.includes('"request"')) {
          const message = JSON.parse(data) as { type: string; reqId: number; intent: { type: string; value: number } };
          if (message.type === "request") wire.sends.push({ reqId: message.reqId, at: performance.now(), frame: frames, intent: message.intent });
        }
        super.send(data);
      }
    }
    window.WebSocket = Spy;
    document.addEventListener(
      "pointermove",
      () => {
        wire.moves.push(performance.now());
      },
      true,
    );
  });
}

export async function wireOf(page: Page): Promise<Wire> {
  return page.evaluate(() => {
    if (!window.__wire) throw new Error("the page has no spy: call spyOnTheWire before it loads");
    return window.__wire;
  });
}

/** The whole knob that has this slider. */
export function knob(page: Page, name: string): Locator {
  return page.locator(".tang-knob", { has: page.getByRole("slider", { name, exact: true }) });
}

/** One look at the cutoff knob's dot and the LFO's bar, in the page. */
export interface Sample {
  at: number;
  /** Where the dot is on the knob's sweep, 0 to 1; null if it is not shown. */
  dot: number | null;
  /** What the LFO's bar says it puts out, -1 to 1. */
  lfo: number | null;
}

/**
 * Look at the cutoff knob's dot and the LFO's bar on every frame of the page for `ms`
 * milliseconds. (Inside the page, so that nothing waits for the test between two looks.)
 */
export async function sampleCutoff(page: Page, ms: number): Promise<Sample[]> {
  return page.evaluate(
    (duration) =>
      new Promise<Sample[]>((resolve) => {
        const slider = document.querySelector('[role="slider"][aria-label="cutoff"]');
        const dot = slider?.closest(".tang-knob")?.querySelector(".tang-knob__dot") ?? null;
        const meter = document.querySelector('[role="meter"][aria-label$=" output"]');
        const samples: Sample[] = [];
        const start = performance.now();
        const look = (now: number): void => {
          let where: number | null = null;
          if (dot?.getAttribute("visibility") === "visible") {
            const x = Number(dot.getAttribute("cx")) - 24;
            const y = 24 - Number(dot.getAttribute("cy"));
            // The sweep is 270 degrees from straight down-left, clockwise from up.
            where = (((Math.atan2(x, y) * 180) / Math.PI + 135) / 270);
          }
          const lfo = meter?.getAttribute("aria-valuenow");
          samples.push({ at: now, dot: where, lfo: lfo === null || lfo === undefined ? null : Number(lfo) });
          if (now - start < duration) requestAnimationFrame(look);
          else resolve(samples);
        };
        requestAnimationFrame(look);
      }),
    ms,
  );
}

/** What of the session a test looks at: the parameters of the plugins, the LFO's settings, and the depths of the cables. */
interface SessionView {
  graph: {
    nodes: { id: number; name: string; parent?: number | null; plugin?: { params: { index: number; value: number }[] }; source?: { type: string; rate?: number; waveform?: string } }[];
    edges: { kind: string; id?: number; amount?: number; from?: { node: number }; to?: { node: number; port: { type: string; index?: number } } }[];
  };
}

/** A message of the server, of which a test looks at the kind and, for a snapshot, the session. */
interface Message {
  type: string;
  snapshot?: SessionView;
}

/** What the server says of the tracer's LFO and of the cable from it to the cutoff. */
export interface LfoView {
  rate: number;
  waveform: string;
  depth: number;
}

/**
 * Another client of the server (its own socket, as the TUI would be): the LFO's rate and waveform
 * and the depth of its cable, as the server has them now.
 */
export async function lfoAccordingToTheServer(page: Page): Promise<LfoView> {
  return page.evaluate(
    (port) =>
      new Promise<LfoView>((resolve, reject) => {
        const Socket = window.__RealWebSocket ?? WebSocket;
        const socket = new Socket(`ws://127.0.0.1:${String(port)}/ws`);
        const timer = setTimeout(() => {
          reject(new Error("no snapshot"));
        }, 5000);
        socket.addEventListener("message", (event: MessageEvent<unknown>) => {
          if (typeof event.data !== "string") return;
          const message = JSON.parse(event.data) as Message;
          if (message.type === "hello") socket.send(JSON.stringify({ type: "subscribe" }));
          if (message.type === "snapshot") {
            clearTimeout(timer);
            socket.close();
            const lfo = message.snapshot?.graph.nodes.find((node) => node.name === "LFO");
            const cable = message.snapshot?.graph.edges.find((edge) => edge.kind === "modulation");
            if (lfo?.source?.rate !== undefined && lfo.source.waveform !== undefined && cable?.amount !== undefined) {
              resolve({ rate: lfo.source.rate, waveform: lfo.source.waveform, depth: cable.amount });
            } else {
              reject(new Error("the tracer has no LFO with a cable"));
            }
          }
        });
      }),
    TANG,
  );
}

/** What of the session a test looks at, as another client of the server is told it. */
export type Session = SessionView;

/**
 * The session as the server has it now, told to another client of it (its own socket, as the TUI would
 * be): what the page shows is the page's own copy of it, which is what a test is of; this is the server's.
 */
export async function sessionAccordingToTheServer(page: Page): Promise<Session> {
  return page.evaluate(
    (port) =>
      new Promise<SessionView>((resolve, reject) => {
        const Socket = window.__RealWebSocket ?? WebSocket;
        const socket = new Socket(`ws://127.0.0.1:${String(port)}/ws`);
        const timer = setTimeout(() => {
          reject(new Error("no snapshot"));
        }, 5000);
        socket.addEventListener("message", (event: MessageEvent<unknown>) => {
          if (typeof event.data !== "string") return;
          const message = JSON.parse(event.data) as Message;
          if (message.type === "hello") socket.send(JSON.stringify({ type: "subscribe" }));
          if (message.type === "snapshot" && message.snapshot) {
            clearTimeout(timer);
            socket.close();
            resolve(message.snapshot);
          }
        });
      }),
    TANG,
  );
}

/**
 * Another client of the server, as the TUI would be: its own WebSocket, from the page's
 * context, which sends no Origin. Resolves with the filter's cutoff as the server has it now.
 */
export async function cutoffAccordingToTheServer(page: Page): Promise<number> {
  return page.evaluate(
    (port) =>
      new Promise<number>((resolve, reject) => {
        // (Not the page's own socket, which is the spy's: what a test says is not what the page does.)
        const Socket = window.__RealWebSocket ?? WebSocket;
        const socket = new Socket(`ws://127.0.0.1:${String(port)}/ws`);
        const timer = setTimeout(() => {
          reject(new Error("no snapshot"));
        }, 5000);
        socket.addEventListener("message", (event: MessageEvent<unknown>) => {
          if (typeof event.data !== "string") return;
          const message = JSON.parse(event.data) as Message;
          if (message.type === "hello") socket.send(JSON.stringify({ type: "subscribe" }));
          if (message.type === "snapshot") {
            clearTimeout(timer);
            socket.close();
            const filter = message.snapshot?.graph.nodes.find((node) => node.name === "Filter");
            const cutoff = filter?.plugin?.params.find((param) => param.index === 0);
            if (cutoff) resolve(cutoff.value);
            else reject(new Error("the filter has no cutoff"));
          }
        });
      }),
    TANG,
  );
}

/**
 * Put the tracer's filter and LFO back as `examples/tracer.toml` has them, from another client of the
 * server. The tests of the real program share its one session, so what a test turns is what the
 * next would find (and what the page of another test would see).
 */
export async function resetTracer(page: Page): Promise<void> {
  await page.evaluate(
    (port) =>
      new Promise<void>((resolve, reject) => {
        // (Not the page's own socket, which is the spy's: what a test says is not what the page does.)
        const Socket = window.__RealWebSocket ?? WebSocket;
        const socket = new Socket(`ws://127.0.0.1:${String(port)}/ws`);
        const timer = setTimeout(() => {
          reject(new Error("the tracer was not reset"));
        }, 5000);
        let filter: number | undefined;
        let oscillator: number | undefined;
        let lfo: number | undefined;
        let cable: number | undefined;
        let waiting = 0;
        socket.addEventListener("message", (event: MessageEvent<unknown>) => {
          if (typeof event.data !== "string") return;
          const message = JSON.parse(event.data) as Message & { reqId?: number };
          if (message.type === "hello") socket.send(JSON.stringify({ type: "subscribe" }));
          if (message.type === "snapshot") {
            const nodes = message.snapshot?.graph.nodes ?? [];
            filter = nodes.find((node) => node.name === "Filter")?.id;
            oscillator = nodes.find((node) => node.name === "Oscillator")?.id;
            lfo = nodes.filter((node) => node.name === "LFO").sort((a, b) => a.id - b.id)[0]?.id;
            cable = message.snapshot?.graph.edges.find((edge) => edge.kind === "modulation")?.id;
            // What a test built is taken out: the modulators but the tracer's own LFO (the first, whose id is the lowest), and
            // the cables of that one but the first. (A modulator takes its cables with it.)
            const edges = message.snapshot?.graph.edges ?? [];
            const modulators = nodes.filter((node) => node.source !== undefined).map((node) => node.id);
            const own = lfo === undefined ? undefined : Math.min(...modulators);
            const extraNodes = modulators.filter((id) => id !== own);
            const ownCables = edges.filter((edge) => edge.kind === "modulation" && edge.from?.node === own).map((edge) => edge.id ?? -1);
            const extraCables = ownCables.filter((id) => id !== cable);
            const intents = [
              ...extraNodes.map((node) => ({ type: "removeNode", node })),
              ...extraCables.map((edge) => ({ type: "removeEdge", edge })),
              { type: "setModulatorField", node: lfo, field: "rate", value: 0.5 },
              { type: "setModulatorField", node: lfo, field: "waveform", value: 0 },
              { type: "setEdgeDepth", edge: cable, depth: 0.4 },
              { type: "setParam", node: filter, param: 0, value: 0.5 },
              { type: "setParam", node: filter, param: 1, value: 2 },
              { type: "setParam", node: filter, param: 2, value: 0 },
              { type: "setParam", node: filter, param: 3, value: 1 },
              { type: "setGain", node: filter, gain: "mix", value: 1 },
              { type: "setGain", node: oscillator, gain: "volume", value: 0.1 },
            ];
            waiting = intents.length;
            intents.forEach((intent, i) => {
              socket.send(JSON.stringify({ type: "request", reqId: i + 1, intent }));
            });
          }
          if (message.type === "ack") {
            waiting -= 1;
            if (waiting === 0) {
              clearTimeout(timer);
              socket.close();
              resolve();
            }
          }
        });
      }),
    TANG,
  );
}
