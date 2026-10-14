// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Profiler, useState } from "react";
import { afterEach, describe, expect, test, vi } from "vitest";

import { Client } from "../../client/connection";
import { MockServer } from "../../client/mock";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { ClientProvider, useNotices } from "../ClientProvider";
import { holdable, rig } from "../testing";
import { useKeyboardPiano } from "../useKeyboardPiano";
import { Piano } from "./Piano";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  document.elementFromPoint = () => null;
});

/** Make the keys this wide: the observer of sizes tells every element it watches that width. */
function widthIs(width: number): void {
  class Observer {
    constructor(private readonly callback: (entries: unknown[], observer: unknown) => void) {}
    observe(target: Element): void {
      this.callback([{ target, contentRect: { width, height: 100 } }], this);
    }
    unobserve = (): void => undefined;
    disconnect = (): void => undefined;
  }
  vi.stubGlobal("ResizeObserver", Observer);
}

/** The piano as the page has it: its keys follow the octave of the keys of the computer, which are played here too. */
function Page() {
  const octave = useKeyboardPiano();
  return <Piano octave={octave} />;
}

const keyOf = (container: HTMLElement, note: number): HTMLElement => {
  const found = container.querySelector<HTMLElement>(`[data-note="${String(note)}"]`);
  if (!found) throw new Error(`no key for note ${String(note)}`);
  return found;
};
/** The notes that have a key, lowest first (the white keys come before the black ones in the page). */
const notesOf = (container: HTMLElement): number[] => Array.from(container.querySelectorAll<HTMLElement>("[data-note]"), (key) => Number(key.dataset.note)).sort((a, b) => a - b);
const roleOf = (container: HTMLElement, note: number): string | undefined => keyOf(container, note).dataset.role;
const rolesOf = (container: HTMLElement, notes: number[]): (string | undefined)[] => notes.map((note) => roleOf(container, note));

/** The notes that went to the engine, as `on 60` and `off 60`. */
const played = (server: MockServer): string[] =>
  server.received.flatMap((message) => (message.type === "noteOn" ? [`on ${String(message.note)}`] : message.type === "noteOff" ? [`off ${String(message.note)}`] : []));

/** A key under the pointer, wherever the pointer is. */
function pointerIsOver(container: HTMLElement, note: number | null): void {
  document.elementFromPoint = () => (note === null ? null : keyOf(container, note));
}

describe("the keys of the piano", () => {
  test("are as many octaves as the width has room for, from the octave the keys of the computer start at, and are named as the TUI names notes", async () => {
    widthIs(1260);
    const { view } = await rig(<Page />);
    const notes = notesOf(view.container);
    // Six octaves: the window is centred on what the keys of the computer play.
    expect([notes.length, notes[0], notes.at(-1)]).toEqual([72, 24, 95]);
    expect(keyOf(view.container, 60).getAttribute("aria-label")).toBe("C4");
    expect(keyOf(view.container, 61).getAttribute("aria-label")).toBe("C#4");
    expect(keyOf(view.container, 24).getAttribute("aria-label")).toBe("C1");
    expect(view.container.querySelectorAll(".tang-key--white")).toHaveLength(42);
    expect(view.container.querySelectorAll(".tang-key--black")).toHaveLength(30);
  });

  test("are six octaves at the most, however wide the place is", async () => {
    widthIs(3000);
    const { view } = await rig(<Page />);
    expect(notesOf(view.container)).toHaveLength(72);
  });

  test("are fewer octaves in a narrower place, and the window still has the notes the keys of the computer play", async () => {
    widthIs(430);
    const { view } = await rig(<Page />);
    // (The keys of the computer start at the C of note 48 and go on for 29 semitones.)
    expect(notesOf(view.container)).toEqual(Array.from({ length: 24 }, (_, i) => 48 + i));
  });

  test("move with the octave of the keys of the computer", async () => {
    widthIs(630);
    const { view } = await rig(<Page />);
    expect(notesOf(view.container)[0]).toBe(48);
    fireEvent.keyDown(document.body, { code: "BracketRight" });
    expect(notesOf(view.container)[0]).toBe(60);
    fireEvent.keyDown(document.body, { code: "BracketLeft" });
    fireEvent.keyDown(document.body, { code: "BracketLeft" });
    expect(notesOf(view.container)[0]).toBe(36);
  });

  test("are white where a piano's are and black where its black ones are, and a black key is between two white ones", async () => {
    widthIs(630);
    const { view } = await rig(<Page />);
    const white = [48, 50, 52, 53, 55, 57, 59];
    for (const note of [48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59]) {
      expect(keyOf(view.container, note).classList.contains(white.includes(note) ? "tang-key--white" : "tang-key--black"), String(note)).toBe(true);
    }
    // (Three octaves: twenty-one white keys, which the black ones are placed by.)
    expect(screen.getByRole("group", { name: "Piano keys" }).style.getPropertyValue("--whites")).toBe("21");
    // The black keys are placed by how many white keys come before them (the first octave: C# after one, D# after two, F# after four…).
    expect([49, 51, 54, 56, 58].map((note) => keyOf(view.container, note).style.getPropertyValue("--after"))).toEqual(["1", "2", "4", "5", "6"]);
    expect(keyOf(view.container, 61).style.getPropertyValue("--after")).toBe("8");
  });

  test("say the letter of each white key, and the octave on the C", async () => {
    widthIs(630);
    const { view } = await rig(<Page />);
    expect([48, 50, 52, 53, 59, 60].map((note) => keyOf(view.container, note).textContent)).toEqual(["C3", "D", "E", "F", "B", "C4"]);
    expect(keyOf(view.container, 49).textContent).toBe("");
  });

  test("are not there before the session is: there is no scale to draw them in", () => {
    widthIs(630);
    const never = { connect: () => undefined, send: () => undefined, close: () => undefined };
    const client = new Client({ transport: () => never });
    const view = render(
      <ClientProvider client={client}>
        <Piano octave={4} />
      </ClientProvider>,
    );
    expect(view.container.querySelector("[data-note]")).toBeNull();
    expect(screen.queryByRole("group", { name: "Piano keys" })).toBeNull();
    expect(screen.queryByRole("combobox", { name: "Scale root" })).toBeNull();
    // (The place for them is there: see the page's tests.)
    expect(screen.getByRole("region", { name: "Piano" }).children).toHaveLength(0);
  });
});

