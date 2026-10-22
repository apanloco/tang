// @vitest-environment jsdom
import { cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { nodeNamed, reachFixture, tracerSnapshot } from "../../client/fixtures";
import { displayNames } from "../../client/names";
import type { Snapshot } from "../../protocol";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");
const filter = nodeNamed(tracerSnapshot, "Filter");

const mute = (name = "Oscillator") => screen.findByRole("button", { name: `Mute ${name}` });
const solo = (name = "Oscillator") => screen.findByRole("button", { name: `Solo ${name}` });

/** The tracer with the oscillator's lane muted and/or soloed. */
function flagged(flags: { muted?: boolean; solo?: boolean }): Snapshot {
  const snapshot = structuredClone(tracerSnapshot);
  const node = snapshot.graph.nodes.find((n) => n.id === oscillator.id);
  if (node?.kind !== "instrument") throw new Error("no lane");
  Object.assign(node, flags);
  return snapshot;
}

describe("the mute and the solo of a lane or a group", () => {
  test("are two buttons in a cell of every lane and every group, an empty lane too, and on nothing else", async () => {
    await rig(<PatchView />);
    expect(await mute()).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /^Mute / })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: /^Solo / })).toHaveLength(1);
    cleanup();

    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    await screen.findAllByRole("button", { name: /^Mute / });
    const flagged = reachFixture.snapshot.graph.nodes.filter((node) => node.kind === "instrument" || node.kind === "group");
    expect(flagged.some((node) => node.kind === "instrument" && node.plugin === null)).toBe(true);
    expect(screen.getAllByRole("button", { name: /^Mute / })).toHaveLength(flagged.length);
    expect(screen.getAllByRole("button", { name: /^Solo / })).toHaveLength(flagged.length);
  });

  test("are in a cell called Mute / Solo that is the last of the lane's and of the group's controls", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    const buttons = await screen.findAllByRole("button", { name: /^Mute Group / });
    expect(buttons.length).toBeGreaterThan(0);
    for (const button of buttons) {
      const node = button.closest(".tang-node");
      expect(node?.classList.contains("tang-node--group")).toBe(true);
      const cells = Array.from(node?.querySelectorAll(".tang-node__controls > .tang-cell") ?? []);
      expect(cells).toHaveLength(3); // the volume, the pan and the mute and solo
      expect(cells.at(-1)?.textContent).toContain("Mute");
      expect(within(cells.at(-1) as HTMLElement).getAllByRole("button")).toHaveLength(3); // the mute, the solo and, under them, the way out
    }
    const lanes = await screen.findAllByRole("button", { name: /^Mute Oscillator/ });
    expect(lanes.length).toBeGreaterThan(1);
    for (const button of lanes) {
      const node = button.closest(".tang-node");
      expect(node?.classList.contains("tang-node--instrument")).toBe(true);
      const cells = Array.from(node?.querySelectorAll(".tang-node__controls > .tang-cell") ?? []);
      expect(cells.at(-1)?.textContent).toContain("Mute");
      // (The keys and the group come before it.)
      expect(cells.at(-3)?.textContent).toContain("Keys");
      expect(cells.at(-2)?.textContent).toContain("Group");
    }
  });

  test("are not on an effect, a modulator or the master", async () => {
    await rig(<PatchView />);
    const node = await screen.findByTestId(`rf__node-${String(filter.id)}`);
    expect(within(node).queryByRole("button", { name: /^(Mute|Solo) / })).toBeNull();
    expect(screen.getAllByRole("button", { name: /^Mute / })).toHaveLength(1);
  });

  test("say what the session says: pressed when the lane is muted, pressed when it is soloed", async () => {
    await rig(<PatchView />);
    expect((await mute()).getAttribute("aria-pressed")).toBe("false");
    expect((await solo()).getAttribute("aria-pressed")).toBe("false");
    cleanup();
    await rig(<PatchView />, { snapshot: flagged({ muted: true }) });
    expect((await mute()).getAttribute("aria-pressed")).toBe("true");
    expect((await solo()).getAttribute("aria-pressed")).toBe("false");
    cleanup();
    await rig(<PatchView />, { snapshot: flagged({ solo: true }) });
    expect((await mute()).getAttribute("aria-pressed")).toBe("false");
    expect((await solo()).getAttribute("aria-pressed")).toBe("true");
  });

  test("a press asks the server for the other way, and the button shows what the server has done: a mute, and an unmute", async () => {
    const { requests } = await rig(<PatchView />);
    fireEvent.click(await mute());
    await waitFor(() => { expect((screen.getByRole("button", { name: "Mute Oscillator" })).getAttribute("aria-pressed")).toBe("true"); });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setMute", node: oscillator.id, muted: true }]);
    fireEvent.click(screen.getByRole("button", { name: "Mute Oscillator" }));
    await waitFor(() => { expect((screen.getByRole("button", { name: "Mute Oscillator" })).getAttribute("aria-pressed")).toBe("false"); });
    expect(requests().map((request) => request.intent).at(-1)).toEqual({ type: "setMute", node: oscillator.id, muted: false });
  });

  test("a press of the solo asks for a solo, and the lane's mute is left as it was", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: flagged({ muted: true }) });
    fireEvent.click(await solo());
    await waitFor(() => { expect(screen.getByRole("button", { name: "Solo Oscillator" }).getAttribute("aria-pressed")).toBe("true"); });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setSolo", node: oscillator.id, solo: true }]);
    expect(screen.getByRole("button", { name: "Mute Oscillator" }).getAttribute("aria-pressed")).toBe("true");
  });

  test("a group's is the group's: its buttons are named for it and ask for it", async () => {
    const { requests } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    const group = reachFixture.snapshot.graph.nodes.find((node) => node.kind === "group");
    if (!group) throw new Error("the fixture has a group");
    fireEvent.click(await screen.findByRole("button", { name: `Mute ${group.name}` }));
    await waitFor(() => { expect(screen.getByRole("button", { name: `Mute ${group.name}` }).getAttribute("aria-pressed")).toBe("true"); });
    expect(requests().map((request) => request.intent)).toEqual([{ type: "setMute", node: group.id, muted: true }]);
  });

  test("have a title that says what a press does: it mutes or unmutes, it solos or unsolos", async () => {
    await rig(<PatchView />);
    expect((await mute()).title).toBe("Mute Oscillator");
    expect((await solo()).title).toBe("Solo Oscillator");
    cleanup();
    await rig(<PatchView />, { snapshot: flagged({ muted: true }) });
    expect((await mute()).title).toBe("Unmute Oscillator");
    expect((await solo()).title).toBe("Solo Oscillator");
    cleanup();
    await rig(<PatchView />, { snapshot: flagged({ solo: true }) });
    expect((await mute()).title).toBe("Mute Oscillator");
    expect((await solo()).title).toBe("Unsolo Oscillator");
  });
});

