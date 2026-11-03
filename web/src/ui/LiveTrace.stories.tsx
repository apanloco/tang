import type { Meta, StoryObj } from "@storybook/react-vite";

import { LiveTrace } from "./LiveTrace";

const seconds = (): number => performance.now() / 1000;

/** A value that goes round (a stand-in for telemetry). */
const sine =
  (hertz: number, low: number, high: number): (() => number | undefined) =>
  () =>
    low + ((high - low) * (1 + Math.sin(seconds() * hertz * 2 * Math.PI))) / 2;

/** A value whose rate is driven by another that goes round: it wanders, as an LFO does when a modulator drives its rate. */
const wandering = (): number => Math.sin(2 * Math.PI * (0.6 * seconds() + 0.5 * Math.sin(2 * Math.PI * 0.11 * seconds())));

/** What an envelope puts out when a note is played every three seconds and held for one. */
const envelope = (): number => {
  const t = seconds() % 3;
  if (t < 0.1) return t / 0.1;
  if (t < 0.5) return 1 - (0.3 * (t - 0.1)) / 0.4;
  if (t < 1.1) return 0.7;
  return Math.max(0, 0.7 * (1 - (t - 1.1) / 0.8));
};

const meta = {
  title: "Displays/LiveTrace",
  component: LiveTrace,
  args: { label: "Output", min: -1, max: 1, centred: true, read: sine(0.5, -1, 1) },
  decorators: [
    (Story) => (
      <div style={{ width: 280 }}>
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof LiveTrace>;

export default meta;
type Story = StoryObj<typeof meta>;

/** What a LFO puts out: -1 to 1, with a mark across the middle. */
export const Lfo: Story = {};

/** An LFO whose rate another modulator drives: the trace is how it is seen to wander. */
export const Wandering: Story = { args: { read: wandering } };

/** What an envelope puts out: 0 to 1, as notes are played. */
export const Envelope: Story = { args: { min: 0, max: 1, centred: false, read: envelope } };

/** Less of the past, and more. */
export const OneSecond: Story = { args: { seconds: 1, read: sine(2, -1, 1) } };

export const TenSeconds: Story = { args: { seconds: 10, read: wandering } };

/** A value that does not move is drawn once. */
export const Flat: Story = { args: { read: () => 0.25 } };

export const NothingToShow: Story = { args: { read: () => undefined } };

/** Many at once, each on its own rate: what a rack of modulators looks like. */
export const ThirtyTwo: Story = {
  render: () => (
    <div style={{ display: "grid", gap: 6, width: 280 }}>
      {Array.from({ length: 32 }, (_, index) => (
        <LiveTrace key={index} label={`Output ${String(index + 1)}`} min={-1} max={1} centred read={sine(0.2 + index * 0.13, -1, 1)} />
      ))}
    </div>
  ),
};
