// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { RenderResult } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { patternBoxesFixture } from "../../client/fixtures";
import type { Intent, NodeId, Snapshot } from "../../protocol";
import type { Rig } from "../testing";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// The pattern boxes on the canvas (step 124 C): the node that a box is drawn as, its controls, what they ask of the server and what the box says it is doing, and the cables that go to it and from it. The session is the fixture's with boxes
// (`app::patterns`): the keys (2), a device (3, "Launchkey Mini MK4 37"), a box called Bass (4: 8 beats, overdubbing, two notes, a cable of notes from the keys and one to the first lane), a box called Pattern (5: 4 beats, no notes,
// no cable of notes in, so that it records every source: the two links by which the keys and the device reach it are drawn faint, and a cable out to the second lane) and two lanes (0 and 1, both called Oscillator). What the engine makes
// of a box that is told to record is held in `patternBoxEngine.test.ts` and `patternBoxTakes.test.ts`; this is what the page asks and shows.
const found = patternBoxesFixture.cases.find((entry) => entry.name === "boxes");
if (!found) throw new Error("the fixture has the session with boxes");
const snapshot: Snapshot = found.snapshot;
const [KEYS, DEVICE, BASS, PLAIN, FIRST, SECOND] = [2, 3, 4, 5, 0, 1];
/** A beat at the tempo of the fixture (120 a minute), in seconds. */
const BEAT = 0.5;

const nodeOf = (id: NodeId): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);
const cablesOf = (view: RenderResult, kind: "notes" | "hears"): SVGPathElement[] => Array.from(view.container.querySelectorAll<SVGPathElement>(`.tang-cable--${kind}`));
/** The nodes that have a handle of this name, by the id of the node, in order. */
const nodesWith = (view: RenderResult, handle: string): number[] =>
  Array.from(view.container.querySelectorAll(`.react-flow__handle[data-handleid="${handle}"]`), (port) => Number(port.getAttribute("data-nodeid"))).sort((a, b) => a - b);
/** The group of controls of the box that is called `name` (the name that the canvas gives it). */
const boxOf = (name: string): HTMLElement => screen.getByRole("group", { name: `Pattern box ${name}` });
const lineOf = (name: string): string => boxOf(name).querySelector(".tang-pattern__line")?.textContent ?? "(no line)";
const button = (name: string, label: string | RegExp): HTMLElement => within(boxOf(name)).getByRole("button", { name: label });
/** The intents about boxes that the server has been asked for, in order. */
const asked = (made: Rig): Intent[] =>
  made
    .requests()
    .flatMap(({ intent }) => (typeof intent === "object" && intent !== null && "type" in intent ? [intent as Intent] : []))
    .filter((intent) => ["addPattern", "setPatternBoxField", "patternBoxTransport", "clearPatternBox", "setPatternBoxNotes", "addCable", "removeNode"].includes(intent.type));
const turn = (): Promise<void> =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30));
  });

/** The fixture's session with the plain box changed, and the session's meta. */
function withPlain(change: Record<string, unknown>, meta: Record<string, unknown> = {}): Snapshot {
  const changed = structuredClone(snapshot);
  const box = changed.graph.nodes.find((n) => n.id === PLAIN);
  if (box?.kind !== "pattern") throw new Error("the fixture has a box as node 5");
  Object.assign(box, change);
  Object.assign(changed.meta, meta);
  return changed;
}

