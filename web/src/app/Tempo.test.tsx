// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, test, vi } from "vitest";

import type { Intent } from "../protocol";

import { Tempo } from "./Tempo";
import { rig } from "./testing";

afterEach(cleanup);

// The tempo in the bar of the session: a field of beats a minute that patterns are recorded at, set with `SetBpm` (the server holds it to the tempos there can be, and what the
// controller and the mock engine do with it is in `connection.test.ts` and `apply.rs`). What is looked at here is the page: what it shows, what it asks, and when.

const bar = () => rig(<Tempo />);
const field = (): HTMLInputElement => screen.getByRole<HTMLInputElement>("spinbutton", { name: "Tempo" });

/** The tempos that the server has been asked for, in order. */
const asked = (made: { requests: () => { intent: unknown }[] }): number[] =>
  made.requests().flatMap(({ intent }) => (typeof intent === "object" && intent !== null && "type" in intent && intent.type === "setBpm" && "bpm" in intent && typeof intent.bpm === "number" ? [intent.bpm] : []));

const type = (text: string): void => {
  fireEvent.change(field(), { target: { value: text } });
};
const press = (key: string, extra: { shiftKey?: boolean } = {}): boolean => fireEvent.keyDown(field(), { key, ...extra });
/** The page's turn: what a request that was made now would have come to the server by (an absence is looked for after it, not before). */
const turn = (): Promise<void> =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 30));
  });

