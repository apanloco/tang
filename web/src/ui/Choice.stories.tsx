import type { Meta, StoryObj } from "@storybook/react-vite";
import { useState } from "react";

import { Choice } from "./Choice";
import type { ChoiceProps } from "./Choice";

/** A choice that keeps its own value, as the app's parameters do. */
function Interactive({ value: start, onChange, ...props }: ChoiceProps) {
  const [value, setValue] = useState(start);
  return (
    <Choice
      {...props}
      value={value}
      onChange={(index) => {
        setValue(index);
        onChange?.(index);
      }}
    />
  );
}

const meta = {
  title: "Controls/Choice",
  component: Choice,
  args: { label: "Type", labels: ["lowpass", "highpass", "bandpass", "notch", "ladder", "acid", "ms20"], value: 0 },
  render: (args) => <Interactive {...args} />,
} satisfies Meta<typeof Choice>;

export default meta;
type Story = StoryObj<typeof meta>;

export const FilterType: Story = {};

export const Waveform: Story = { args: { label: "Waveform", labels: ["sine", "triangle", "square", "saw"], value: 3 } };

export const ReadOnly: Story = { render: (args) => <Choice label={args.label} labels={args.labels} value={args.value} /> };
