// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { Client } from "../client/connection";
import { nodeNamed, tracerSnapshot } from "../client/fixtures";
import { MockServer } from "../client/mock";
import { NoticeBoard } from "../ui/NoticeBoard";
import { ClientProvider, useNotices } from "./ClientProvider";
import { SessionBar } from "./SessionBar";
import { rig } from "./testing";

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");

function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

/** The bar and the board its refusals are on, over a session with this much of a meta. */
async function bar(meta: Partial<typeof tracerSnapshot.meta> = {}) {
  const made = await rig(
    <>
      <SessionBar />
      <Board />
    </>,
    { snapshot: { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, ...meta } } },
  );
  return made;
}

/** A change another front-end (or this page) makes: the session has something that is not saved. */
async function change(rig: Awaited<ReturnType<typeof bar>>): Promise<void> {
  await rig.client.request({ type: "setGain", node: oscillator.id, gain: "volume", value: 0.5 });
}

const saveButton = (): HTMLButtonElement => screen.getByRole<HTMLButtonElement>("button", { name: "Save" });

describe("the bar of the session", () => {
  test("names the file the session is saved to, and says it has nothing unsaved, with a button that has nothing to do", async () => {
    await bar();
    expect(screen.getByText("tracer.toml")).toBeTruthy();
    expect(screen.getByText("Saved")).toBeTruthy();
    expect(saveButton().disabled).toBe(true);
    // (A change in what it says is told to a screen reader, politely.)
    expect(screen.getByText("Saved").getAttribute("aria-live")).toBe("polite");
  });

  test("says when there are changes that are not saved, and the button can save them", async () => {
    const made = await bar();
    await change(made);
    await waitFor(() => {
      expect(screen.getByText("Unsaved changes")).toBeTruthy();
    });
    expect(screen.queryByText("Saved")).toBeNull();
    expect(saveButton().disabled).toBe(false);
  });

  test("saves when the button is pressed, and says so, and the button has nothing to do again", async () => {
    const made = await bar();
    await change(made);
    await waitFor(() => {
      expect(saveButton().disabled).toBe(false);
    });
    fireEvent.click(saveButton());
    await waitFor(() => {
      expect(made.requests().at(-1)).toEqual({ intent: { type: "save" } });
    });
    await waitFor(() => {
      expect(screen.getByText("Saved")).toBeTruthy();
    });
    expect(saveButton().disabled).toBe(true);
    expect(made.client.state.snapshot?.meta.dirty).toBe(false);
  });

  test("keeps the button from being pressed again while it is saving", async () => {
    const made = await bar();
    await change(made);
    await waitFor(() => {
      expect(saveButton().disabled).toBe(false);
    });
    fireEvent.click(saveButton());
    expect(saveButton().disabled).toBe(true);
    fireEvent.click(saveButton());
    await waitFor(() => {
      expect(made.client.state.snapshot?.meta.dirty).toBe(false);
    });
    expect(made.requests().filter((request) => JSON.stringify(request.intent) === '{"type":"save"}')).toHaveLength(1);
  });

  test("says why when it could not save, and the changes are still unsaved, and the button can be pressed again", async () => {
    const made = await bar({ file: null });
    await change(made);
    await waitFor(() => {
      expect(saveButton().disabled).toBe(false);
    });
    fireEvent.click(saveButton());
    expect(await screen.findByText("Could not save the session: No session path — cannot save")).toBeTruthy();
    expect(screen.getByText("Unsaved changes")).toBeTruthy();
    expect(saveButton().disabled).toBe(false);
  });

  test("has the buttons of Undo and Redo before the one that saves and the one that saves under another name, which work on the session", async () => {
    const made = await bar();
    const buttons = screen.getAllByRole("button").map((button) => button.textContent);
    expect(buttons).toEqual(["Undo", "Redo", "Save", "Save as…"]);
    await change(made);
    await waitFor(() => {
      expect(screen.getByRole<HTMLButtonElement>("button", { name: "Undo set volume" }).disabled).toBe(false);
    });
    fireEvent.click(screen.getByRole("button", { name: "Undo set volume" }));
    await waitFor(() => {
      expect(screen.getByRole<HTMLButtonElement>("button", { name: "Redo set volume" }).disabled).toBe(false);
    });
  });

  test("says when the session has no file", async () => {
    await bar({ file: null });
    expect(screen.getByText("no file")).toBeTruthy();
  });

  test("shows nothing until the session has come", () => {
    const client = new Client({ transport: new MockServer().transport });
    const view = render(
      <ClientProvider client={client}>
        <SessionBar />
      </ClientProvider>,
    );
    expect(view.container.firstChild).toBeNull();
  });
});

