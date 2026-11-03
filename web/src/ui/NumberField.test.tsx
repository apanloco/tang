// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { NumberField } from "./NumberField";
import type { NumberFieldProps } from "./NumberField";

afterEach(cleanup);

// The field in itself, with a stub that is asked: what it shows, what it asks and when, and what it does with the answers. (The tempo's field in the bar, which is made of it, has its
// own tests of what a page asks of the server; the length of a pattern has them in its cell's.)

/** A field of a number 8 from 1 to 32 that turns by 1 (4 with Shift), and the numbers it has asked for. */
function field(props: Partial<NumberFieldProps> = {}) {
  const asked: number[] = [];
  const answers: (() => void)[] = [];
  const view = render(
    <NumberField
      label="Length"
      value={8}
      range={[1, 32]}
      step={1}
      bigStep={4}
      ask={(value) => {
        asked.push(value);
        return new Promise<void>((resolve) => answers.push(resolve));
      }}
      {...props}
    />,
  );
  const input = (): HTMLInputElement => screen.getByRole<HTMLInputElement>("spinbutton", { name: props.label ?? "Length" });
  const rerender = (next: Partial<NumberFieldProps>): void => {
    view.rerender(<NumberField label="Length" value={8} range={[1, 32]} step={1} bigStep={4} ask={() => Promise.resolve()} {...next} />);
  };
  return { asked, answers, view, input, rerender };
}

const type = (input: HTMLElement, text: string): void => {
  fireEvent.change(input, { target: { value: text } });
};
const answerAll = async (answers: (() => void)[]): Promise<void> => {
  await act(async () => {
    for (const answer of answers.splice(0)) answer();
    await Promise.resolve();
  });
};

