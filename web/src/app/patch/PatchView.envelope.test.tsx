// @vitest-environment jsdom
// What an envelope's node shows of its settings: the shape they make, with a dot where the envelope is while notes are played.
import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, test } from "vitest";

import { envelopePath, placeIn, shapeOf } from "../../client/envelope";
import { reachFixture } from "../../client/fixtures";
import { ENVELOPE_HEIGHT, ENVELOPE_PAD, ENVELOPE_WIDTH } from "../../ui/LiveEnvelope";
import { rig } from "../testing";
import { PatchView } from "./PatchView";

afterEach(cleanup);

const BOX = { width: ENVELOPE_WIDTH, height: ENVELOPE_HEIGHT, pad: ENVELOPE_PAD };
const NEW = { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 };

// The reach fixture's modulators: LFOs (12, 15, 17), envelopes (13 and 16: "ADSR 1" and "ADSR 2") and a MIDI control (14).
const nodeOf = (id: number): Promise<HTMLElement> => screen.findByTestId(`rf__node-${String(id)}`);

describe("the shape of an envelope on its node", () => {
  test("is an image named for the envelope and its four settings, on the node of an envelope and on no other", async () => {
    await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    for (const [id, name] of [[13, "ADSR 1"], [16, "ADSR 2"]] as const) {
      const image = within(await nodeOf(id)).getByRole("img");
      expect(image.getAttribute("aria-label")).toBe(`${name} shape: attack 10 ms, decay 300 ms, sustain 0.7, release 500 ms`);
      expect(image.querySelector(".tang-envelope__line")?.getAttribute("d")).toBe(envelopePath(shapeOf(NEW), BOX));
    }
    // An LFO draws what it puts out, a MIDI control is bound to a control: neither has stages to draw.
    for (const id of [12, 14, 15, 17]) expect(within(await nodeOf(id)).queryByRole("img"), String(id)).toBeNull();
    // (A plugin's node has none either.)
    expect(within(await nodeOf(5)).queryByRole("img")).toBeNull();
  });

  test("follows a stage that is turned, the line and the words, and no other node's", async () => {
    const { server } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    const first = within(await nodeOf(13)).getByRole("img");
    const second = within(await nodeOf(16)).getByRole("img");
    act(() => {
      server.apply({ type: "setModulatorField", node: 13, field: "attack", value: 2 });
      server.apply({ type: "setModulatorField", node: 13, field: "release", value: 0.002 });
      server.apply({ type: "setModulatorField", node: 13, field: "sustain", value: 0.25 });
    });
    const stages = { attack: 2, decay: 0.3, sustain: 0.25, release: 0.002 };
    await waitFor(() => {
      expect(first.getAttribute("aria-label")).toBe("ADSR 1 shape: attack 2 s, decay 300 ms, sustain 0.25, release 2 ms");
    });
    expect(first.querySelector(".tang-envelope__line")?.getAttribute("d")).toBe(envelopePath(shapeOf(stages), BOX));
    expect(second.getAttribute("aria-label")).toBe("ADSR 2 shape: attack 10 ms, decay 300 ms, sustain 0.7, release 500 ms");
    expect(second.querySelector(".tang-envelope__line")?.getAttribute("d")).toBe(envelopePath(shapeOf(NEW), BOX));
  });

  test("has its dot at the start at rest, along the hold while a note is held (the mock's envelope puts out the sustain) and at the start when it is let go", async () => {
    const { client, clock } = await rig(<PatchView />, { snapshot: reachFixture.snapshot });
    const dot = (await nodeOf(13)).querySelector(".tang-envelope__dot");
    if (!dot) throw new Error("the envelope has no dot");
    const at = () => ({ x: Number(dot.getAttribute("cx")), y: Number(dot.getAttribute("cy")) });
    // (Frames are turned by hand, with real time between them for the mock's telemetry to come. Not inside `waitFor`: what a frame writes to
    // the DOM runs its callback again, and one whose assertion keeps failing never times out. The values are 32-bit floats: three digits.)
    const until = async (condition: () => boolean, what: string): Promise<void> => {
      const deadline = Date.now() + 2000;
      while (!condition()) {
        if (Date.now() > deadline) throw new Error(`gave up waiting for ${what}: the dot is at ${JSON.stringify(at())}`);
        await new Promise((resolve) => setTimeout(resolve, 10));
        act(() => {
          clock.tick(performance.now());
        });
      }
    };
    const near = (a: number, b: number): boolean => Math.abs(a - b) < 1e-3;
    const shape = shapeOf(NEW);
    const start = placeIn(BOX, 0, 0);
    await until(() => dot.getAttribute("visibility") === "visible" && near(at().x, start.x), "the dot at the start");
    expect(near(at().y, start.y)).toBe(true);

    client.noteOn(60);
    const middle = placeIn(BOX, (shape.decayEnd + shape.holdEnd) / 2, 0.7);
    await until(() => near(at().x, middle.x) && near(at().y, middle.y), "the dot along the hold");

    client.noteOff(60);
    await until(() => near(at().x, start.x) && near(at().y, start.y), "the dot back at the start");
  });
});
