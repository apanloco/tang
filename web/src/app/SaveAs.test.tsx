// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { Client } from "../client/connection";
import { tracerSnapshot } from "../client/fixtures";
import { MockServer } from "../client/mock";
import type { MockOptions } from "../client/mock";
import { NoticeBoard } from "../ui/NoticeBoard";
import { ClientProvider, useNotices } from "./ClientProvider";
import { SessionBar } from "./SessionBar";
import { holdable, rig } from "./testing";

afterEach(cleanup);

// "Save as…" in the bar of the session: a field for the name of a file, which asks the server to save the session as it (the server says what a name may be and
// where the file goes: `saveas.test.ts` and `filenames.test.ts` have the rules). What is looked at here is the page: what it shows, what it asks and what it does with
// the answer.

function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

/** The bar and the board its refusals are on, over a session in a directory that has these other files. */
const bar = (options: MockOptions = {}) =>
  rig(
    <>
      <SessionBar />
      <Board />
    </>,
    options,
  );

const opener = (): HTMLButtonElement => screen.getByRole<HTMLButtonElement>("button", { name: "Save as…" });
const field = (): HTMLInputElement => screen.getByRole<HTMLInputElement>("textbox", { name: "File name" });
const okButton = (): HTMLButtonElement => screen.getByRole<HTMLButtonElement>("button", { name: "OK" });
const closed = (): boolean => screen.queryByRole("textbox", { name: "File name" }) === null;

/** The requests for a save as that the server has had, by the name asked for. */
const asked = (made: { requests: () => { intent: unknown }[] }): unknown[] =>
  made.requests().flatMap(({ intent }) => (typeof intent === "object" && intent !== null && "type" in intent && intent.type === "saveAs" && "name" in intent ? [intent.name] : []));

const type = (text: string): void => {
  fireEvent.change(field(), { target: { value: text } });
};
/** Enter in the field: false if the key was taken (the page kept the browser from doing what it does with it). */
const enter = (): boolean => fireEvent.keyDown(field(), { key: "Enter" });
/** The page's turn: what a request that was made now would have come to the server by (an absence is looked for after it, not before). */
const turn = (): Promise<void> =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30));
  });

