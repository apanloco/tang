// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import { PATTERN_COUNT_IN_BEATS, PATTERN_FIELD_RANGES } from "../../protocol";
import type { Intent, PatternInfo, Snapshot } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { PatchView } from "./PatchView";
import { PatternCell } from "./PatternCell";

afterEach(cleanup);

// The pattern of a lane under its controls: a recording of the notes that are played, which a held key then plays. What is looked at here is the page: what it shows, what it asks and
// when, and what it says when it is refused (what the controller makes of each request is in `apply.rs` and `patterns.test.ts`, and what the engine makes of a recording in
// `recording.test.ts`).

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
/** The beat of the tracer's tempo (120 a minute), in seconds. */
const BEAT = 0.5;

const cell = (): HTMLElement => screen.getByRole("group", { name: "Pattern of Oscillator" });
const recordButton = (): HTMLElement => within(cell()).getByRole("button", { name: /^(Record a pattern on|Stop recording) Oscillator$/ });
const clearButton = (): HTMLElement => within(cell()).getByRole("button", { name: "Clear the pattern of Oscillator" });
const length = (): HTMLInputElement => within(cell()).getByRole<HTMLInputElement>("spinbutton", { name: "Length of the pattern of Oscillator" });
const line = (): string => cell().querySelector(".tang-pattern__line")?.textContent ?? "(no line)";
const SWITCHES = { enabled: "Play the pattern of Oscillator", looping: "Loop the pattern of Oscillator", transpose: "Transpose the pattern of Oscillator in key" } as const;
const switchOf = (field: keyof typeof SWITCHES): HTMLElement => within(cell()).getByRole("button", { name: SWITCHES[field] });
const pressed = (field: keyof typeof SWITCHES): string | null => switchOf(field).getAttribute("aria-pressed");

/** The intents about a pattern that the server has been asked for, in order. */
const asked = (made: { requests: () => { intent: unknown }[] }): Intent[] =>
  made.requests().flatMap(({ intent }) => (typeof intent === "object" && intent !== null && "type" in intent && ["setRecording", "clearPattern", "setPatternField"].includes(String(intent.type)) ? [intent as Intent] : []));

/** The tracer with a pattern on the oscillator's lane. */
function withPattern(over: Partial<PatternInfo> = {}): Snapshot {
  const snapshot = structuredClone(tracerSnapshot);
  const lane = snapshot.graph.nodes.find((n) => n.id === oscillator.id);
  if (lane?.kind !== "instrument") throw new Error("no lane");
  lane.pattern = { recording: false, enabled: true, looping: true, inKey: false, lengthBeats: 2, bpm: 90, notes: 3, ...over };
  return snapshot;
}

/** What the page says when it could not do what was asked. */
function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

const view = (snapshot: Snapshot = tracerSnapshot) => rig(<PatternCell node={oscillator.id} name="Oscillator" />, { snapshot });
const turn = (): Promise<void> =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30));
  });

describe("the pattern of a lane", () => {
  test("is a group of controls on every lane, an empty one too, and on no other node", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    const lanes = reachFixture.snapshot.graph.nodes.filter((node) => node.kind === "instrument");
    await waitFor(() => {
      expect(screen.getAllByRole("group", { name: /^Pattern of / })).toHaveLength(lanes.length);
    });
    expect(lanes.some((lane) => lane.plugin === null)).toBe(true);
  });

  test("is drawn for no node that is not a lane", async () => {
    await rig(<PatternCell node={nodeNamed(tracerSnapshot, "Filter").id} name="Filter" />);
    await turn();
    expect(screen.queryByRole("group")).toBeNull();
  });

  test("with none has a button to record one and the length it would have, and nothing to clear or to switch", async () => {
    await view();
    expect(recordButton().textContent).toBe("● Record");
    expect(recordButton().getAttribute("title")).toBe(`Record a pattern: ${String(PATTERN_COUNT_IN_BEATS)} beats are counted in, then the notes you play for 4 beats at 120 BPM are the pattern`);
    expect(line()).toBe("No pattern yet");
    expect(length().value).toBe("4");
    expect([length().getAttribute("aria-valuemin"), length().getAttribute("aria-valuemax")]).toEqual(PATTERN_FIELD_RANGES.length.map(String));
    expect(clearButton().hasAttribute("disabled")).toBe(true);
    for (const field of ["enabled", "looping", "transpose"] as const) expect(switchOf(field).hasAttribute("disabled"), field).toBe(true);
  });

  test("says in the hint of the record button how long the next recording is, which is the length of the pattern the lane has", async () => {
    await view(withPattern({ lengthBeats: 2 }));
    expect(recordButton().getAttribute("title")).toContain("for 2 beats at 120 BPM");
  });

  test("with one says what it is, with its switches as they are and a button to clear it", async () => {
    await view(withPattern({ lengthBeats: 2, bpm: 90, notes: 3, enabled: true, looping: false, inKey: true }));
    expect(line()).toBe("2 beats · 3 notes · 90 BPM");
    expect(length().value).toBe("2");
    expect([pressed("enabled"), pressed("looping"), pressed("transpose")]).toEqual(["true", "false", "true"]);
    expect(switchOf("enabled").hasAttribute("disabled")).toBe(false);
    expect(clearButton().hasAttribute("disabled")).toBe(false);
    expect(recordButton().textContent).toBe("● Record");
  });

  test("shows the visible words of the switches: they are Play, Loop and In key", async () => {
    await view(withPattern());
    expect(["enabled", "looping", "transpose"].map((field) => switchOf(field as keyof typeof SWITCHES).textContent)).toEqual(["Play", "Loop", "In key"]);
  });
});