describe("a pattern box on the canvas", () => {
  test("is a node of a kind of its own for each box, with the name the session gives it and, in small type, that it is a pattern box", async () => {
    await rig(<PatchView />, { snapshot });
    for (const [id, name] of [
      [BASS, "Bass"],
      [PLAIN, "Pattern"],
    ] as const) {
      const node = await nodeOf(id);
      expect(node.querySelector(".tang-node--pattern"), name).not.toBeNull();
      expect(node.querySelector(".tang-node__name")?.textContent, name).toBe(name);
      expect(node.querySelector(".tang-node__badge")?.textContent, name).toBe("Pattern box");
    }
  });

  test("calls two boxes of one name by a number each, as the canvas does for any nodes, which the controls say too", async () => {
    const twins = structuredClone(snapshot);
    const bass = twins.graph.nodes.find((n) => n.id === BASS);
    if (bass) bass.name = "Pattern";
    await rig(<PatchView />, { snapshot: twins });
    expect((await nodeOf(BASS)).querySelector(".tang-node__name")?.textContent).toBe("Pattern 1");
    expect((await nodeOf(PLAIN)).querySelector(".tang-node__name")?.textContent).toBe("Pattern 2");
    expect(boxOf("Pattern 1")).toBeDefined();
    expect(boxOf("Pattern 2")).toBeDefined();
  });

  test("has the two menus that take notes in and send them out on the line of its badge, and the buttons that rename it and take it out after the title", async () => {
    await rig(<PatchView />, { snapshot });
    for (const id of [BASS, PLAIN]) {
      const header = (await nodeOf(id)).querySelector(".tang-node__header");
      const title = header?.querySelector(".tang-node__title");
      expect(Array.from(title?.children ?? [], (child) => child.className), String(id)).toEqual([
        "tang-node__name",
        "tang-node__badge",
        "tang-menu-button nodrag nopan tang-node__notes",
        "tang-menu-button nodrag nopan tang-node__notes",
      ]);
      expect(Array.from(title?.querySelectorAll("button") ?? [], (each) => each.textContent), String(id)).toEqual(["▸♫", "♫▸"]);
      expect(Array.from(header?.querySelectorAll(".tang-node__actions button") ?? [], (each) => each.getAttribute("aria-label")), String(id)).toEqual([
        id === BASS ? "Rename Bass" : "Rename Pattern",
        id === BASS ? "Remove Bass" : "Remove Pattern",
      ]);
    }
  });

  test("has a port that notes come in at and one that they leave at, and the canvas draws the cables that end at them and start at them (a node that has no entry among the kinds is drawn empty, and its cables are not drawn)", async () => {
    const view = (await rig(<PatchView />, { snapshot })).view;
    await nodeOf(PLAIN);
    // (Lanes take notes in, sources send them out, and a box does both.)
    expect(nodesWith(view, "eventsIn")).toEqual([FIRST, SECOND, BASS, PLAIN]);
    expect(nodesWith(view, "eventsOut")).toEqual([KEYS, DEVICE, BASS, PLAIN]);
    // (Three cables of notes: the keys to Bass, Bass to the first lane, Pattern to the second; and the two links by which the sources reach the box that no cable of notes ends at.)
    expect(cablesOf(view, "notes")).toHaveLength(3);
    expect(cablesOf(view, "hears")).toHaveLength(2);
    for (const edge of snapshot.graph.edges.filter((each) => each.kind === "events")) {
      expect(screen.getByTestId(`rf__edge-notes:${String(edge.id)}`), String(edge.id)).toBeDefined();
    }
  });

  test("is the node of every box that the session has, the sixteen of a full one included", async () => {
    const full = patternBoxesFixture.cases.find((entry) => entry.name === "full")?.snapshot;
    if (!full) throw new Error("the fixture has a session with sixteen boxes");
    await rig(<PatchView />, { snapshot: full });
    await waitFor(() => {
      expect(screen.getAllByRole("group", { name: /^Pattern box / })).toHaveLength(16);
    });
  });
});