describe("what a key is in the scale", () => {
  test("is told by its colour, which the page reads off the scale the server tells: the root, the other notes of the scale, and the rest", async () => {
    widthIs(630);
    const { view } = await rig(<Page />);
    // C major to begin with.
    expect(rolesOf(view.container, [60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72])).toEqual([
      "root", "off", "scale", "off", "scale", "scale", "off", "scale", "off", "scale", "off", "scale", "root",
    ]);
  });

  test("follows the scale as it is set from elsewhere, and the root is that of the new scale in every octave", async () => {
    widthIs(630);
    const { view, server } = await rig(<Page />);
    act(() => {
      server.apply({ type: "setScale", scale: "F# dorian" });
    });
    await waitFor(() => {
      expect(roleOf(view.container, 66)).toBe("root");
    });
    // F# G# A B C# D# E, and C# (61) is one of them while C (60) is not.
    expect(rolesOf(view.container, [54, 66, 78, 60, 61, 68, 69, 70])).toEqual(["root", "root", "root", "off", "scale", "scale", "scale", "off"]);
  });
});

describe("the lock", () => {
  test("is a button that says whether the piano is locked to its scale, and the keys that are not in the scale say they cannot be played when it is", async () => {
    widthIs(630);
    const { view, server } = await rig(<Page />);
    const button = await screen.findByRole("button", { name: "Lock to scale" });
    const group = screen.getByRole("group", { name: "Piano keys" });
    expect(button.getAttribute("aria-pressed")).toBe("false");
    expect(group.hasAttribute("data-locked")).toBe(false);
    act(() => {
      server.apply({ type: "setLocked", locked: true });
    });
    await waitFor(() => {
      expect(button.getAttribute("aria-pressed")).toBe("true");
    });
    expect(group.hasAttribute("data-locked")).toBe(true);
    // (The scale is the same: the keys only say, with this, which of them are not played.)
    expect(roleOf(view.container, 61)).toBe("off");
  });

  test("asks for the lock when it is pressed, and for the other way when it is pressed again, whatever the answer was yet to be", async () => {
    widthIs(630);
    const { requests } = await rig(<Page />);
    const button = await screen.findByRole("button", { name: "Lock to scale" });
    fireEvent.click(button);
    fireEvent.click(button);
    await waitFor(() => {
      expect(requests()).toHaveLength(2);
    });
    expect(requests().map((request) => request.intent)).toEqual([
      { type: "setLocked", locked: true },
      { type: "setLocked", locked: false },
    ]);
    await waitFor(() => {
      expect(button.getAttribute("aria-pressed")).toBe("false");
    });
  });

  test("shows what was asked for at once, and what the session has when the answer is in", async () => {
    widthIs(630);
    await rig(<Page />);
    const button = await screen.findByRole("button", { name: "Lock to scale" });
    fireEvent.click(button);
    expect(button.getAttribute("aria-pressed")).toBe("true");
    await waitFor(() => {
      expect(button.getAttribute("aria-pressed")).toBe("true");
    });
  });
});