describe("recording a pattern", () => {
  test("is started by the record button, which then stops it, and the line says what it is doing", async () => {
    const made = await view();
    fireEvent.click(recordButton());
    await waitFor(() => {
      expect(recordButton().textContent).toBe("■ Stop");
    });
    expect(recordButton().getAttribute("aria-label")).toBe("Stop recording Oscillator");
    // (The button is red all through while a recording goes on, and not otherwise: the style says so by this class.)
    expect(recordButton().classList.contains("tang-pattern__record--going")).toBe(true);
    expect(line()).toBe("Counting in, then 4 beats at 120 BPM: play now");
    expect(asked(made)).toEqual([{ type: "setRecording", node: oscillator.id, recording: true }]);
    fireEvent.click(recordButton());
    await waitFor(() => {
      expect(recordButton().textContent).toBe("● Record");
    });
    expect(recordButton().classList.contains("tang-pattern__record--going")).toBe(false);
    expect(asked(made)).toEqual([
      { type: "setRecording", node: oscillator.id, recording: true },
      { type: "setRecording", node: oscillator.id, recording: false },
    ]);
  });

  test("is shown while it goes on in another page, and when it is done by itself the pattern is what the lane has: its notes, and the switches to play it", async () => {
    const made = await view();
    act(() => {
      made.server.apply({ type: "setRecording", node: oscillator.id, recording: true });
    });
    await waitFor(() => {
      expect(recordButton().textContent).toBe("■ Stop");
    });
    // Two notes are played while it goes on; a recording of four beats, counted in with four, is done after eight beats.
    act(() => {
      made.client.noteOn(60);
      made.client.noteOn(64);
    });
    await turn();
    act(() => {
      made.time.seconds += (PATTERN_COUNT_IN_BEATS + 4) * BEAT;
      made.server.settle();
    });
    await waitFor(() => {
      expect(line()).toBe("4 beats · 2 notes · 120 BPM");
    });
    expect(recordButton().textContent).toBe("● Record");
    expect(pressed("enabled")).toBe("true");
    expect(clearButton().hasAttribute("disabled")).toBe(false);
  });

  test("says the tempo of the session while it records, and offers it in the hint of the button", async () => {
    const made = await view();
    act(() => {
      made.server.apply({ type: "setBpm", bpm: 90 });
    });
    await waitFor(() => {
      expect(recordButton().getAttribute("title")).toContain("at 90 BPM");
    });
    fireEvent.click(recordButton());
    await waitFor(() => {
      expect(line()).toContain("at 90 BPM");
    });
  });

  test("says what it was asked for when it is refused", async () => {
    const made = await rig(
      <>
        <PatternCell node={oscillator.id} name="Oscillator" />
        <Board />
      </>,
    );
    made.server.drop();
    await waitFor(() => {
      expect(made.client.state.status).not.toBe("open");
    });
    fireEvent.click(recordButton());
    expect(await screen.findByText("Could not start recording a pattern on Oscillator: Not connected to the server.")).toBeTruthy();
  });
});