describe("Control+S and Command+S", () => {
  const press = (init: KeyboardEventInit): boolean => fireEvent.keyDown(window, { key: "s", ...init });
  const saves = (made: Awaited<ReturnType<typeof bar>>): number => made.requests().filter((request) => JSON.stringify(request.intent) === '{"type":"save"}').length;

  test("save the session, and keep the browser from offering to save the page", async () => {
    for (const modifier of [{ ctrlKey: true }, { metaKey: true }]) {
      const made = await bar();
      await change(made);
      await waitFor(() => {
        expect(saveButton().disabled).toBe(false);
      });
      expect(press(modifier)).toBe(false); // (prevented)
      await waitFor(() => {
        expect(made.client.state.snapshot?.meta.dirty).toBe(false);
      });
      expect(saves(made)).toBe(1);
      cleanup();
    }
  });

  test("pressed again while the session is being saved ask for one save only", async () => {
    const made = await bar();
    await change(made);
    await waitFor(() => {
      expect(saveButton().disabled).toBe(false);
    });
    press({ ctrlKey: true });
    press({ ctrlKey: true });
    await waitFor(() => {
      expect(made.client.state.snapshot?.meta.dirty).toBe(false);
    });
    expect(saves(made)).toBe(1);
  });

  test("keep the browser from offering to save the page when there is nothing to save, and ask for nothing", async () => {
    const made = await bar();
    expect(press({ ctrlKey: true })).toBe(false);
    // (Nothing is asked for, which is what would show a moment after, if it were.)
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    expect(saves(made)).toBe(0);
  });

  test("are for the key S and no other: Control+D is the browser's", async () => {
    const made = await bar();
    await change(made);
    await waitFor(() => {
      expect(saveButton().disabled).toBe(false);
    });
    expect(fireEvent.keyDown(window, { key: "d", ctrlKey: true })).toBe(true);
    expect(fireEvent.keyDown(window, { key: "Control", ctrlKey: true })).toBe(true);
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    expect(saves(made)).toBe(0);
  });

  test("are not for a key that is only an S, or with Alt, and with Shift they are not a save (they are Save as, which `SaveAs.test.tsx` has)", async () => {
    const made = await bar();
    await change(made);
    await waitFor(() => {
      expect(saveButton().disabled).toBe(false);
    });
    expect(press({})).toBe(true);
    expect(press({ ctrlKey: true, altKey: true })).toBe(true);
    expect(press({ ctrlKey: true, shiftKey: true, altKey: true })).toBe(true);
    expect(press({ ctrlKey: true, shiftKey: true })).toBe(false);
    expect(press({ metaKey: true, shiftKey: true })).toBe(false);
    expect(saves(made)).toBe(0);
    expect(made.client.state.snapshot?.meta.dirty).toBe(true);
  });

  test("are heard from a capital S too (Caps Lock), and stop being listened to when the bar is gone", async () => {
    const made = await bar();
    await change(made);
    await waitFor(() => {
      expect(saveButton().disabled).toBe(false);
    });
    expect(fireEvent.keyDown(window, { key: "S", ctrlKey: true })).toBe(false);
    await waitFor(() => {
      expect(made.client.state.snapshot?.meta.dirty).toBe(false);
    });
    made.view.unmount();
    expect(press({ ctrlKey: true })).toBe(true); // nobody is listening
  });
});
