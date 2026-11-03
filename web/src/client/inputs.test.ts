import { describe, expect, test } from "vitest";

import { inputsFixture } from "./fixtures";
import { inputsStoodFor } from "./inputs";

// The rule for which inputs a device of a session stands for (`midi::matching`): the page's, pinned to the controller's by what it makes of a list of names against some sets of inputs (the fixture).
describe("the inputs that a device stands for", () => {
  test("are what the controller finds for each row of its fixture", () => {
    expect(inputsFixture.rows.length).toBeGreaterThan(100);
    const wrong = inputsFixture.rows.flatMap((row) => {
      const found = inputsStoodFor(row.name, row.open);
      return JSON.stringify(found) === JSON.stringify(row.stands) ? [] : [`${JSON.stringify(row.name)} among ${JSON.stringify(row.open)}: ${JSON.stringify(found)}, and the controller says ${JSON.stringify(row.stands)}`];
    });
    expect(wrong).toEqual([]);
  });

  test("have been tried with every kind of answer: none, one, both ports of a device, and a name that is the whole of one port and the part of another", () => {
    const sizes = new Set(inputsFixture.rows.map((row) => row.stands.length));
    expect([...sizes].sort()).toEqual([0, 1, 2, 3]);
    const exactAndPart = inputsFixture.rows.find((row) => row.name === "IAC Driver Bus 1");
    expect(exactAndPart?.open).toContain("IAC Driver Bus 10");
    expect(exactAndPart?.stands).toEqual(["IAC Driver Bus 1"]);
    expect(inputsFixture.rows.some((row) => row.stands.length > 0 && row.stands.every((input) => input !== row.name))).toBe(true);
  });

  test("by the whole name first, and then by the text in it, whatever the case, and none for a name that is blank or a device that is not there", () => {
    const open = ["Launchkey Mini MK4 37 MIDI Out", "Launchkey Mini MK4 37 DAW Out", "IAC Driver Bus 1", "IAC Driver Bus 10"];
    expect(inputsStoodFor("Launchkey Mini MK4 37", open)).toEqual([open[0], open[1]]);
    expect(inputsStoodFor("  launchkey  ", open)).toEqual([open[0], open[1]]);
    expect(inputsStoodFor("IAC Driver Bus 1", open)).toEqual(["IAC Driver Bus 1"]);
    expect(inputsStoodFor("IAC Driver Bus", open)).toEqual(["IAC Driver Bus 1", "IAC Driver Bus 10"]);
    expect(inputsStoodFor("", open)).toEqual([]);
    expect(inputsStoodFor(" \u00a0 ", open)).toEqual([]);
    expect(inputsStoodFor("Pianoteq", open)).toEqual([]);
    expect(inputsStoodFor("Launchkey", [])).toEqual([]);
    expect(inputsStoodFor("launchkey mini mk4 37 midi out and more", open)).toEqual([]);
  });
});
