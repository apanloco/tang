// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, tracerSnapshot } from "../client/fixtures";
import { NoticeBoard } from "../ui/NoticeBoard";
import { useNotices } from "./ClientProvider";
import { UndoRedo } from "./UndoRedo";
import { rig } from "./testing";

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");

function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

type Made = Awaited<ReturnType<typeof buttons>>;

/** The two buttons and the board their refusals are on, over the tracer. */
async function buttons() {
  return rig(
    <>
      <UndoRedo />
      <Board />
    </>,
  );
}

/** A change a person makes: the volume of the oscillator, which is the step "set volume" (and another a moment later is the same step). */
async function turn(made: Made, value: number): Promise<void> {
  await made.client.request({ type: "setGain", node: oscillator.id, gain: "volume", value });
}

const volume = (made: Made): number | undefined => {
  const node = made.client.state.snapshot?.graph.nodes.find((n) => n.id === oscillator.id);
  return node?.kind === "instrument" ? node.volume : undefined;
};

const undoButton = (): HTMLButtonElement => screen.getByRole<HTMLButtonElement>("button", { name: /^Undo/ });
const redoButton = (): HTMLButtonElement => screen.getByRole<HTMLButtonElement>("button", { name: /^Redo/ });
const asked = (made: Made, type: "undo" | "redo"): number => made.requests().filter((request) => JSON.stringify(request.intent) === JSON.stringify({ type })).length;
/** Let what a pressed key might have asked for come, if it were asked. */
const moment = async (): Promise<void> => {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30));
  });
};

describe("the buttons of Undo and Redo", () => {
  test("have nothing to do to begin with, and say so", async () => {
    await buttons();
    expect([undoButton().disabled, redoButton().disabled]).toEqual([true, true]);
    expect([undoButton().title, redoButton().title]).toEqual(["Nothing to undo", "Nothing to redo"]);
  });

  test("say what a change would be put back, in their names and their tips, with the keys that do it", async () => {
    const made = await buttons();
    await turn(made, 0.5);
    await waitFor(() => {
      expect(undoButton().disabled).toBe(false);
    });
    expect(undoButton().getAttribute("aria-label")).toBe("Undo set volume");
    expect(undoButton().title).toBe("Undo set volume (Control+Z, or Command+Z)");
    expect([redoButton().disabled, redoButton().title]).toEqual([true, "Nothing to redo"]);
  });

  test("undo when pressed: the session has what it had, and the button of Redo says what it would do again", async () => {
    const made = await buttons();
    const before = volume(made);
    await turn(made, 0.5);
    await waitFor(() => {
      expect(undoButton().disabled).toBe(false);
    });
    fireEvent.click(undoButton());
    await waitFor(() => {
      expect(volume(made)).toBe(before);
    });
    await waitFor(() => {
      expect(redoButton().disabled).toBe(false);
    });
    expect(made.requests().at(-1)).toEqual({ intent: { type: "undo" } });
    expect(redoButton().getAttribute("aria-label")).toBe("Redo set volume");
    expect(redoButton().title).toBe("Redo set volume (Control+Shift+Z, Command+Shift+Z, or Control+Y)");
    expect(undoButton().disabled).toBe(true);
  });

  test("redo when pressed does it again, and the button of Undo is as it was", async () => {
    const made = await buttons();
    await turn(made, 0.5);
    await waitFor(() => {
      expect(undoButton().disabled).toBe(false);
    });
    fireEvent.click(undoButton());
    await waitFor(() => {
      expect(redoButton().disabled).toBe(false);
    });
    fireEvent.click(redoButton());
    // (Not again while it is awaited.)
    expect(redoButton().disabled).toBe(true);
    fireEvent.click(redoButton());
    await waitFor(() => {
      expect(volume(made)).toBe(0.5);
    });
    await waitFor(() => {
      expect(redoButton().disabled).toBe(true);
    });
    expect(made.requests().at(-1)).toEqual({ intent: { type: "redo" } });
    expect(asked(made, "redo")).toBe(1);
    expect(undoButton().getAttribute("aria-label")).toBe("Undo set volume");
  });

  test("keep a button from being pressed again while its answer is awaited, so that one press is one step", async () => {
    const made = await buttons();
    await turn(made, 0.25);
    made.time.seconds += 5; // (a change a second after the last is a step of its own)
    await turn(made, 0.5);
    await waitFor(() => {
      expect(undoButton().disabled).toBe(false);
    });
    fireEvent.click(undoButton());
    expect(undoButton().disabled).toBe(true);
    fireEvent.click(undoButton());
    await waitFor(() => {
      expect(volume(made)).toBe(0.25);
    });
    await moment();
    expect(asked(made, "undo")).toBe(1);
    // (The first step is there to be undone still.)
    expect(undoButton().disabled).toBe(false);
  });

  test("say why when they could not, and the page goes on: the history they showed was another front-end's to change", async () => {
    const made = await buttons();
    await turn(made, 0.5);
    await waitFor(() => {
      expect(undoButton().disabled).toBe(false);
    });
    // Something that cannot be undone (another instrument in a lane), which empties the history without telling this page.
    made.server.applyQuietly({ type: "replaceInstrument", node: oscillator.id, plugin: "builtin:osc" });
    fireEvent.click(undoButton());
    expect(await screen.findByText("Could not undo: there is nothing to undo")).toBeTruthy();
    expect(undoButton().disabled).toBe(false);
  });
});

