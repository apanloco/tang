import { useEffect, useMemo, useRef } from "react";

import { FLOOR_DB, Spectrum } from "../client/fft";
import { pageFrames } from "../client/frameloop";
import type { FrameLoop } from "../client/frameloop";
import { frequencyColumns, levelRows, plotSpectrum, plotWave, triggerStart } from "../client/scope";
import type { PlotSize, SpectrumRange } from "../client/scope";
import type { Plot } from "../client/trace";
import { SCOPE_FRAME_LENGTH } from "../protocol";
import { formatValue } from "./Knob";
import { drawSpectrum, drawWave } from "./scope-draw";
import type { ScopeContext, ScopeStyle } from "./scope-draw";
import "./LiveScope.css";

/** What a scope shows: the wave of the output, or the spectrum of it. */
export type ScopeView = "wave" | "spectrum";

/** Room above and below the line, so that the line at the ends of the range is drawn whole. */
const PAD = 4;

/** How fast a level of the spectrum falls when the sound is quieter, as a meter's peak does: 40 dB a second. */
const FALL_DB_PER_SECOND = 40;

/** The longest wait between two frames that the fall is made up for, in seconds: a page that was in the background does not draw a spectrum that has fallen to nothing, but one that is falling. */
const LONGEST_GAP = 0.25;

/** The frequencies that have a line down the spectrum and a label under it, and what the label says. */
const GRID = [
  { hz: 100, label: "100" },
  { hz: 1000, label: "1k" },
  { hz: 10_000, label: "10k" },
];

/** The levels, in dB, that have a line across the spectrum. */
const GRID_DB = [-20, -40, -60, -80];

export interface LiveScopeProps {
  label: string;
  /**
   * The latest frames of the output, read when there is a new frame: the left side and then the right, `SCOPE_FRAME_LENGTH` of each, the newest last; undefined when
   * there is none. It does not make the scope render again: the scope is drawn straight onto its canvas. Keep the function the same from one render to the next
   * (the scope is subscribed to it).
   */
  read: () => Float32Array | undefined;
  /** How many frames have been taken, read on every frame: a picture is made again when it changes (or when the size, the view or the time base does). */
  readFrames: () => number;
  view: ScopeView;
  /** For the wave: how many samples the width shows (the time base). */
  samples: number;
  /** For the spectrum: what frequency each of its places is. */
  sampleRate: number;
  frames?: FrameLoop;
}

/** The colours of a scope, from the tokens of the page. */
function styleOf(element: Element): ScopeStyle {
  const css = getComputedStyle(element);
  const get = (name: string, fallback: string) => css.getPropertyValue(name).trim() || fallback;
  return {
    line: get("--tang-scope-line", "#7aa2f7"),
    second: get("--tang-scope-second", "#5fc86f"),
    grid: get("--tang-scope-grid", "#353946"),
    fill: get("--tang-scope-fill", "rgba(122, 162, 247, 0.18)"),
  };
}

/** Whether the two sides of the picture that starts at `start` and is `length` samples long are not the same. */
function sidesDiffer(data: Float32Array, start: number, length: number): boolean {
  for (let at = start; at < start + length; at += 1) {
    if (Math.abs((data[at] ?? 0) - (data[SCOPE_FRAME_LENGTH + at] ?? 0)) > 1e-4) return true;
  }
  return false;
}

/** The loudest sample on either side. */
function loudest(data: Float32Array): number {
  let peak = 0;
  for (const sample of data) peak = Math.max(peak, Math.abs(sample));
  return peak;
}

/**
 * The output as it goes by, drawn on a canvas on every new frame of the scope channel and not by React: its wave, from where the left side goes up through the middle so that a
 * wave that goes round again and again stands still, with the right side under it where it is not the same; or its spectrum, with the levels falling slowly where the sound
 * is quieter than it was. It is a meter to what reads the page, whose value is the loudest sample of the picture. A picture is made again only when there is a new frame, or the
 * size, the view or the time base is not the same.
 */
