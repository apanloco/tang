// @vitest-environment jsdom
import { act, cleanup, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { MASTER, nodeNamed, tracerSnapshot } from "../../client/fixtures";
import type { Intent } from "../../protocol";
import { NoticeBoard } from "../../ui/NoticeBoard";
import { AddFreeEffectMenu } from "../AddFreeEffect";
import { useNotices } from "../ClientProvider";
import { rig } from "../testing";
import { UndoRedo } from "../UndoRedo";
import { PatchView } from "./PatchView";

afterEach(cleanup);

// An effect that stands by itself (step 87): to the controller a lane of its own with no instrument, and to the page the effect alone. The lane has no card on the canvas; the effect is a module like any other,
// and has what the lane's card said of the lane as a whole: its mute, its solo and its way out, and the "+" that adds a modulator to it. What the page shows of it, what it asks and when.
// (The tracer has the nodes 0 to 2: a lane, its filter and its LFO; the first effect that is added is the lane 3 and the effect 4, and the next the lane 5 and the effect 6.)

const oscillator = nodeNamed(tracerSnapshot, "Oscillator");

const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);

/** What the page says when it could not do what was asked. */
function Board() {
  return <NoticeBoard notices={useNotices()} />;
}

/** The page, the bar's button that adds an effect, its Undo and Redo and the notices, over the tracer with an effect that stands by itself added to it (a reverb, in the lane 3). */
async function withReverb() {
  const made = await rig(
    <>
      <PatchView />
      <AddFreeEffectMenu />
      <UndoRedo />
      <Board />
    </>,
  );
  made.server.apply({ type: "addFreeEffect", plugin: "builtin:reverb" });
  await nodeOf(4);
  return made;
}

/** The intents that the page asked of the server, after the ones the test made on its own side. */
const asked = (requests: () => { intent: unknown }[]): Intent[] => requests().map((request) => request.intent as Intent);