describe("the Save as of the bar of the session", () => {
  test("is a button that is there when nothing is unsaved and is not the one that saves, and has no field until it is pressed", async () => {
    await bar();
    expect(opener().disabled).toBe(false);
    expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(true);
    expect(closed()).toBe(true);
    expect(screen.queryByRole("button", { name: "OK" })).toBeNull();
  });

  test("opens a field in its place with the name of the file that the session has, which has the focus and its text selected", async () => {
    await bar();
    fireEvent.click(opener());
    expect(field().value).toBe("tracer.toml");
    expect(document.activeElement).toBe(field());
    expect([field().selectionStart, field().selectionEnd]).toEqual([0, "tracer.toml".length]);
    expect(screen.queryByRole("button", { name: "Save as…" })).toBeNull();
    expect(okButton().disabled).toBe(false);
    expect(screen.getByRole("button", { name: "Cancel" })).toBeTruthy();
  });

  test("opens an empty field for a session that has no file", async () => {
    await bar({ snapshot: { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, file: null } } });
    fireEvent.click(opener());
    expect(field().value).toBe("");
    expect(okButton().disabled).toBe(true);
  });

  test("asks for the name as it was typed when Enter is pressed, and then shows the file the session has: the field is gone and the button has the focus", async () => {
    const made = await bar();
    fireEvent.click(opener());
    type("  copy ");
    expect(enter()).toBe(false); // (the key is taken)
    await waitFor(() => {
      expect(screen.getByText("copy.toml")).toBeTruthy();
    });
    expect(asked(made)).toEqual(["  copy "]);
    expect(closed()).toBe(true);
    expect(screen.getByText("Saved")).toBeTruthy();
    expect(screen.queryByText("tracer.toml")).toBeNull();
    expect(document.activeElement).toBe(opener());
  });

  test("asks the same when OK is pressed, and OK has nothing to do while the field is blank, as Enter has", async () => {
    const made = await bar();
    fireEvent.click(opener());
    type("   ");
    expect(okButton().disabled).toBe(true);
    enter();
    await turn();
    expect(asked(made)).toEqual([]);
    expect(closed()).toBe(false);
    type("mix");
    fireEvent.click(okButton());
    await waitFor(() => {
      expect(screen.getByText("mix.toml")).toBeTruthy();
    });
    expect(asked(made)).toEqual(["mix"]);
    expect(closed()).toBe(true);
  });

  test("saves what is unsaved with the file it makes, so that the session has nothing unsaved after it", async () => {
    const made = await bar();
    await made.client.request({ type: "setLocked", locked: true });
    await waitFor(() => {
      expect(screen.getByText("Unsaved changes")).toBeTruthy();
    });
    fireEvent.click(opener());
    type("copy");
    enter();
    await waitFor(() => {
      expect(screen.getByText("Saved")).toBeTruthy();
    });
    expect(screen.getByText("copy.toml")).toBeTruthy();
  });

  test("takes an Enter that confirms what an input method is composing for the input method's: nothing is asked, and the next Enter asks", async () => {
    const made = await bar();
    fireEvent.click(opener());
    type("コピー");
    fireEvent.keyDown(field(), { key: "Enter", isComposing: true });
    await turn();
    expect(asked(made)).toEqual([]);
    expect(closed()).toBe(false);
    enter();
    await waitFor(() => {
      expect(screen.getByText("コピー.toml")).toBeTruthy();
    });
    expect(asked(made)).toEqual(["コピー"]);
  });

  test("is refused as the server refuses it: the page says why, the field stays with what was typed, selected and with the focus, and another name can be tried", async () => {
    const made = await bar({ files: ["other.toml"] });
    fireEvent.click(opener());
    type("other");
    enter();
    expect(await screen.findByText('Could not save the session as "other": a file called other.toml is there already')).toBeTruthy();
    expect(field().value).toBe("other");
    expect(document.activeElement).toBe(field());
    expect([field().selectionStart, field().selectionEnd]).toEqual([0, "other".length]);
    expect(screen.getByText("tracer.toml")).toBeTruthy();
    expect(okButton().disabled).toBe(false);
    type("mine");
    enter();
    await waitFor(() => {
      expect(screen.getByText("mine.toml")).toBeTruthy();
    });
    expect(asked(made)).toEqual(["other", "mine"]);
    expect(closed()).toBe(true);
  });

  test("is refused when OK asked for it as when Enter did: the focus, which was on OK, comes back to the field with the name selected", async () => {
    await bar({ files: ["other.toml"] });
    fireEvent.click(opener());
    type("other");
    okButton().focus();
    expect(document.activeElement).toBe(okButton());
    fireEvent.click(okButton());
    expect(await screen.findByText('Could not save the session as "other": a file called other.toml is there already')).toBeTruthy();
    await waitFor(() => {
      expect(document.activeElement).toBe(field());
    });
    expect([field().selectionStart, field().selectionEnd]).toEqual([0, "other".length]);
  });

  test("is refused for a name that is a path, in the server's words, and the field stays", async () => {
    await bar();
    fireEvent.click(opener());
    type("../x");
    enter();
    expect(await screen.findByText('Could not save the session as "../x": a file name has no folder in it: `/` and `\\` are not allowed')).toBeTruthy();
    expect(field().value).toBe("../x");
  });

  test("is left with Escape and with Cancel, which ask for nothing, and the button has the focus again", async () => {
    const made = await bar();
    fireEvent.click(opener());
    type("copy");
    expect(fireEvent.keyDown(field(), { key: "Escape" })).toBe(false); // (the key is taken)
    expect(closed()).toBe(true);
    expect(document.activeElement).toBe(opener());
    fireEvent.click(opener());
    expect(field().value).toBe("tracer.toml"); // (what was typed is not kept for the next time)
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(closed()).toBe(true);
    expect(document.activeElement).toBe(opener());
    await turn();
    expect(asked(made)).toEqual([]);
    expect(screen.getByText("tracer.toml")).toBeTruthy();
  });

  test("asks one at a time: Enter again, or OK, while the first is not answered asks nothing more, and the field is closed by the answer", async () => {
    const server = new MockServer();
    const connection = holdable(server);
    const client = new Client({ transport: connection.transport });
    render(
      <ClientProvider client={client}>
        <SessionBar />
      </ClientProvider>,
    );
    await waitFor(() => {
      expect(client.state.status).toBe("open");
    });
    fireEvent.click(await screen.findByRole("button", { name: "Save as…" }));
    type("copy");
    connection.hold();
    enter();
    enter();
    expect(okButton().disabled).toBe(true);
    fireEvent.click(okButton());
    await waitFor(() => {
      expect(connection.answers()).toBe(1);
    });
    expect(asked({ requests: () => server.received.flatMap((message) => (message.type === "request" ? [{ intent: message.intent }] : [])) })).toEqual(["copy"]);
    expect(closed()).toBe(false);
    act(() => {
      connection.answer();
    });
    await waitFor(() => {
      expect(closed()).toBe(true);
    });
    expect(screen.getByText("copy.toml")).toBeTruthy();
    client.stop();
  });
});

