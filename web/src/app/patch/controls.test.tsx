// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { Profiler } from "react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { Modulation } from "../../client/modulation";
import type { Param, Snapshot } from "../../protocol";
import { angleOf, arcPath, pointAt } from "../../ui/knob-geometry";
import { rig } from "../testing";
import { GainControl, ParamControl, sameModulations, sameParamFacts } from "./controls";

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

const slider = (name: string) => screen.getByRole("slider", { name });

/** Let what the mock engine has been told reach it: it takes a moment (a microtask) to send and to answer. */
const settle = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

/** The dot of the knob that is in `root`, as it is drawn now. */
function dotOf(root: HTMLElement): { x: number; y: number; visible: boolean } {
  const dot = root.querySelector(".tang-knob__dot");
  return { x: Number(dot?.getAttribute("cx")), y: Number(dot?.getAttribute("cy")), visible: dot?.getAttribute("visibility") === "visible" };
}

/** A drag of a knob up by `pixels`, which is that many 200ths of its range. */
function drag(dial: HTMLElement, pixels: number, from = 300): void {
  fireEvent.pointerDown(dial, { clientY: from, button: 0, pointerId: 1 });
  fireEvent.pointerMove(dial, { clientY: from - pixels, pointerId: 1 });
}

describe("a parameter of a node", () => {
  test("is a knob with the parameter's name, range and value", async () => {
    await rig(<ParamControl node={filter.id} index={0} />);
    const cutoff = slider("cutoff");
    expect(cutoff.getAttribute("aria-valuemin")).toBe("0");
    expect(cutoff.getAttribute("aria-valuemax")).toBe("1");
    expect(cutoff.getAttribute("aria-valuenow")).toBe("0.5");
    expect(cutoff.getAttribute("aria-valuetext")).toBe("0.5"); // a parameter with no unit is written with none
  });

  test("says its value with its unit when it has one", async () => {
    await rig(<ParamControl node={oscillator.id} index={1} />);
    expect(slider("detune").getAttribute("aria-valuetext")).toBe("0 st");
    expect(slider("detune").getAttribute("aria-valuemin")).toBe("-24");
  });

  test("is a choice when its values have names, with the chosen one selected", async () => {
    await rig(<ParamControl node={oscillator.id} index={0} />);
    const select = screen.getByRole<HTMLSelectElement>("combobox", { name: "waveform" });
    expect(Array.from(select.options).map((option) => option.textContent)).toEqual(["sine", "triangle", "square", "saw"]);
    expect(select.value).toBe("3");
    expect(screen.queryByRole("slider")).toBeNull();
  });

  test("shows nothing for a parameter that is not there, or a node that is not", async () => {
    const { view } = await rig(
      <div data-testid="where">
        <ParamControl node={filter.id} index={99} />
        <ParamControl node={4242} index={0} />
      </div>,
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });

  test("follows a change somebody else makes", async () => {
    const { server } = await rig(<ParamControl node={filter.id} index={0} />);
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.8 });
    });
    await waitFor(() => {
      expect(slider("cutoff").getAttribute("aria-valuenow")).toBe("0.8");
    });
  });

  test("shows what it is dragged to at once, and sends it on the frame, once", async () => {
    const { clock, requests } = await rig(<ParamControl node={filter.id} index={0} />);
    drag(slider("cutoff"), 50); // a quarter of the range
    expect(slider("cutoff").getAttribute("aria-valuenow")).toBe("0.75");
    await settle();
    expect(requests()).toEqual([]); // not before the frame
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 0, value: 0.75 } }]);
    });
  });

  test("goes on showing what it is dragged to while the engine answers, and then what the engine has", async () => {
    const { clock, client, requests } = await rig(<ParamControl node={filter.id} index={0} />);
    const dial = slider("cutoff");
    fireEvent.pointerDown(dial, { clientY: 300, button: 0, pointerId: 1 });
    for (let step = 1; step <= 4; step += 1) {
      fireEvent.pointerMove(dial, { clientY: 300 - step * 10, pointerId: 1 });
      act(() => {
        clock.tick(step * 16);
      });
    }
    expect(Number(dial.getAttribute("aria-valuenow"))).toBeCloseTo(0.7, 6);
    fireEvent.pointerUp(dial, { pointerId: 1 });
    await waitFor(() => {
      const node = client.state.snapshot?.graph.nodes.find((candidate) => candidate.id === filter.id);
      expect(node?.kind === "effect" && node.plugin.params[0]?.value).toBeCloseTo(0.7, 6);
    });
    expect(requests()).toHaveLength(4);
    await waitFor(() => {
      expect(Number(slider("cutoff").getAttribute("aria-valuenow"))).toBeCloseTo(0.7, 6);
    });
  });

  test("does not let somebody else's change pull it away from the pointer while it is dragged", async () => {
    const { server, clock, client } = await rig(<ParamControl node={filter.id} index={0} />);
    const dial = slider("cutoff");
    drag(dial, 20);
    act(() => {
      clock.tick(16);
    });
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.05 });
    });
    // The engine has the other value now (or will in a moment), but the knob is under the pointer.
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBeGreaterThanOrEqual(2);
    });
    expect(Number(dial.getAttribute("aria-valuenow"))).toBeCloseTo(0.6, 6);
    fireEvent.pointerUp(dial, { pointerId: 1 });
    await waitFor(() => {
      // Its own request was answered after the other change: that is the engine's last word.
      expect(Number(slider("cutoff").getAttribute("aria-valuenow"))).toBeCloseTo(0.6, 6);
    });
  });

  test("sends a key press at once", async () => {
    const { requests } = await rig(<ParamControl node={filter.id} index={0} />);
    fireEvent.keyDown(slider("cutoff"), { key: "ArrowUp" });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 0, value: 0.51 } }]);
    });
  });

  test("goes back to its default on a double click, at once", async () => {
    const { server, requests } = await rig(<ParamControl node={filter.id} index={1} />);
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 1, value: 5 });
    });
    await waitFor(() => {
      expect(slider("resonance").getAttribute("aria-valuenow")).toBe("5");
    });
    fireEvent.doubleClick(slider("resonance"));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: filter.id, param: 1, value: 0.707 } }]);
    });
  });

  test("sends the choice that is made at once, as the index", async () => {
    const { requests } = await rig(<ParamControl node={oscillator.id} index={0} />);
    fireEvent.change(screen.getByRole("combobox"), { target: { value: "1" } });
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setParam", node: oscillator.id, param: 0, value: 1 } }]);
    });
    await waitFor(() => {
      expect(screen.getByRole<HTMLSelectElement>("combobox").value).toBe("1");
    });
  });
});