describe("an effect that stands by itself", () => {
  test("is a module on the canvas, and the lane that holds it is not: it has no card", async () => {
    await withReverb();
    expect(await nodeOf(4)).toBeTruthy();
    expect(screen.queryByTestId("rf__node-3")).toBeNull();
    expect(screen.queryByRole("button", { name: "Choose an instrument for (empty)" })).toBeNull();
    // (The session's own lane has its card still, and the module is one of the nodes of the canvas, after the tracer's lane: the modulators come after the lanes.)
    expect(await nodeOf(oscillator.id)).toBeTruthy();
    expect(Array.from(document.querySelectorAll(".tang-node__name")).map((name) => name.textContent)).toEqual(["Oscillator", "Filter", "Reverb", "LFO", "Master"]);
  });

  test("has the mute, the solo and the \"+\" of its lane, which the lane's card would have, and no cable at all to begin with: nothing feeds it from the lane, which has no sound, and it sends nowhere", async () => {
    await withReverb();
    const reverb = await nodeOf(4);
    expect(within(reverb).getByRole("button", { name: "Mute Reverb" })).toBeTruthy();
    expect(within(reverb).getByRole("button", { name: "Solo Reverb" })).toBeTruthy();
    expect(within(reverb).getByRole("button", { name: "Add a modulator to Reverb" })).toBeTruthy();
    // (No button says where its audio goes or where it is fed from: that is its cables, and a module that is made by itself has none.)
    expect(screen.queryAllByRole("button", { name: /^Remove cable Reverb/ })).toHaveLength(0);
    expect(screen.queryAllByRole("button", { name: /→ Reverb$/ })).toHaveLength(0);
    expect(screen.queryAllByRole("button", { name: /^Cut the (way out|input) of / })).toHaveLength(0);
    // The effects of the lane that has an instrument have none of the lane's cells, and the lane has its own.
    const filter = await nodeOf(1);
    expect(within(filter).queryByRole("button", { name: "Mute Filter" })).toBeNull();
    expect(within(await nodeOf(oscillator.id)).getByRole("button", { name: "Mute Oscillator" })).toBeTruthy();
  });

  test("is muted and soloed by the buttons of its module, which ask for them for its lane and show what the server has", async () => {
    const { requests } = await withReverb();
    const reverb = await nodeOf(4);
    fireEvent.click(within(reverb).getByRole("button", { name: "Mute Reverb" }));
    await waitFor(() => {
      expect(within(reverb).getByRole("button", { name: "Mute Reverb" }).getAttribute("aria-pressed")).toBe("true");
    });
    fireEvent.click(within(reverb).getByRole("button", { name: "Solo Reverb" }));
    await waitFor(() => {
      expect(within(reverb).getByRole("button", { name: "Solo Reverb" }).getAttribute("aria-pressed")).toBe("true");
    });
    expect(asked(requests)).toEqual([
      { type: "setMute", node: 3, muted: true },
      { type: "setSolo", node: 3, solo: true },
    ]);
  });

  test("sends nowhere until a cable is drawn from it, and a cable from it to the master is the way out of its lane given back: the menu that sends its audio offers the master, and what is drawn is a link, with no level", async () => {
    const { requests } = await withReverb();
    const reverb = await nodeOf(4);
    // (A module that is made by itself has its way out cut, and so no link out of it.)
    expect(screen.queryByRole("button", { name: "Remove cable Reverb → Master" })).toBeNull();
    fireEvent.click(within(reverb).getByRole("button", { name: "Send audio from Reverb" }));
    fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: "Master" }));
    expect(await screen.findByRole("button", { name: "Remove cable Reverb → Master" })).toBeTruthy();
    expect(asked(requests)).toEqual([{ type: "addCable", from: { node: 4, port: { type: "audioOut" } }, to: { node: MASTER, port: { type: "audioIn" } }, gain: null }]);
    // (The server made it the link that was cut, which has no level: no cable of a person's, which has a slider.)
    expect(document.querySelectorAll(".tang-cable--send")).toHaveLength(0);
    expect(screen.queryByRole("slider", { name: /^Level of / })).toBeNull();
  });

  test("is dimmed, as the effects of a lane are, while its lane cannot be heard: muted, or not soloed while something is", async () => {
    await withReverb();
    const reverb = await nodeOf(4);
    expect(reverb.querySelector(".tang-node--unheard")).toBeNull();
    fireEvent.click(within(reverb).getByRole("button", { name: "Mute Reverb" }));
    await waitFor(() => {
      expect(reverb.querySelector(".tang-node--unheard")).not.toBeNull();
    });
    // (The effect of the session's own lane is heard, and so is the lane.)
    expect((await nodeOf(1)).querySelector(".tang-node--unheard")).toBeNull();
  });

  test("is taken out by the module's \"×\" with its lane, which is no more than the effect has (no modulator in it): one request, for the lane, and the focus goes to the bar's button that adds one", async () => {
    const { requests } = await withReverb();
    const reverb = await nodeOf(4);
    fireEvent.click(within(reverb).getByRole("button", { name: "Remove Reverb" }));
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-4")).toBeNull();
    });
    expect(asked(requests)).toEqual([{ type: "removeNode", node: 3 }]);
    expect(screen.queryByTestId("rf__node-3")).toBeNull();
    await waitFor(() => {
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an effect" }));
    });
  });

  test("has Undo and Redo in the words of the button that adds an effect, which put the lane and the effect back and take them out again", async () => {
    await withReverb();
    fireEvent.click(await screen.findByRole("button", { name: "Undo add Reverb" }));
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-4")).toBeNull();
    });
    expect(screen.queryByTestId("rf__node-3")).toBeNull();
    fireEvent.click(await screen.findByRole("button", { name: "Redo add Reverb" }));
    expect(await nodeOf(4)).toBeTruthy();
    expect(screen.queryByTestId("rf__node-3")).toBeNull();
  });

  test("is the first of a chain of effects when there are more, which has the lane's cells and the lane's \"+\"; the others have none, and a \"×\" that takes out the effect alone", async () => {
    const { server, requests } = await withReverb();
    server.apply({ type: "addEffect", parent: 3, plugin: "builtin:filter" });
    const second = await nodeOf(5);
    const first = await nodeOf(4);
    expect(within(first).getByRole("button", { name: "Mute Reverb" })).toBeTruthy();
    expect(within(first).getByRole("button", { name: "Add a modulator to Reverb" })).toBeTruthy();
    expect(within(second).queryByRole("button", { name: /^Mute / })).toBeNull();
    expect(within(second).queryByRole("button", { name: /^Add a modulator / })).toBeNull();
    // The link between them is a cable like any other, and the input of the second, as in any chain; the first has none into it.
    expect(screen.getByRole("button", { name: "Remove cable Reverb → Filter 2" })).toBeTruthy();
    expect(screen.queryAllByRole("button", { name: /^Remove cable .* → Reverb$/ })).toHaveLength(0);
    // Either's "×" takes out the effect and no more: the lane is the chain's.
    fireEvent.click(within(second).getByRole("button", { name: /^Remove Filter/ }));
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-5")).toBeNull();
    });
    expect(asked(requests).slice(-1)).toEqual([{ type: "removeNode", node: 5 }]);
    expect(await nodeOf(4)).toBeTruthy();
  });

  test("is the lane's to take out when a modulator hears the lane: the module's \"×\" takes the lane with the effect, and the modulator stays, hearing every note", async () => {
    const { server, requests } = await withReverb();
    server.apply({ type: "addFreeModulator", kind: "lfo", hears: 3 });
    const reverb = await nodeOf(4);
    // (The LFO is a module of its own, above the lane's strip.)
    expect(await nodeOf(5)).toBeTruthy();
    fireEvent.click(within(reverb).getByRole("button", { name: "Remove Reverb" }));
    await waitFor(() => {
      expect(screen.queryByTestId("rf__node-4")).toBeNull();
    });
    expect(asked(requests).slice(-1)).toEqual([{ type: "removeNode", node: 3 }]);
    // The lane went with the effect, and the LFO is still there, with nothing to hear but every note.
    expect(screen.queryByTestId("rf__node-3")).toBeNull();
    expect(await nodeOf(5)).toBeTruthy();
    const left = server.snapshot.graph.nodes.find((n) => n.id === 5);
    expect(left?.kind).toBe("modulator");
    expect(left).not.toHaveProperty("hears");
  });

  test("has a \"+\" that adds a modulator that hears its lane, and the modulator is on the canvas, over the effect that it hears", async () => {
    const { requests } = await withReverb();
    const reverb = await nodeOf(4);
    fireEvent.click(within(reverb).getByRole("button", { name: "Add a modulator to Reverb" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "LFO" }));
    await waitFor(() => {
      expect(asked(requests).slice(-1)).toEqual([{ type: "addFreeModulator", kind: "lfo", hears: 3 }]);
    });
    const lfo = await nodeOf(5);
    expect(within(lfo).getAllByText("LFO 2").length).toBeGreaterThan(0);
    // (Above the effect, as the modulators of a lane are above its row: a modulator is laid out above the strip of the lane that it hears.)
    const top = (found: HTMLElement): number => Number(/translate\([^,]+,\s*(-?[\d.]+)px\)/.exec(found.style.transform)?.[1]);
    expect(top(lfo)).toBeLessThan(top(reverb));
  });

  test("does not take the card of a lane that has an instrument: every lane the session had has its own, with the cells it had", async () => {
    await withReverb();
    const lane = await nodeOf(oscillator.id);
    for (const name of ["Change the instrument of Oscillator", "Add an effect to Oscillator", "Add a modulator to Oscillator", "Mute Oscillator", "Solo Oscillator"]) {
      expect(within(lane).getByRole("button", { name }), name).toBeTruthy();
    }
  });

  test("says why it could not be taken out, among the notices, and the module is where it was", async () => {
    const { server, client } = await withReverb();
    server.drop();
    await waitFor(() => {
      expect(client.state.status).not.toBe("open");
    });
    fireEvent.click(within(await nodeOf(4)).getByRole("button", { name: "Remove Reverb" }));
    expect(await screen.findByText("Could not remove Reverb: Not connected to the server.")).toBeTruthy();
    expect(await nodeOf(4)).toBeTruthy();
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 30));
    });
    expect(document.activeElement).not.toBe(screen.getByRole("button", { name: "Add an effect" }));
  });
});