describe("what a box shows", () => {
  test("is its length, its switches and its notes and what it is doing, as the session has them: a box that has notes and one that has none", async () => {
    await rig(<PatchView />, { snapshot });
    await nodeOf(BASS);
    const length = (name: string): HTMLInputElement => within(boxOf(name)).getByRole<HTMLInputElement>("spinbutton", { name: `Length of ${name}` });
    const pressed = (name: string, label: string): string | null => button(name, label).getAttribute("aria-pressed");
    expect(length("Bass").value).toBe("8");
    expect([pressed("Bass", "Loop Bass"), pressed("Bass", "Overdub into Bass"), pressed("Bass", "Click while Bass records")]).toEqual(["true", "true", "true"]);
    expect(lineOf("Bass")).toBe("8 beats · 2 notes");
    expect(within(boxOf("Bass")).getByRole("img", { name: "Notes of Bass: 2 notes in 8 beats" }).querySelectorAll(".tang-roll__note")).toHaveLength(2);
    expect(length("Pattern").value).toBe("4");
    expect([pressed("Pattern", "Loop Pattern"), pressed("Pattern", "Overdub into Pattern"), pressed("Pattern", "Click while Pattern records")]).toEqual(["true", "false", "true"]);
    expect(lineOf("Pattern")).toBe("4 beats · no notes");
    expect(within(boxOf("Pattern")).getByRole("img", { name: "Notes of Pattern: no notes in 4 beats" }).querySelectorAll(".tang-roll__note")).toHaveLength(0);
  });

  test("says in the hint of each switch what it does, and in the label what box it is of", async () => {
    await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    expect(button("Pattern", "Loop Pattern").getAttribute("title")).toBe("The box goes round: its notes play over and over (off: it plays them once and stops)");
    expect(button("Pattern", "Overdub into Pattern").getAttribute("title")).toBe(
      "A recording is added to the notes the box has, and goes on with another pass until you finish it (off: a recording replaces them)",
    );
    expect(button("Pattern", "Click while Pattern records").getAttribute("title")).toBe("A metronome clicks while the box counts in and records");
    expect(button("Pattern", "Loop Pattern").textContent).toBe("Loop");
    expect(button("Pattern", "Overdub into Pattern").textContent).toBe("Overdub");
    expect(button("Pattern", "Click while Pattern records").textContent).toBe("Click");
    expect(within(boxOf("Pattern")).getByText("beats").closest("label")?.getAttribute("title")).toBe("How long the box's loop is, in beats (the next recording takes this long)");
  });

  test("has the host's switch say what it does when it is there", async () => {
    await rig(<PatchView />, { snapshot: withPlain({}, { hostTempo: true }) });
    await nodeOf(PLAIN);
    expect(button("Pattern", "Follow the host's beat with Pattern").getAttribute("title")).toBe(
      "The box goes by the host's beat and not by its own clock: it plays while the host plays, and records from the start of its loop (off: its own clock)",
    );
    expect(button("Pattern", "Follow the host's beat with Pattern").textContent).toBe("Host");
    expect(button("Pattern", "Play Pattern").getAttribute("title")).toBe("Play the box from its start: 4 beats, round and round if it loops");
  });

  test("has the line that says what it is doing be a live region, so that a person who does not see it is told, and say it all in its title when it is cut", async () => {
    await rig(<PatchView />, { snapshot });
    await nodeOf(BASS);
    const text = boxOf("Bass").querySelector(".tang-pattern__line");
    expect(text?.getAttribute("aria-live")).toBe("polite");
    expect(text?.getAttribute("title")).toBe("8 beats · 2 notes");
    expect(text?.getAttribute("title")).toBe(text?.textContent);
  });

  test("says Record and Play while it is stopped, with a hint for each that says what it does", async () => {
    await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    expect(button("Pattern", "Record into Pattern").textContent).toBe("● Record");
    expect(button("Pattern", "Record into Pattern").getAttribute("title")).toBe("Record into the box: after 4 beats are counted in, the notes you play for 4 beats replace the ones the box has");
    expect(button("Pattern", "Play Pattern").textContent).toBe("▶ Play");
    expect(button("Pattern", "Play Pattern").getAttribute("title")).toBe("Play the box from its start: 4 beats, round and round if it loops");
    expect(button("Bass", "Record into Bass").getAttribute("title")).toContain("added to the ones the box has");
  });

  test("shows what it is doing as the server says it: a box that plays, counts in, makes a pass and waits for the host's loop, each with the buttons that belong to it", async () => {
    // (What a box is doing is the engine's: the mock engine has a player for a box that is told to go, and a snapshot that says a box plays is no player, so each state is reached as a person reaches it.)
    const made = await rig(<PatchView />, { snapshot: withPlain({}, { hostTempo: true }) });
    await nodeOf(PLAIN);
    const state = (): string[] => {
      const buttons = Array.from(boxOf("Pattern").querySelectorAll(".tang-pattern__row:first-child > button"), (each) => each.textContent);
      return [buttons[0] ?? "", buttons[1] ?? "", lineOf("Pattern")];
    };
    expect(state()).toEqual(["● Record", "▶ Play", "4 beats · no notes"]);
    fireEvent.click(button("Pattern", "Play Pattern"));
    await waitFor(() => {
      expect(state()).toEqual(["● Record", "■ Stop", "Playing · 4 beats · no notes"]);
    });
    fireEvent.click(button("Pattern", "Record into Pattern"));
    await waitFor(() => {
      expect(state()).toEqual(["■ Finish", "■ Stop", "Counting in, then 4 beats: play now"]);
    });
    act(() => {
      made.time.seconds += 4 * BEAT;
      made.server.settle();
    });
    await waitFor(() => {
      expect(state()).toEqual(["■ Finish", "■ Stop", "Recording 4 beats: play now"]);
    });
    fireEvent.click(button("Pattern", "Stop Pattern"));
    await waitFor(() => {
      expect(state()).toEqual(["● Record", "▶ Play", "4 beats · no notes"]);
    });
    // (On the host's clock a recording waits for the loop to begin, and the mock has no host to begin it.)
    fireEvent.click(button("Pattern", "Follow the host's beat with Pattern"));
    await waitFor(() => {
      expect(button("Pattern", "Follow the host's beat with Pattern").getAttribute("aria-pressed")).toBe("true");
    });
    fireEvent.click(button("Pattern", "Record into Pattern"));
    await waitFor(() => {
      expect(state()).toEqual(["■ Finish", "■ Stop", "Waiting for the host's loop to begin, then recording"]);
    });
  });

  test("lights the record button and pulses it while it counts in and records, so that a person who has looked away can see", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    expect(button("Pattern", "Record into Pattern").classList.contains("tang-pattern__record--going")).toBe(false);
    fireEvent.click(button("Pattern", "Record into Pattern"));
    await waitFor(() => {
      expect(button("Pattern", "Finish recording Pattern").classList.contains("tang-pattern__record--going")).toBe(true);
    });
    act(() => {
      made.time.seconds += 4 * BEAT;
      made.server.settle();
    });
    await waitFor(() => {
      expect(lineOf("Pattern")).toBe("Recording 4 beats: play now");
    });
    expect(button("Pattern", "Finish recording Pattern").classList.contains("tang-pattern__record--going")).toBe(true);
    // (The other box is not recording and is not lit.)
    expect(button("Bass", "Record into Bass").classList.contains("tang-pattern__record--going")).toBe(false);
  });
});

