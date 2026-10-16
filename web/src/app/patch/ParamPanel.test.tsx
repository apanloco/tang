// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import type { Param, Snapshot } from "../../protocol";
import { useClientState } from "../ClientProvider";
import { rig } from "../testing";
import { ParamPanel } from "./ParamPanel";
import { PatchView } from "./PatchView";

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

const OSCILLATOR_PARAMS = ["waveform", "detune", "volume", "attack", "decay", "sustain", "release", "pan", "unison", "spread", "width", "pulse", "fm", "fm_ratio", "sub", "sync", "drift", "velocity", "bend"];

const buttonOf = (name: string) => screen.findByRole<HTMLButtonElement>("button", { name: `Parameters of ${name}` });
const panelOf = (name: string) => screen.findByRole("complementary", { name: `Parameters of ${name}` });
const searchOf = (name: string) => screen.findByRole<HTMLInputElement>("searchbox", { name: `Filter the parameters of ${name}` });

/** The names of the parameters a panel lists, in order. */
const listed = (panel: HTMLElement): string[] => Array.from(panel.querySelectorAll(".tang-row__name")).map((label) => label.textContent);

/** The node of the canvas that is the picture of this node of the session. */
const nodeOf = (id: number) => screen.findByTestId(`rf__node-${String(id)}`);

/** A session whose filter has a hundred parameters, "Param 0" to "Param 99", over 0 to 1. */
function withManyParams(): Snapshot {
  const snapshot = structuredClone(tracerSnapshot);
  const node = snapshot.graph.nodes.find((n) => n.id === filter.id);
  if (node?.kind !== "effect") throw new Error("no filter");
  node.plugin.params = Array.from({ length: 100 }, (_, index): Param => ({ index, name: `Param ${String(index)}`, value: 0.5, min: 0, max: 1, default: 0.5, unit: null, labels: null, group: [], stepCount: 0 }));
  return snapshot;
}

describe("the button that opens the parameters of a plugin", () => {
  test("is on every instrument and effect that has a plugin, named for it, and on nothing else", async () => {
    await rig(<PatchView />);
    expect((await buttonOf("Oscillator")).getAttribute("aria-expanded")).toBe("false");
    expect(await buttonOf("Filter")).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /^Parameters of / })).toHaveLength(2); // not the LFO, not the master
  });

  test("is not on a plugin that has no parameters", async () => {
    const bare = structuredClone(tracerSnapshot);
    const node = bare.graph.nodes.find((n) => n.id === filter.id);
    if (node?.kind !== "effect") throw new Error("no filter");
    node.plugin.params = [];
    await rig(<PatchView />, { snapshot: bare });
    await buttonOf("Oscillator");
    expect(screen.queryByRole("button", { name: "Parameters of Filter" })).toBeNull();
  });

  test("is on the effects of a group's bus too, and not on a lane that has no instrument, a group or a modulator", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    await screen.findAllByRole("button", { name: /^Parameters of / });
    const withPlugin = reachFixture.snapshot.graph.nodes.filter((node) => (node.kind === "instrument" || node.kind === "effect") && node.plugin !== null);
    expect(withPlugin.some((node) => node.kind === "effect" && reachFixture.snapshot.graph.nodes.find((n) => n.id === node.parent)?.kind === "group")).toBe(true);
    expect(screen.getAllByRole("button", { name: /^Parameters of / })).toHaveLength(withPlugin.length);
  });
});

