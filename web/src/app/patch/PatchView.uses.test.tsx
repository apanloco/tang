// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { sideCablesFixture } from "../../client/fixtures";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// The session of the second input's fixture: group 0 has a math effect (1, "Math 1") on its bus, with nothing at its second input; lane 2 has Filter (3) and a math effect (4, "Math 2"), with three
// cables at its second input (from Oscillator 2, Group 1 and the filter). Both are rings to begin with, which reads none of `amount`, `bits`, `attack` and `release`.
const { snapshot } = sideCablesFixture;
const [BUS_MATH, LANE_MATH] = [1, 4];

const NAMES = ["mode", "amount", "bits", "pitch", "wave", "attack", "release", "level"];
const RING = ["amount", "bits", "attack", "release"];

const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);

/** The cell of the node's grid that holds the control named `name`: a knob, or a choice if its values have names. */
function cellOf(node: HTMLElement, name: string): HTMLElement {
  const control = within(node).queryByRole("slider", { name }) ?? within(node).getByRole("combobox", { name });
  const cell = control.closest<HTMLElement>(".tang-cell");
  if (!cell) throw new Error(`${name} is not in a cell`);
  return cell;
}

/** The parameters on the node that are dimmed: the ones that have no effect now. */
const dimmed = (node: HTMLElement): string[] => NAMES.filter((name) => cellOf(node, name).hasAttribute("data-unused"));

/** Choose the mode that is the `place`th of the list in the control of the node or the panel, and let the frame go by that sends what was chosen (one request a frame). */
const setMode = (node: HTMLElement, place: number, clock: { tick: (time: number) => void }): void => {
  fireEvent.change(within(node).getByRole("combobox", { name: "mode" }), { target: { value: String(place) } });
  act(() => {
    clock.tick(16);
  });
};

describe("the knobs of a node that have no effect now", () => {
  test("are dimmed, and say why to the pointer: the carrier while a cable is patched at B, the rest in the modes that do not read them", async () => {
    await rig(<PatchView />, { snapshot });
    const lane = await nodeOf(LANE_MATH);
    expect(dimmed(lane)).toEqual(["amount", "bits", "pitch", "wave", "attack", "release"]);
    expect(cellOf(lane, "pitch").title).toBe("No effect now: only while nothing is patched at B");
    expect(cellOf(lane, "wave").title).toBe("No effect now: only while nothing is patched at B");
    expect(cellOf(lane, "bits").title).toBe("No effect now: only in the modes xor, and, or");
    expect(cellOf(lane, "amount").title).toBe("No effect now: only in the modes am, fold, warp, compare, hold, follow");
    expect(cellOf(lane, "attack").title).toBe("No effect now: only in the mode follow");
    // The mode and the level always have one, and say nothing.
    for (const name of ["mode", "level"]) {
      expect(cellOf(lane, name).hasAttribute("data-unused"), name).toBe(false);
      expect(cellOf(lane, name).hasAttribute("title"), name).toBe(false);
    }
    // The group's has nothing patched at B, so its carrier is B: `pitch` and `wave` have an effect there.
    expect(dimmed(await nodeOf(BUS_MATH))).toEqual(RING);
  });

  test("follow with the mode that is set: turning the mode dims what it does not read and lights what it does, the value being the one that the session has", async () => {
    const { clock } = await rig(<PatchView />, { snapshot });
    const bus = await nodeOf(BUS_MATH);
    setMode(bus, 11, clock);
    await waitFor(() => {
      expect(dimmed(bus)).toEqual(["bits"]);
    });
    setMode(bus, 7, clock);
    await waitFor(() => {
      expect(dimmed(bus)).toEqual(["amount", "attack", "release"]);
    });
    setMode(bus, 0, clock);
    await waitFor(() => {
      expect(dimmed(bus)).toEqual(RING);
    });
    // (Another effect's mode is not this one's.)
    expect(dimmed(await nodeOf(LANE_MATH))).toEqual(["amount", "bits", "pitch", "wave", "attack", "release"]);
  });

  test("are lit when the last cable at B is taken out, and dimmed again when one is drawn", async () => {
    const { view } = await rig(<PatchView />, { snapshot });
    const lane = await nodeOf(LANE_MATH);
    const out = ["Oscillator 2", "Group 1", "Filter"];
    for (const [done, from] of out.entries()) {
      fireEvent.click(await screen.findByRole("button", { name: `Remove cable ${from} → Math 2: B` }));
      await waitFor(() => {
        expect(screen.queryByRole("button", { name: `Remove cable ${from} → Math 2: B` })).toBeNull();
      });
      // (While one is left, the carrier is not B.)
      expect(dimmed(lane).includes("pitch"), `after ${String(done + 1)} cables out`).toBe(done < out.length - 1);
    }
    expect(dimmed(lane)).toEqual(RING);
    expect(view.container.querySelectorAll(".tang-cable--send")).toHaveLength(0);
  });

  test("can still be turned: a dimmed knob keeps its value for when it has an effect", async () => {
    const { requests } = await rig(<PatchView />, { snapshot });
    const lane = await nodeOf(LANE_MATH);
    expect(cellOf(lane, "pitch").hasAttribute("data-unused")).toBe(true);
    const pitch = within(lane).getByRole("slider", { name: "pitch" });
    pitch.focus();
    fireEvent.keyDown(pitch, { key: "ArrowUp" });
    await waitFor(() => {
      expect(requests().some((request) => JSON.stringify(request.intent).includes('"type":"setParam"') && JSON.stringify(request.intent).includes('"param":3'))).toBe(true);
    });
    expect(dimmed(lane).includes("pitch")).toBe(true);
  });
});

describe("the rows of a plugin's panel that have no effect now", () => {
  test("are dimmed as the knobs are, and say why", async () => {
    const { clock } = await rig(<PatchView />, { snapshot });
    fireEvent.click(await screen.findByRole("button", { name: "Parameters of Math 2" }));
    const panel = await screen.findByRole("complementary", { name: "Parameters of Math 2" });
    const rowOf = (name: string): HTMLElement => {
      const row = within(panel).getByText(name, { selector: ".tang-row__name" }).closest<HTMLElement>(".tang-row");
      if (!row) throw new Error(`${name} has no row`);
      return row;
    };
    expect(NAMES.filter((name) => rowOf(name).hasAttribute("data-unused"))).toEqual(["amount", "bits", "pitch", "wave", "attack", "release"]);
    expect(rowOf("pitch").title).toBe("No effect now: only while nothing is patched at B");
    expect(rowOf("mode").hasAttribute("title")).toBe(false);
    // The panel follows what is set, as the knobs do.
    setMode(panel, 7, clock);
    await waitFor(() => {
      expect(NAMES.filter((name) => rowOf(name).hasAttribute("data-unused"))).toEqual(["amount", "pitch", "wave", "attack", "release"]);
    });
  });
});

describe("a plugin that says nothing of when its parameters are used", () => {
  test("has none of them dimmed", async () => {
    await rig(<PatchView />, { snapshot });
    const filter = await nodeOf(3);
    expect(filter.querySelectorAll("[data-unused]")).toHaveLength(0);
  });
});