export function LiveScope({ label, read, readFrames, view, samples, sampleRate, frames = pageFrames }: LiveScopeProps) {
  const meter = useRef<HTMLDivElement>(null);
  const surface = useRef<HTMLCanvasElement>(null);
  const range = useMemo<SpectrumRange>(() => ({ minHz: 20, maxHz: Math.min(20_000, sampleRate / 2), floorDb: -90, topDb: 0 }), [sampleRate]);

  useEffect(() => {
    const element = meter.current;
    const canvas = surface.current;
    if (!element || !canvas) return;
    const context: ScopeContext | null = canvas.getContext("2d");
    const style = styleOf(element);
    const analyser = new Spectrum(SCOPE_FRAME_LENGTH);
    const mono = new Float32Array(SCOPE_FRAME_LENGTH);
    const level = new Float32Array(SCOPE_FRAME_LENGTH / 2 + 1);
    // The levels as they are drawn: what the last frames were, each falling from where it was.
    const shown = new Float32Array(SCOPE_FRAME_LENGTH / 2 + 1).fill(FLOOR_DB);
    const left: Plot = { points: new Float32Array(2 * SCOPE_FRAME_LENGTH), count: 0 };
    const right: Plot = { points: new Float32Array(2 * SCOPE_FRAME_LENGTH), count: 0 };
    const spectrum: Plot = { points: new Float32Array(0), count: 0 };
    let data: Float32Array | undefined;
    let taken = -1;
    let takenAt: number | undefined;
    let peak: string | undefined;
    // The size the bitmap was made at, in the pixels of the page and in how many pixels of the device each is.
    let width = 0;
    let height = 0;
    let ratio = 0;

    return frames.subscribe((time) => {
      const count = readFrames();
      const nowWidth = canvas.offsetWidth;
      const nowHeight = canvas.offsetHeight;
      const nowRatio = window.devicePixelRatio || 1;
      const resized = nowWidth !== width || nowHeight !== height || nowRatio !== ratio;
      if (resized) {
        width = nowWidth;
        height = nowHeight;
        ratio = nowRatio;
        canvas.width = Math.round(width * ratio);
        canvas.height = Math.round(height * ratio);
        spectrum.points = new Float32Array(2 * (Math.floor(width) + 1));
      }
      if (count === taken && !resized) return;

      if (count !== taken) {
        taken = count;
        const values = read();
        data = values?.length === 2 * SCOPE_FRAME_LENGTH ? values : undefined;
        const now = data ? formatValue(loudest(data)) : undefined;
        if (now !== peak) {
          peak = now;
          if (now === undefined) element.removeAttribute("aria-valuenow");
          else element.setAttribute("aria-valuenow", now);
        }
        if (view === "spectrum") {
          if (data) {
            for (let at = 0; at < SCOPE_FRAME_LENGTH; at += 1) mono[at] = ((data[at] ?? 0) + (data[SCOPE_FRAME_LENGTH + at] ?? 0)) / 2;
            analyser.decibels(mono, level);
          } else {
            level.fill(FLOOR_DB);
          }
          const fall = FALL_DB_PER_SECOND * Math.min(LONGEST_GAP, Math.max(0, takenAt === undefined ? 0 : (time - takenAt) / 1000));
          for (let at = 0; at < shown.length; at += 1) shown[at] = Math.max(level[at] ?? FLOOR_DB, (shown[at] ?? FLOOR_DB) - fall);
        }
        takenAt = time;
      }

      if (!context || width === 0 || height === 0) return;
      const size: PlotSize = { width, height, pad: PAD };
      const scaled = { ...size, scale: ratio };
      if (view === "spectrum") {
        spectrum.count = plotSpectrum(shown, sampleRate, size, range, spectrum.points);
        drawSpectrum(context, spectrum, scaled, { columns: frequencyColumns(GRID.map((line) => line.hz), range, width), rows: levelRows(GRID_DB, range, size) }, style);
        return;
      }
      let second = false;
      if (data) {
        const side = data.subarray(0, SCOPE_FRAME_LENGTH);
        const start = triggerStart(side, samples);
        left.count = plotWave(side, start, samples, size, left.points);
        second = sidesDiffer(data, start, left.count);
        right.count = second ? plotWave(data.subarray(SCOPE_FRAME_LENGTH), start, samples, size, right.points) : 0;
      } else {
        left.count = 0;
        right.count = 0;
      }
      drawWave(context, left, second ? right : null, scaled, style);
    });
  }, [frames, read, readFrames, view, samples, sampleRate, range]);

  return (
    <div ref={meter} className="tang-scope" role="meter" aria-label={label} aria-valuemin={0} aria-valuemax={1} data-view={view}>
      <canvas ref={surface} className="tang-scope__canvas" aria-hidden="true" />
      {view === "spectrum"
        ? GRID.map((line) => (
            <span key={line.hz} className="tang-scope__label" aria-hidden="true" style={{ left: `${String(frequencyColumns([line.hz], range, 100)[0])}%` }}>
              {line.label}
            </span>
          ))
        : null}
    </div>
  );
}