describe("the parameters of a plugin", () => {
  test("are listed in a panel when the button is pressed, every one, in the plugin's order; the button says it is open and the focus is in the filter", async () => {
    await rig(<PatchView />);
    expect(screen.queryByRole("complementary")).toBeNull();
    fireEvent.click(await buttonOf("Oscillator"));
    const panel = await panelOf("Oscillator");
    expect(listed(panel)).toEqual(OSCILLATOR_PARAMS);
    expect(within(panel).getByText("19 parameters")).toBeTruthy();
    expect((await buttonOf("Oscillator")).getAttribute("aria-expanded")).toBe("true");
    expect(document.activeElement).toBe(await searchOf("Oscillator"));
  });

  test("have a control each that sets the plugin's parameter: a slider, and a choice for what has named values", async () => {
    const { requests, clock } = await rig(<PatchView />);
    fireEvent.click(await buttonOf("Oscillator"));
    const panel = await panelOf("Oscillator");
    expect(within(panel).getByRole("combobox", { name: "waveform" })).toBeTruthy();
    const detune = within(panel).getByRole<HTMLInputElement>("slider", { name: "detune" });
    expect([detune.min, detune.max]).toEqual(["-24", "24"]);
    fireEvent.change(detune, { target: { value: "7" } });
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: oscillator.id, param: 1, value: 7 } }]);
    });
  });

  test("are the node's own knobs' as well: what is set in the panel is what the knob shows, and the other way round", async () => {
    const { server, clock, requests } = await rig(<PatchView />);
    fireEvent.click(await buttonOf("Filter"));
    const panel = await panelOf("Filter");
    const node = await nodeOf(filter.id);
    fireEvent.change(within(panel).getByRole("slider", { name: "resonance" }), { target: { value: "3" } });
    expect(within(node).getByRole("slider", { name: "resonance" }).getAttribute("aria-valuenow")).toBe("3");
    // (Once the engine has answered, what is shown is what the session has: somebody else's change shows.)
    fireEvent.pointerUp(within(panel).getByRole("slider", { name: "resonance" }));
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    await waitFor(() => {
      expect(within(node).getByRole("slider", { name: "resonance" }).getAttribute("aria-valuenow")).toBe("3");
    });
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 1, value: 6 });
    });
    await waitFor(() => {
      expect(within(node).getByRole("slider", { name: "resonance" }).getAttribute("aria-valuenow")).toBe("6");
    });
    await waitFor(() => {
      expect(within(panel).getByRole<HTMLInputElement>("slider", { name: "resonance" }).value).toBe("6");
    });
  });

  test("have a bar of what the engine makes of them where a cable drives them", async () => {
    await rig(<PatchView />);
    fireEvent.click(await buttonOf("Filter"));
    const panel = await panelOf("Filter");
    expect(within(panel).getAllByRole("meter").map((bar) => bar.getAttribute("aria-label"))).toEqual(["cutoff, as modulated"]);
  });

  test("turn the plugin's parameter by its own index, which is not its place in the list: a plugin can number its parameters with gaps", async () => {
    const gaps = structuredClone(tracerSnapshot);
    const node = gaps.graph.nodes.find((n) => n.id === filter.id);
    if (node?.kind !== "effect") throw new Error("no filter");
    node.plugin.params = node.plugin.params.map((param, position) => ({ ...param, index: [3, 8, 20, 21][position] ?? 99 }));
    const { requests, clock } = await rig(<PatchView />, { snapshot: gaps });
    fireEvent.click(await buttonOf("Filter"));
    const panel = await panelOf("Filter");
    fireEvent.change(within(panel).getByRole("slider", { name: "resonance" }), { target: { value: "4" } });
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 8, value: 4 } }]);
    });
  });

  test("include the ones a node does not show: the reverb's last two, which are turned from the panel", async () => {
    const { server, requests, clock } = await rig(<PatchView />);
    act(() => {
      server.apply({ type: "addEffect", parent: oscillator.id, plugin: "builtin:reverb" });
    });
    fireEvent.click(await buttonOf("Reverb"));
    const panel = await panelOf("Reverb");
    expect(listed(panel)).toEqual(["decay", "size", "damping", "predelay", "diffusion", "mod_rate", "mod_depth", "width", "lowcut", "level"]);
    // (The node has the first eight as knobs.)
    const reverbNode = screen.getAllByRole("button", { name: "Load a preset into Reverb" })[0]?.closest(".tang-node");
    expect(reverbNode?.querySelector('[role="slider"][aria-label="lowcut"]')).toBeNull();
    expect(reverbNode?.querySelector('[role="slider"][aria-label="width"]')).not.toBeNull();
    const lowcut = within(panel).getByRole<HTMLInputElement>("slider", { name: "lowcut" });
    expect([lowcut.min, lowcut.max, lowcut.value]).toEqual(["20", "500", "80"]);
    expect(within(panel).getByText("80 Hz")).toBeTruthy();
    fireEvent.change(lowcut, { target: { value: "120" } });
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests().at(-1)).toEqual({ intent: { type: "setParam", node: expect.any(Number) as number, param: 8, value: 120 } });
    });
  });
});