const root = () => screen.findByRole<HTMLSelectElement>("combobox", { name: "Scale root" });
const kind = () => screen.findByRole<HTMLSelectElement>("combobox", { name: "Scale kind" });
/** What the two choices of the scale picker show: the text of the chosen options. */
const shown = async (): Promise<[string, string]> => {
  const text = (select: HTMLSelectElement): string => select.selectedOptions[0]?.textContent ?? "?";
  return [text(await root()), text(await kind())];
};

/** The page over a connection whose answers can be held back; the client is open, and what comes after `hold()` waits. */
async function overHeldAnswers() {
  const server = new MockServer();
  const connection = holdable(server);
  const client = new Client({ transport: connection.transport });
  render(
    <ClientProvider client={client}>
      <Page />
    </ClientProvider>,
  );
  await waitFor(() => {
    expect(client.state.status).toBe("open");
  });
  return { server, client, connection };
}

describe("the scale picker", () => {
  test("is a root and a kind of scale, each a choice, which say what the session's scale is", async () => {
    widthIs(630);
    await rig(<Page />);
    expect(await shown()).toEqual(["C", "Major"]);
    expect(Array.from((await root()).options, (option) => option.textContent)).toEqual(["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]);
    const kinds = Array.from((await kind()).options, (option) => option.textContent);
    expect(kinds.slice(0, 3)).toEqual(["Major", "Natural Minor", "Harmonic Minor"]);
    expect(kinds).toHaveLength(15);
  });

  test("asks for the scale the choice makes, with the root and the kind the other one has", async () => {
    widthIs(630);
    const { requests } = await rig(<Page />);
    fireEvent.change(await root(), { target: { value: "2" } });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toEqual({ type: "setScale", scale: "D major" });
    await waitFor(async () => {
      expect(await shown()).toEqual(["D", "Major"]);
    });
    fireEvent.change(await kind(), { target: { value: "dorian" } });
    await waitFor(() => {
      expect(requests()).toHaveLength(2);
    });
    expect(requests()[1]?.intent).toEqual({ type: "setScale", scale: "D dorian" });
  });

  test("shows what was chosen at once, and a second choice before the answer to the first keeps the first", async () => {
    widthIs(630);
    const { requests } = await rig(<Page />);
    fireEvent.change(await root(), { target: { value: "6" } });
    expect(await shown()).toEqual(["F#", "Major"]);
    fireEvent.change(await kind(), { target: { value: "dorian" } });
    expect(await shown()).toEqual(["F#", "Dorian"]);
    expect(requests().map((request) => request.intent)).toEqual([
      { type: "setScale", scale: "F# major" },
      { type: "setScale", scale: "F# dorian" },
    ]);
    // After the answers: what the session has, which is what was asked for.
    await waitFor(async () => {
      expect(await shown()).toEqual(["F#", "Dorian"]);
    });
  });

  test("follows the scale as it is set from elsewhere", async () => {
    widthIs(630);
    const { server } = await rig(<Page />);
    expect(await shown()).toEqual(["C", "Major"]);
    act(() => {
      server.apply({ type: "setScale", scale: "A min pent" });
    });
    await waitFor(async () => {
      expect(await shown()).toEqual(["A", "Minor Pentatonic"]);
    });
  });

  test("says what could not be done, and shows what the session has, when the connection is gone", async () => {
    widthIs(630);
    function Notices() {
      return <NoticeBoard notices={useNotices()} />;
    }
    const { client } = await rig(
      <>
        <Page />
        <Notices />
      </>,
    );
    client.stop();
    fireEvent.change(await root(), { target: { value: "9" } });
    await waitFor(() => {
      expect(screen.getByText(/Could not set the scale of the piano to A major: Not connected to the server/)).toBeTruthy();
    });
    await waitFor(async () => {
      expect(await shown()).toEqual(["C", "Major"]);
    });
  });
});

describe("the scale and the lock, chosen again before the first choice is answered", () => {
  test("go on showing the last choice of the scale while the earlier ones are answered, and then what the session has", async () => {
    widthIs(630);
    const { client, connection } = await overHeldAnswers();
    connection.hold();
    fireEvent.change(await root(), { target: { value: "6" } });
    fireEvent.change(await kind(), { target: { value: "dorian" } });
    await waitFor(() => {
      expect(connection.waiting()).toBe(4); // an event and an answer for each
    });
    // The first is answered: the session has F# major, but the last choice is what is shown.
    act(() => {
      connection.release(2);
    });
    await waitFor(() => {
      expect(client.state.snapshot?.meta.piano.scale).toBe("F# major");
    });
    expect(await shown()).toEqual(["F#", "Dorian"]);
    act(() => {
      connection.release(2);
    });
    await waitFor(() => {
      expect(client.state.snapshot?.meta.piano.scale).toBe("F# dorian");
    });
    expect(await shown()).toEqual(["F#", "Dorian"]);
    client.stop();
  });

  test("go on showing the last choice of the lock while the earlier one is answered, and then what the session has", async () => {
    widthIs(630);
    const { client, connection } = await overHeldAnswers();
    const button = await screen.findByRole("button", { name: "Lock to scale" });
    connection.hold();
    fireEvent.click(button);
    fireEvent.click(button);
    await waitFor(() => {
      expect(connection.waiting()).toBe(4);
    });
    act(() => {
      connection.release(2);
    });
    await waitFor(() => {
      expect(client.state.snapshot?.meta.piano.locked).toBe(true);
    });
    // (Locked for the session, and let go again by the choice that has not been answered: the button says the choice.)
    expect(button.getAttribute("aria-pressed")).toBe("false");
    act(() => {
      connection.release(2);
    });
    await waitFor(() => {
      expect(client.state.snapshot?.meta.piano.locked).toBe(false);
    });
    expect(button.getAttribute("aria-pressed")).toBe("false");
    client.stop();
  });
});

describe("the scale picker, when the answers are slow", () => {
  test("keeps the kind that was chosen when the root is chosen next, before the answer to the first", async () => {
    widthIs(630);
    const { server, client, connection } = await overHeldAnswers();
    connection.hold();
    fireEvent.change(await kind(), { target: { value: "dorian" } });
    fireEvent.change(await root(), { target: { value: "6" } });
    expect(await shown()).toEqual(["F#", "Dorian"]);
    // (The session still has C major: what is asked for the second time is made of the first choice, not of the session.)
    expect(client.state.snapshot?.meta.piano.scale).toBe("C major");
    expect(server.received.flatMap((message) => (message.type === "request" ? [message.intent] : []))).toEqual([
      { type: "setScale", scale: "C dorian" },
      { type: "setScale", scale: "F# dorian" },
    ]);
    client.stop();
  });
});

describe("a pointer on the keys", () => {
  test("plays the key it goes down on, as hard as the keys of the computer do, until it goes up", async () => {
    widthIs(630);
    const { view, server } = await rig(<Page />);
    fireEvent.pointerDown(keyOf(view.container, 60), { pointerId: 1, button: 0 });
    await waitFor(() => {
      expect(played(server)).toEqual(["on 60"]);
    });
    expect(server.received.find((message) => message.type === "noteOn")).toEqual({ type: "noteOn", note: 60, velocity: 100 });
    fireEvent.pointerUp(keyOf(view.container, 60), { pointerId: 1 });
    await waitFor(() => {
      expect(played(server)).toEqual(["on 60", "off 60"]);
    });
  });

  test("plays the keys it moves over while it is down, one note at a time, and ends the note where it leaves the keys", async () => {
    widthIs(630);
    const { view, server } = await rig(<Page />);
    const group = screen.getByRole("group", { name: "Piano keys" });
    fireEvent.pointerDown(keyOf(view.container, 60), { pointerId: 1, button: 0 });
    pointerIsOver(view.container, 62);
    fireEvent.pointerMove(group, { pointerId: 1, clientX: 10, clientY: 10 });
    pointerIsOver(view.container, null);
    fireEvent.pointerMove(group, { pointerId: 1, clientX: 900, clientY: 10 });
    pointerIsOver(view.container, 64);
    fireEvent.pointerMove(group, { pointerId: 1, clientX: 20, clientY: 10 });
    fireEvent.pointerUp(group, { pointerId: 1 });
    await waitFor(() => {
      expect(played(server)).toEqual(["on 60", "off 60", "on 62", "off 62", "on 64", "off 64"]);
    });
  });

  test("plays nothing while it only moves over the keys, and nothing for a button that is not the first", async () => {
    widthIs(630);
    const { view, server } = await rig(<Page />);
    const group = screen.getByRole("group", { name: "Piano keys" });
    pointerIsOver(view.container, 62);
    fireEvent.pointerMove(group, { pointerId: 1, clientX: 10, clientY: 10 });
    fireEvent.pointerDown(keyOf(view.container, 60), { pointerId: 2, button: 2 });
    fireEvent.pointerUp(group, { pointerId: 2 });
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(played(server)).toEqual([]);
  });

  test("ends the note where it goes up, wherever that is, even if the page does not see it go up on a key", async () => {
    widthIs(630);
    const { view, server } = await rig(<Page />);
    fireEvent.pointerDown(keyOf(view.container, 60), { pointerId: 1, button: 0 });
    fireEvent.pointerUp(window, { pointerId: 1 });
    await waitFor(() => {
      expect(played(server)).toEqual(["on 60", "off 60"]);
    });
    fireEvent.pointerDown(keyOf(view.container, 62), { pointerId: 3, button: 0 });
    fireEvent.pointerCancel(window, { pointerId: 3 });
    await waitFor(() => {
      expect(played(server)).toEqual(["on 60", "off 60", "on 62", "off 62"]);
    });
  });

  test("asks to keep the pointer when it goes down, so that it plays on over the edge of the keys", async () => {
    widthIs(630);
    const { view } = await rig(<Page />);
    const group = screen.getByRole("group", { name: "Piano keys" });
    const capture = vi.fn();
    group.setPointerCapture = capture;
    fireEvent.pointerDown(keyOf(view.container, 60), { pointerId: 5, button: 0 });
    expect(capture).toHaveBeenCalledWith(5);
    fireEvent.pointerUp(group, { pointerId: 5 });
    // (And plays where that cannot be done: a pointer that is gone before it is captured is no harm.)
    group.setPointerCapture = () => {
      throw new DOMException("no such pointer", "NotFoundError");
    };
    fireEvent.pointerDown(keyOf(view.container, 62), { pointerId: 6, button: 0 });
    fireEvent.pointerUp(group, { pointerId: 6 });
  });

  test("keeps the focus where it is and the text as it is: a press on a key is not a click that selects", async () => {
    widthIs(630);
    const { view } = await rig(<Page />);
    const down = new MouseEvent("pointerdown", { bubbles: true, cancelable: true, button: 0 });
    Object.defineProperty(down, "pointerId", { value: 1 });
    keyOf(view.container, 60).dispatchEvent(down);
    expect(down.defaultPrevented).toBe(true);
    const menu = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
    keyOf(view.container, 60).dispatchEvent(menu);
    expect(menu.defaultPrevented).toBe(true);
  });

  test("lets go of what it plays when the window loses the focus and when the page is hidden", async () => {
    widthIs(630);
    const { view, server } = await rig(<Page />);
    fireEvent.pointerDown(keyOf(view.container, 60), { pointerId: 1, button: 0 });
    fireEvent.pointerDown(keyOf(view.container, 64), { pointerId: 2, button: 0 });
    fireEvent.blur(window);
    await waitFor(() => {
      expect(played(server).sort()).toEqual(["off 60", "off 64", "on 60", "on 64"]);
    });
    fireEvent.pointerDown(keyOf(view.container, 67), { pointerId: 3, button: 0 });
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
    fireEvent(document, new Event("visibilitychange"));
    Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
    await waitFor(() => {
      expect(played(server)).toContain("off 67");
    });
  });

  test("lets go of what it plays when the piano is taken off the page, and the page stays", async () => {
    widthIs(630);
    function Hideable() {
      const [shown, setShown] = useState(true);
      return (
        <>
          <button
            type="button"
            onClick={() => {
              setShown(false);
            }}
          >
            hide
          </button>
          {shown ? <Page /> : null}
        </>
      );
    }
    const { view, server } = await rig(<Hideable />);
    fireEvent.pointerDown(keyOf(view.container, 69), { pointerId: 4, button: 0 });
    fireEvent.click(screen.getByRole("button", { name: "hide" }));
    expect(view.container.querySelector("[data-note]")).toBeNull();
    await waitFor(() => {
      expect(played(server)).toEqual(["on 69", "off 69"]);
    });
  });

  test("shares a note with the keys of the computer: it ends when the second player lets go", async () => {
    widthIs(630);
    const { view, server } = await rig(<Page />);
    // Z is the C of note 48.
    fireEvent.keyDown(document.body, { code: "KeyZ" });
    fireEvent.pointerDown(keyOf(view.container, 48), { pointerId: 1, button: 0 });
    fireEvent.pointerUp(window, { pointerId: 1 });
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(played(server)).toEqual(["on 48"]);
    fireEvent.keyUp(document.body, { code: "KeyZ" });
    await waitFor(() => {
      expect(played(server)).toEqual(["on 48", "off 48"]);
    });
  });
});

describe("the keys that are held", () => {
  const connectOther = async (server: MockServer): Promise<Client> => {
    const other = new Client({ transport: server.transport, reconnectDelay: () => 60_000 });
    other.start();
    await waitFor(() => {
      expect(other.state.status).toBe("open");
    });
    return other;
  };

  test("are lit on the frame for the notes the engine says are held, whoever holds them, and are let go when the notes end", async () => {
    widthIs(630);
    const { view, server, client, clock } = await rig(<Page />);
    const other = await connectOther(server);
    other.noteOn(60);
    other.noteOn(64);
    await waitFor(() => {
      expect(client.telemetry.isHeld(60) && client.telemetry.isHeld(64)).toBe(true);
    });
    act(() => {
      clock.tick(100);
    });
    expect([60, 61, 64].map((note) => keyOf(view.container, note).hasAttribute("data-held"))).toEqual([true, false, true]);
    expect([60, 61, 64].map((note) => keyOf(view.container, note).getAttribute("aria-pressed"))).toEqual(["true", "false", "true"]);
    other.noteOff(60);
    await waitFor(() => {
      expect(client.telemetry.isHeld(60)).toBe(false);
    });
    act(() => {
      clock.tick(200);
    });
    expect([60, 64].map((note) => keyOf(view.container, note).hasAttribute("data-held"))).toEqual([false, true]);
    other.stop();
  });

  test("are lit again where the keys are drawn again, on another octave", async () => {
    widthIs(630);
    const { view, server, client, clock } = await rig(<Page />);
    const other = await connectOther(server);
    other.noteOn(72);
    await waitFor(() => {
      expect(client.telemetry.isHeld(72)).toBe(true);
    });
    act(() => {
      clock.tick(100);
    });
    expect(keyOf(view.container, 72).hasAttribute("data-held")).toBe(true);
    fireEvent.keyDown(document.body, { code: "BracketRight" });
    expect(notesOf(view.container)[0]).toBe(60);
    // (The key is kept through the move, lit, and no frame has come since.)
    expect(keyOf(view.container, 72).hasAttribute("data-held")).toBe(true);
    // The note that ends after the move is let go all the same: what is lit is read off the keys that were kept, and not taken for dark.
    other.noteOff(72);
    await waitFor(() => {
      expect(client.telemetry.isHeld(72)).toBe(false);
    });
    act(() => {
      clock.tick(300);
    });
    expect(keyOf(view.container, 72).hasAttribute("data-held")).toBe(false);
    other.stop();
  });

  test("do not make the piano render again, however many frames come", async () => {
    widthIs(630);
    let renders = 0;
    const { server, client, clock } = await rig(
      <Profiler
        id="piano"
        onRender={() => {
          renders += 1;
        }}
      >
        <Page />
      </Profiler>,
    );
    const other = await connectOther(server);
    await waitFor(() => {
      expect(client.telemetry.has("held")).toBe(true);
    });
    const settled = renders;
    for (let note = 60; note < 70; note += 1) {
      other.noteOn(note);
      await new Promise((resolve) => setTimeout(resolve, 8));
      act(() => {
        clock.tick(performance.now());
      });
    }
    expect(renders).toBe(settled);
    other.stop();
  });

  test("are not lit for what the page cannot know: after the connection is lost no note is held", async () => {
    widthIs(630);
    const { view, server, client, clock } = await rig(<Page />);
    const other = await connectOther(server);
    other.noteOn(60);
    await waitFor(() => {
      expect(client.telemetry.isHeld(60)).toBe(true);
    });
    act(() => {
      clock.tick(100);
    });
    expect(keyOf(view.container, 60).hasAttribute("data-held")).toBe(true);
    server.drop();
    await waitFor(() => {
      expect(client.state.status).toBe("reconnecting");
    });
    act(() => {
      clock.tick(200);
    });
    expect(keyOf(view.container, 60).hasAttribute("data-held")).toBe(false);
    other.stop();
  });
});
