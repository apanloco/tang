// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { RenderResult } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { MASTER, sideCablesFixture } from "../../client/fixtures";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// The session of the second input's fixture: group 0 ("Group 1") has a math effect (1, "Math 1") on its bus; lane 2 ("Oscillator 1") has Filter (3) and a math effect (4, "Math 2"); lane 5 ("Oscillator 2") has its
// way out cut; 6 is a free LFO. Three audio cables end at the second input of Math 2: 0 from Oscillator 2, 1 from Group 1 and 2 from Filter.
const { snapshot } = sideCablesFixture;

const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);
const sendsOf = (view: RenderResult): SVGPathElement[] => Array.from(view.container.querySelectorAll<SVGPathElement>(".tang-cable--send"));
/** The nodes that have a handle of this name, by the id of the node, in order. */
const nodesWith = (view: RenderResult, handle: string): number[] =>
  Array.from(view.container.querySelectorAll(`.react-flow__handle[data-handleid="${handle}"]`), (port) => Number(port.getAttribute("data-nodeid"))).sort((a, b) => a - b);
const buttonIn = async (id: number, name: string): Promise<HTMLButtonElement> => within(await nodeOf(id)).getByRole<HTMLButtonElement>("button", { name: `Send audio from ${name}` });
const listed = (): [string | null, string[]][] =>
  within(screen.getByRole("menu"))
    .getAllByRole("group")
    .map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);

describe("an effect that has a second input, on the canvas", () => {
  test("has two inputs, A and B, one above the other, where every other effect, a group and the master have one, and a lane has none", async () => {
    const { view } = await rig(<PatchView />, { snapshot });
    await nodeOf(MASTER);
    // (The two math effects, 1 and 4, have both; the filter 3, the group 0 and the master have the first alone.)
    expect(nodesWith(view, "audioInSide")).toEqual([1, 4]);
    expect(nodesWith(view, "audioIn")).toEqual([0, 1, 3, 4, MASTER].sort((a, b) => a - b));
    const names = Array.from(view.container.querySelectorAll(".tang-port-label"), (label) => label.textContent);
    expect(names).toEqual(["A", "B", "A", "B"]);
  });

  test("has the cables that end at its second input drawn there, each named for what it joins and the input it ends at, and a cable of the first input says A", async () => {
    const { view } = await rig(<PatchView />, { snapshot });
    await waitFor(() => {
      expect(sendsOf(view)).toHaveLength(3);
    });
    expect(sendsOf(view).map((path) => path.id).sort()).toEqual(["send:0", "send:1", "send:2"]);
    for (const words of ["Oscillator 2 → Math 2: B", "Group 1 → Math 2: B", "Filter → Math 2: B"]) {
      expect(screen.getByRole("slider", { name: `Level of ${words}` }), words).toBeTruthy();
      expect(screen.getByRole("button", { name: `Remove cable ${words}` }), words).toBeTruthy();
    }
    // (The link of the chain into the math effect is at its first input, which says so: its words are those of a cable.)
    expect(screen.getByRole("button", { name: "Remove cable Filter → Math 2: A" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Remove cable Oscillator 1 → Filter" })).toBeTruthy();
  });
});

describe("the menu that sends the audio of a node, to an effect that has two inputs", () => {
  test("lists each of the two inputs by name, the second only while it takes the cable, and an effect that has one by its name alone", async () => {
    await rig(<PatchView />, { snapshot });
    fireEvent.click(await buttonIn(5, "Oscillator 2"));
    // (Math 2's second input has a cable from here already: only its first is listed.)
    expect(listed()).toEqual([
      ["Effects", ["Math 1 (A)", "Math 1 (B)", "Filter", "Math 2 (A)"]],
      ["Group buses", ["Group 1"]],
      ["Mix", ["Master"]],
    ]);
  });

  test("draws the cable to the input that is chosen, the second as a cable of its own, and that input is not listed again while the first is", async () => {
    const { requests, view } = await rig(<PatchView />, { snapshot });
    const menu = await buttonIn(5, "Oscillator 2");
    fireEvent.click(menu);
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Math 1 (B)" }));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "addCable", from: { node: 5, port: { type: "audioOut" } }, to: { node: 1, port: { type: "audioInSide" } }, gain: null } }]);
    });
    await screen.findByRole("slider", { name: "Level of Oscillator 2 → Math 1: B" });
    expect(sendsOf(view)).toHaveLength(4);
    fireEvent.click(menu);
    expect(listed()[0]).toEqual(["Effects", ["Math 1 (A)", "Filter", "Math 2 (A)"]]);
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Math 1 (A)" }));
    await screen.findByRole("slider", { name: "Level of Oscillator 2 → Math 1: A" });
    expect(sendsOf(view)).toHaveLength(5);
  });

  test("follows the session: a cable that is drawn by somebody else to a second input takes it off the list", async () => {
    const { server } = await rig(<PatchView />, { snapshot });
    const menu = await buttonIn(5, "Oscillator 2");
    act(() => {
      server.apply({ type: "addCable", from: { node: 5, port: { type: "audioOut" } }, to: { node: 1, port: { type: "audioInSide" } }, gain: null });
    });
    await screen.findByRole("slider", { name: "Level of Oscillator 2 → Math 1: B" });
    fireEvent.click(menu);
    expect(listed()[0]).toEqual(["Effects", ["Math 1 (A)", "Filter", "Math 2 (A)"]]);
  });

  test("is shut for a node whose audio can go nowhere: Math 2 would feed itself, or Group 1's math effect, which Group 1's cable feeds back into Math 2, and the mix is its way out already", async () => {
    await rig(<PatchView />, { snapshot });
    expect((await buttonIn(4, "Math 2")).disabled).toBe(true);
    // (Not so for Math 1, which can send to Math 2: the group's cable is into the second input of Math 2, and Math 1 is in the group.)
    expect((await buttonIn(1, "Math 1")).disabled).toBe(false);
  });
});