describe("finding a parameter in the panel", () => {
  test("narrows the list to the names that have what is typed in them, and says how many that is", async () => {
    await rig(<PatchView />);
    fireEvent.click(await buttonOf("Oscillator"));
    const panel = await panelOf("Oscillator");
    fireEvent.change(await searchOf("Oscillator"), { target: { value: "ele" } });
    expect(listed(panel)).toEqual(["release"]);
    expect(within(panel).getByText("1 of 19 parameters")).toBeTruthy();
    fireEvent.change(await searchOf("Oscillator"), { target: { value: "  A " } });
    expect(listed(panel)).toEqual(["waveform", "attack", "decay", "sustain", "release", "pan", "spread", "fm_ratio"]);
    expect(within(panel).getByText("8 of 19 parameters")).toBeTruthy();
    fireEvent.change(await searchOf("Oscillator"), { target: { value: "" } });
    expect(listed(panel)).toEqual(OSCILLATOR_PARAMS);
    expect(within(panel).getByText("19 parameters")).toBeTruthy();
  });

  test("is the whole list for blanks, as for nothing", async () => {
    await rig(<PatchView />);
    fireEvent.click(await buttonOf("Oscillator"));
    const panel = await panelOf("Oscillator");
    fireEvent.change(await searchOf("Oscillator"), { target: { value: "   " } });
    expect(listed(panel)).toEqual(OSCILLATOR_PARAMS);
    expect(within(panel).getByText("19 parameters")).toBeTruthy();
    expect(within(panel).queryByText(/^No parameter has/)).toBeNull();
  });

  test("says so when no name has what is typed in it", async () => {
    await rig(<PatchView />);
    fireEvent.click(await buttonOf("Oscillator"));
    const panel = await panelOf("Oscillator");
    fireEvent.change(await searchOf("Oscillator"), { target: { value: "zzz" } });
    expect(listed(panel)).toEqual([]);
    expect(within(panel).getByText('No parameter has "zzz" in its name')).toBeTruthy();
    expect(within(panel).getByText("0 of 19 parameters")).toBeTruthy();
  });

  test("shows a page of a long list at a time: the first forty, then forty more as it is asked for, and the button goes with the last", async () => {
    await rig(<PatchView />, { snapshot: withManyParams() });
    fireEvent.click(await buttonOf("Filter"));
    const panel = await panelOf("Filter");
    expect(listed(panel)).toHaveLength(40);
    expect(listed(panel)[0]).toBe("Param 0");
    expect(listed(panel)[39]).toBe("Param 39");
    expect(within(panel).getByText("100 parameters")).toBeTruthy();
    fireEvent.click(within(panel).getByRole("button", { name: "Show more (60 not shown)" }));
    expect(listed(panel)).toHaveLength(80);
    fireEvent.click(within(panel).getByRole("button", { name: "Show more (20 not shown)" }));
    expect(listed(panel)).toHaveLength(100);
    expect(within(panel).queryByRole("button", { name: /^Show more/ })).toBeNull();
  });

  test("finds one among a hundred by its name, and goes back to the first page when what is looked for changes", async () => {
    await rig(<PatchView />, { snapshot: withManyParams() });
    fireEvent.click(await buttonOf("Filter"));
    const panel = await panelOf("Filter");
    fireEvent.click(within(panel).getByRole("button", { name: "Show more (60 not shown)" }));
    expect(listed(panel)).toHaveLength(80);
    fireEvent.change(await searchOf("Filter"), { target: { value: "ram 9" } });
    // (Two words: a name that has both, wherever they are: "9" is in Param 9, 19, 29, ... 90 to 99.)
    expect(listed(panel)).toEqual(["Param 9", "Param 19", "Param 29", "Param 39", "Param 49", "Param 59", "Param 69", "Param 79", "Param 89", "Param 90", "Param 91", "Param 92", "Param 93", "Param 94", "Param 95", "Param 96", "Param 97", "Param 98", "Param 99"]);
    expect(within(panel).getByText("19 of 100 parameters")).toBeTruthy();
    fireEvent.change(await searchOf("Filter"), { target: { value: "" } });
    expect(listed(panel)).toHaveLength(40);
  });
});

