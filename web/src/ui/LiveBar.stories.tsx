import type { Meta, StoryObj } from "@storybook/react-vite";

import { LiveBar } from "./LiveBar";

/** A value that goes round (a stand-in for telemetry). */
const sine =
  (hertz: number, low: number, high: number): (() => number | undefined) =>
  () =>
    low + ((high - low) * (1 + Math.sin((performance.now() / 1000) * hertz * 2 * Math.PI))) / 2;

const meta = {
  title: "Displays/LiveBar",
  component: LiveBar,
  args: { label: "Output", min: 0, max: 1, read: sine(0.5, 0, 1) },
  decorators: [
    (Story) => (
      <div style={{ width: 240 }}>
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof LiveBar>;

export default meta;
type Story = StoryObj<typeof meta>;

/** What an envelope puts out: 0 to 1. */
export const FromTheLeft: Story = {};

/** What a LFO puts out: -1 to 1, drawn from the middle. */
export const FromTheMiddle: Story = { args: { min: -1, max: 1, centred: true, read: sine(0.5, -1, 1) } };

export const NothingToShow: Story = { args: { read: () => undefined } };

export const Full: Story = { args: { read: () => 1 } };

/** Many at once, each on its own rate: what a rack of modulators looks like. */
export const ThirtyTwo: Story = {
  render: () => (
    <div style={{ display: "grid", gap: 6, width: 240 }}>
      {Array.from({ length: 32 }, (_, index) => (
        <LiveBar key={index} label={`Modulator ${String(index + 1)}`} min={-1} max={1} centred read={sine(0.2 + index / 8, -1, 1)} />
      ))}
    </div>
  ),
};
