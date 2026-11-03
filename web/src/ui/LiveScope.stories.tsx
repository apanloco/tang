import type { Meta, StoryObj } from "@storybook/react-vite";

import { SCOPE_FRAME_LENGTH } from "../protocol";
import { LiveScope } from "./LiveScope";

const SAMPLE_RATE = 48_000;

/** A frame of the scope as the server would send it for a sound made of `notes` (frequencies in Hz): a tone and its two harmonics each, the same on both sides, the newest sample last. */
function sound(notes: readonly number[], level = 0.35): Float32Array {
  const seconds = performance.now() / 1000;
  const values = new Float32Array(2 * SCOPE_FRAME_LENGTH);
  for (let frame = 0; frame < SCOPE_FRAME_LENGTH; frame += 1) {
    const time = seconds - (SCOPE_FRAME_LENGTH - 1 - frame) / SAMPLE_RATE;
    let sample = 0;
    for (const hertz of notes) {
      const turn = 2 * Math.PI * hertz * time;
      sample += Math.sin(turn) + 0.5 * Math.sin(2 * turn) + 0.33 * Math.sin(3 * turn);
    }
    values[frame] = (sample * level) / (1.83 * Math.max(1, notes.length));
    values[SCOPE_FRAME_LENGTH + frame] = values[frame] ?? 0;
  }
  return values;
}

/** The count of frames at thirty a second, as the server's pusher makes them. */
const thirtyAsecond = (): number => Math.floor(performance.now() / 33);

const meta = {
  title: "Displays/LiveScope",
  component: LiveScope,
  args: { label: "Output", view: "wave", samples: 1024, sampleRate: SAMPLE_RATE, read: (): Float32Array | undefined => sound([220]), readFrames: thirtyAsecond },
  decorators: [
    (Story) => (
      <div style={{ width: 300 }}>
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof LiveScope>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A note, as a wave that stands still. */
export const Wave: Story = {};

/** Shorter in time: a few turns of the note. */
export const WaveClose: Story = { args: { samples: 256, read: () => sound([440]) } };

/** The same note as its spectrum: the tone and its harmonics. */
export const Spectrum: Story = { args: { view: "spectrum" } };

/** A chord: the wave is a mixture and the spectrum has a peak for each note. */
export const Chord: Story = { args: { view: "spectrum", read: () => sound([130.81, 164.81, 196]) } };

export const ChordWave: Story = { args: { read: () => sound([130.81, 164.81, 196]) } };

/** Nothing played: a line across the middle, and a spectrum at the floor. */
export const Silent: Story = { args: { read: () => sound([]) } };

export const SilentSpectrum: Story = { args: { view: "spectrum", read: () => sound([]) } };

/** No frame yet. */
export const NothingToShow: Story = { args: { read: () => undefined } };
