// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import type { PatternNote } from "../../protocol";
import { PatternRoll } from "./PatternRoll";

afterEach(cleanup);

// The roll of a pattern box: a picture of its notes (what it draws, in numbers, is in `client/patternRoll.test.ts`; here is what the page makes of it: the picture, and what a screen reader is told).

const note = (start: number, length: number, pitch: number, velocity = 100): PatternNote => ({ start, length, note: pitch, velocity });
const picture = (): HTMLElement => screen.getByRole("img");

describe("the roll of a pattern box", () => {
  test("is a picture that says whose notes it is of, how many and how long the loop is, for a person who does not see it", () => {
    render(<PatternRoll notes={[note(0, 1, 60), note(2, 0.5, 64)]} length={8} name="Bass" />);
    expect(picture().getAttribute("aria-label")).toBe("Notes of Bass: 2 notes in 8 beats");
  });

  test("says that a box has no notes, and one note", () => {
    const { rerender } = render(<PatternRoll notes={[]} length={4} name="Pattern" />);
    expect(picture().getAttribute("aria-label")).toBe("Notes of Pattern: no notes in 4 beats");
    rerender(<PatternRoll notes={[note(0, 1, 60)]} length={1} name="Pattern" />);
    expect(picture().getAttribute("aria-label")).toBe("Notes of Pattern: 1 note in 1 beat");
  });

  test("is as wide as the loop in beats and as high as the notes go in semitones, stretched to the node, with no pointer to take", () => {
    render(<PatternRoll notes={[note(0, 1, 60), note(1, 1, 90)]} length={8} name="Bass" />);
    expect(picture().getAttribute("viewBox")).toBe("0 0 8 31");
    expect(picture().getAttribute("preserveAspectRatio")).toBe("none");
  });

  test("has a mark for each note, at the place of its start, as long as it is, and as hard as it was played", () => {
    render(<PatternRoll notes={[note(1, 2, 60, 127), note(4, 0.5, 64, 1)]} length={8} name="Bass" />);
    const marks = Array.from(picture().querySelectorAll(".tang-roll__note"));
    expect(marks.map((mark) => [mark.getAttribute("x"), mark.getAttribute("width")])).toEqual([
      ["1", "2"],
      ["4", "0.5"],
    ]);
    expect(marks.map((mark) => Number(mark.getAttribute("opacity")))).toEqual([1, 0.4 + 0.6 / 127]);
  });

  test("has a line at each beat inside the loop, heavier at each bar, which do not scale their strokes with the picture", () => {
    render(<PatternRoll notes={[]} length={8} name="Bass" />);
    const lines = Array.from(picture().querySelectorAll("line"));
    expect(lines.map((line) => line.getAttribute("x1"))).toEqual(["1", "2", "3", "4", "5", "6", "7"]);
    expect(lines.filter((line) => line.classList.contains("tang-roll__line--bar")).map((line) => line.getAttribute("x1"))).toEqual(["4"]);
    for (const line of lines) expect(line.getAttribute("vector-effect")).toBe("non-scaling-stroke");
    // (Each is straight up and down, from the top of the picture to the bottom of it: twelve semitones.)
    for (const line of lines) expect([line.getAttribute("x1") === line.getAttribute("x2"), line.getAttribute("y1"), line.getAttribute("y2")]).toEqual([true, "0", "12"]);
  });

  test("shows the notes that it is given, and the next ones when they change, a note taken out and one put in", () => {
    const { rerender } = render(<PatternRoll notes={[note(0, 1, 60)]} length={4} name="Bass" />);
    expect(picture().querySelectorAll(".tang-roll__note")).toHaveLength(1);
    rerender(<PatternRoll notes={[note(0, 1, 60), note(1, 1, 62)]} length={4} name="Bass" />);
    expect(picture().querySelectorAll(".tang-roll__note")).toHaveLength(2);
    rerender(<PatternRoll notes={[]} length={4} name="Bass" />);
    expect(picture().querySelectorAll(".tang-roll__note")).toHaveLength(0);
    expect(picture().getAttribute("aria-label")).toBe("Notes of Bass: no notes in 4 beats");
  });
});