describe("the modulation of a parameter", () => {
  test("is a ring around the knob from where the cables that end at it can take it", async () => {
    const { view } = await rig(<ParamControl node={filter.id} index={0} />);
    // The LFO's depth is 0.4 of the range of 0 to 1, around the 0.5 the user set.
    expect(view.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.1, 0.9));
  });

  test("is drawn over the parameter's own range", async () => {
    // The LFO drives the resonance as well, which goes from 0.5 to 10 and is set to 2.
    const both: Snapshot = structuredClone(tracerSnapshot);
    both.graph.edges.push({
      kind: "modulation",
      id: 9,
      amount: 0.1,
      from: { node: nodeNamed(tracerSnapshot, "LFO").id, port: { type: "modOut" } },
      to: { node: filter.id, port: { type: "param", index: 1 } },
    });
    const { view } = await rig(<ParamControl node={filter.id} index={1} />, { snapshot: both });
    // 0.1 of the range of 9.5 on both sides of 2: from 1.05 to 2.95.
    expect(view.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.55 / 9.5, 2.45 / 9.5));
  });

  test("moves with the value the user sets", async () => {
    const { view } = await rig(<ParamControl node={filter.id} index={0} />);
    drag(slider("cutoff"), 50); // to 0.75: the ring is 0.35 up to the top
    expect(view.container.querySelector(".tang-knob__ring")?.getAttribute("d")).toBe(arcPath(24, 21.5, 0.35, 1));
  });

  test("is not there for a parameter that nothing drives, or one that has named values", async () => {
    const { view } = await rig(
      <>
        <ParamControl node={filter.id} index={1} />
        <ParamControl node={oscillator.id} index={0} />
      </>,
    );
    expect(view.container.querySelector(".tang-knob__ring")).toBeNull();
  });

  test("is the dot where the engine says the parameter is, taken from the telemetry on the frame", async () => {
    const { view, client, clock, time } = await rig(<ParamControl node={filter.id} index={0} />);
    time.seconds = 0.5; // the LFO is at its peak: the cutoff is 0.5 + 0.4
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      expect(dotOf(view.container).visible).toBe(true);
      expect(dotOf(view.container).x).toBeCloseTo(pointAt(24, 18, angleOf(0.9)).x, 3);
    });
    const dot = dotOf(view.container);
    const peak = pointAt(24, 18, angleOf(0.9));
    expect([dot.x, dot.y]).toEqual([expect.closeTo(peak.x, 3), expect.closeTo(peak.y, 3)]);
    // And where it goes next.
    time.seconds = 1.5; // a quarter of a period on: sine(0.75 of a cycle) is the other end
    await waitFor(() => {
      act(() => {
        clock.tick(performance.now());
      });
      expect(dotOf(view.container).x).toBeCloseTo(pointAt(24, 18, angleOf(0.1)).x, 3);
    });
  });

  test("has no dot for a parameter that nothing drives", async () => {
    const { view, client, clock } = await rig(<ParamControl node={filter.id} index={1} />);
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    act(() => {
      clock.tick(100);
    });
    expect(dotOf(view.container).visible).toBe(false);
  });

  test("does not render the control again for the telemetry, however many frames come", async () => {
    let renders = 0;
    const { client, clock, time } = await rig(
      <Profiler id="cutoff" onRender={() => (renders += 1)}>
        <ParamControl node={filter.id} index={0} />
      </Profiler>,
    );
    await waitFor(() => {
      expect(client.telemetry.has("modulation")).toBe(true);
    });
    const settled = renders;
    for (let frame = 1; frame <= 60; frame += 1) {
      time.seconds = frame / 40;
      await new Promise((resolve) => setTimeout(resolve, 6)); // the engine makes a frame every 5 ms
      act(() => {
        clock.tick(frame * 16);
      });
    }
    expect(client.telemetry.frames).toBeGreaterThan(20);
    expect(renders).toBe(settled);
  });

  test("does not render the control again when another parameter of the plugin changes", async () => {
    let renders = 0;
    const { server, client } = await rig(
      <Profiler id="cutoff" onRender={() => (renders += 1)}>
        <ParamControl node={filter.id} index={0} />
      </Profiler>,
    );
    const settled = renders;
    const rev = client.state.snapshot?.rev ?? 0;
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 1, value: 4 });
      server.apply({ type: "setParam", node: filter.id, param: 3, value: 2 });
    });
    await waitFor(() => {
      expect(client.state.snapshot?.rev).toBe(rev + 2);
    });
    expect(renders).toBe(settled);
    act(() => {
      server.apply({ type: "setParam", node: filter.id, param: 0, value: 0.2 });
    });
    await waitFor(() => {
      expect(renders).toBeGreaterThan(settled);
    });
  });
});