describe("closing the panel", () => {
  test("is done by pressing the button again, which is where the focus stays", async () => {
    await rig(<PatchView />);
    fireEvent.click(await buttonOf("Oscillator"));
    await panelOf("Oscillator");
    fireEvent.click(await buttonOf("Oscillator"));
    expect(screen.queryByRole("complementary")).toBeNull();
    expect((await buttonOf("Oscillator")).getAttribute("aria-expanded")).toBe("false");
  });

  test("is done with its own button, and the focus goes back to the button that opened it", async () => {
    await rig(<PatchView />);
    fireEvent.click(await buttonOf("Oscillator"));
    const panel = await panelOf("Oscillator");
    fireEvent.click(within(panel).getByRole("button", { name: "Close the parameters of Oscillator" }));
    expect(screen.queryByRole("complementary")).toBeNull();
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Parameters of Oscillator" }));
    });
  });

  test("is done with Escape, wherever in the panel the focus is, and the focus goes back to the button", async () => {
    await rig(<PatchView />);
    fireEvent.click(await buttonOf("Oscillator"));
    const panel = await panelOf("Oscillator");
    fireEvent.keyDown(within(panel).getByRole("slider", { name: "detune" }), { key: "Escape" });
    expect(screen.queryByRole("complementary")).toBeNull();
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Parameters of Oscillator" }));
    });
  });

  test("happens when the node is taken out, and there is nowhere for the focus to go back to", async () => {
    const { server } = await rig(<PatchView />);
    fireEvent.click(await buttonOf("Filter"));
    await panelOf("Filter");
    act(() => {
      server.apply({ type: "removeNode", node: filter.id });
    });
    await waitFor(() => {
      expect(screen.queryByRole("complementary")).toBeNull();
    });
  });
});

describe("the panel of one plugin at a time", () => {
  test("is the panel of the plugin whose button was pressed last, with its own filter, and the focus in it", async () => {
    await rig(<PatchView />);
    fireEvent.click(await buttonOf("Oscillator"));
    await panelOf("Oscillator");
    fireEvent.change(await searchOf("Oscillator"), { target: { value: "att" } });
    fireEvent.click(await buttonOf("Filter"));
    const panel = await panelOf("Filter");
    expect(screen.getAllByRole("complementary")).toHaveLength(1);
    expect(listed(panel)).toEqual(["cutoff", "resonance", "type", "drive"]);
    expect((await searchOf("Filter")).value).toBe("");
    expect(document.activeElement).toBe(await searchOf("Filter"));
    expect((await buttonOf("Oscillator")).getAttribute("aria-expanded")).toBe("false");
    expect((await buttonOf("Filter")).getAttribute("aria-expanded")).toBe("true");
  });
});

/** What is drawn once the session is there (a panel asked for before it is has no node to show either). */
function WhenThereIsASession({ children }: { children: ReactNode }) {
  const there = useClientState((state) => state.snapshot !== null);
  return there ? children : null;
}

