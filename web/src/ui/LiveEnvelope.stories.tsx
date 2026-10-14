import type { Meta, StoryObj } from "@storybook/react-vite";

import { LiveEnvelope } from "./LiveEnvelope";

const seconds = (): number => performance.now() / 1000;

/** What an envelope with these stages puts out when a note is played every four seconds and held for two (the dot follows it round the shape). */
const played =
  (attack: number, decay: number, sustain: number, release: number): (() => number | undefined) =>
  () => {
    const t = seconds() % 4;
    if (t < attack) return t / attack;
    if (t < attack + decay) return 1 - (1 - sustain) * ((t - attack) / decay);
    if (t < 2) return sustain;
    return Math.max(0, sustain * (1 - (t - 2) / release));
  };

const NEW = { attack: 0.01, decay: 0.3, sustain: 0.7, release: 0.5 };

const meta = {
  title: "Displays/LiveEnvelope",
  component: LiveEnvelope,
  args: { label: "ADSR shape", stages: NEW, read: played(NEW.attack, NEW.decay, NEW.sustain, NEW.release) },
  decorators: [
    (Story) => (
      <div style={{ width: 280 }}>
        <Story />
      </div>
    ),
  ],
} satisfies Meta<typeof LiveEnvelope>;

export default meta;
type Story = StoryObj<typeof meta>;

/** A new envelope, with a note played every four seconds: the dot goes up the attack (a hair of a stage), down the decay, along the hold and down the release. */
export const Played: Story = {};

/** At rest: the dot is at the start. */
export const AtRest: Story = { args: { read: () => 0 } };

/** Slow stages: a long attack and a long release, which the shape shows as wide as they are long (the scale of time is a logarithm). */
export const Slow: Story = {
  args: { stages: { attack: 1.5, decay: 1, sustain: 0.4, release: 3 }, read: played(1.5, 1, 0.4, 3) },
};

/** A pluck: no sustain, so the decay goes all the way down. */
export const Pluck: Story = {
  args: { stages: { attack: 0.002, decay: 0.8, sustain: 0, release: 0.1 }, read: played(0.002, 0.8, 0, 0.1) },
};

/** A pad: the sustain at the top, and a long release. */
export const Pad: Story = {
  args: { stages: { attack: 0.4, decay: 0.01, sustain: 1, release: 4 }, read: played(0.4, 0.01, 1, 4) },
};

/** Nothing to follow: the shape, and no dot. */
export const NothingToShow: Story = { args: { read: () => undefined } };