describe("the tempo of the bar of the session", () => {
  test("shows the tempo of the session in beats a minute, and the tempos it can be", async () => {
    await bar();
    expect(field().value).toBe("120");
    expect(screen.getByText("BPM")).toBeTruthy();
    expect([field().getAttribute("aria-valuemin"), field().getAttribute("aria-valuemax"), field().getAttribute("aria-valuenow")]).toEqual(["20", "300", "120"]);
  });

  test("shows a tempo that another front-end sets (the terminal, another browser)", async () => {
    const made = await bar();
    act(() => {
      made.server.apply({ type: "setBpm", bpm: 96.5 });
    });
    await waitFor(() => {
      expect(field().value).toBe("96.5");
    });
    expect(field().getAttribute("aria-valuenow")).toBe("96.5");
  });

  test("selects its text when it gets the focus, so that what is typed is a tempo of its own", async () => {
    await bar();
    field().focus();
    expect([field().selectionStart, field().selectionEnd]).toEqual([0, 3]);
  });

  test("asks nothing while a tempo is being typed, and asks for it when Enter is pressed, once", async () => {
    const made = await bar();
    type("9");
    type("90");
    expect(field().value).toBe("90");
    await turn();
    expect(asked(made)).toEqual([]);
    press("Enter");
    await turn();
    expect(asked(made)).toEqual([90]);
    expect(made.client.state.snapshot?.meta.bpm).toBe(90);
    expect(field().value).toBe("90");
  });

  test("asks for what was typed when the field is left", async () => {
    const made = await bar();
    type("96.5");
    fireEvent.blur(field());
    await turn();
    expect(asked(made)).toEqual([96.5]);
    expect(field().value).toBe("96.5");
  });

  test("goes back to the tempo it has, and asks nothing, for what is not a number", async () => {
    const made = await bar();
    for (const text of ["abc", "", "1,5", "nan", "9e2", "--3", "1 2"]) {
      type(text);
      press("Enter");
      await turn();
      expect(field().value, `after ${JSON.stringify(text)}`).toBe("120");
    }
    expect(asked(made)).toEqual([]);
  });

  test("gives what was typed up when Escape is pressed, and asks nothing", async () => {
    const made = await bar();
    type("90");
    press("Escape");
    expect(field().value).toBe("120");
    fireEvent.blur(field());
    await turn();
    expect(asked(made)).toEqual([]);
  });

  test("asks nothing for the tempo that the session has", async () => {
    const made = await bar();
    type("120");
    press("Enter");
    type("120.0");
    fireEvent.blur(field());
    await turn();
    expect(asked(made)).toEqual([]);
  });

  test("steps with the arrow keys: one beat a minute, ten with Shift, each from what the field shows", async () => {
    const made = await bar();
    press("ArrowUp");
    press("ArrowUp");
    press("ArrowDown", { shiftKey: true });
    press("ArrowUp", { shiftKey: true });
    await turn();
    expect(asked(made)).toEqual([121, 122, 112, 122]);
    expect(made.client.state.snapshot?.meta.bpm).toBe(122);
    expect(field().value).toBe("122");
  });

  test("takes the key that it is given, so that the browser does not scroll or move the cursor with it", async () => {
    await bar();
    expect(press("ArrowUp")).toBe(false);
    expect(press("ArrowDown")).toBe(false);
    expect(press("Enter")).toBe(false);
    expect(press("a")).toBe(true);
  });

  test("steps from a tempo with a fraction without the noise of the 64-bit numbers", async () => {
    const made = await bar();
    act(() => {
      made.server.apply({ type: "setBpm", bpm: 133.3 });
    });
    await waitFor(() => {
      expect(field().value).toBe("133.3");
    });
    press("ArrowDown", { shiftKey: true });
    await turn();
    expect(asked(made)).toEqual([123.3]);
    expect(field().value).toBe("123.3");
  });

  test("keeps showing the tempo that was asked for last while an earlier one is answered", async () => {
    const made = await bar();
    // (The answers are held, and let go in the order the requests were made.)
    const answers: (() => void)[] = [];
    const request = made.client.request.bind(made.client);
    vi.spyOn(made.client, "request").mockImplementation(
      (intent: Intent) =>
        new Promise<number>((resolve, reject) => {
          answers.push(() => {
            request(intent).then(resolve, reject);
          });
        }),
    );
    press("ArrowUp");
    press("ArrowUp");
    expect(field().value).toBe("122");
    await act(async () => {
      answers[0]?.();
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    expect(made.client.state.snapshot?.meta.bpm).toBe(121);
    expect(field().value, "the first is answered and the second is not: the field is not the session's tempo yet").toBe("122");
    await act(async () => {
      answers[1]?.();
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    expect(made.client.state.snapshot?.meta.bpm).toBe(122);
    expect(field().value).toBe("122");
  });

  test("leaves the Enter that confirms what an input method composes to the input method", async () => {
    const made = await bar();
    type("90");
    expect(fireEvent.keyDown(field(), { key: "Enter", isComposing: true })).toBe(true);
    await turn();
    expect(asked(made)).toEqual([]);
    expect(field().value).toBe("90");
  });

  test("holds a step to the tempos there can be, and asks nothing at the end of them", async () => {
    const made = await bar();
    act(() => {
      made.server.apply({ type: "setBpm", bpm: 295 });
    });
    await waitFor(() => {
      expect(field().value).toBe("295");
    });
    press("ArrowUp", { shiftKey: true });
    press("ArrowUp");
    press("ArrowUp", { shiftKey: true });
    await turn();
    expect(asked(made)).toEqual([300]);
    act(() => {
      made.server.apply({ type: "setBpm", bpm: 25 });
    });
    await waitFor(() => {
      expect(field().value).toBe("25");
    });
    press("ArrowDown", { shiftKey: true });
    press("ArrowDown");
    await turn();
    expect(asked(made)).toEqual([300, 20]);
  });

  test("asks for a tempo beyond them as it was typed, and shows what the server holds it to", async () => {
    const made = await bar();
    type("1000");
    press("Enter");
    await turn();
    expect(asked(made)).toEqual([1000]);
    expect(field().value).toBe("300");
    type("5");
    press("Enter");
    await turn();
    expect(asked(made)).toEqual([1000, 5]);
    expect(field().value).toBe("20");
  });

  test("keeps what is being typed when another front-end sets the tempo", async () => {
    const made = await bar();
    type("9");
    act(() => {
      made.server.apply({ type: "setBpm", bpm: 80 });
    });
    await turn();
    expect(field().value).toBe("9");
    type("90");
    press("Enter");
    await turn();
    expect(asked(made)).toEqual([90]);
  });

  test("shows the tempo again when the server does not answer it, and says why", async () => {
    const made = await bar();
    made.client.stop();
    type("90");
    press("Enter");
    await turn();
    expect(asked(made)).toEqual([]);
    expect(field().value).toBe("120");
  });
});