describe("a host-side gain of a node", () => {
  test("is a knob called Volume for an instrument, over 0 to 4, at the value it has", async () => {
    await rig(<GainControl node={oscillator.id} gain="volume" />);
    const volume = slider("Volume");
    expect(volume.getAttribute("aria-valuemin")).toBe("0");
    expect(volume.getAttribute("aria-valuemax")).toBe("4");
    expect(Number(volume.getAttribute("aria-valuenow"))).toBeCloseTo(0.1, 6);
  });

  test("is a knob called Mix for an effect, over 0 to 1", async () => {
    await rig(<GainControl node={filter.id} gain="mix" />);
    expect(slider("Mix").getAttribute("aria-valuemax")).toBe("1");
    expect(slider("Mix").getAttribute("aria-valuenow")).toBe("1");
  });

  test("is sent as a gain, on the frame, and shown meanwhile", async () => {
    const { clock, requests } = await rig(<GainControl node={oscillator.id} gain="volume" />);
    drag(slider("Volume"), 50); // a quarter of 0 to 4, from 0.1
    expect(Number(slider("Volume").getAttribute("aria-valuenow"))).toBeCloseTo(1.1, 6);
    await settle();
    expect(requests()).toEqual([]);
    act(() => {
      clock.tick(16);
    });
    await waitFor(() => {
      expect(requests()).toHaveLength(1);
    });
    expect(requests()[0]?.intent).toMatchObject({ type: "setGain", node: oscillator.id, gain: "volume" });
  });

  test("goes back to unity on a double click (volume) and to wet (mix)", async () => {
    const { requests } = await rig(
      <>
        <GainControl node={oscillator.id} gain="volume" />
        <GainControl node={filter.id} gain="mix" />
      </>,
    );
    // The mix is at its default already, so a double click changes nothing; the volume is not.
    fireEvent.doubleClick(slider("Mix"));
    fireEvent.doubleClick(slider("Volume"));
    await waitFor(() => {
      expect(requests()).toEqual([{ intent: { type: "setGain", node: oscillator.id, gain: "volume", value: 1 } }]);
    });
  });

  test("shows a group's volume and pan, over their ranges, and pan goes back to the middle", async () => {
    const withGroup: Snapshot = structuredClone(tracerSnapshot);
    withGroup.graph.nodes.push({ id: 77, parent: null, name: "Group 1", kind: "group", volume: 0.8, pan: -0.5 });
    const { requests } = await rig(
      <div data-testid="group">
        <GainControl node={77} gain="groupVolume" />
        <GainControl node={77} gain="groupPan" />
      </div>,
      { snapshot: withGroup },
    );
    const group = screen.getByTestId("group");
    const volume = within(group).getByRole("slider", { name: "Volume" });
    const pan = within(group).getByRole("slider", { name: "Pan" });
    expect([volume.getAttribute("aria-valuemin"), volume.getAttribute("aria-valuemax"), volume.getAttribute("aria-valuenow")]).toEqual(["0", "4", "0.8"]);
    expect([pan.getAttribute("aria-valuemin"), pan.getAttribute("aria-valuemax"), pan.getAttribute("aria-valuenow")]).toEqual(["-1", "1", "-0.5"]);
    fireEvent.doubleClick(pan);
    fireEvent.doubleClick(volume);
    await waitFor(() => {
      expect(requests()).toEqual([
        { intent: { type: "setGain", node: 77, gain: "groupPan", value: 0 } },
        { intent: { type: "setGain", node: 77, gain: "groupVolume", value: 1 } },
      ]);
    });
  });

  test("shows nothing for a gain the node does not have", async () => {
    const { view } = await rig(
      <div data-testid="where">
        <GainControl node={oscillator.id} gain="mix" />
        <GainControl node={filter.id} gain="volume" />
        <GainControl node={4242} gain="volume" />
      </div>,
    );
    expect(view.getByTestId("where").innerHTML).toBe("");
  });
});

