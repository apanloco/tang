import type { Meta, StoryObj } from "@storybook/react-vite";

import { Meter } from "./Meter";

/** A peak that falls the way the engine's does (20 dB in half a second) and is hit now and then. */
function fallingPeak(): { readPeak: () => number; readClips: () => number } {
  let level = 0;
  let clips = 0;
  let last = performance.now();
  return {
    readPeak: () => {
      const now = performance.now();
      const seconds = (now - last) / 1000;
      last = now;
      level *= 10 ** ((-40 * seconds) / 20);
      if (Math.random() < 0.02) level = Math.max(level, 0.05 + Math.random() * 1.1);
      if (level > 1 && Math.random() < 0.1) clips += 1;
      return level;
    },
    readClips: () => clips,
  };
}

const meta = {
  title: "Displays/Meter",
  component: Meter,
  args: { label: "Master", ...fallingPeak() },
  decorators: [
    (Story) => (
      <div style={{ width: 260 }}>
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof Meter>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Hit at random, falling in between; now and then it clips, and the light stays on for two seconds. */
export const Playing: Story = {};

export const Silent: Story = { args: { readPeak: () => 0, readClips: () => 0 } };

export const AtFullScale: Story = { args: { readPeak: () => 1, readClips: () => 0 } };

export const Clipping: Story = { args: { readPeak: () => 1.2, readClips: () => 0 } };

export const WithoutAClipLight: Story = { args: { readPeak: () => 0.3 }, render: (args) => <Meter label={args.label} readPeak={args.readPeak} /> };