describe("the buttons of a box", () => {
  test("record, finish, play and stop ask the server for what they say, and the box then says what it is doing", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    fireEvent.click(button("Pattern", "Record into Pattern"));
    await waitFor(() => {
      expect(button("Pattern", "Finish recording Pattern")).toBeDefined();
    });
    expect(lineOf("Pattern")).toBe("Counting in, then 4 beats: play now");
    expect(button("Pattern", "Stop Pattern").getAttribute("title")).toBe("Stop: the pass that is being recorded is lost, and the box keeps the notes it had");
    expect(button("Pattern", "Finish recording Pattern").getAttribute("title")).toContain("the pass that is going on is the last");
    // (The count-in is four beats: then the box takes a pass.)
    act(() => {
      made.time.seconds += 4 * BEAT;
      made.server.settle();
    });
    await waitFor(() => {
      expect(lineOf("Pattern")).toBe("Recording 4 beats: play now");
    });
    fireEvent.click(button("Pattern", "Finish recording Pattern"));
    fireEvent.click(button("Pattern", "Stop Pattern"));
    await waitFor(() => {
      expect(button("Pattern", "Record into Pattern")).toBeDefined();
    });
    expect(button("Pattern", "Play Pattern")).toBeDefined();
    expect(lineOf("Pattern")).toBe("4 beats · no notes");
    fireEvent.click(button("Pattern", "Play Pattern"));
    await waitFor(() => {
      expect(button("Pattern", "Stop Pattern")).toBeDefined();
    });
    expect(lineOf("Pattern")).toBe("Playing · 4 beats · no notes");
    expect(asked(made)).toEqual([
      { type: "patternBoxTransport", node: PLAIN, action: "record" },
      { type: "patternBoxTransport", node: PLAIN, action: "finishRecording" },
      { type: "patternBoxTransport", node: PLAIN, action: "stop" },
      { type: "patternBoxTransport", node: PLAIN, action: "play" },
    ]);
  });

  test("take the keys that are played in the pass, at the beat that each is at, and the notes that come of it are the box's, with a step in the history, shown in the roll and the line", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    fireEvent.click(button("Pattern", "Record into Pattern"));
    await waitFor(() => {
      expect(lineOf("Pattern")).toBe("Counting in, then 4 beats: play now");
    });
    act(() => {
      made.time.seconds += 4 * BEAT;
      made.server.settle();
    });
    await waitFor(() => {
      expect(lineOf("Pattern")).toBe("Recording 4 beats: play now");
    });
    // (A note a beat into the pass that is held for a beat, and one on the fourth beat that is held to the end of the pass. The client's message is delivered a moment after it is sent: the mock's clock is moved when it has been.)
    act(() => {
      made.time.seconds += BEAT;
      made.client.noteOn(60, 100);
    });
    await turn();
    act(() => {
      made.time.seconds += BEAT;
      made.client.noteOff(60);
    });
    await turn();
    act(() => {
      made.time.seconds += BEAT;
      made.client.noteOn(67, 80);
    });
    await turn();
    act(() => {
      made.time.seconds += BEAT;
      made.server.settle();
    });
    await waitFor(() => {
      expect(lineOf("Pattern")).toBe("Playing · 4 beats · 2 notes");
    });
    expect(within(boxOf("Pattern")).getByRole("img", { name: "Notes of Pattern: 2 notes in 4 beats" }).querySelectorAll(".tang-roll__note")).toHaveLength(2);
    expect(made.client.state.history.undo).toBe("record into Pattern");
    expect(made.client.state.snapshot?.meta.dirty).toBe(true);
    const box = made.client.state.snapshot?.graph.nodes.find((n) => n.id === PLAIN);
    expect(box?.kind === "pattern" ? box.notes : null).toEqual([
      { start: 1, length: 1, note: 60, velocity: 100 },
      { start: 3, length: 1, note: 67, velocity: 80 },
    ]);
  });

  test("clear the notes after asking: a menu keeps them or clears them for good, and the focus goes to the record button when they are cleared", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(BASS);
    fireEvent.click(button("Bass", "Clear the notes of Bass"));
    expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keep the notes", "Clear the notes of Bass: 2 notes"]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Keep the notes" }));
    await turn();
    expect(asked(made)).toEqual([]);
    expect(lineOf("Bass")).toBe("8 beats · 2 notes");
    fireEvent.click(button("Bass", "Clear the notes of Bass"));
    fireEvent.click(screen.getByRole("menuitem", { name: /^Clear the notes of Bass/ }));
    await waitFor(() => {
      expect(lineOf("Bass")).toBe("8 beats · no notes");
    });
    expect(asked(made)).toEqual([{ type: "clearPatternBox", node: BASS }]);
    expect(within(boxOf("Bass")).getByRole("img", { name: "Notes of Bass: no notes in 8 beats" })).toBeDefined();
    // (Undo is the history's: the notes are given back.)
    expect(made.client.state.history.undo).toBe("clear Bass");
    await waitFor(() => {
      expect(document.activeElement?.id).toBe(`pattern-box-record-${String(BASS)}`);
    });
  });

  test("have no notes to clear in a box that has none: the button is shut", async () => {
    await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    expect(button("Pattern", "Clear the notes of Pattern").hasAttribute("disabled")).toBe(true);
    expect(button("Bass", "Clear the notes of Bass").hasAttribute("disabled")).toBe(false);
  });

  test("ask for the length that is typed, held to the lengths there can be, and for a beat more with the arrow key", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    const length = (): HTMLInputElement => within(boxOf("Pattern")).getByRole<HTMLInputElement>("spinbutton", { name: "Length of Pattern" });
    fireEvent.change(length(), { target: { value: "16" } });
    fireEvent.keyDown(length(), { key: "Enter" });
    await waitFor(() => {
      expect(lineOf("Pattern")).toBe("16 beats · no notes");
    });
    expect(length().value).toBe("16");
    fireEvent.change(length(), { target: { value: "0.1" } });
    fireEvent.keyDown(length(), { key: "Enter" });
    await waitFor(() => {
      expect(length().value).toBe("0.25");
    });
    expect(asked(made)).toEqual([
      { type: "setPatternBoxField", node: PLAIN, field: { type: "lengthBeats", beats: 16 } },
      { type: "setPatternBoxField", node: PLAIN, field: { type: "lengthBeats", beats: 0.1 } },
    ]);
    fireEvent.keyDown(length(), { key: "ArrowUp" });
    await waitFor(() => {
      expect(length().value).toBe("1.25");
    });
    // (Four beats with Shift.)
    fireEvent.keyDown(length(), { key: "ArrowUp", shiftKey: true });
    await waitFor(() => {
      expect(length().value).toBe("5.25");
    });
    expect([length().getAttribute("aria-valuemin"), length().getAttribute("aria-valuemax")]).toEqual(["0.25", "256"]);
  });

  test("turn Loop, Overdub and Click over, each asking for the opposite of what it is, and say which they are", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    const pressed = (label: string): string | null => button("Pattern", label).getAttribute("aria-pressed");
    fireEvent.click(button("Pattern", "Loop Pattern"));
    fireEvent.click(button("Pattern", "Overdub into Pattern"));
    fireEvent.click(button("Pattern", "Click while Pattern records"));
    await waitFor(() => {
      expect([pressed("Loop Pattern"), pressed("Overdub into Pattern"), pressed("Click while Pattern records")]).toEqual(["false", "true", "false"]);
    });
    expect(asked(made)).toEqual([
      { type: "setPatternBoxField", node: PLAIN, field: { type: "looping", on: false } },
      { type: "setPatternBoxField", node: PLAIN, field: { type: "overdub", on: true } },
      { type: "setPatternBoxField", node: PLAIN, field: { type: "click", on: false } },
    ]);
    // (The hint of the record button follows the switch that was turned: a recording that overdubs adds to the notes.)
    expect(button("Pattern", "Record into Pattern").getAttribute("title")).toContain("added to the ones the box has");
  });

  test("have the host's beat only where the session has a host's tempo or the box goes by it already, so that a box can be put back on its own clock", async () => {
    await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    expect(within(boxOf("Pattern")).queryByRole("button", { name: /host/i })).toBeNull();
    cleanup();
    const hosted = await rig(<PatchView />, { snapshot: withPlain({}, { hostTempo: true }) });
    await nodeOf(PLAIN);
    expect(button("Pattern", "Follow the host's beat with Pattern").getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(button("Pattern", "Follow the host's beat with Pattern"));
    await waitFor(() => {
      expect(button("Pattern", "Follow the host's beat with Pattern").getAttribute("aria-pressed")).toBe("true");
    });
    expect(asked(hosted)).toEqual([{ type: "setPatternBoxField", node: PLAIN, field: { type: "sync", sync: "host" } }]);
    // (The record button says how it begins then.)
    expect(button("Pattern", "Record into Pattern").getAttribute("title")).toContain("when the host's loop next begins");
    cleanup();
    // A session file that says `sync = "host"`, in a program that has no host: the control is there to put it back.
    const stuck = await rig(<PatchView />, { snapshot: withPlain({ sync: "host" }) });
    await nodeOf(PLAIN);
    expect(button("Pattern", "Follow the host's beat with Pattern").getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(button("Pattern", "Follow the host's beat with Pattern"));
    // (And once it is back on its own clock there is no host to give it to again: the control goes.)
    await waitFor(() => {
      expect(within(boxOf("Pattern")).queryByRole("button", { name: /host/i })).toBeNull();
    });
    expect(asked(stuck)).toEqual([{ type: "setPatternBoxField", node: PLAIN, field: { type: "sync", sync: "own" } }]);
    expect(button("Pattern", "Record into Pattern").getAttribute("title")).toContain("after 4 beats are counted in");
  });

  test("are not taken for the keys of the piano: a button that has the focus leaves the keys of the computer to the page, which plays them", async () => {
    const made = await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    button("Pattern", "Record into Pattern").focus();
    expect(document.activeElement).toBe(button("Pattern", "Record into Pattern"));
    // (The page's own listener is the app's, `useKeyboardPiano`; what is checked here is that the box does not stop a key from getting to it.)
    const event = new KeyboardEvent("keydown", { key: "z", bubbles: true, cancelable: true });
    let reachedWindow = false;
    const listen = (): void => {
      reachedWindow = true;
    };
    window.addEventListener("keydown", listen);
    button("Pattern", "Record into Pattern").dispatchEvent(event);
    window.removeEventListener("keydown", listen);
    expect(reachedWindow).toBe(true);
    expect(made.requests().filter(({ intent }) => (intent as { type?: string }).type === "patternBoxTransport")).toEqual([]);
  });
});