describe("whether two parameters are drawn the same", () => {
  const cutoff = (): Param => {
    const found = tracerSnapshot.graph.nodes.flatMap((node) => (node.kind === "effect" ? node.plugin.params : [])).find((param) => param.index === 0);
    if (!found) throw new Error("no cutoff");
    return found;
  };

  test("they are when only the value differs, or they are the same", () => {
    const base = cutoff();
    expect(sameParamFacts(base, base)).toBe(true);
    expect(sameParamFacts(base, { ...base, value: 0.9 })).toBe(true);
    expect(sameParamFacts(undefined, undefined)).toBe(true);
  });

  test("they are not when one is there and the other is not", () => {
    expect(sameParamFacts(cutoff(), undefined)).toBe(false);
    expect(sameParamFacts(undefined, cutoff())).toBe(false);
  });

  test("they are not when what is drawn differs: the index, name, range, default, unit or the names of the values", () => {
    const base = cutoff();
    const differences: Partial<Param>[] = [
      { index: 9 },
      { name: "other" },
      { min: -1 },
      { max: 2 },
      { default: 0.25 },
      { unit: "Hz" },
      { labels: ["a", "b"] },
    ];
    for (const difference of differences) {
      expect(sameParamFacts(base, { ...base, ...difference }), JSON.stringify(difference)).toBe(false);
      expect(sameParamFacts({ ...base, ...difference }, base), JSON.stringify(difference)).toBe(false);
    }
  });

  test("the names of the values are the same when they are the very same list", () => {
    const labels = ["a", "b"];
    const base = { ...cutoff(), labels };
    expect(sameParamFacts(base, { ...base, value: 1 })).toBe(true);
    expect(sameParamFacts(base, { ...base, labels: ["a", "b"] })).toBe(false); // another list, though equal: the plugin was loaded again
  });
});

describe("whether two sets of modulators are the same", () => {
  const lfo = { type: "lfo", waveform: "sine", rate: 1 } as const;
  const envelope = { type: "envelope", attack: 0.1, decay: 0.1, sustain: 0.5, release: 0.1 } as const;
  const one: Modulation = { source: lfo, amount: 0.4 };

  test("they are when they are the same modulators at the same depths, in the same order", () => {
    expect(sameModulations([], [])).toBe(true);
    expect(sameModulations([one], [one])).toBe(true);
    expect(sameModulations([one], [{ source: lfo, amount: 0.4 }])).toBe(true);
    const two: Modulation = { source: envelope, amount: -0.2 };
    expect(sameModulations([one, two], [one, two])).toBe(true);
  });

  test("they are not when there are more or fewer", () => {
    expect(sameModulations([one], [])).toBe(false);
    expect(sameModulations([], [one])).toBe(false);
    expect(sameModulations([one], [one, one])).toBe(false);
  });

  test("they are not when a depth differs, or a source is another one", () => {
    expect(sameModulations([one], [{ source: lfo, amount: 0.5 }])).toBe(false);
    expect(sameModulations([one], [{ source: { ...lfo }, amount: 0.4 }])).toBe(false); // another modulator, though alike
    expect(sameModulations([one], [{ source: envelope, amount: 0.4 }])).toBe(false);
  });

  test("they are not when the order differs", () => {
    const two: Modulation = { source: envelope, amount: -0.2 };
    expect(sameModulations([one, two], [two, one])).toBe(false);
  });
});
