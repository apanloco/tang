import type { Meta, StoryObj } from "@storybook/react-vite";
import { useEffect, useMemo } from "react";

import { Client } from "../../client/connection";
import { tracerSnapshot } from "../../client/fixtures";
import { MockServer } from "../../client/mock";
import type { PianoMeta } from "../../protocol";
import { ClientProvider } from "../ClientProvider";
import { Piano } from "./Piano";

/** The piano over a mock engine whose session has this scale; `chord` is held by another client, so that the keys that are held are lit. */
function OverMock({ piano, chord, octave = 4, width }: { piano?: PianoMeta; chord?: number[]; octave?: number; width?: number }) {
  const server = useMemo(
    () => new MockServer({ telemetryPeriod: 33, snapshot: { ...tracerSnapshot, meta: { ...tracerSnapshot.meta, piano: piano ?? tracerSnapshot.meta.piano } } }),
    [piano],
  );
  const client = useMemo(() => new Client({ transport: server.transport, telemetry: ["held"] }), [server]);
  useEffect(() => {
    // (Another front-end holds the notes: the page's own keys are played with the pointer, which a story is no place for.)
    const player = new Client({ transport: server.transport });
    player.start();
    const timer = setInterval(() => {
      if (player.state.status !== "open") return;
      clearInterval(timer);
      for (const note of chord ?? []) player.noteOn(note);
    }, 20);
    return () => {
      clearInterval(timer);
      player.stop();
    };
  }, [server, chord]);
  return (
    <ClientProvider client={client}>
      <div style={{ width: width ?? "100%" }}>
        <Piano octave={octave} />
      </div>
    </ClientProvider>
  );
}

const meta = {
  title: "App/Piano",
  component: OverMock,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof OverMock>;

export default meta;
type Story = StoryObj<typeof meta>;

const fSharpDorian: PianoMeta = { scale: "F# dorian", root: 6, kind: "dorian", notes: [1, 3, 4, 6, 8, 9, 11], locked: false };

/** C major, as a new session has it: the C of each octave is gold, the other notes of the scale teal. */
export const InCMajor: Story = {};

/** Another scale: the root moves, and so does what is teal. */
export const InFSharpDorian: Story = { args: { piano: fSharpDorian } };

/** Locked to the scale: the keys that are not in it are dimmed, for they are not played. */
export const Locked: Story = { args: { piano: { ...fSharpDorian, locked: true } } };

/** A chord of C, E and G held, and C sharp, which is not in C major: the notes of the scale sink, and the one that is not goes red. */
export const WithNotesHeld: Story = { args: { chord: [60, 64, 67, 61] } };

/** Narrower, there is room for fewer octaves; and they are the ones the keys of the computer play. */
export const Narrow: Story = { args: { width: 480 } };

/** The keys of the computer an octave up, which the window follows. */
export const OctaveUp: Story = { args: { octave: 6 } };