describe("a panel whose node is not there", () => {
  test("draws nothing and asks to be closed, with no focus to give back", async () => {
    const closed: boolean[] = [];
    const { view } = await rig(
      <WhenThereIsASession>
        <ParamPanel node={4242} onClose={(focusBack) => closed.push(focusBack)} />
      </WhenThereIsASession>,
    );
    await waitFor(() => {
      expect(closed).toEqual([false]);
    });
    expect(view.container.innerHTML).toBe("");
  });

  test("asks nothing when the node is there, until the person closes it", async () => {
    const closed: boolean[] = [];
    await rig(
      <WhenThereIsASession>
        <ParamPanel node={filter.id} onClose={(focusBack) => closed.push(focusBack)} />
      </WhenThereIsASession>,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Close the parameters of Filter" }));
    expect(closed).toEqual([true]);
  });
});

describe("pinning parameters to a node from its panel", () => {
  const pinOf = (panel: HTMLElement, name: string) => within(panel).getByRole<HTMLButtonElement>("button", { name: `Show ${name} on the node` });
  const resetButton = () => screen.queryByRole("button", { name: "Show the first eight parameters of Filter again" });

  test("puts a knob on the node for a parameter it does not show, keeping the first eight, and takes it off again when the pin is pressed again", async () => {
    await rig(<PatchView />, { snapshot: withManyParams() });
    fireEvent.click(await buttonOf("Filter"));
    const panel = await panelOf("Filter");
    const node = await nodeOf(filter.id);
    expect(within(node).queryByRole("slider", { name: "Param 50" })).toBeNull();
    fireEvent.change(await searchOf("Filter"), { target: { value: "Param 50" } });
    fireEvent.click(pinOf(panel, "Param 50"));
    await waitFor(() => {
      expect(within(node).getByRole("slider", { name: "Param 50" })).toBeTruthy();
    });
    expect(within(node).getByRole("slider", { name: "Param 0" })).toBeTruthy();
    expect(within(node).getByRole("slider", { name: "Param 7" })).toBeTruthy();
    expect(pinOf(panel, "Param 50").getAttribute("aria-pressed")).toBe("true");
    fireEvent.click(pinOf(panel, "Param 50"));
    await waitFor(() => {
      expect(within(node).queryByRole("slider", { name: "Param 50" })).toBeNull();
    });
    expect(within(node).getByRole("slider", { name: "Param 0" })).toBeTruthy();
  });

  test("takes a knob off the node that is among the first eight, and leaves the other seven", async () => {
    await rig(<PatchView />, { snapshot: withManyParams() });
    fireEvent.click(await buttonOf("Filter"));
    const panel = await panelOf("Filter");
    const node = await nodeOf(filter.id);
    expect(within(node).getByRole("slider", { name: "Param 3" })).toBeTruthy();
    fireEvent.change(await searchOf("Filter"), { target: { value: "Param 3" } });
    fireEvent.click(pinOf(panel, "Param 3"));
    await waitFor(() => {
      expect(within(node).queryByRole("slider", { name: "Param 3" })).toBeNull();
    });
    for (const kept of ["Param 0", "Param 2", "Param 4", "Param 7"]) expect(within(node).getByRole("slider", { name: kept })).toBeTruthy();
  });

  test("has a button that gives the node back to the first eight, while something is pinned and not before", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: withManyParams() });
    fireEvent.click(await buttonOf("Filter"));
    const panel = await panelOf("Filter");
    const node = await nodeOf(filter.id);
    expect(resetButton()).toBeNull();
    fireEvent.change(await searchOf("Filter"), { target: { value: "Param 50" } });
    fireEvent.click(pinOf(panel, "Param 50"));
    await waitFor(() => {
      expect(resetButton()).toBeTruthy();
    });
    const reset = resetButton();
    if (!reset) throw new Error("no button to give the node back to the first eight");
    fireEvent.click(reset);
    await waitFor(() => {
      expect(resetButton()).toBeNull();
    });
    expect(within(node).queryByRole("slider", { name: "Param 50" })).toBeNull();
    expect(within(node).getByRole("slider", { name: "Param 0" })).toBeTruthy();
    expect(requests().map((r) => r.intent)).toEqual([
      { type: "setPinned", node: filter.id, params: [0, 1, 2, 3, 4, 5, 6, 7, 50] },
      { type: "setPinned", node: filter.id, params: [] },
    ]);
  });

  test("shows a pin that somebody else makes, in the panel and on the node", async () => {
    const { server } = await rig(<PatchView />, { snapshot: withManyParams() });
    fireEvent.click(await buttonOf("Filter"));
    const panel = await panelOf("Filter");
    const node = await nodeOf(filter.id);
    act(() => {
      server.apply({ type: "setPinned", node: filter.id, params: [90, 91] });
    });
    await waitFor(() => {
      expect(within(node).getByRole("slider", { name: "Param 90" })).toBeTruthy();
    });
    // (Not the first eight any more, but for Param 0, which the LFO's cable drives: a cable's target is shown while the cable is there.)
    expect(within(node).queryByRole("slider", { name: "Param 3" })).toBeNull();
    expect(within(node).getByRole("slider", { name: "Param 0" })).toBeTruthy();
    expect(resetButton()).toBeTruthy();
    fireEvent.change(await searchOf("Filter"), { target: { value: "Param 91" } });
    expect(pinOf(panel, "Param 91").getAttribute("aria-pressed")).toBe("true");
  });
});