describe("a node that cannot be heard", () => {
  const node = async (name: string, index = 0) => (await screen.findAllByRole("button", { name: `Mute ${name}` }))[index]?.closest(".tang-node");

  test("is dimmed: a lane that is muted is, and one that is not is not", async () => {
    await rig(<PatchView />);
    expect((await node("Oscillator"))?.classList.contains("tang-node--unheard")).toBe(false);
    cleanup();
    await rig(<PatchView />, { snapshot: flagged({ muted: true }) });
    expect((await node("Oscillator"))?.classList.contains("tang-node--unheard")).toBe(true);
  });

  test("is the effects of the lane as well, and not its modulators", async () => {
    await rig(<PatchView />, { snapshot: flagged({ muted: true }) });
    await node("Oscillator");
    const dimmed = (id: number) => screen.getByTestId(`rf__node-${String(id)}`).querySelector(".tang-node")?.classList.contains("tang-node--unheard");
    expect(dimmed(filter.id)).toBe(true);
    expect(dimmed(nodeNamed(tracerSnapshot, "LFO").id)).toBe(false);
  });

  test("is every lane and group that is not soloed while something is", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    await screen.findAllByRole("button", { name: /^Solo / });
    const lanes = reachFixture.snapshot.graph.nodes.filter((n) => n.kind === "instrument");
    const target = lanes.find((lane) => lane.plugin !== null && lane.parent === null);
    if (!target) throw new Error("the fixture has a lane that is in no group");
    const name = displayNames(reachFixture.snapshot.graph).get(target.id) ?? "";
    expect(name).not.toBe("");
    fireEvent.click(screen.getByRole("button", { name: `Solo ${name}` }));
    await waitFor(() => { expect(document.querySelectorAll(".tang-node--unheard").length).toBeGreaterThan(0); });
    const heard = Array.from(document.querySelectorAll(".tang-node--instrument:not(.tang-node--unheard), .tang-node--group:not(.tang-node--unheard)"));
    expect(heard).toHaveLength(1);
    expect(heard[0]?.textContent).toContain(name);
  });

  test("is not dimmed when the group of a lane that is soloed carries it: the group is heard, and the other lane of the group is not", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    const first = reachFixture.snapshot.graph.nodes.find((n) => n.kind === "instrument" && n.parent !== null && n.plugin !== null);
    if (first?.kind !== "instrument" || first.parent === null) throw new Error("the fixture has a lane in a group");
    const group = reachFixture.snapshot.graph.nodes.find((n) => n.id === first.parent);
    if (!group) throw new Error("the lane's group");
    await screen.findAllByRole("button", { name: /^Solo / });
    const soloOf = (id: number): HTMLElement => {
      const button = document.querySelector<HTMLElement>(`[data-id="${String(id)}"] button[aria-label^="Solo "]`);
      if (!button) throw new Error(`node ${String(id)} has no solo button`);
      return button;
    };
    const unheard = (id: number) => document.querySelector(`[data-id="${String(id)}"] .tang-node`)?.classList.contains("tang-node--unheard");
    fireEvent.click(soloOf(first.id));
    // (The solo is on when the button says so; what the other nodes look like is the answer to it.)
    await waitFor(() => { expect(soloOf(first.id).getAttribute("aria-pressed")).toBe("true"); });
    expect(unheard(first.id)).toBe(false);
    expect(unheard(group.id)).toBe(false);
    const others = reachFixture.snapshot.graph.nodes.filter((n) => n.kind === "instrument" && n.parent === group.id && n.id !== first.id);
    expect(others.length).toBeGreaterThan(0);
    for (const other of others) expect(unheard(other.id), `lane ${String(other.id)}`).toBe(true);
  });
});