describe("a number as a field of text", () => {
  test("is a spinbutton with its name, its number and the numbers there can be, and the class and the title it is given", () => {
    const { input } = field({ className: "tang-x", title: "How long" });
    expect(input().value).toBe("8");
    expect([input().getAttribute("aria-valuemin"), input().getAttribute("aria-valuemax"), input().getAttribute("aria-valuenow")]).toEqual(["1", "32", "8"]);
    expect([input().className, input().title]).toEqual(["tang-x", "How long"]);
  });

  test("asks nothing while a number is typed, and asks for it once when Enter is pressed", () => {
    const { asked, input } = field();
    type(input(), "1");
    type(input(), "12");
    expect(asked).toEqual([]);
    fireEvent.keyDown(input(), { key: "Enter" });
    expect(asked).toEqual([12]);
    // (Nothing is typed any more: Enter again has nothing to ask.)
    fireEvent.keyDown(input(), { key: "Enter" });
    expect(asked).toEqual([12]);
  });

  test("asks for what was typed when the field is left", () => {
    const { asked, input } = field();
    type(input(), "2.5");
    fireEvent.blur(input());
    expect(asked).toEqual([2.5]);
  });

  test("goes back to its number, and asks nothing, for what is not a number, for the number it has and for what is given up with Escape", () => {
    const { asked, input } = field();
    // (A string of digits that is too long for a number is not one either: it is infinity.)
    for (const text of ["", "abc", "1e2", "1/2", "--3", "8", " 8 ", "9".repeat(400)]) {
      type(input(), text);
      fireEvent.keyDown(input(), { key: "Enter" });
      expect(input().value, JSON.stringify(text)).toBe("8");
    }
    type(input(), "20");
    fireEvent.keyDown(input(), { key: "Escape" });
    expect(input().value).toBe("8");
    expect(asked).toEqual([]);
  });

  test("takes a number that is signed, has a fraction or begins with a point, and trims what is round it", () => {
    const { asked, input } = field();
    for (const text of ["+3", "-2", "4.", ".5", " 7.25 "]) {
      type(input(), text);
      fireEvent.keyDown(input(), { key: "Enter" });
    }
    expect(asked).toEqual([3, -2, 4, 0.5, 7.25]);
  });

  test("asks for a number beyond the range as it was typed: the server holds it, and the field shows what it was held to", async () => {
    const { asked, answers, input, rerender } = field();
    type(input(), "100");
    fireEvent.keyDown(input(), { key: "Enter" });
    expect(asked).toEqual([100]);
    expect(input().value).toBe("100");
    rerender({ value: 32 });
    await answerAll(answers);
    expect(input().value).toBe("32");
  });

  test("takes the Enter and the Escape that it acts on, so that a form or a dialog round it does not", () => {
    const { input } = field();
    type(input(), "20");
    expect(fireEvent.keyDown(input(), { key: "Enter" })).toBe(false);
    type(input(), "21");
    expect(fireEvent.keyDown(input(), { key: "Escape" })).toBe(false);
    // (And a key that is not its own is left alone.)
    expect(fireEvent.keyDown(input(), { key: "a" })).toBe(true);
  });

  test("turns by a step with the arrow keys and by the big one with Shift, each from what the field shows, and takes the key", () => {
    const { asked, input } = field();
    expect(fireEvent.keyDown(input(), { key: "ArrowUp" })).toBe(false);
    // (The field shows the 9 that was asked for, and the next step is from it.)
    expect(fireEvent.keyDown(input(), { key: "ArrowDown", shiftKey: true })).toBe(false);
    expect(asked).toEqual([9, 5]);
  });

  test("turns from what is typed and not from the number it has, and gives the typing up for the step", () => {
    const { asked, input } = field();
    type(input(), "20");
    fireEvent.keyDown(input(), { key: "ArrowUp" });
    expect(asked).toEqual([21]);
  });

  test("turns from the number it has when what is typed is not one", () => {
    const { asked, input } = field();
    type(input(), "abc");
    fireEvent.keyDown(input(), { key: "ArrowUp" });
    expect(asked).toEqual([9]);
  });

  test("stops at the ends of the range and asks nothing there", () => {
    // (Each from a field of its own: a field shows the number it asked for until it is answered.)
    const cases: { value: number; key: string; shiftKey: boolean; asks: number[] }[] = [
      { value: 31, key: "ArrowUp", shiftKey: true, asks: [32] },
      { value: 32, key: "ArrowUp", shiftKey: false, asks: [] },
      { value: 32, key: "ArrowUp", shiftKey: true, asks: [] },
      { value: 2, key: "ArrowDown", shiftKey: true, asks: [1] },
      { value: 1, key: "ArrowDown", shiftKey: false, asks: [] },
      { value: 1, key: "ArrowDown", shiftKey: true, asks: [] },
    ];
    for (const { value, key, shiftKey, asks } of cases) {
      const { asked, input } = field({ value });
      fireEvent.keyDown(input(), { key, shiftKey });
      expect(asked, `${String(value)} ${key}${shiftKey ? " with Shift" : ""}`).toEqual(asks);
      cleanup();
    }
  });

  test("steps from a number with a fraction without the noise of the 64-bit numbers", () => {
    const { asked, input } = field({ value: 0.1, range: [0, 10], step: 0.1, bigStep: 1 });
    // 0.1 + 0.1 + 0.1 is 0.30000000000000004 in 64 bits.
    fireEvent.keyDown(input(), { key: "ArrowUp" });
    expect(asked).toEqual([0.2]);
    type(input(), "0.2");
    fireEvent.keyDown(input(), { key: "ArrowUp" });
    expect(asked).toEqual([0.2, 0.3]);
  });

  test("shows the number it asked for until it is answered, and then the number it is given", async () => {
    const { answers, input, rerender } = field();
    fireEvent.keyDown(input(), { key: "ArrowUp" });
    expect(input().value).toBe("9");
    expect(input().getAttribute("aria-valuenow")).toBe("9");
    await answerAll(answers);
    // (Refused: the session still has 8.)
    expect(input().value).toBe("8");
    fireEvent.keyDown(input(), { key: "ArrowUp" });
    rerender({ value: 9 });
    await answerAll(answers);
    expect(input().value).toBe("9");
  });

  test("shows the number that was asked for last while an earlier one is answered", async () => {
    const { answers, input } = field();
    fireEvent.keyDown(input(), { key: "ArrowUp" });
    fireEvent.keyDown(input(), { key: "ArrowUp" });
    expect(input().value).toBe("10");
    // The first answer comes: the second is still out.
    await act(async () => {
      answers[0]?.();
      await Promise.resolve();
    });
    expect(input().value).toBe("10");
    await act(async () => {
      answers[1]?.();
      await Promise.resolve();
    });
    expect(input().value).toBe("8");
  });

  test("follows a number that another front-end sets, and keeps what is being typed", () => {
    const { input, rerender } = field();
    rerender({ value: 12 });
    expect(input().value).toBe("12");
    type(input(), "2");
    rerender({ value: 16 });
    expect(input().value).toBe("2");
  });

  test("leaves the Enter that confirms what an input method composes to the input method", () => {
    const { asked, input } = field();
    type(input(), "20");
    fireEvent.keyDown(input(), { key: "Enter", isComposing: true });
    expect(asked).toEqual([]);
    expect(input().value).toBe("20");
  });

  test("selects its text when it gets the focus", () => {
    const { input } = field();
    input().focus();
    expect([input().selectionStart, input().selectionEnd]).toEqual([0, 1]);
  });
});
