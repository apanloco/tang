// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { Choice } from "./Choice";

afterEach(cleanup);

const types = ["lowpass", "highpass", "bandpass", "notch"];

describe("a choice of named values", () => {
  test("is a select with its label and every name, in order, the chosen one selected", () => {
    render(<Choice label="Type" labels={types} value={2} onChange={() => undefined} />);
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "Type" });
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(types);
    expect(Array.from(select.options).map((option) => option.value)).toEqual(["0", "1", "2", "3"]);
    expect(select.value).toBe("2");
    expect(select.selectedOptions[0]?.textContent).toBe("bandpass");
  });

  test("says the index that was chosen, as a number", () => {
    const chosen: number[] = [];
    render(<Choice label="Type" labels={types} value={0} onChange={(index) => chosen.push(index)} />);
    const select = screen.getByRole("combobox");
    fireEvent.change(select, { target: { value: "3" } });
    fireEvent.change(select, { target: { value: "1" } });
    expect(chosen).toEqual([3, 1]);
  });

  test("takes a value that is not whole as the nearest whole", () => {
    render(<Choice label="Type" labels={types} value={1.6} onChange={() => undefined} />);
    expect(screen.getByRole<HTMLSelectElement>("combobox").value).toBe("2");
  });

  test("follows the value it is given", () => {
    const { rerender } = render(<Choice label="Type" labels={types} value={0} onChange={() => undefined} />);
    rerender(<Choice label="Type" labels={types} value={3} onChange={() => undefined} />);
    expect(screen.getByRole<HTMLSelectElement>("combobox").value).toBe("3");
  });

  test("one with no way to say what is chosen only shows, and so does one that is disabled", () => {
    const { rerender } = render(<Choice label="Type" labels={types} value={1} />);
    expect(screen.getByRole<HTMLSelectElement>("combobox").disabled).toBe(true);
    rerender(<Choice label="Type" labels={types} value={1} onChange={() => undefined} disabled />);
    expect(screen.getByRole<HTMLSelectElement>("combobox").disabled).toBe(true);
    rerender(<Choice label="Type" labels={types} value={1} onChange={() => undefined} />);
    expect(screen.getByRole<HTMLSelectElement>("combobox").disabled).toBe(false);
  });

  test("shows the label where it can be read", () => {
    render(<Choice label="Waveform" labels={["sine", "saw"]} value={0} />);
    expect(screen.getByText("Waveform")).toBeTruthy();
  });
});