describe("the name of a box", () => {
  test("is changed in place: the pencil turns it into a field, Enter asks the server for what was typed, and the focus goes back to the pencil", async () => {
    const made = await rig(<PatchView />, { snapshot });
    const node = await nodeOf(PLAIN);
    fireEvent.click(within(node).getByRole("button", { name: "Rename Pattern" }));
    const field = within(node).getByRole<HTMLInputElement>("textbox", { name: "Name of Pattern" });
    expect(field.value).toBe("Pattern");
    fireEvent.change(field, { target: { value: "  Lead  " } });
    fireEvent.keyDown(field, { key: "Enter" });
    await waitFor(() => {
      expect(within(node).getByRole("button", { name: "Rename Lead" })).toBeDefined();
    });
    expect(node.querySelector(".tang-node__name")?.textContent).toBe("Lead");
    expect(boxOf("Lead")).toBeDefined();
    expect(asked(made)).toEqual([{ type: "setPatternBoxField", node: PLAIN, field: { type: "name", name: "  Lead  " } }]);
    await waitFor(() => {
      expect(document.activeElement?.id).toBe(`rename-pattern-${String(PLAIN)}`);
    });
  });

  test("is left as it was by Escape, and by a name that is the one it has: nothing is asked", async () => {
    const made = await rig(<PatchView />, { snapshot });
    const node = await nodeOf(PLAIN);
    fireEvent.click(within(node).getByRole("button", { name: "Rename Pattern" }));
    fireEvent.change(within(node).getByRole("textbox", { name: "Name of Pattern" }), { target: { value: "Lead" } });
    fireEvent.keyDown(within(node).getByRole("textbox", { name: "Name of Pattern" }), { key: "Escape" });
    expect(within(node).queryByRole("textbox")).toBeNull();
    fireEvent.click(within(node).getByRole("button", { name: "Rename Pattern" }));
    fireEvent.keyDown(within(node).getByRole("textbox", { name: "Name of Pattern" }), { key: "Enter" });
    await turn();
    expect(asked(made)).toEqual([]);
    expect(node.querySelector(".tang-node__name")?.textContent).toBe("Pattern");
  });
});