describe("the keys of Undo and Redo", () => {
  const press = (init: KeyboardEventInit, target: Element | Window = window): boolean => fireEvent.keyDown(target, { key: "z", ...init });

  /** A session with something that was done and, if `undone`, something that was undone. */
  async function done(undone = false): Promise<Made> {
    const made = await buttons();
    await turn(made, 0.5);
    await waitFor(() => {
      expect(undoButton().disabled).toBe(false);
    });
    if (undone) {
      fireEvent.click(undoButton());
      await waitFor(() => {
        expect(redoButton().disabled).toBe(false);
      });
    }
    return made;
  }

  test("Control+Z and Command+Z undo, and keep the browser from doing its own", async () => {
    for (const modifier of [{ ctrlKey: true }, { metaKey: true }]) {
      const made = await done();
      const before = volume(made);
      expect(press(modifier)).toBe(false); // (prevented)
      await waitFor(() => {
        expect(redoButton().disabled).toBe(false);
      });
      expect(asked(made, "undo")).toBe(1);
      expect(volume(made)).not.toBe(0.5);
      expect(before).toBe(0.5);
      cleanup();
    }
  });

  test("Control+Shift+Z, Command+Shift+Z and Control+Y redo, and keep the browser from doing its own", async () => {
    for (const [init, key] of [[{ ctrlKey: true, shiftKey: true }, "z"], [{ metaKey: true, shiftKey: true }, "z"], [{ ctrlKey: true }, "y"]] as const) {
      const made = await done(true);
      expect(fireEvent.keyDown(window, { key, ...init })).toBe(false);
      await waitFor(() => {
        expect(volume(made)).toBe(0.5);
      });
      expect(asked(made, "redo")).toBe(1);
      cleanup();
    }
  });

  test("are those chords and no other: a plain Z, Alt, Command+Y (the browser's History), Control+Shift+Y, Control+Command+Y, Control+Alt+Z and the other keys are left alone", async () => {
    const made = await done(true);
    // (The setup asked for an undo itself, by its button.)
    const before = [asked(made, "undo"), asked(made, "redo")];
    const left = [
      press({}),
      press({ shiftKey: true }),
      press({ ctrlKey: true, altKey: true }),
      press({ metaKey: true, altKey: true, shiftKey: true }),
      fireEvent.keyDown(window, { key: "y", metaKey: true }),
      fireEvent.keyDown(window, { key: "y", ctrlKey: true, shiftKey: true }),
      fireEvent.keyDown(window, { key: "y", ctrlKey: true, metaKey: true }),
      fireEvent.keyDown(window, { key: "x", ctrlKey: true }),
      fireEvent.keyDown(window, { key: "Control", ctrlKey: true }),
    ];
    expect(left).toEqual(left.map(() => true));
    await moment();
    expect([asked(made, "undo"), asked(made, "redo")]).toEqual(before);
  });

  test("are heard from a capital Z too (Caps Lock)", async () => {
    const made = await done();
    expect(fireEvent.keyDown(window, { key: "Z", ctrlKey: true })).toBe(false);
    await waitFor(() => {
      expect(asked(made, "undo")).toBe(1);
    });
  });

  test("are left to a field that takes text, which has its own undo, and heard from the rest of the page", async () => {
    const made = await done();
    const field = document.createElement("input");
    field.type = "text";
    const slider = document.createElement("input");
    slider.type = "range";
    document.body.append(field, slider);
    // The field: the browser's own, which is not prevented, and nothing is asked.
    expect(press({ ctrlKey: true }, field)).toBe(true);
    await moment();
    expect(asked(made, "undo")).toBe(0);
    // A slider is no place for text: the page undoes.
    expect(press({ ctrlKey: true }, slider)).toBe(false);
    await waitFor(() => {
      expect(asked(made, "undo")).toBe(1);
    });
    field.remove();
    slider.remove();
  });

  test("do nothing when there is nothing to undo or redo, and leave the key to the browser", async () => {
    const made = await buttons();
    expect(press({ ctrlKey: true })).toBe(true);
    expect(press({ ctrlKey: true, shiftKey: true })).toBe(true);
    expect(fireEvent.keyDown(window, { key: "y", ctrlKey: true })).toBe(true);
    await moment();
    expect([asked(made, "undo"), asked(made, "redo")]).toEqual([0, 0]);
  });

  test("pressed again while the first is awaited ask for one step only", async () => {
    const made = await buttons();
    await turn(made, 0.25);
    made.time.seconds += 5;
    await turn(made, 0.5);
    await waitFor(() => {
      expect(undoButton().disabled).toBe(false);
    });
    press({ ctrlKey: true });
    press({ ctrlKey: true });
    await waitFor(() => {
      expect(volume(made)).toBe(0.25);
    });
    await moment();
    expect(asked(made, "undo")).toBe(1);
  });

  test("are listened to no more when the buttons are gone", async () => {
    const made = await done();
    made.view.unmount();
    expect(press({ ctrlKey: true })).toBe(true); // nobody is listening
    await moment();
    expect(asked(made, "undo")).toBe(0);
  });
});