describe("the length of a pattern", () => {
  test("is typed in beats and asked for with Enter, which gives a lane that has no pattern an empty one", async () => {
    const made = await view();
    fireEvent.change(length(), { target: { value: "8" } });
    fireEvent.keyDown(length(), { key: "Enter" });
    await waitFor(() => {
      expect(line()).toBe("8 beats · no notes · 120 BPM");
    });
    expect(asked(made)).toEqual([{ type: "setPatternField", node: oscillator.id, field: "length", value: 8 }]);
    expect(length().value).toBe("8");
  });

  test("is turned by the arrow keys, a beat at a time and four with Shift, and is held to the lengths there can be", async () => {
    const made = await view(withPattern({ lengthBeats: 2 }));
    fireEvent.keyDown(length(), { key: "ArrowUp" });
    await waitFor(() => {
      expect(length().value).toBe("3");
    });
    fireEvent.keyDown(length(), { key: "ArrowUp", shiftKey: true });
    await waitFor(() => {
      expect(length().value).toBe("7");
    });
    expect(asked(made).map((intent) => (intent.type === "setPatternField" ? intent.value : null))).toEqual([3, 7]);
    fireEvent.change(length(), { target: { value: "100" } });
    fireEvent.keyDown(length(), { key: "Enter" });
    await waitFor(() => {
      expect(length().value).toBe("32");
    });
  });

  test("shows a length that another front-end sets", async () => {
    const made = await view(withPattern({ lengthBeats: 2 }));
    act(() => {
      made.server.apply({ type: "setPatternField", node: oscillator.id, field: "length", value: 12 });
    });
    await waitFor(() => {
      expect(length().value).toBe("12");
    });
    expect(line()).toBe("12 beats · 3 notes · 90 BPM");
  });
});

describe("the switches of a pattern", () => {
  test("turn what a held key does with it: each asks its own setting, the other way round from what it is", async () => {
    const made = await view(withPattern({ enabled: true, looping: true, inKey: false }));
    fireEvent.click(switchOf("enabled"));
    await waitFor(() => {
      expect(pressed("enabled")).toBe("false");
    });
    fireEvent.click(switchOf("looping"));
    await waitFor(() => {
      expect(pressed("looping")).toBe("false");
    });
    fireEvent.click(switchOf("transpose"));
    await waitFor(() => {
      expect(pressed("transpose")).toBe("true");
    });
    expect(asked(made)).toEqual([
      { type: "setPatternField", node: oscillator.id, field: "enabled", value: 0 },
      { type: "setPatternField", node: oscillator.id, field: "looping", value: 0 },
      { type: "setPatternField", node: oscillator.id, field: "transpose", value: 1 },
    ]);
    fireEvent.click(switchOf("enabled"));
    await waitFor(() => {
      expect(pressed("enabled")).toBe("true");
    });
    expect(asked(made).at(-1)).toEqual({ type: "setPatternField", node: oscillator.id, field: "enabled", value: 1 });
  });

  test("follow what another front-end sets", async () => {
    const made = await view(withPattern({ enabled: true, looping: true, inKey: false }));
    act(() => {
      made.server.apply({ type: "setPatternField", node: oscillator.id, field: "looping", value: 0 });
      made.server.apply({ type: "setPatternField", node: oscillator.id, field: "transpose", value: 1 });
    });
    await waitFor(() => {
      expect([pressed("enabled"), pressed("looping"), pressed("transpose")]).toEqual(["true", "false", "true"]);
    });
  });
});

describe("clearing a pattern", () => {
  const menu = () => screen.getByRole("menu", { name: "Clear the pattern of Oscillator" });

  test("asks whether to, in words that say what goes, and asks the server nothing until it is said", async () => {
    const made = await view(withPattern({ lengthBeats: 2, notes: 3 }));
    fireEvent.click(clearButton());
    expect(within(menu()).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keep the pattern", "Clear the pattern of Oscillator for good: 2 beats, 3 notes"]);
    await turn();
    expect(asked(made)).toEqual([]);
    fireEvent.keyDown(menu(), { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    fireEvent.click(clearButton());
    fireEvent.click(screen.getByRole("menuitem", { name: "Keep the pattern" }));
    await turn();
    expect(asked(made)).toEqual([]);
    expect(line()).toBe("2 beats · 3 notes · 90 BPM");
  });

  test("takes the pattern out when it is said to, and the lane has none again", async () => {
    const made = await view(withPattern());
    fireEvent.click(clearButton());
    fireEvent.click(screen.getByRole("menuitem", { name: /^Clear the pattern of Oscillator for good/ }));
    await waitFor(() => {
      expect(line()).toBe("No pattern yet");
    });
    expect(asked(made)).toEqual([{ type: "clearPattern", node: oscillator.id }]);
    expect(clearButton().hasAttribute("disabled")).toBe(true);
    // (It has to say that the length of the next one is the default again.)
    expect(length().value).toBe("4");
  });

  test("says why when it could not be cleared: a pattern that another front-end took out already", async () => {
    const made = await rig(
      <>
        <PatternCell node={oscillator.id} name="Oscillator" />
        <Board />
      </>,
      { snapshot: withPattern() },
    );
    // (Taken out without telling this page, which still shows it.)
    made.server.applyQuietly({ type: "clearPattern", node: oscillator.id });
    fireEvent.click(clearButton());
    fireEvent.click(screen.getByRole("menuitem", { name: /^Clear the pattern of Oscillator for good/ }));
    expect(await screen.findByText("Could not clear the pattern of Oscillator: that lane has no pattern")).toBeTruthy();
  });
});