describe("the button in the bar that adds an effect that stands by itself", () => {
  test("opens a menu of the effects that are installed, under the format of each, and not of the instruments", async () => {
    await rig(<AddFreeEffectMenu />);
    fireEvent.click(await screen.findByRole("button", { name: "Add an effect" }));
    const menu = screen.getByRole("menu", { name: "Add an effect" });
    const groups = within(menu)
      .getAllByRole("group")
      .map((group) => [group.getAttribute("aria-label"), within(group).getAllByRole("menuitem").map((item) => item.textContent)]);
    expect(groups.map(([format]) => format)).toEqual(["Built-in"]);
    expect(groups[0]?.[1]).toContain("Reverb");
    expect(groups[0]?.[1]).not.toContain("Oscillator");
  });

  test("asks for an effect of its own with the plugin that was chosen, and the focus is back on the button when the menu has gone", async () => {
    const { requests } = await rig(
      <>
        <PatchView />
        <AddFreeEffectMenu />
      </>,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Add an effect" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Reverb" }));
    await waitFor(() => {
      expect(asked(requests)).toEqual([{ type: "addFreeEffect", plugin: "builtin:reverb" }]);
    });
    expect(await nodeOf(4)).toBeTruthy();
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Add an effect" }));
  });

  test("says why an effect could not be added, among the notices", async () => {
    const { server, client } = await rig(
      <>
        <AddFreeEffectMenu />
        <Board />
      </>,
    );
    server.drop();
    await waitFor(() => {
      expect(client.state.status).not.toBe("open");
    });
    fireEvent.click(await screen.findByRole("button", { name: "Add an effect" }));
    fireEvent.click(screen.getByRole("menuitem", { name: "Reverb" }));
    expect(await screen.findByText("Could not add Reverb as an effect of its own: Not connected to the server.")).toBeTruthy();
  });
});