describe("Control+Shift+S and Command+Shift+S", () => {
  const press = (init: KeyboardEventInit): boolean => fireEvent.keyDown(window, { key: "s", ...init });

  test("open the field, and keep the browser from offering to save the page", async () => {
    for (const modifier of [{ ctrlKey: true }, { metaKey: true }]) {
      await bar();
      expect(press({ ...modifier, shiftKey: true })).toBe(false); // (prevented)
      expect(field().value).toBe("tracer.toml");
      expect(document.activeElement).toBe(field());
      cleanup();
    }
  });

  test("find the field open already and have its text selected, with the focus", async () => {
    await bar();
    fireEvent.click(opener());
    type("copy");
    field().blur();
    expect(document.activeElement).not.toBe(field());
    expect(press({ ctrlKey: true, shiftKey: true })).toBe(false);
    expect(document.activeElement).toBe(field());
    expect(field().value).toBe("copy");
    expect([field().selectionStart, field().selectionEnd]).toEqual([0, "copy".length]);
  });

  test("are heard from a capital S (Caps Lock) too, and open the field when the focus is in another field", async () => {
    await bar();
    const other = document.createElement("input");
    document.body.append(other);
    other.focus();
    expect(fireEvent.keyDown(other, { key: "S", ctrlKey: true, shiftKey: true })).toBe(false);
    expect(document.activeElement).toBe(field());
    other.remove();
  });

  test("are not for a key that is only an S, for Alt with them, for Control+S (which saves) or for another letter", async () => {
    const made = await bar();
    expect(press({})).toBe(true);
    expect(press({ shiftKey: true })).toBe(true);
    expect(press({ ctrlKey: true, shiftKey: true, altKey: true })).toBe(true);
    // (Control+Shift+Z is the chord of Redo, which has nothing to do here.)
    for (const key of ["d", "z", "a"]) expect(fireEvent.keyDown(window, { key, ctrlKey: true, shiftKey: true }), key).toBe(true);
    expect(fireEvent.keyDown(window, { key: "z", metaKey: true, shiftKey: true })).toBe(true);
    expect(closed()).toBe(true);
    // (Control+S is the save: the browser's own is kept from the page, and nothing is asked for, there being nothing to save.)
    expect(press({ ctrlKey: true })).toBe(false);
    expect(closed()).toBe(true);
    expect(asked(made)).toEqual([]);
  });

  test("stop being listened to when the bar is gone", async () => {
    const made = await bar();
    made.view.unmount();
    expect(press({ ctrlKey: true, shiftKey: true })).toBe(true); // nobody is listening
  });
});
