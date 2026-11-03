// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import type { Client } from "../client/connection";
import { ClientProvider } from "./ClientProvider";
import { useKeyboardPiano } from "./useKeyboardPiano";

afterEach(cleanup);

/** A client that writes down the notes it is asked to play, there and then (a real one sends them to the server a moment later, and only while it is connected). */
function fake() {
  const sent: string[] = [];
  const client = {
    start: () => undefined,
    stop: () => undefined,
    noteOn: (note: number, velocity = 100) => sent.push(`on ${String(note)} ${String(velocity)}`),
    noteOff: (note: number) => sent.push(`off ${String(note)}`),
  } as unknown as Client;
  return { client, sent };
}

function Octave() {
  return <span>octave {useKeyboardPiano()}</span>;
}

function mount() {
  const { client, sent } = fake();
  const view = render(
    <ClientProvider client={client}>
      <Octave />
    </ClientProvider>,
  );
  return { sent, view };
}

describe("useKeyboardPiano", () => {
  test("plays the keys of the window through the client, and gives the octave", () => {
    const { sent } = mount();
    expect(screen.getByText("octave 4")).toBeTruthy();
    fireEvent.keyDown(document.body, { code: "KeyZ" });
    fireEvent.keyUp(document.body, { code: "KeyZ" });
    expect(sent).toEqual(["on 48 100", "off 48"]);
    fireEvent.keyDown(document.body, { code: "BracketLeft" });
    expect(screen.getByText("octave 3")).toBeTruthy();
    fireEvent.keyDown(document.body, { code: "KeyZ" });
    expect(sent.at(-1)).toBe("on 36 100");
  });

  test("lets go of what is held when the window loses the focus", () => {
    const { sent } = mount();
    fireEvent.keyDown(document.body, { code: "KeyZ" });
    fireEvent.keyDown(document.body, { code: "KeyC" });
    fireEvent.blur(window);
    expect(sent).toEqual(["on 48 100", "on 52 100", "off 48", "off 52"]);
  });

  test("lets go of what is held when it is taken off, and no longer listens to the keys", () => {
    const { sent, view } = mount();
    fireEvent.keyDown(document.body, { code: "KeyZ" });
    view.unmount();
    expect(sent).toEqual(["on 48 100", "off 48"]);
    // Nothing is listening: not for a key going down, not for one going up, not for the window losing the focus.
    fireEvent.keyDown(document.body, { code: "KeyC" });
    fireEvent.keyUp(document.body, { code: "KeyC" });
    fireEvent.keyDown(document.body, { code: "KeyB" });
    fireEvent.blur(window);
    expect(sent).toEqual(["on 48 100", "off 48"]);
  });

  test("takes away every listener it put on the window and the document, so that none is left to hold the piano", () => {
    const added: [string, unknown][] = [];
    const removed: [string, unknown][] = [];
    for (const target of [window, document]) {
      const add = target.addEventListener.bind(target);
      const remove = target.removeEventListener.bind(target);
      vi.spyOn(target, "addEventListener").mockImplementation((type: string, listener: unknown, options?: unknown) => {
        added.push([type, listener]);
        add(type, listener as EventListener, options as AddEventListenerOptions);
      });
      vi.spyOn(target, "removeEventListener").mockImplementation((type: string, listener: unknown, options?: unknown) => {
        removed.push([type, listener]);
        remove(type, listener as EventListener, options as EventListenerOptions);
      });
    }
    const { view } = mount();
    const mine = added.filter(([type]) => ["keydown", "keyup", "blur", "visibilitychange"].includes(type));
    expect(mine.map(([type]) => type).sort()).toEqual(["blur", "keydown", "keyup", "visibilitychange"]);
    view.unmount();
    for (const listener of mine) expect(removed, `${listener[0]} is taken away`).toContainEqual(listener);
    vi.restoreAllMocks();
  });

  test("lets go of what is held when the page is hidden, and not when it is shown", () => {
    const { sent } = mount();
    fireEvent.keyDown(document.body, { code: "KeyZ" });
    fireEvent(document, new Event("visibilitychange"));
    expect(sent).toEqual(["on 48 100"]);
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
    fireEvent(document, new Event("visibilitychange"));
    Object.defineProperty(document, "hidden", { configurable: true, get: () => false });
    expect(sent).toEqual(["on 48 100", "off 48"]);
  });
});