describe("the menus of notes on the canvas", () => {
  test("show a note on a lane and a source, as they always did, and an arrow towards the note on a box that takes notes in and one away from it on a box that sends them", async () => {
    await rig(<PatchView />, { snapshot });
    await nodeOf(PLAIN);
    expect((await nodeOf(KEYS)).querySelector(".tang-node__notes")?.textContent).toBe("♫");
    expect((await nodeOf(FIRST)).querySelector(".tang-node__notes")?.textContent).toBe("♫");
    expect(within(await nodeOf(PLAIN)).getByRole("button", { name: "Take notes into Pattern" }).textContent).toBe("▸♫");
    expect(within(await nodeOf(PLAIN)).getByRole("button", { name: "Send notes from Pattern" }).textContent).toBe("♫▸");
  });
});

describe("the cables of a box", () => {
  test("are drawn from its menu: ▸♫ takes notes in from a source that has no cable to it, ♫▸ sends them to a lane that has none from it", async () => {
    const made = await rig(<PatchView />, { snapshot });
    const node = await nodeOf(PLAIN);
    fireEvent.click(within(node).getByRole("button", { name: "Take notes into Pattern" }));
    expect(screen.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Keys", "Launchkey Mini MK4 37"]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Launchkey Mini MK4 37" }));
    await waitFor(() => {
      expect(asked(made)).toEqual([{ type: "addCable", from: { node: DEVICE, port: { type: "eventsOut" } }, to: { node: PLAIN, port: { type: "eventsIn" } }, gain: null }]);
    });
    fireEvent.click(within(node).getByRole("button", { name: "Send notes from Pattern" }));
    // (The two lanes are called alike and the canvas numbers them: the second has a cable from this box already.)
    expect(screen.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Oscillator 1"]);
    fireEvent.click(screen.getByRole("menuitem", { name: "Oscillator 1" }));
    await waitFor(() => {
      expect(asked(made)).toHaveLength(2);
    });
    expect(asked(made)[1]).toEqual({ type: "addCable", from: { node: PLAIN, port: { type: "eventsOut" } }, to: { node: FIRST, port: { type: "eventsIn" } }, gain: null });
  });

  test("are what take the box out with it: the cables of notes at it go, and the lanes it sent to hear every source again", async () => {
    const made = await rig(<PatchView />, { snapshot });
    const view = made.view;
    await nodeOf(BASS);
    expect(cablesOf(view, "notes")).toHaveLength(3);
    fireEvent.click(within(await nodeOf(BASS)).getByRole("button", { name: "Remove Bass" }));
    await waitFor(() => {
      expect(screen.queryByTestId(`rf__node-${String(BASS)}`)).toBeNull();
    });
    expect(asked(made)).toEqual([{ type: "removeNode", node: BASS }]);
    // (The cable from the keys to Bass and the one from Bass to the first lane went with it; the first lane has none now and hears both sources.)
    await waitFor(() => {
      expect(cablesOf(view, "notes")).toHaveLength(1);
    });
    expect(cablesOf(view, "hears")).toHaveLength(2 + 2);
  });
});
