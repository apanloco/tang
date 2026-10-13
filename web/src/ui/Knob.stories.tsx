import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";

import { Knob } from "./Knob";
import type { KnobProps } from "./Knob";

/** A knob that keeps its own value, as the app's parameters do. */
function Interactive({ value: start, onChange, ...props }: KnobProps) {
  const [value, setValue] = useState(start);
  return (
    <Knob
      {...props}
      value={value}
      onChange={(next) => {
        setValue(next);
        onChange?.(next);
      }}
    />
  );
}

/** A value that goes round, the way an LFO would take a parameter (a stand-in for telemetry). */
const wobble = (centre: number, depth: number, hertz: number) => (): number => centre + depth * Math.sin((performance.now() / 1000) * hertz * 2 * Math.PI);

const meta = {
  title: "Controls/Knob",
  component: Knob,
  args: { label: "Cutoff", value: 0.5, min: 0, max: 1, defaultValue: 0.5 },
  render: (args) => <Interactive {...args} />,
} satisfies Meta<typeof Knob>;

export default meta;
type Story = StoryObj<typeof meta>;

/** Drag up and down (shift for fine), use the arrows, Home and End, double click to reset. */
export const Default: Story = {};

export const AtTheEnds: Story = {
  render: () => (
    <div style={{ display: "flex", gap: 16 }}>
      <Interactive label="Min" value={0} min={0} max={1} />
      <Interactive label="Middle" value={0.5} min={0} max={1} />
      <Interactive label="Max" value={1} min={0} max={1} />
    </div>
  ),
};

export const Bipolar: Story = {
  args: { label: "Detune", value: 0, min: -24, max: 24, defaultValue: 0, format: (value) => `${value.toFixed(1)} st` },
};

/** The ring is how far modulation swings the value; the dot is where it is now. */
export const Modulated: Story = {
  args: { modulation: { low: 0.3, high: 0.7 }, readEffective: wobble(0.5, 0.2, 0.5) },
};

export const ModulatedBeyondTheEnds: Story = {
  args: { value: 0.9, modulation: { low: 0.7, high: 1.1 }, readEffective: wobble(0.9, 0.2, 0.8) },
};

/** A knob with no way to say what it wants only shows (an LFO's rate, until it can be set). */
export const ReadOnly: Story = {
  render: () => <Knob label="Rate" value={0.5} min={0} max={1} format={(value) => `${value.toFixed(2)} Hz`} />,
};

export const Disabled: Story = {
  args: { disabled: true },
};

/** What a page of them costs: 64 knobs, every dot moving on every frame. */
export const SixtyFour: Story = {
  render: () => (
    <div style={{ display: "grid", gridTemplateColumns: "repeat(8, auto)", gap: 8, width: "fit-content" }}>
      {Array.from({ length: 64 }, (_, index) => (
        <Interactive
          key={index}
          label={`Knob ${String(index + 1)}`}
          value={0.5}
          min={0}
          max={1}
          defaultValue={0.5}
          modulation={{ low: 0.3, high: 0.7 }}
          readEffective={wobble(0.5, 0.2, 0.25 + index / 16)}
        />
      ))}
    </div>
  ),
};
